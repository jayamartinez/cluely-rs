//! The overlay's side of calendars (`crate::calendar`): Google sign-in and sign-out, the sync
//! schedule, and the reminders behind the upcoming-meeting row (`upcoming`). Settings › Calendar
//! draws from and acts on this; both platforms share it.
//!
//! Credential-store reads and writes, the loopback listener and every request run off the UI
//! thread. Tokens never reach settings.json, the UI or logs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{AppContext, Context, Entity, Window};

use super::Overlay;
use crate::calendar::google::{self, SyncError, Synced};
use crate::calendar::oauth::{self, AccessToken, AuthError, Endpoints, Loopback, Pkce};
use crate::calendar::reminders::Reminders;
use crate::calendar::{self, CalendarInfo, CalendarSettings, Event};
use crate::input::{InputEvent, TextInput};
use crate::settings_view::Tab;
use crate::{archive, platform, secrets};

/// How often reminders are looked at (the row's countdown moves with it).
const TICK: Duration = Duration::from_secs(10);
/// Events are read again this often while signed in, and soon after launch.
const SYNC_EVERY: i64 = 5 * 60;
const FIRST_SYNC_AFTER: i64 = 5;
/// A rate-limited sync backs off, doubling up to this.
const MAX_BACKOFF: i64 = 30 * 60;
/// A tick this late means the PC slept: sync now.
const RESUMED_AFTER: i64 = 60;

/// Calendars as the overlay keeps them, with Settings › Calendar's own fields.
pub(crate) struct CalendarState {
    /// The signed-in account's calendars, from the last sync.
    pub(crate) calendars: Vec<CalendarInfo>,
    /// The chosen calendars' events for the next day, soonest first.
    pub(crate) events: Vec<Event>,
    pub(crate) synced_at: Option<i64>,
    pub(crate) sync_error: Option<SyncError>,
    pub(crate) syncing: bool,
    /// Only in memory; refreshed from the stored refresh token when missing or stale.
    access: Option<AccessToken>,
    next_sync: i64,
    backoff: i64,
    last_tick: i64,
    /// While the browser is open for sign-in: set to stop the loopback listener.
    pub(crate) signing_in: Option<Arc<AtomicBool>>,
    /// Why the last sign-in, sign-out or Advanced save didn't work.
    pub(crate) error: Option<String>,
    pub(crate) reminders: Reminders,
    /// Advanced: use your own Google client ID.
    pub(crate) advanced_open: bool,
    pub(crate) client_id_input: Entity<TextInput>,
    pub(crate) client_secret_input: Entity<TextInput>,
    /// The reminder whose lead-time list is open (by position), and the one whose minutes are being
    /// typed (by its minutes: the list re-sorts as they change).
    pub(crate) open_reminder: Option<usize>,
    pub(crate) typed_reminder: Option<u32>,
    pub(crate) reminder_input: Entity<TextInput>,
}

impl CalendarState {
    pub(super) fn new(settings: &CalendarSettings, window: &mut Window, cx: &mut Context<Overlay>) -> Self {
        let client_id_input = cx.new(|cx| { let mut input = TextInput::new("…apps.googleusercontent.com", cx); input.set_text(settings.google_client_id.clone(), cx); input });
        let client_secret_input = cx.new(|cx| TextInput::new("Client secret", cx).masked(true));
        let reminder_input = cx.new(|cx| TextInput::new("2", cx));
        cx.subscribe_in(&reminder_input, window, |this, input, event, window, cx| {
            if !matches!(event, InputEvent::Changed | InputEvent::Submit) { return; }
            let Some(typed) = this.calendar.typed_reminder else { return };
            if let Ok(minutes) = input.read(cx).text().trim().parse::<u32>() { this.retype_reminder(typed, minutes, window, cx); }
            if matches!(event, InputEvent::Submit) { this.calendar.typed_reminder = None; cx.notify(); }
        }).detach();
        cx.subscribe_in(&client_id_input, window, |this, _, event, window, cx| { if matches!(event, InputEvent::Submit) { this.save_own_client(window, cx); } }).detach();
        cx.subscribe_in(&client_secret_input, window, |this, _, event, window, cx| { if matches!(event, InputEvent::Submit) { this.save_own_client(window, cx); } }).detach();
        let now = archive::unix_now() as i64;
        Self { calendars: Vec::new(), events: Vec::new(), synced_at: None, sync_error: None, syncing: false, access: None,
            next_sync: now + FIRST_SYNC_AFTER, backoff: SYNC_EVERY, last_tick: now, signing_in: None, error: None, reminders: Reminders::default(),
            advanced_open: false, client_id_input, client_secret_input, open_reminder: None, typed_reminder: None, reminder_input }
    }
}

