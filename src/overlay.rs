//! The overlay: a draggable pill (mark · Start/live timer · Hide · Stop) above one
//! glass panel. Spike scope: layout, live state, keybind movement and scrolling.
//! Answers are placeholder text until the provider slice lands.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    ScrollHandle, SharedString, Styled, Window, div, point, prelude::*, px, size,
};
use windows::Win32::Foundation::HWND;

use crate::answer::{self, Exchange};
use crate::archive::{self, Archive, Recorder};
use crate::input::{InputEvent, TextInput};
use crate::settings::Provider;
use crate::sessions_window::{self, SessionsWindow};
use crate::hotkeys::{Action, Hotkeys};
use crate::settings::{Settings, Store};
use crate::settings_view::Tab;
use crate::theme;
use crate::ui::{self, chip, keycap};
use crate::win;

const WIDTH: f32 = 600.0;
const IDLE_HEIGHT: f32 = 120.0;
const LIVE_HEIGHT: f32 = 600.0;
/// Held movement eases in so a tap nudges, then cruises. Pixels per ~16 ms frame.
const MOVE_START: f32 = 4.0;
const MOVE_CRUISE: f32 = 22.0;
const SCROLL_CRUISE: f32 = 26.0;
const EASE_PER_FRAME: f32 = 0.6;
const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Motion { Move, Scroll }

#[derive(Clone, PartialEq)]
enum Status { Streaming, Done, Failed(SharedString) }

struct Turn {
    id: u64,
    action: SharedString,
    question: String,
    text: String,
    status: Status,
    /// Seconds into the Live session, for the saved timeline.
    at_ms: u64,
    screenshot: Option<Vec<u8>>,
}

pub struct Overlay {
    hwnd: Option<HWND>,
    pub(crate) hotkeys: Hotkeys,
    pub(crate) store: Store,
    settings_tab: Option<Tab>,
    /// The Sessions review window, while it's open.
    sessions_window: Option<gpui::WindowHandle<SessionsWindow>>,
    /// Where Live sessions are saved; `None` when no data folder is available.
    pub(crate) archive: Option<Archive>,
    /// Receives Esc while settings is open.
    focus: FocusHandle,
    /// Last window shape applied, so the region is only rebuilt when the layout changes.
    shape: Rc<RefCell<Vec<win::Shape>>>,
    /// Interactive areas laid out this frame; everything else is click-through.
    pub(crate) hits: Hits,
    composer: Entity<TextInput>,
    pub(crate) key_input: Entity<TextInput>,
    pub(crate) model_input: Entity<TextInput>,
    pub(crate) base_url_input: Entity<TextInput>,
    /// Feedback under the API key field ("Saved", errors).
    pub(crate) key_notice: Option<SharedString>,
    pub(crate) loaded_models: Vec<String>,
    pub(crate) models_loading: bool,
    /// Cancels the answer currently streaming.
    job_cancel: Option<Arc<AtomicBool>>,
    next_turn: u64,
    /// True while the cursor is over a control and the window takes mouse input.
    catching_mouse: bool,
    /// The app that had focus before the Focus shortcut, so sending or Esc can return to it.
    return_focus: Option<HWND>,
    /// The ChatGPT subscription client (one Codex app-server for the app's lifetime).
    pub(crate) codex: Arc<crate::codex::CodexClient>,
    /// Last known sign-in state for the ChatGPT and Claude subscriptions (None while checking).
    pub(crate) codex_status: Option<crate::chat::SubscriptionStatus>,
    pub(crate) claude_status: Option<crate::chat::SubscriptionStatus>,
    pub(crate) signing_in: bool,
    /// The Live session being saved to History, when saving is on.
    recorder: Option<Recorder>,
    /// The held-key loop that is currently running, if any.
    motion: Option<Motion>,
    live_since: Option<Instant>,
    turns: Vec<Turn>,
    scroll: ScrollHandle,
}

