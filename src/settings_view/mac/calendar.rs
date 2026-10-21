//! Settings › Calendar: Google, signed in with the browser, then the account's calendars to read;
//! the user's own client ID; and the reminders, each showing its own upcoming-meeting row on the
//! overlay. The state and what each control does are the shared ones in `overlay::calendar`.

use gpui::{AnyElement, Context, Div, ImageSource, IntoElement, MouseButton, MouseDownEvent, ParentElement, Resource, Rgba, Styled, deferred, div, img, prelude::*, px, rgb};

use super::{action, label, link, menu, menu_row, row, section, status, text_field};
use crate::calendar::google::SyncError;
use crate::calendar::{self, CalendarInfo, MAX_REMINDERS, oauth};
use crate::overlay::Overlay;
use crate::settings_view::about::spinner;
use crate::settings_view::{MENU_MAX_HEIGHT, listen};
use crate::ui_v3::{self, Kind};
use crate::{archive, theme, ui};

/// "Sign-in needs a client ID", "No client ID set".
const WARNING: u32 = 0xe8c46a;
const GOOGLE_CLOUD: &str = "https://console.cloud.google.com/apis/credentials";
/// The lead times offered, in minutes; "Custom…" types any other.
const LEADS: [u32; 10] = [1, 2, 3, 5, 10, 15, 30, 45, 60, 120];
/// What "Add reminder" picks: the first of these not already set.
const NEXT_LEADS: [u32; 9] = [30, 10, 5, 15, 60, 1, 3, 45, 120];
const CUSTOM: u32 = 0;
const LEAD_WIDTH: f32 = 170.0;

fn logo(path: &'static str, size: f32) -> impl IntoElement {
    img(ImageSource::Resource(Resource::Embedded(path.into()))).size(px(size)).flex_none()
}

fn colored(text: impl Into<gpui::SharedString>, color: Rgba) -> AnyElement {
    div().text_size(px(12.0)).line_height(px(16.0)).text_color(color).child(text.into()).into_any_element()
}

/// A status with a dot before it, in the dot's colour ("● Synced 2 min ago").
fn dot_line(text: impl Into<gpui::SharedString>, color: Rgba) -> AnyElement {
    div().flex().items_center().gap(px(6.0)).child(div().size(px(6.0)).flex_none().rounded(px(3.0)).bg(color))
        .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(color).child(text.into())).into_any_element()
}

fn checkbox(checked: bool) -> Div {
    let tile = div().size(px(18.0)).flex_none().flex().items_center().justify_center().rounded(px(4.0));
    if checked { tile.bg(theme::accent()).child(ui::icon("icons/check.svg", 12.0, theme::accent_ink())) }
    else { tile.border_1().border_color(theme::muted()) }
}

/// A row whose tile holds a logo instead of an icon.
fn logo_row(path: &'static str, title: impl Into<gpui::SharedString>, status: Option<AnyElement>, control: impl IntoElement) -> Div {
    div().flex().items_center().gap(px(14.0)).px(px(12.0)).py(px(8.0))
        .child(ui_v3::tile(40.0, false).child(logo(path, 20.0)))
        .child(label(title, status))
        .child(control)
}

/// Google's dark "Sign in with Google" button, as its branding guidelines draw it.
fn google_button() -> gpui::Stateful<Div> {
    div().id("google-sign-in").flex().flex_none().items_center().gap(px(10.0)).h(px(36.0)).px(px(12.0)).rounded(px(4.0)).cursor_pointer()
        .bg(rgb(0x131314)).border_1().border_color(rgb(0x8e918f))
        .child(logo("logos/google-g.svg", 18.0))
        .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(gpui::FontWeight::MEDIUM).text_color(rgb(0xe3e3e3)).child("Sign in with Google"))
}

impl Overlay {
    pub(super) fn mac_calendar_tab(&self, cx: &mut Context<Self>) -> Div {
        let mut google = vec![self.mac_google_row(cx)];
        if self.store.value.calendar.google_account.is_some() {
            if self.calendar.calendars.is_empty() {
                google.push(div().pl(px(66.0)).py(px(4.0)).child(status(if self.calendar.syncing { "Loading your calendars…" } else { "No calendars yet" })).into_any_element());
            }
            google.extend(self.calendar.calendars.iter().enumerate().map(|(index, info)| self.mac_calendar_row(index, info, cx)));
        }
        if let Some(error) = self.calendar.error.clone() { google.push(div().pl(px(66.0)).child(colored(error, theme::danger())).into_any_element()); }
        google.push(self.mac_advanced(cx));

        let leads = &self.store.value.calendar.reminders;
        let mut reminders: Vec<AnyElement> = leads.iter().enumerate().map(|(index, lead)| self.mac_reminder_row(index, *lead, cx)).collect();
        if leads.len() < MAX_REMINDERS {
            reminders.push(row("icons/plus.svg", "Add reminder", Some(status(format!("{} of {MAX_REMINDERS} · each shows its own row on the overlay", leads.len()))),
                action("reminder-add", "Add", Kind::Secondary, cx, |this, window, cx| {
                    let leads = this.store.value.calendar.reminders.clone();
                    let Some(next) = NEXT_LEADS.into_iter().find(|lead| !leads.contains(lead)) else { return };
                    this.set_reminder(leads.len(), next, window, cx);
                })).into_any_element());
        }
        div().flex().flex_col().gap(px(30.0))
            .child(section("Google Calendar", 12.0, google))
            .child(section("Reminders", 12.0, reminders))
            .child(div().px(px(12.0)).child(status("CluelyRS only reads your calendars, on this Mac, to name sessions and remind you before meetings.")))
    }