impl Overlay {
    /// Look at reminders every few seconds and sync while signed in: every 5 minutes, soon after
    /// launch, and on waking from sleep.
    pub(super) fn start_calendar(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            if this.update_in(cx, |this, window, cx| this.calendar_tick(window, cx)).is_err() { break; }
        }).detach();
    }

    fn calendar_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = archive::unix_now() as i64;
        let state = &mut self.calendar;
        if now - state.last_tick > RESUMED_AFTER { state.next_sync = now; }
        state.last_tick = now;
        if self.store.value.calendar.google_account.is_some() && now >= state.next_sync { self.sync_calendar(window, cx); }
        self.tick_reminders(now, window, cx);
    }

    /// Bring the upcoming-meeting row up to date. A new row only appears while the overlay is on
    /// screen and Live isn't running; nothing here ever shows, raises or focuses the overlay.
    fn tick_reminders(&mut self, now: i64, window: &mut Window, cx: &mut Context<Self>) {
        let can_show = self.live_since.is_none() && self.native.is_some_and(platform::is_visible);
        let changed = self.calendar.reminders.tick(&self.calendar.events, &self.store.value.calendar.reminders, now, can_show);
        if changed { self.fit(window); }
        // The row's countdown ("in 2 min") moves on.
        if changed || self.calendar.reminders.current().is_some() { cx.notify(); }
    }

    /// Settings › Calendar opened: read the calendars again unless that was just done.
    pub(crate) fn calendar_opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        (self.calendar.open_reminder, self.calendar.typed_reminder) = (None, None);
        let stale = self.calendar.synced_at.is_none_or(|at| archive::unix_now() as i64 - at > 60);
        if self.store.value.calendar.google_account.is_some() && stale { self.sync_calendar(window, cx); }
    }

    /// Read the calendars and events now, off the UI thread. Does nothing while a sync runs.
    pub(crate) fn sync_calendar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.calendar.syncing || self.calendar.signing_in.is_some() { return; }
        self.calendar.syncing = true;
        let settings = self.store.value.calendar.clone();
        let access = self.calendar.access.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (access, result) = cx.background_executor().spawn(async move { sync_now(&settings, access) }).await;
            let _ = this.update_in(cx, |this, window, cx| this.synced(access, result, window, cx));
        }).detach();
    }

    fn synced(&mut self, access: Option<AccessToken>, result: Result<Synced, SyncError>, window: &mut Window, cx: &mut Context<Self>) {
        let now = archive::unix_now() as i64;
        let state = &mut self.calendar;
        state.syncing = false;
        state.access = access;
        // Signed out meanwhile: drop what came back.
        if self.store.value.calendar.google_account.is_none() { return; }
        match result {
            Ok(synced) => {
                state.backoff = SYNC_EVERY;
                state.next_sync = now + SYNC_EVERY;
                state.synced_at = Some(now);
                state.sync_error = None;
                if let Some(account) = synced.account().map(str::to_owned) {
                    self.update_settings(|s| s.calendar.google_account = Some(account), window, cx);
                }
                let state = &mut self.calendar;
                state.calendars = synced.calendars;
                state.events = synced.events;
            }
            Err(error) => {
                if error == SyncError::RateLimited { state.backoff = (state.backoff * 2).min(MAX_BACKOFF); }
                state.next_sync = now + state.backoff;
                if matches!(error, SyncError::SignInAgain) { state.access = None; }
                eprintln!("calendar sync failed: {error:?}");
                state.sync_error = Some(error);
            }
        }
        self.tick_reminders(now, window, cx);
        cx.notify();
    }

    /// "Sign in with Google": open the browser at Google's consent page and wait for it to come
    /// back to the loopback listener. Without a client ID, Advanced opens instead (the page puts
    /// the keyboard in its Client ID field).
    pub(crate) fn calendar_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.calendar.signing_in.is_some() { return; }
        let own_id = self.store.value.calendar.google_client_id.clone();
        if !oauth::has_client(&own_id) {
            self.calendar.advanced_open = true;
            cx.notify();
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.calendar.signing_in = Some(cancel.clone());
        self.calendar.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let started = cx.background_executor().spawn(async move { begin_sign_in(&own_id) }).await;
            let begun = match started {
                Ok(begun) => begun,
                Err(error) => { let _ = this.update_in(cx, |this, window, cx| this.signed_in(&cancel, Err(error), window, cx)); return; }
            };
            // The browser opens from here, on the UI thread, as every other link does.
            if cx.update(|_, cx| cx.open_url(&begun.url)).is_err() { return; }
            let flag = cancel.clone();
            let result = cx.background_executor().spawn(async move { finish_sign_in(begun, &flag) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let signed_in = result.is_ok();
                this.signed_in(&cancel, result, window, cx);
                if signed_in { this.sync_calendar(window, cx); }
            });
        }).detach();
    }

    /// A sign-in finished. One that was cancelled (and maybe replaced by a newer one) changes nothing.
    /// Either way Settings › Calendar is brought forward from the browser, as far as Windows allows.
    fn signed_in(&mut self, attempt: &Arc<AtomicBool>, result: Result<(AccessToken, Option<Synced>), AuthError>, window: &mut Window, cx: &mut Context<Self>) {
        if attempt.load(Ordering::SeqCst) { return; }
        if self.calendar.signing_in.as_ref().is_some_and(|current| Arc::ptr_eq(current, attempt)) { self.calendar.signing_in = None; }
        match result {
            Ok((access, synced)) => {
                self.calendar.access = Some(access);
                self.calendar.sync_error = None;
                let account = synced.as_ref().and_then(Synced::account).unwrap_or_default().to_owned();
                self.store.value.calendar.google_account = Some(account);
                self.store.save();
                if let Some(synced) = synced {
                    self.calendar.calendars = synced.calendars;
                    self.calendar.synced_at = Some(archive::unix_now() as i64);
                }
            }
            Err(AuthError::Cancelled) => {}
            Err(error) => {
                eprintln!("google sign-in failed: {error:?}");
                self.calendar.error = Some(error.message());
            }
        }
        self.show_calendar_settings(window, cx);
        cx.notify();
    }

    /// Settings › Calendar in front: for a `cluelyrs://calendar` link ("Back to CluelyRS") or a sign-in
    /// that just finished. Only the Settings window comes forward, never the overlay. On Windows an
    /// open Settings window is raised without simulated input (`platform::raise`): GPUI's own
    /// activation presses Alt, which would reach the browser in front.
    pub(crate) fn show_calendar_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(windows)]
        if self.fluent.window.is_some() {
            if self.fluent.tab != Some(Tab::Calendar) || self.fluent.mode_page.is_some() {
                (self.fluent.tab, self.fluent.mode_page) = (Some(Tab::Calendar), None);
                self.fluent.scroll_to_top();
                self.prepare_tab(Tab::Calendar, window, cx);
            }
            cx.notify();
            self.raise_settings(cx);
            return;
        }
        self.open_settings(Tab::Calendar, window, cx);
        // A new window opens behind the browser: raise it once it's there.
        #[cfg(windows)]
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let _ = this.update(cx, |this, cx| this.raise_settings(cx));
        }).detach();
    }

    /// Bring the open Settings window to the front (`platform::raise`).
    #[cfg(windows)]
    fn raise_settings(&self, cx: &mut Context<Self>) {
        let Some(handle) = self.fluent.window else { return };
        cx.defer(move |cx| { let _ = handle.update(cx, |_, window, _| if let Some(hwnd) = platform::native_window(window) { platform::raise(hwnd); }); });
    }

    /// Cancel on "Finish signing in in your browser…": the listener stops within a moment.
    pub(crate) fn cancel_calendar_sign_in(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.calendar.signing_in.take() { cancel.store(true, Ordering::SeqCst); }
        cx.notify();
    }

    /// Revoke the grant at Google, delete the refresh token, and forget the account's calendars.
    /// Offline, the token is still deleted here; Google drops unused grants after six months.
    pub(crate) fn calendar_sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                if let Some(token) = secrets::get(oauth::REFRESH_TOKEN_ID) && let Err(error) = oauth::revoke(&Endpoints::google(), &token) {
                    eprintln!("google token not revoked: {error:?}");
                }
                secrets::remove(oauth::REFRESH_TOKEN_ID)
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if result.is_err() {
                    this.calendar.error = Some(format!("Couldn't remove the sign-in from {}.", secrets::STORE_NAME));
                    cx.notify();
                    return;
                }
                let state = &mut this.calendar;
                (state.calendars, state.events, state.access, state.synced_at, state.sync_error, state.error) = (Vec::new(), Vec::new(), None, None, None, None);
                state.reminders.clear_rows();
                this.update_settings(|s| { s.calendar.google_account = None; s.calendar.google_calendars = None; }, window, cx);
                this.fit(window);
                cx.notify();
            });
        }).detach();
    }

    /// Tick or untick a calendar; its events are read (or dropped) with a sync right away.
    pub(crate) fn toggle_calendar(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let settings = &self.store.value.calendar;
        let mut chosen: Vec<String> = self.calendar.calendars.iter().filter(|calendar| settings.uses(calendar)).map(|calendar| calendar.id.clone()).collect();
        if let Some(index) = chosen.iter().position(|chosen| chosen == id) { chosen.remove(index); } else { chosen.push(id.to_owned()); }
        self.update_settings(|s| s.calendar.google_calendars = Some(chosen), window, cx);
        self.calendar.next_sync = 0;
        self.sync_calendar(window, cx);
    }

    /// Advanced › Save: the user's own client ID (in settings.json) and secret (in the credential
    /// store). An empty ID goes back to the built-in one and deletes the saved secret.
    pub(crate) fn save_own_client(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.calendar.client_id_input.read(cx).text().trim().to_owned();
        let secret = self.calendar.client_secret_input.read(cx).text().trim().to_owned();
        if !id.is_empty() && !oauth::plausible_client_id(&id) {
            self.calendar.error = Some("That isn't a Desktop client ID (…apps.googleusercontent.com).".into());
            cx.notify();
            return;
        }
        self.calendar.error = None;
        self.calendar.client_secret_input.update(cx, |input, cx| input.clear(cx));
        cx.spawn_in(window, async move |this, cx| {
            let (saved_id, store_secret) = (id.clone(), secret.clone());
            let result = cx.background_executor().spawn(async move {
                if saved_id.is_empty() { secrets::remove(oauth::OWN_SECRET_ID).map(|()| None) }
                else if store_secret.is_empty() { Ok(None) }
                else { secrets::set(oauth::OWN_SECRET_ID, &store_secret).map(|()| secrets::masked(&store_secret)) }
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(hint) => this.update_settings(|s| {
                        s.calendar.google_client_id = id.clone();
                        if id.is_empty() { s.saved_keys.remove(oauth::OWN_SECRET_ID); }
                        if let Some(hint) = hint { s.saved_keys.insert(oauth::OWN_SECRET_ID.into(), hint); }
                    }, window, cx),
                    Err(_) => this.calendar.error = Some(format!("Couldn't save the client secret to {}.", secrets::STORE_NAME)),
                }
                cx.notify();
            });
        }).detach();
    }

    /// Set reminder `index` (or add one, past the end) to `minutes`, kept in order without repeats,
    /// and close its list.
    pub(crate) fn set_reminder(&mut self, index: usize, minutes: u32, window: &mut Window, cx: &mut Context<Self>) {
        let mut leads = self.store.value.calendar.reminders.clone();
        if index < leads.len() { leads[index] = minutes; } else { leads.push(minutes); }
        let leads = CalendarSettings::normalize_reminders(leads);
        (self.calendar.open_reminder, self.calendar.typed_reminder) = (None, None);
        self.update_settings(|s| s.calendar.reminders = leads, window, cx);
        cx.notify();
    }

    /// The typed minutes changed: the reminder at `old` minutes now has `minutes`. Out of range, or
    /// the same as another reminder, it waits for more typing.
    fn retype_reminder(&mut self, old: u32, minutes: u32, window: &mut Window, cx: &mut Context<Self>) {
        let leads = &self.store.value.calendar.reminders;
        if minutes == old || !(1..=calendar::MAX_LEAD_MINUTES).contains(&minutes) || leads.contains(&minutes) { return; }
        let leads = CalendarSettings::normalize_reminders(leads.iter().map(|lead| if *lead == old { minutes } else { *lead }));
        self.calendar.typed_reminder = Some(minutes);
        self.update_settings(|s| s.calendar.reminders = leads, window, cx);
        cx.notify();
    }

    pub(crate) fn remove_reminder(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mut leads = self.store.value.calendar.reminders.clone();
        if index < leads.len() { leads.remove(index); }
        (self.calendar.open_reminder, self.calendar.typed_reminder) = (None, None);
        self.update_settings(|s| s.calendar.reminders = leads, window, cx);
        cx.notify();
    }
}