impl Overlay {
    pub fn new(hotkeys: Hotkeys, presses: UnboundedReceiver<Action>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let hwnd = win::hwnd(window);
        let store = Store::load();
        if let Some(hwnd) = hwnd {
            // Failures here leave a visible, usable window; they are not fatal.
            apply_capture_setting(hwnd, &store.value);
            win::remove_frame(hwnd);
            let _ = win::set_topmost(hwnd);
        }
        let mut hotkeys = hotkeys;
        hotkeys.set_overlay_visible(true);
        if !hotkeys.unavailable.is_empty() { eprintln!("shortcuts in use by another app: {:?}", hotkeys.unavailable); }

        cx.spawn_in(window, async move |this, cx| {
            let mut presses = presses;
            while let Some(action) = presses.next().await {
                if this.update_in(cx, |this, window, cx| this.handle(action, window, cx)).is_err() { break; }
            }
        }).detach();
        // Text, answers and the transcript line let clicks through; only controls catch them.
        if let Some(hwnd) = hwnd { win::enable_passthrough(hwnd); }
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(30)).await;
            if this.update(cx, |this, _| this.update_passthrough()).is_err() { break; }
        }).detach();
        // One-second tick for the live timer; idle ticks do no work.
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this.update(cx, |this, cx| if this.live_since.is_some() { cx.notify() }).is_err() { break; }
        }).detach();

        let archive = Archive::default_location();
        if let Some(archive) = &archive {
            // A previous run that was killed mid-session leaves an unfinished record.
            archive.recover();
            if store.value.save_sessions { archive.prune(store.value.keep_sessions, archive::unix_now()); }
        }
        let start_live = store.value.start_live_on_launch;
        let composer = cx.new(|cx| TextInput::new("Ask about your screen or conversation…", cx));
        let key_input = cx.new(|cx| TextInput::new("Paste your API key", cx).masked(true));
        let model_input = cx.new(|cx| TextInput::new("Model id, e.g. from the list below", cx));
        let base_url_input = cx.new(|cx| TextInput::new("https://your-endpoint.example/v1", cx));
        model_input.update(cx, |input, cx| input.set_text(store.value.api_model().to_string(), cx));
        base_url_input.update(cx, |input, cx| input.set_text(store.value.custom_base_url.clone(), cx));
        cx.subscribe_in(&composer, window, |this, input, event, window, cx| {
            if matches!(event, InputEvent::Submit) { this.send_composer(input.clone(), window, cx); }
        }).detach();
        cx.subscribe_in(&key_input, window, |this, _, event, _, cx| { if matches!(event, InputEvent::Submit) { this.save_key(cx); } }).detach();
        cx.subscribe_in(&model_input, window, |this, input, event, window, cx| {
            if matches!(event, InputEvent::Changed) {
                let model = input.read(cx).text().trim().to_string();
                this.update_settings(|s| { s.api_models.insert(s.api_provider.clone(), model); }, window, cx);
            }
        }).detach();
        cx.subscribe_in(&base_url_input, window, |this, input, event, window, cx| {
            if matches!(event, InputEvent::Changed) {
                let url = input.read(cx).text().trim().to_string();
                this.update_settings(|s| s.custom_base_url = url, window, cx);
            }
        }).detach();
        let mut overlay = Self { hwnd, hotkeys, store, settings_tab: None, sessions_window: None, archive, focus: cx.focus_handle(),
            recorder: None, shape: Rc::default(), hits: Hits::default(), composer, key_input, model_input, base_url_input,
            key_notice: None, loaded_models: Vec::new(), models_loading: false, job_cancel: None, next_turn: 0, catching_mouse: true, return_focus: None,
            codex: crate::codex::CodexClient::new(), codex_status: None, claude_status: None, signing_in: false,
            motion: None, live_since: None, turns: Vec::new(), scroll: ScrollHandle::new() };
        if start_live { overlay.set_live(true, window, cx); }
        overlay
    }

    pub fn update_settings(&mut self, change: impl FnOnce(&mut Settings), _window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.store.value.clone();
        change(&mut self.store.value);
        if self.store.value == previous { return; }
        self.store.save();
        if let Some(hwnd) = self.hwnd && previous.hide_from_capture != self.store.value.hide_from_capture {
            apply_capture_setting(hwnd, &self.store.value);
        }
        if previous.api_provider != self.store.value.api_provider {
            // Each provider keeps its own model; show the one saved for the new provider.
            let model = self.store.value.api_model().to_string();
            self.model_input.update(cx, |input, cx| input.set_text(model, cx));
            self.loaded_models.clear();
            self.key_notice = None;
        }
        cx.notify();
    }

    pub fn open_settings(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if tab == Tab::Model { self.refresh_subscriptions(window, cx); }
        self.settings_tab = Some(tab);
        self.open_panel(window, cx);
    }

    /// Open (or focus) the full-size Sessions review window.
    pub fn open_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.archive.as_ref().map(|archive| archive.root().to_path_buf()) else { return };
        let _ = std::fs::create_dir_all(&root);
        sessions_window::open(&mut self.sessions_window, root, self.codex.clone(), cx);
    }

    fn open_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Esc works through the global shortcut; focus also lets it work when the overlay is active.
        window.focus(&self.focus);
        self.hotkeys.set_panel_open(true);
        self.fit(window);
        cx.notify();
    }

    /// Close settings and return to the overlay.
    pub fn close_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_tab = None;
        self.hotkeys.set_panel_open(false);
        self.fit(window);
        cx.notify();
    }

    pub fn reveal_archive(&self) {
        let Some(archive) = &self.archive else { return };
        let _ = std::fs::create_dir_all(archive.root());
        if let Err(error) = std::process::Command::new("explorer.exe").arg(archive.root()).spawn() { eprintln!("could not open folder: {error}"); }
    }

    /// Size the window to its content state; transparent area outside the content still takes clicks.
    fn fit(&self, window: &mut Window) {
        let tall = self.live_since.is_some() || self.settings_tab.is_some();
        window.resize(size(px(WIDTH), px(if tall { LIVE_HEIGHT } else { IDLE_HEIGHT })));
    }

    fn handle(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Toggle => self.toggle_visible(),
            Action::Live => self.set_live(self.live_since.is_none(), window, cx),
            Action::Assist => self.send("Assist", String::new(), window, cx),
            Action::Focus => self.focus_composer(window, cx),
            Action::MoveUp | Action::MoveDown | Action::MoveLeft | Action::MoveRight => self.start_motion(Motion::Move, window, cx),
            Action::ScrollUp | Action::ScrollDown => self.start_motion(Motion::Scroll, window, cx),
            Action::Close => self.close_panels(window, cx),
        }
    }

    fn set_live(&mut self, live: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.live_since = live.then(Instant::now);
        self.hotkeys.set_live(live);
        if !live {
            if let Some(cancel) = self.job_cancel.take() { cancel.store(true, Ordering::Relaxed); }
            self.turns.clear();
        }
        self.record(live);
        if live { self.settings_tab = None; self.hotkeys.set_panel_open(false); }
        self.fit(window);
        cx.notify();
    }

    /// Open a History record when Live starts and close it when Live stops.
    fn record(&mut self, live: bool) {
        if let Some(recorder) = self.recorder.take() {
            match recorder.finish(archive::unix_now()) {
                Ok(Some(session)) => self.write_notes(session),
                Ok(None) => {}
                Err(error) => eprintln!("session could not be saved: {error}"),
            }
        }
        if live && self.store.value.save_sessions && let Some(archive) = &self.archive {
            match archive.start_now() {
                Ok(recorder) => self.recorder = Some(recorder),
                Err(error) => eprintln!("session recording unavailable: {error}"),
            }
        }
    }

    fn update_passthrough(&mut self) {
        let Some(hwnd) = self.hwnd else { return };
        let over_control = win::cursor_in_window(hwnd).is_some_and(|point| self.hits.contains(point));
        if over_control != self.catching_mouse {
            self.catching_mouse = over_control;
            win::set_mouse_passthrough(hwnd, !over_control);
        }
    }

    /// Start Live if needed, take the foreground and put the caret in the composer.
    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show();
        if self.settings_tab.is_some() { self.close_panels(window, cx); }
        if self.live_since.is_none() { self.set_live(true, window, cx); }
        if let Some(hwnd) = self.hwnd {
            if let Some(previous) = win::foreground().filter(|previous| *previous != hwnd) { self.return_focus = Some(previous); }
            win::activate(hwnd);
        }
        window.focus(&self.composer.focus_handle(cx));
        cx.notify();
    }

    /// Hand the keyboard back to the app the user was in before typing here.
    fn return_to_previous_app(&mut self) {
        if let Some(previous) = self.return_focus.take() { win::activate(previous); }
    }

    /// Title, overview, topics and follow-ups for a finished session, written off the UI thread.
    fn write_notes(&self, session: archive::Session) {
        let Some(root) = self.archive.as_ref().map(|archive| archive.root().to_path_buf()) else { return };
        let (settings, codex) = (self.store.value.clone(), self.codex.clone());
        std::thread::spawn(move || match crate::notes::generate(&settings, &codex, &session) {
            Ok((title, notes)) => { let _ = Archive::at(root).update(&session.id, |s| { s.title = Some(title); s.notes = Some(notes); }); }
            Err(error) => eprintln!("session notes unavailable: {error}"),
        });
    }

    /// Re-check both subscriptions in the background (settings shows the result).
    pub fn refresh_subscriptions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let codex = self.codex.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (codex_status, claude_status) = cx.background_executor()
                .spawn(async move { (codex.status(), crate::claude_cli::ClaudeCli::status()) }).await;
            let _ = this.update(cx, |this, cx| { this.codex_status = Some(codex_status); this.claude_status = Some(claude_status); cx.notify(); });
        }).detach();
    }

    pub fn sign_out_codex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let codex = self.codex.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { codex.logout() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = result { this.key_notice = Some(error.into()); }
                this.refresh_subscriptions(window, cx);
            });
        }).detach();
    }

    /// Run the official CLI's browser sign-in, then refresh status.
    pub fn sign_in(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        if self.signing_in { return; }
        self.signing_in = true;
        let codex = self.codex.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                match provider { Provider::Codex => codex.login(), Provider::Claude => crate::claude_cli::ClaudeCli::login(), Provider::ApiKey => Ok(()) }
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.signing_in = false;
                if let Err(error) = result { this.key_notice = Some(error.into()); }
                this.refresh_subscriptions(window, cx);
            });
        }).detach();
    }

    fn send_composer(&mut self, input: Entity<TextInput>, window: &mut Window, cx: &mut Context<Self>) {
        let question = input.read(cx).text().trim().to_string();
        if question.is_empty() { return; }
        input.update(cx, |input, cx| input.clear(cx));
        self.send("Ask", question, window, cx);
        self.return_to_previous_app();
    }

    /// Ask the selected provider: optional screenshot, recent exchanges as context, streamed reply.
    fn send(&mut self, action: &str, question: String, window: &mut Window, cx: &mut Context<Self>) {
        self.show();
        if self.live_since.is_none() { self.set_live(true, window, cx); }
        if let Some(cancel) = self.job_cancel.take() { cancel.store(true, Ordering::Relaxed); }
        for turn in &mut self.turns { if turn.status == Status::Streaming { turn.status = Status::Failed("Stopped for a newer request.".into()); } }
        let history: Vec<Exchange> = self.turns.iter().filter(|t| t.status == Status::Done)
            .map(|t| Exchange { action: t.action.to_string(), question: t.question.clone(), answer: t.text.clone() }).collect();
        self.next_turn += 1;
        let id = self.next_turn;
        let at_ms = self.live_since.map(|start| start.elapsed().as_millis() as u64).unwrap_or(0);
        self.turns.push(Turn { id, action: action.to_string().into(), question: question.clone(), text: String::new(), status: Status::Streaming, at_ms, screenshot: None });
        self.scroll.scroll_to_bottom();
        cx.notify();
        let settings = self.store.value.clone();
        let codex = self.codex.clone();
        let point = self.hwnd.and_then(win::center);
        let action = action.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let screenshot = match (settings.screen_on_send, point) {
                (true, Some((x, y))) => cx.background_executor().spawn(async move { crate::capture::screen_jpeg(x, y).ok() }).await,
                _ => None,
            };
            let request = answer::build(&settings, &codex, &action, &question, &history, screenshot.clone());
            let mut job = match request {
                Ok(request) => answer::start(request),
                Err(reason) => { let _ = this.update(cx, |this, cx| this.finish_turn(id, Err(reason), cx)); return; }
            };
            let cancel = job.cancel.clone();
            if this.update(cx, |this, _| {
                this.job_cancel = Some(cancel);
                if let Some(turn) = this.turns.iter_mut().find(|t| t.id == id) { turn.screenshot = screenshot; }
            }).is_err() { return; }
            while let Some(event) = job.events.next().await {
                let finished = matches!(event, answer::Event::Done(_));
                let alive = this.update(cx, |this, cx| match event {
                    answer::Event::Delta(delta) => {
                        if let Some(turn) = this.turns.iter_mut().find(|t| t.id == id && t.status == Status::Streaming) { turn.text.push_str(&delta); }
                        this.scroll.scroll_to_bottom();
                        cx.notify();
                    }
                    answer::Event::Done(result) => this.finish_turn(id, result, cx),
                }).is_ok();
                if finished || !alive { break; }
            }
        }).detach();
    }

    fn finish_turn(&mut self, id: u64, result: Result<String, String>, cx: &mut Context<Self>) {
        let Some(turn) = self.turns.iter_mut().find(|t| t.id == id && t.status == Status::Streaming) else { return };
        match result {
            Ok(text) => {
                turn.text = text;
                turn.status = Status::Done;
                if let Some(recorder) = &mut self.recorder {
                    let shot = if self.store.value.save_screenshots { turn.screenshot.as_deref() } else { None };
                    let saved = archive::Turn { at_ms: turn.at_ms, action: turn.action.to_string(), question: turn.question.clone(),
                        answer: turn.text.clone(), screenshot: None };
                    if let Err(error) = recorder.add_turn(saved, shot) { eprintln!("answer could not be saved: {error}"); }
                }
            }
            Err(reason) => turn.status = Status::Failed(reason.into()),
        }
        self.job_cancel = None;
        cx.notify();
    }

    pub fn save_key(&mut self, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).text().trim().to_string();
        if key.is_empty() { return; }
        let provider = self.store.value.api_provider.clone();
        self.key_notice = Some(match crate::secrets::set(&provider, &key) {
            Ok(()) => { self.key_input.update(cx, |input, cx| input.clear(cx)); "Saved to Windows Credential Manager.".into() }
            Err(error) => format!("Couldn't save the key: {error}").into(),
        });
        cx.notify();
    }

    pub fn remove_key(&mut self, cx: &mut Context<Self>) {
        let provider = self.store.value.api_provider.clone();
        self.key_notice = Some(match crate::secrets::remove(&provider) { Ok(()) => "Key removed.".into(), Err(_) => "No saved key to remove.".into() });
        cx.notify();
    }

    /// Fetch the provider's model list off the UI thread.
    pub fn load_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = crate::providers::preset(&self.store.value.api_provider) else { return };
        let base = if preset.base_url.is_empty() { self.store.value.custom_base_url.trim().to_string() } else { preset.base_url.to_string() };
        let key = crate::secrets::get(preset.id);
        let wire = preset.wire;
        self.models_loading = true;
        self.key_notice = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { crate::providers::list_models(wire, &base, key.as_deref()) }).await;
            let _ = this.update(cx, |this, cx| {
                this.models_loading = false;
                match result {
                    Ok(models) => this.loaded_models = models,
                    Err(error) => this.key_notice = Some(format!("Couldn't load models: {error}").into()),
                }
                cx.notify();
            });
        }).detach();
    }

    pub fn choose_model(&mut self, model: String, window: &mut Window, cx: &mut Context<Self>) {
        self.model_input.update(cx, |input, cx| input.set_text(model.clone(), cx));
        self.update_settings(|s| { s.api_models.insert(s.api_provider.clone(), model); }, window, cx);
    }

    /// Label for the composer's provider chip.
    fn provider_label(&self) -> String {
        let s = &self.store.value;
        match s.provider {
            Provider::Codex => "ChatGPT".into(),
            Provider::Claude => format!("Claude · {:?}", s.claude_model),
            Provider::ApiKey => {
                let name = crate::providers::preset(&s.api_provider).map(|p| p.label).unwrap_or("API");
                if s.api_model().is_empty() { name.to_string() } else { format!("{name} · {}", s.api_model()) }
            }
        }
    }

    fn toggle_visible(&mut self) {
        let Some(hwnd) = self.hwnd else { return };
        let visible = !win::is_visible(hwnd);
        win::set_visible(hwnd, visible);
        self.hotkeys.set_overlay_visible(visible);
        // A hidden panel must not keep claiming Esc.
        self.hotkeys.set_panel_open(visible && self.settings_tab.is_some());
    }

    fn show(&mut self) {
        if let Some(hwnd) = self.hwnd.filter(|hwnd| !win::is_visible(*hwnd)) {
            win::set_visible(hwnd, true);
            self.hotkeys.set_overlay_visible(true);
        }
    }

    /// Start one polling loop per motion kind; it reads every arrow each frame, so
    /// holding two arrows moves diagonally and releasing the chord stops it.
    fn start_motion(&mut self, kind: Motion, window: &mut Window, cx: &mut Context<Self>) {
        if self.motion == Some(kind) { return; }
        self.motion = Some(kind);
        if !self.motion_step(kind, 0, cx) { return; }
        cx.spawn_in(window, async move |this, cx| {
            let mut frame = 1;
            loop {
                cx.background_executor().timer(FRAME).await;
                if !this.update(cx, |this, cx| this.motion_step(kind, frame, cx)).unwrap_or(false) { break; }
                frame += 1;
            }
        }).detach();
    }

    fn motion_step(&mut self, kind: Motion, frame: u32, cx: &mut Context<Self>) -> bool {
        let Some((dx, dy)) = win::held_direction(kind == Motion::Scroll) else {
            self.motion = None;
            return false;
        };
        let cruise = if kind == Motion::Move { MOVE_CRUISE } else { SCROLL_CRUISE };
        let speed = (MOVE_START + frame as f32 * EASE_PER_FRAME).min(cruise);
        match kind {
            Motion::Move => if let Some(hwnd) = self.hwnd {
                let _ = win::move_by(hwnd, (dx as f32 * speed).round() as i32, (dy as f32 * speed).round() as i32);
            },
            Motion::Scroll => self.scroll_by(dy as f32 * speed, cx),
        }
        true
    }

    fn scroll_by(&self, delta: f32, cx: &mut Context<Self>) {
        let offset = self.scroll.offset();
        let max = self.scroll.max_offset().height;
        // GPUI offsets are negative as content moves up.
        let y = (offset.y - px(delta)).clamp(-max, px(0.0));
        self.scroll.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn elapsed(&self) -> String {
        let seconds = self.live_since.map(|start| start.elapsed().as_secs()).unwrap_or(0);
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}


impl Overlay {
    fn pill(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.live_since.is_some();
        let mark = ui::mark(30.0);
        let status: AnyElement = if live {
            chip().child(div().size(px(7.0)).rounded_full().bg(theme::accent()))
                .child(div().font_family(theme::MONO).text_size(px(12.0)).text_color(theme::text()).child(self.elapsed()))
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child("Listening"))
                .into_any_element()
        } else {
            chip().id("start").bg(theme::accent()).cursor_pointer()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.set_live(true, window, cx)))
                .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink()).child("▶  Start"))
                .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(gpui::rgb(0x16336b)).child(self.hotkeys.label(Action::Live)))
                .into_any_element()
        };
        let hide = chip().id("hide").cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.toggle_visible()))
            .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Hide"))
            .child(keycap(self.hotkeys.label(Action::Toggle)));
        let mut pill = div().id("pill").relative().flex().items_center().gap(px(6.0)).p(px(5.0)).rounded_full()
            .bg(theme::glass()).border_1().border_color(theme::hairline())
            // Dragging the pill background moves the whole overlay.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(self.hits.mark())
            .child(mark).child(status).child(hide);
        let active = |button: gpui::Stateful<gpui::Div>, on: bool| button.when(on, |button| button.bg(theme::bubble()).border_1().border_color(theme::bubble_border()));
        if !live {
            pill = pill.child(ui::round_button("sessions")
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.open_sessions(cx)))
                .child(ui::icon("icons/history.svg", 16.0, theme::body())));
        }
        let settings_open = self.settings_tab.is_some();
        pill = pill.child(active(ui::round_button("settings"), settings_open)
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                if settings_open { this.close_panels(window, cx) } else { this.open_settings(Tab::default(), window, cx) }
            }))
            .child(ui::icon("icons/gear.svg", 16.0, theme::body())));
        if live {
            pill = pill.child(ui::round_button("stop")
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.set_live(false, window, cx)))
                .child(div().size(px(10.0)).rounded(px(2.0)).bg(theme::text())));
        }
        pill
    }

    fn panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ticker = div().flex().items_center().gap(px(10.0)).px(px(16.0)).py(px(10.0))
            .border_b_1().border_color(theme::divider())
            .child(div().text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()).child("HEARD"))
            .child(div().text_size(px(12.0)).text_color(theme::muted()).truncate()
                .child("Listening for the conversation…"));
        let mut thread = div().id("thread").flex().flex_col().gap(px(14.0)).px(px(18.0)).py(px(14.0))
            .flex_1().overflow_y_scroll().track_scroll(&self.scroll);
        if self.turns.is_empty() {
            thread = thread.child(div().text_size(px(13.0)).text_color(theme::muted())
                .child(format!("Press {} to answer from your screen and the conversation.", self.hotkeys.label(Action::Assist))));
        }
        for turn in &self.turns {
            let asked = if turn.question.is_empty() { turn.action.to_string() } else { turn.question.clone() };
            let body: AnyElement = match &turn.status {
                Status::Failed(reason) => div().text_size(px(13.0)).line_height(px(20.0)).text_color(gpui::rgb(0xffb4a8)).child(reason.clone()).into_any_element(),
                Status::Streaming if turn.text.is_empty() => div().text_size(px(13.0)).text_color(theme::muted()).child("Thinking…").into_any_element(),
                _ => div().id(("answer", turn.id as usize)).min_w_0().text_color(theme::text()).child(crate::markdown::render(&turn.text, 14.0)).into_any_element(),
            };
            thread = thread
                .child(div().flex().justify_end().child(div().max_w(px(420.0)).text_size(px(13.0)).text_color(theme::text())
                    .px(px(12.0)).py(px(6.0)).rounded(px(12.0)).border_1().border_color(theme::bubble_border()).bg(theme::bubble())
                    .child(asked)))
                .child(body);
        }
        let mut actions = div().flex().items_center().gap(px(4.0)).px(px(12.0)).pt(px(8.0)).pb(px(8.0));
        for (index, label) in answer::ACTIONS.into_iter().enumerate() {
            actions = actions.child(div().id(("action", index)).relative().px(px(10.0)).py(px(6.0)).rounded(px(8.0)).cursor_pointer()
                .text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).hover(|b| b.bg(theme::raised()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.send(label, String::new(), window, cx)))
                .child(self.hits.mark()).child(label));
        }
        let composer = div().relative().mx(px(12.0)).mb(px(12.0)).flex().flex_col().gap(px(12.0)).p(px(12.0)).rounded(px(13.0))
            .bg(theme::field()).border_1().border_color(theme::hairline())
            .child(self.hits.mark())
            .child(div().id("composer-input").cursor_text()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.composer.focus_handle(cx))))
                .child(self.composer.clone()))
            .child(div().flex().items_center().justify_between()
                .child(div().flex().items_center().gap(px(6.0))
                    .child(div().flex().items_center().gap(px(6.0)).px(px(9.0)).py(px(4.0)).rounded_full().border_1().border_color(theme::hairline())
                        .child(div().size(px(6.0)).rounded_full().bg(theme::ok()))
                        .child(div().text_size(px(12.0)).text_color(theme::body()).child(self.provider_label())))
                    .when(self.store.value.screen_on_send, |row| row.child(div().px(px(9.0)).py(px(4.0)).rounded_full().border_1().border_color(theme::hairline())
                        .text_size(px(12.0)).text_color(theme::body()).child("Screen on send")))
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child("Enter to send ·"))
                    .child(keycap(self.hotkeys.label(Action::Assist))).child(div().text_size(px(12.0)).text_color(theme::muted()).child("Assist")))
                .child(div().id("send").size(px(30.0)).rounded_full().bg(theme::accent()).flex().items_center().justify_center().cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { let input = this.composer.clone(); this.send_composer(input, window, cx); }))
                    .text_color(theme::accent_ink()).font_weight(FontWeight::BOLD).child("↑")));
        div().w(px(560.0)).flex_1().min_h_0().flex().flex_col().rounded(px(18.0)).bg(theme::glass())
            .border_1().border_color(theme::hairline()).overflow_hidden()
            .child(ticker).child(thread).child(actions).child(composer)
    }
}