    /// Not signed in, signing in in the browser, or the signed-in account.
    fn mac_google_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.store.value.calendar;
        if self.calendar.signing_in.is_some() {
            return logo_row("logos/google-calendar.svg", "Finish signing in in your browser…", Some(status("Google opened in your default browser")),
                div().flex().items_center().gap(px(10.0)).child(spinner())
                    .child(action("google-cancel", "Cancel", Kind::Secondary, cx, |this, _, cx| this.cancel_calendar_sign_in(cx)))).into_any_element();
        }
        let Some(account) = settings.google_account.clone() else {
            let has_client = oauth::has_client(&settings.google_client_id);
            let line = if has_client { status("Sign in, then choose which calendars to use") } else { colored("Sign-in needs a client ID · set one below", rgb(WARNING)) };
            let button = google_button().on_mouse_down(MouseButton::Left, listen(cx, move |this, _: &MouseDownEvent, window, cx| {
                if !oauth::has_client(&this.store.value.calendar.google_client_id) { this.calendar.advanced_open = true; cx.notify(); return; }
                this.calendar_sign_in(window, cx);
            }));
            return logo_row("logos/google-calendar.svg", "Google Calendar", Some(line), button).into_any_element();
        };
        let sync = match (&self.calendar.sync_error, self.calendar.synced_at) {
            (Some(error), _) => dot_line(error.message(), theme::danger()),
            (None, Some(at)) => dot_line(calendar::synced_label(at, archive::unix_now() as i64), theme::ok()),
            (None, None) => dot_line("Syncing…", theme::muted()),
        };
        let signed_out_by_google = self.calendar.sync_error == Some(SyncError::SignInAgain);
        let controls = div().flex().flex_none().items_center().gap(px(12.0))
            .when(signed_out_by_google, |controls| controls.child(link("google-sign-in-again", "Sign in", true)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _: &MouseDownEvent, window, cx| this.calendar_sign_in(window, cx)))))
            .child(link("google-sign-out", "Sign out", false).on_mouse_down(MouseButton::Left, listen(cx, |this, _: &MouseDownEvent, window, cx| this.calendar_sign_out(window, cx))));
        let title = if account.is_empty() { "Google account".to_string() } else { account };
        logo_row("logos/google-calendar.svg", title, Some(sync), controls).into_any_element()
    }

    fn mac_calendar_row(&self, index: usize, info: &CalendarInfo, cx: &mut Context<Self>) -> AnyElement {
        let id = info.id.clone();
        div().id(("google-calendar", index)).flex().items_center().gap(px(12.0)).pl(px(66.0)).pr(px(12.0)).py(px(6.0)).cursor_pointer()
            .rounded(px(ui_v3::RADII.small)).hover(|row| row.bg(theme::selected_fill()))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _: &MouseDownEvent, window, cx| this.toggle_calendar(&id, window, cx)))
            .child(checkbox(self.store.value.calendar.uses(info)))
            .child(div().size(px(8.0)).flex_none().rounded(px(4.0)).bg(info.color.map_or(theme::muted(), rgb)))
            .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(18.0)).text_color(theme::text()).child(info.name.clone()))
            .when(info.primary, |row| row.child(status("Primary")))
            .into_any_element()
    }

    /// Your own Google client ID: opens by itself when sign-in needs one.
    fn mac_advanced(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.store.value.calendar;
        let open = self.calendar.advanced_open;
        let which = if !oauth::has_client(&settings.google_client_id) { colored("No client ID set", rgb(WARNING)) }
            else if settings.google_client_id.trim().is_empty() { status("Using the built-in ID") } else { status("Using your own ID") };
        let toggle = action("google-advanced", if open { "Hide" } else { "Set up" }, Kind::Secondary, cx, |this, _, cx| {
            this.calendar.advanced_open = !this.calendar.advanced_open;
            cx.notify();
        });
        let header = row("icons/setup-key.svg", "Your own Google client ID", Some(which), toggle);
        if !open { return header.into_any_element(); }
        let saved_secret = self.store.value.saved_keys.get(oauth::OWN_SECRET_ID).map(|hint| format!("Saved {hint}"));
        let field = |name: &'static str, control: gpui::Stateful<Div>| div().flex().items_center().gap(px(12.0))
            .child(div().w(px(90.0)).flex_none().text_size(px(12.0)).text_color(theme::body()).child(name))
            .child(control);
        let body = div().flex().flex_col().gap(px(10.0)).pl(px(66.0)).pr(px(12.0)).pb(px(8.0))
            .child(div().flex().flex_col().gap(px(2.0)).text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted())
                .child("1. In Google Cloud Console, create a project and enable the Calendar API")
                .child("2. Create an OAuth client ID of type Desktop app")
                .child("3. On the OAuth consent screen, set publishing status to In production"))
            .child(field("Client ID", text_field(&self.calendar.client_id_input, "google-client-id")))
            .child(field("Client secret", text_field(&self.calendar.client_secret_input, "google-client-secret")))
            .children(saved_secret.map(|saved| div().pl(px(102.0)).child(status(saved))))
            .child(div().flex().items_center().gap(px(12.0))
                .child(div().flex_1().min_w_0().text_size(px(12.0)).text_color(theme::muted()).child("Your own ID overrides the built-in one."))
                .child(link("google-cloud", "Open Google Cloud ↗", true).on_mouse_down(MouseButton::Left, listen(cx, |_, _: &MouseDownEvent, _, cx| cx.open_url(GOOGLE_CLOUD))))
                .child(action("google-client-save", "Save", Kind::Primary, cx, |this, window, cx| this.save_own_client(window, cx))));
        div().flex().flex_col().child(header).child(body).into_any_element()
    }

    fn mac_reminder_row(&self, index: usize, lead: u32, cx: &mut Context<Self>) -> AnyElement {
        let control = if self.calendar.typed_reminder == Some(lead) {
            div().flex().items_center().gap(px(8.0)).w(px(LEAD_WIDTH))
                .child(text_field(&self.calendar.reminder_input, "reminder-minutes").w(px(64.0)).flex_none())
                .child(status("min before"))
        } else {
            self.mac_lead_dropdown(index, lead, cx)
        };
        let remove = div().id(("reminder-remove", index)).size(px(28.0)).flex().flex_none().items_center().justify_center().rounded(px(ui_v3::RADII.small))
            .cursor_pointer().hover(|button| button.bg(theme::selected_fill()))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _: &MouseDownEvent, window, cx| this.remove_reminder(index, window, cx)))
            .child(ui::icon("icons/close.svg", 10.0, theme::body()));
        row("icons/bell.svg", "Remind me", None, div().flex().flex_none().items_center().gap(px(8.0)).child(control).child(remove)).into_any_element()
    }

    /// A reminder's lead time: the presets and "Custom…", which types the minutes.
    fn mac_lead_dropdown(&self, index: usize, lead: u32, cx: &mut Context<Self>) -> Div {
        let open = self.calendar.open_reminder == Some(index);
        let face = ui_v3::dropdown_face(("reminder-lead", index), None, calendar::lead_label(lead), open)
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.calendar.open_reminder = if this.calendar.open_reminder == Some(index) { None } else { Some(index) };
                cx.notify();
            }));
        let mut wrapper = div().relative().w(px(LEAD_WIDTH)).flex_none().child(face);
        if !open { return wrapper; }
        let mut list = menu().id("reminder-leads").w_full().max_h(px(MENU_MAX_HEIGHT)).overflow_y_scroll()
            .on_mouse_down_out(listen(cx, |this, _: &MouseDownEvent, _, cx| { this.calendar.open_reminder = None; cx.notify(); }));
        for (position, minutes) in LEADS.into_iter().chain([CUSTOM]).enumerate() {
            let text = if minutes == CUSTOM { "Custom…".to_string() } else { calendar::lead_label(minutes) };
            let input = self.calendar.reminder_input.clone();
            list = list.child(menu_row(("reminder-lead-option", position), None, text, minutes == lead)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if minutes != CUSTOM { this.calendar.open_reminder = None; this.set_reminder(index, minutes, window, cx); return; }
                    this.calendar.open_reminder = None;
                    this.calendar.typed_reminder = Some(lead);
                    input.update(cx, |input, cx| { input.set_text(lead.to_string(), cx); input.select_all_text(cx); });
                    cx.notify();
                })));
        }
        wrapper = wrapper.child(deferred(div().absolute().top(px(36.0)).left_0().w(px(LEAD_WIDTH.max(200.0))).child(list)).with_priority(1));
        wrapper
    }
}