/// What the browser is sent to, and what's kept to finish once it comes back.
struct Begun {
    url: String,
    loopback: Loopback,
    pkce: Pkce,
    state: String,
    client: oauth::Client,
}

fn begin_sign_in(own_id: &str) -> Result<Begun, AuthError> {
    let client = oauth::resolve_client(own_id, || secrets::get(oauth::OWN_SECRET_ID)).ok_or(AuthError::BadClient)?;
    let loopback = Loopback::bind()?;
    let (pkce, state) = (Pkce::generate(), oauth::new_state());
    let url = oauth::auth_url(&Endpoints::google(), &client.id, &loopback.redirect_uri(), &pkce.challenge, &state);
    Ok(Begun { url, loopback, pkce, state, client })
}

/// Wait for the redirect (until cancelled: there is no time limit while Settings shows "Signing
/// in…"), trade the code for tokens, store the refresh token, and read the calendars once for the
/// account's address. The listener then answers late callbacks for a while (`Loopback::linger`).
fn finish_sign_in(begun: Begun, cancel: &AtomicBool) -> Result<(AccessToken, Option<Synced>), AuthError> {
    let redirect_uri = begun.loopback.redirect_uri();
    let code = begun.loopback.wait(&begun.state, cancel)?;
    let loopback = begun.loopback;
    let _ = std::thread::Builder::new().name("cluelyrs-sign-in-linger".into()).spawn(move || loopback.linger(oauth::LINGER));
    let tokens = oauth::exchange_code(&Endpoints::google(), &begun.client, &code, &begun.pkce.verifier, &redirect_uri)?;
    if cancel.load(Ordering::SeqCst) { return Err(AuthError::Cancelled); }
    secrets::set(oauth::REFRESH_TOKEN_ID, &tokens.refresh).map_err(|_| AuthError::NotSaved)?;
    let (access, synced) = google::sync_account(&Endpoints::google(), google::API, &begun.client, &tokens.refresh, Some(tokens.access.clone()), &|_| false, archive::unix_now() as i64);
    Ok((access.unwrap_or(tokens.access), synced.ok()))
}

/// One sync with the stored refresh token. Blocking.
fn sync_now(settings: &CalendarSettings, access: Option<AccessToken>) -> (Option<AccessToken>, Result<Synced, SyncError>) {
    let Some(client) = oauth::resolve_client(&settings.google_client_id, || secrets::get(oauth::OWN_SECRET_ID)) else { return (None, Err(SyncError::BadClient)) };
    let Some(refresh) = secrets::get(oauth::REFRESH_TOKEN_ID) else { return (None, Err(SyncError::SignInAgain)) };
    google::sync_account(&Endpoints::google(), google::API, &client, &refresh, access, &|calendar| settings.uses(calendar), archive::unix_now() as i64)
}