impl Render for Overlay {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.live_since.is_some();
        self.hits.clear();
        let mut root = div().size_full().flex().flex_col().items_center().gap(px(10.0)).pt(px(8.0)).pb(px(12.0))
            .font_family(theme::FONT).text_color(theme::text())
            .track_focus(&self.focus)
            .on_children_prepainted(click_through(self.hwnd, self.hits.clone(), self.shape.clone()))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key != "escape" { return; }
                if this.settings_tab.is_some() { this.close_panels(window, cx); }
                else if this.composer.focus_handle(cx).is_focused(window) {
                    this.composer.update(cx, |input, cx| input.clear(cx));
                    this.return_to_previous_app();
                }
            }))
            .child(self.pill(cx));
        if let Some(tab) = self.settings_tab { root = root.child(self.settings_panel(tab, cx)); }
        else if live { root = root.child(self.panel(cx)); }
        else {
            root = root.child(div().text_size(px(12.0)).text_color(theme::body()).px(px(12.0)).py(px(5.0)).rounded_full()
                .bg(gpui::rgba(0x0f1012b3)).child("Start listens to your desktop + mic and attaches your screen to every message."));
        }
        root
    }
}

/// Bounds of the controls that should catch the mouse, collected while laying out a frame.
#[derive(Clone, Default)]
pub struct Hits(Rc<RefCell<Vec<Hit>>>, Rc<std::cell::Cell<f32>>);

type Hit = gpui::Bounds<gpui::Pixels>;

impl Hits {
    fn clear(&self) { self.0.borrow_mut().clear(); }

    /// Whether a window-relative physical point lies on an interactive control.
    fn contains(&self, (x, y): (i32, i32)) -> bool {
        let scale = self.1.get().max(0.5);
        let point = gpui::point(gpui::px(x as f32 / scale), gpui::px(y as f32 / scale));
        self.0.borrow().iter().any(|bounds| bounds.contains(&point))
    }

    /// An invisible child that records its parent's bounds. The parent must be `relative()`.
    pub fn mark(&self) -> impl IntoElement {
        let hits = self.clone();
        gpui::canvas(move |bounds, _, _| hits.0.borrow_mut().push(bounds), |_, _, _, _| {}).absolute().top_0().left_0().size_full()
    }
}

/// Shape the window to the interactive controls only (pill, quick actions, composer, settings),
/// so answers, the transcript ticker and the space around everything are click-through.
fn click_through(hwnd: Option<HWND>, hits: Hits, applied: Rc<RefCell<Vec<win::Shape>>>) -> impl Fn(Vec<gpui::Bounds<gpui::Pixels>>, &mut Window, &mut gpui::App) + 'static {
    move |children, window, _| {
        let Some(hwnd) = hwnd else { return };
        let scale = window.scale_factor();
        hits.1.set(scale);
        let physical = |value: gpui::Pixels| (f32::from(value) * scale).round() as i32;
        // The window keeps the shape of everything visible (pill, panel, caption); the space
        // around them is cut away. Inside the panel, `passthrough` decides per cursor position.
        let shapes: Vec<win::Shape> = children.iter().map(|bounds| {
            let height = f32::from(bounds.size.height);
            let radius = if height <= 44.0 { height / 2.0 } else { 18.0 };
            (physical(bounds.left()) - 1, physical(bounds.top()) - 1, physical(bounds.right()) + 1, physical(bounds.bottom()) + 1,
                (radius * scale).round() as i32)
        }).collect();
        if *applied.borrow() != shapes {
            win::set_shape(hwnd, &shapes);
            *applied.borrow_mut() = shapes;
        }
    }
}

fn apply_capture_setting(hwnd: HWND, settings: &Settings) {
    // CLUELYRS_ALLOW_CAPTURE=1 is a development switch for taking screenshots of the overlay.
    let dev_override = std::env::var_os("CLUELYRS_ALLOW_CAPTURE").is_some_and(|value| value == "1");
    if let Err(error) = win::set_capture_hidden(hwnd, settings.hide_from_capture && !dev_override) {
        eprintln!("capture exclusion unavailable: {error}");
    }
}

