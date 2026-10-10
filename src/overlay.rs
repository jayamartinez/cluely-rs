//! The overlay: one card (see `card`), the same on Windows and macOS, with the live transcript, the
//! answer thread and quick actions above the text box and a toolbar. Listening (capture,
//! transcription, endpointing) runs off this thread; see `transcript_view`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    ScrollHandle, SharedString, Styled, Window, div, point, prelude::*, px, size,
};

use crate::answer::{self, Exchange};
use crate::archive::{self, Archive, Recorder};
use crate::input::{InputEvent, TextInput};
use crate::listening::{self, Listening};
use crate::reasoning::{self, ReasoningSession};
use crate::reasoning::speculation::{self, Budget, Planner, PreparedShot, Step};
use crate::transcript::state::UtteranceId;
use crate::settings::Provider;
use crate::transcript_view::{Download, ProvisionalLine, TranscriptLine};
use crate::sessions_window::{self, SessionsWindow};
use crate::hotkeys::{Action, Hotkeys};
use crate::settings::{Settings, Store};
use crate::settings_view::Tab;
use crate::theme;
use crate::ui;
use crate::platform::{self, NativeWindow, PreviousFocus};

/// The overlay window; the Live panel is 40 px narrower.
pub const WIDTH: f32 = 680.0;
/// The card, its hint and room for a toggle's popover below it.
pub const IDLE_HEIGHT: f32 = 270.0;
/// The card at its tallest (about 520 px) and room for a toggle's popover below it.
const LIVE_HEIGHT: f32 = 680.0;
/// The card, the Settings panel under it (Windows) and room for an open picker list below them.
const SETTINGS_HEIGHT: f32 = 820.0;
/// How long Settings takes to collapse into the conversation after a message is sent from under it.
const COLLAPSE: Duration = Duration::from_millis(200);
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
    /// Shown without a press (Settings → Show answers automatically): the question it answers,
    /// and when it was shown (until that question's line is committed).
    auto: Option<(UtteranceId, u64)>,
}

/// Add an answer to the session's History record, when one is being saved. A `stopped` answer
/// (Live ended while it was being written) keeps the text it had, unless nothing was asked or written.
fn save_turn(recorder: &mut Option<Recorder>, screenshots: bool, turn: &Turn, stopped: bool) {
    let Some(recorder) = recorder else { return };
    if stopped && turn.text.trim().is_empty() && turn.question.trim().is_empty() { return; }
    let shot = if screenshots { turn.screenshot.as_deref() } else { None };
    let saved = archive::Turn { at_ms: turn.at_ms, action: turn.action.to_string(), question: turn.question.clone(),
        answer: turn.text.clone(), screenshot: None, stopped };
    if let Err(error) = recorder.add_turn(saved, shot) { eprintln!("answer could not be saved: {error}"); }
}

pub struct Overlay {
    native: Option<NativeWindow>,
    /// The overlay's own window. Settings handlers run here even when macOS shows the settings
    /// in their own window (see `settings_view::listen`).
    pub(crate) own_window: gpui::AnyWindowHandle,
    pub(crate) hotkeys: Hotkeys,
    pub(crate) store: Store,
    settings_tab: Option<Tab>,
    /// The Sessions review window, while it's open.
    sessions_window: Option<gpui::WindowHandle<SessionsWindow>>,
    /// macOS shows settings in their own window, while it's open.
    #[cfg(target_os = "macos")]
    pub(crate) settings_window: Option<gpui::WindowHandle<crate::settings_window::SettingsWindow>>,
    /// Where Live sessions are saved; `None` when no data folder is available.
    pub(crate) archive: Option<Archive>,
    /// Receives Esc while settings is open.
    focus: FocusHandle,
    /// Last window shape applied, so the region is only rebuilt when the layout changes.
    shape: Rc<RefCell<Vec<platform::Shape>>>,
    /// Interactive areas laid out this frame; everything else is click-through.
    pub(crate) hits: Hits,
    pub(crate) composer: Entity<TextInput>,
    pub(crate) key_input: Entity<TextInput>,
    pub(crate) model_input: Entity<TextInput>,
    pub(crate) base_url_input: Entity<TextInput>,
    /// Feedback under the API key field ("Saved", errors).
    pub(crate) key_notice: Option<SharedString>,
    pub(crate) loaded_models: Vec<String>,
    pub(crate) models_loading: bool,
    /// The macOS Settings window's provider → key → model form.
    #[cfg(target_os = "macos")]
    pub(crate) model_ui: crate::settings_view::ModelUi,
    /// A saved key shown on request for a few seconds (`settings_view::reveal`).
    pub(crate) key_reveal: crate::settings_view::KeyReveal,
    /// Answers for the Live session: warm provider, one request at a time, stale replies dropped.
    reasoning: Option<ReasoningSession>,
    /// Decides when to prepare an answer ahead of time (`reasoning::speculation`).
    planner: Planner,
    /// A screenshot taken when a question started, used by the next answer while fresh.
    prepared_shot: PreparedShot,
    /// The quick action a speculative answer is prepared for: the last one asked on a heard question.
    answer_action: &'static str,
    /// Bumped by every speculation step, so a start still waiting for its screenshot is dropped
    /// once a newer step has replaced or cancelled it.
    speculation_attempt: u64,
    /// Latency marks and export location for this Live session, when `CLUELYRS_METRICS=1`.
    pub(crate) metrics: Option<listening::Metrics>,
    next_turn: u64,
    /// True while the cursor is over a control and the window takes mouse input.
    catching_mouse: bool,
    /// The app that had focus before the Focus shortcut, so sending or Esc can return to it.
    return_focus: Option<PreviousFocus>,
    /// The ChatGPT subscription client (one Codex app-server for the app's lifetime).
    pub(crate) codex: Arc<crate::codex::CodexClient>,
    /// Last known sign-in state for the ChatGPT and Claude subscriptions (None while checking).
    pub(crate) codex_status: Option<crate::chat::SubscriptionStatus>,
    pub(crate) claude_status: Option<crate::chat::SubscriptionStatus>,
    pub(crate) signing_in: bool,
    /// Why the file picked for a subscription's CLI can't be used (Settings › Model).
    pub(crate) cli_error: Option<(Provider, SharedString)>,
    /// The Live session being saved to History, when saving is on.
    pub(crate) recorder: Option<Recorder>,
    /// The held-key loop that is currently running, if any.
    motion: Option<Motion>,
    pub(crate) live_since: Option<Instant>,
    turns: Vec<Turn>,
    scroll: ScrollHandle,
    /// The capture + transcription pipeline while Live is transcribing.
    pub(crate) listening: Option<Listening>,
    pub(crate) listening_status: Option<listening::Status>,
    /// Bumped on every pipeline start, so a replaced pipeline's messages are ignored.
    pub(crate) listening_epoch: u64,
    /// Committed transcript of the current Live session, with source and timestamps.
    pub(crate) transcript: Vec<TranscriptLine>,
    /// What each source is still saying: Me, then Them.
    pub(crate) provisional: [Option<ProvisionalLine>; 2],
    /// The last few audio levels (the louder of Me and Them), newest last, for the Live waveform.
    pub(crate) levels: std::collections::VecDeque<f32>,
    pub(crate) model_installed: bool,
    pub(crate) model_download: Option<Download>,
    /// Feedback under the model row in Settings → Listening.
    pub(crate) model_notice: Option<SharedString>,
    /// Background opacity's slider is being dragged; settings.json is written on release.
    pub(crate) opacity_drag: bool,
    /// The dropdown that is open in Settings, if any.
    pub(crate) open_picker: Option<crate::settings_view::Picker>,
    /// Show subscription account names in Settings → Model (masked by default; never saved).
    pub(crate) reveal_accounts: bool,
    pub(crate) devices: Option<crate::audio::DeviceList>,
    pub(crate) devices_loading: bool,
    /// Bytes used by saved sessions, for Settings → History.
    pub(crate) archive_bytes: Option<u64>,
    /// Where the open dropdown's face was laid out, so a click on it closes rather than reopens.
    pub(crate) picker_face: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// The toolbar toggle under the pointer (shows its state popover).
    pub(crate) toggle_hover: Option<crate::toggles::Toggle>,
    /// When Settings started collapsing into the conversation (a message was sent from under it).
    collapse: Option<Instant>,
    /// The Settings panel's laid-out height, which the collapse starts from.
    settings_height: Rc<std::cell::Cell<f32>>,
    /// The modes and which one is active (`modes`); its context and files are on `store.value.mode`.
    pub(crate) modes: crate::modes::ModeStore,
    /// Settings › Modes (`modes_view`).
    pub(crate) modes_ui: crate::modes_view::ModesUi,
    /// Update checks: what Settings › About shows and the notice above the text box (`update_notice`).
    pub(crate) updates: crate::update::Tracker,
    /// The mode switcher under the card, while it's open.
    switcher: Option<switcher::Switcher>,
    /// Windows: the Modes window, while it's open.
    pub(crate) modes_window: Option<gpui::WindowHandle<crate::modes_window::ModesWindow>>,
}

impl Overlay {
    pub fn new(hotkeys: Hotkeys, presses: UnboundedReceiver<Action>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let native = platform::native_window(window);
        let mut store = Store::load();
        let modes = crate::modes::ModeStore::load();
        store.value.mode = modes.active_material();
        if let Some(native) = native {
            // Failures here leave a visible, usable window; they are not fatal.
            apply_capture_setting(native, &store.value);
            platform::remove_frame(native);
            let _ = platform::set_topmost(native);
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
        if let Some(native) = native { platform::enable_passthrough(native); }
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(30)).await;
            if this.update_in(cx, |this, window, cx| this.update_passthrough(window, cx)).is_err() { break; }
        }).detach();
        // One-second tick for the live timer; idle ticks do no work.
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this.update(cx, |this, cx| if this.live_since.is_some() { cx.notify() }).is_err() { break; }
        }).detach();

        // Every quit (⌘Q, the menu, Settings › About, a quit Apple Event) passes here: stop
        // transcribing and free parakeet.cpp's Metal backend before exit, or ggml aborts.
        #[cfg(target_os = "macos")]
        cx.on_app_quit(|this, _| { this.stop_listening(); crate::stt::parakeet::shutdown(); async {} }).detach();

        let archive = Archive::default_location();
        if let Some(archive) = &archive {
            // A previous run that was killed mid-session leaves an unfinished record.
            archive.recover();
            if store.value.save_sessions { archive.prune(store.value.keep_sessions, archive::unix_now()); }
        }
        let start_live = store.value.start_live_on_launch;
        let composer = cx.new(|cx| TextInput::new(composer_placeholder(&store.value), cx));
        let key_input = cx.new(|cx| TextInput::new("Paste your API key", cx).masked(true));
        let model_input = cx.new(|cx| TextInput::new("Model id, e.g. from the list below", cx));
        let base_url_input = cx.new(|cx| TextInput::new("https://your-endpoint.example/v1", cx));
        model_input.update(cx, |input, cx| input.set_text(store.value.api_model().to_string(), cx));
        base_url_input.update(cx, |input, cx| input.set_text(store.value.custom_base_url.clone(), cx));
        cx.subscribe_in(&composer, window, |this, input, event, window, cx| {
            if matches!(event, InputEvent::Submit) { this.send_composer(input.clone(), window, cx); }
            // The card's return button turns blue once there is text to send.
            if matches!(event, InputEvent::Changed) { cx.notify(); }
        }).detach();
        #[cfg(not(target_os = "macos"))]
        cx.subscribe_in(&key_input, window, |this, _, event, window, cx| { if matches!(event, InputEvent::Submit) { this.save_key(window, cx); } }).detach();
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
        let modes_ui = crate::modes_view::ModesUi::new(&modes, window, cx);
        #[cfg(target_os = "macos")]
        let model_ui = crate::settings_view::ModelUi::new(&key_input, &base_url_input, window, cx);
        let mut overlay = Self { native, own_window: window.window_handle(), hotkeys, store, settings_tab: None, sessions_window: None,
            #[cfg(target_os = "macos")]
            settings_window: None,
            archive, focus: cx.focus_handle(),
            recorder: None, shape: Rc::default(), hits: Hits::default(), composer, key_input, model_input, base_url_input,
            #[cfg(target_os = "macos")]
            model_ui,
            key_reveal: Default::default(), opacity_drag: false,
            key_notice: None, loaded_models: Vec::new(), models_loading: false, reasoning: None, planner: Planner::new(false, Budget::default()), prepared_shot: PreparedShot::default(), answer_action: "Assist", speculation_attempt: 0,
            metrics: None, next_turn: 0, catching_mouse: true, return_focus: None,
            codex: crate::codex::CodexClient::new(), codex_status: None, claude_status: None, signing_in: false,
            cli_error: None,
            motion: None, live_since: None, turns: Vec::new(), scroll: ScrollHandle::new(),
            listening: None, listening_status: None, listening_epoch: 0, transcript: Vec::new(), provisional: Default::default(), levels: Default::default(),
            model_installed: false, model_download: None, model_notice: None,
            open_picker: None, reveal_accounts: false, devices: None, devices_loading: false, archive_bytes: None, picker_face: Rc::default(), toggle_hover: None,
            collapse: None, settings_height: Rc::default(), modes_ui, modes, updates: Default::default(), modes_window: None, switcher: None };
        overlay.refresh_model_status();
        overlay.mark_existing_keys(window, cx);
        #[cfg(target_os = "macos")]
        overlay.update_dock();
        if start_live { overlay.set_live(true, window, cx); }
        overlay.start_update_checks(window, cx);
        overlay
    }

    pub fn update_settings(&mut self, change: impl FnOnce(&mut Settings), window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.store.value.clone();
        change(&mut self.store.value);
        if self.store.value == previous { return; }
        self.store.save();
        // A shown saved key belongs to one provider, and only while Settings is hidden from capture.
        if previous.hide_from_capture != self.store.value.hide_from_capture || previous.api_provider != self.store.value.api_provider {
            self.key_reveal.hide();
        }
        if let Some(native) = self.native && previous.hide_from_capture != self.store.value.hide_from_capture {
            apply_capture_setting(native, &self.store.value);
        }
        #[cfg(target_os = "macos")]
        if previous.show_in_dock != self.store.value.show_in_dock { self.update_dock(); }
        // Grows and shrinks around the card's centre, keeping its top edge (the height doesn't change).
        if let Some(native) = self.native && previous.card_width != self.store.value.card_width {
            platform::set_width_centred(native, self.store.value.card_width.window());
        }
        if previous.screen_on_send != self.store.value.screen_on_send {
            let placeholder = composer_placeholder(&self.store.value);
            self.composer.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
        if previous.api_provider != self.store.value.api_provider {
            // Each provider keeps its own model; show the one saved for the new provider.
            let model = self.store.value.api_model().to_string();
            self.model_input.update(cx, |input, cx| input.set_text(model, cx));
            self.loaded_models.clear();
            self.key_notice = None;
        }
        let now = &self.store.value;
        if previous.transcribe != now.transcribe || previous.stt_provider != now.stt_provider || previous.listen_mic != now.listen_mic || previous.listen_desktop != now.listen_desktop
            || previous.mic_device != now.mic_device || previous.desktop_device != now.desktop_device || previous.use_gpu != now.use_gpu {
            self.restart_listening_if_live(window, cx);
        }
        // Choosing another provider loads its list (a user action, so it may read the saved key).
        #[cfg(target_os = "macos")]
        if previous.api_provider != self.store.value.api_provider || previous.provider != self.store.value.provider {
            self.ensure_models(false, true, window, cx);
        }
        cx.notify();
    }

    pub fn open_settings(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_tab(tab, window, cx);
        #[cfg(target_os = "macos")]
        crate::settings_window::open(tab, cx);
        #[cfg(not(target_os = "macos"))]
        {
            self.settings_tab = Some(tab);
            self.open_panel(window, cx);
        }
    }

    /// In the Dock only while "Show in Dock" is on. CluelyRS stays out of it while Settings is open
    /// too: tools that quit apps whose last window closes (and so would quit CluelyRS when Settings
    /// closes) leave apps outside the Dock alone. Settings handles ⌘W and ⌘Q itself.
    #[cfg(target_os = "macos")]
    pub(crate) fn update_dock(&self) {
        platform::set_in_dock(self.store.value.show_in_dock);
    }

    /// Refresh what a settings tab shows (accounts, devices, history size) as it opens.
    pub(crate) fn prepare_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.open_picker = None;
        // A key shown while typing, or a saved key shown on request, is hidden again whenever a tab opens.
        self.key_input.update(cx, |input, cx| input.set_revealed(false, cx));
        self.key_reveal.hide();
        if tab == Tab::Model { self.refresh_subscriptions(window, cx); }
        #[cfg(target_os = "macos")]
        if tab == Tab::Model { self.ensure_models(true, false, window, cx); }
        if tab == Tab::Listening { self.refresh_model_status(); self.load_devices(window, cx); }
        if tab == Tab::History { self.refresh_archive_size(window, cx); }
        if tab == Tab::Modes { self.prepare_modes_tab(cx); }
    }

    /// The active mode or its context or files changed: answers from now on use it. A running
    /// answer finishes as it started; a speculative one (prepared with the old instructions) is
    /// dropped, and during Live the provider is warmed up with the new instructions.
    pub(crate) fn apply_mode(&mut self) {
        let material = self.modes.active_material();
        if self.store.value.mode == material { return; }
        self.store.value.mode = material;
        if let Some(session) = &mut self.reasoning { session.change_instructions(&self.store.value); }
    }

    /// Open (or focus) the full-size Sessions review window.
    pub fn open_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.archive.as_ref().map(|archive| archive.root().to_path_buf()) else { return };
        let _ = std::fs::create_dir_all(&root);
        // On macOS the window becomes key only once CluelyRS is active (`platform::activate_then`):
        // CluelyRS is usually not the active app, and GPUI deadlocks when a window becomes key while
        // its app isn't. Done after this click, as for Settings.
        #[cfg(target_os = "macos")]
        {
            let (overlay, codex) = (cx.entity(), self.codex.clone());
            cx.defer(move |cx| platform::activate_then(cx, move |active, cx| {
                let mut handle = overlay.read(cx).sessions_window;
                sessions_window::open(&mut handle, root, codex, active, cx);
                overlay.update(cx, |overlay, _| overlay.sessions_window = handle);
            }));
        }
        #[cfg(not(target_os = "macos"))]
        sessions_window::open(&mut self.sessions_window, root, self.codex.clone(), true, cx);
    }

    #[cfg(not(target_os = "macos"))]
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
        self.collapse = None;
        self.open_picker = None;
        self.reveal_accounts = false;
        self.key_reveal.hide();
        self.hotkeys.set_panel_open(false);
        self.fit(window);
        cx.notify();
    }

    pub fn reveal_archive(&self) {
        let Some(archive) = &self.archive else { return };
        let _ = std::fs::create_dir_all(archive.root());
        if let Err(error) = platform::open_folder(archive.root()) { eprintln!("could not open folder: {error}"); }
    }

    /// Size the window to its content state; transparent area outside the content still takes clicks.
    fn fit(&self, window: &mut Window) {
        let mut height = if self.settings_tab.is_some() { SETTINGS_HEIGHT } else if self.live_since.is_some() { LIVE_HEIGHT }
            else if self.update_notice_shown() { IDLE_HEIGHT + update_notice::NOTICE_HEIGHT } else { IDLE_HEIGHT };
        // The mode switcher hangs below the card.
        if self.switcher_open() { height = height.max(switcher::window_height(self.live_since.is_some(), self.update_notice_shown())); }
        platform::resize(window, self.native, size(px(self.store.value.card_width.window()), px(height)));
    }

    fn handle(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Toggle => {
                // Hiding closes the mode switcher, handing the keyboard back.
                if self.native.is_some_and(platform::is_visible) { self.close_switcher(window, cx); }
                self.toggle_visible();
            }
            Action::Assist => self.send("Assist", String::new(), window, cx),
            Action::Focus => self.type_shortcut(window, cx),
            Action::SwitchMode => self.switch_mode_shortcut(window, cx),
            Action::MoveUp | Action::MoveDown | Action::MoveLeft | Action::MoveRight => self.start_motion(Motion::Move, window, cx),
            Action::ScrollUp | Action::ScrollDown => self.start_motion(Motion::Scroll, window, cx),
            // Esc, claimed while the Windows Settings panel is open, closes the switcher over it first.
            Action::Close => if self.switcher_open() { self.close_switcher(window, cx) } else { self.close_panels(window, cx) },
        }
    }

    fn set_live(&mut self, live: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.live_since = live.then(Instant::now);
        // The composer's model switcher lists the subscription's models; fetch them up front.
        if live && self.codex_status.is_none() { self.refresh_subscriptions(window, cx); }
        if !live {
            // An answer still being written is saved as stopped, with what it has so far.
            for turn in self.turns.iter().filter(|turn| turn.status == Status::Streaming) {
                save_turn(&mut self.recorder, self.store.value.save_screenshots, turn, true);
            }
            // Dropping the session cancels the running request; its late replies are stale.
            self.reasoning = None;
            self.turns.clear();
            // The pipeline's own final commits arrive after it's gone and are dropped, so save
            // what each source was still saying from the last provisional text first.
            self.archive_provisional();
            self.stop_listening();
            self.clear_transcript();
            if let Some(metrics) = self.metrics.take() { std::thread::spawn(move || metrics.export()); }
        }
        self.record(live);
        // Starting Live closes Settings, unless a message sent from under it is starting Live: then
        // Settings collapses into the conversation (see `send_composer`).
        if live && self.collapse.is_none() {
            self.settings_tab = None;
            self.hotkeys.set_panel_open(false);
        }
        if live {
            self.metrics = listening::Metrics::for_session(self.live_since.unwrap_or_else(Instant::now));
            self.reasoning = Some(ReasoningSession::new(self.codex.clone(), self.metrics.as_ref().map(|metrics| metrics.recorder.clone())));
            if let Some(session) = &self.reasoning { session.prewarm(&self.store.value); }
            self.planner = Planner::new(self.store.value.speculative_answers, Budget::default());
            self.prepared_shot.clear();
            self.start_listening(window, cx);
        }
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

    /// Whether the open dropdown is shown in the overlay's Settings panel rather than in the macOS
    /// Settings window.
    fn picker_in_overlay(&self) -> bool {
        self.open_picker.is_some() && self.settings_tab.is_some()
    }

    fn update_passthrough(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(native) = self.native else { return };
        let over_control = platform::cursor_in_window(native).is_some_and(|point| self.hits.contains(point));
        if over_control != self.catching_mouse {
            self.catching_mouse = over_control;
            platform::set_mouse_passthrough(native, !over_control);
        }
        // A press anywhere that isn't one of the overlay's controls (the click-through answer
        // area, or another app) never reaches the overlay, so an open list closes from here.
        if self.picker_in_overlay() && !over_control && platform::left_button_down() {
            self.open_picker = None;
            cx.notify();
        }
        // The mode switcher too; the app clicked takes the keyboard.
        if self.switcher_open() && !over_control && platform::left_button_down() { self.dismiss_switcher(window, cx); }
        // Likewise, leaving a control for the click-through area sends the overlay no "hover
        // ended", so a hover tooltip is cleared here.
        if self.toggle_hover.is_some() && !over_control {
            self.toggle_hover = None;
            cx.notify();
        }
    }

    /// Start Live if needed, take the foreground and put the caret in the composer. With Settings
    /// open, Settings stays and Live waits for the message: sending collapses it (`send_composer`).
    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show();
        if self.live_since.is_none() && self.settings_tab.is_none() { self.set_live(true, window, cx); }
        self.take_keyboard(cx);
        window.focus(&self.composer.focus_handle(cx));
        cx.notify();
    }

    /// Give the overlay the keyboard and remember the app that had it, for `return_to_previous_app`.
    #[cfg(not(target_os = "macos"))]
    fn take_keyboard(&mut self, _cx: &mut Context<Self>) {
        if let Some(native) = self.native && let Some(previous) = platform::take_focus(native) { self.return_focus = Some(previous); }
    }

    /// Give the overlay the keyboard and remember the app that had it, for `return_to_previous_app`.
    /// The panel becomes key only once CluelyRS is the active app (`platform::take_focus`), which can
    /// be a moment after this press or click, or never.
    #[cfg(target_os = "macos")]
    fn take_keyboard(&mut self, cx: &mut Context<Self>) {
        let Some(native) = self.native else { return };
        let overlay = cx.entity().downgrade();
        // Deferred: `take_focus` may call back at once, and the overlay is being updated now.
        cx.defer(move |cx| platform::take_focus(native, cx, move |previous, cx| {
            if let Some(previous) = previous { let _ = overlay.update(cx, |this, _| this.return_focus = Some(previous)); }
        }));
    }

    /// Hand the keyboard back to the app the user was in before typing here.
    fn return_to_previous_app(&mut self) {
        if let Some(previous) = self.return_focus.take() { platform::return_focus(previous); }
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

    pub(crate) fn send_composer(&mut self, input: Entity<TextInput>, window: &mut Window, cx: &mut Context<Self>) {
        let question = input.read(cx).text().trim().to_string();
        if question.is_empty() { return; }
        input.update(cx, |input, cx| input.clear(cx));
        // Sent from under Settings: Settings collapses into the conversation the answer appears in.
        if self.settings_tab.is_some() && self.collapse.is_none() {
            self.open_picker = None;
            self.collapse = Some(Instant::now());
        }
        self.send("Ask", question, window, cx);
        self.return_to_previous_app();
    }

    /// Ask the selected provider: optional screenshot, recent exchanges as context, streamed reply.
    /// A speculative answer prepared for exactly this context is shown at once instead.
    fn send(&mut self, action: &str, question: String, window: &mut Window, cx: &mut Context<Self>) {
        self.show();
        if self.live_since.is_none() { self.set_live(true, window, cx); }
        let utterance = self.latest_heard_utterance();
        if let Some(session) = &self.reasoning { session.note_requested(utterance); }
        // The session cancels the previous request when the new one starts (below).
        for turn in &mut self.turns { if turn.status == Status::Streaming { turn.status = Status::Failed("Stopped for a newer request.".into()); } }
        let history = self.history();
        self.next_turn += 1;
        let id = self.next_turn;
        let at_ms = self.live_since.map(|start| start.elapsed().as_millis() as u64).unwrap_or(0);
        self.turns.push(Turn { id, action: action.to_string().into(), question: question.clone(), text: String::new(), status: Status::Streaming, at_ms, screenshot: None, auto: None });
        self.scroll.scroll_to_bottom();
        cx.notify();
        if let Some(claimed) = self.claim_prepared(action, &question, history.len()) {
            if let Some(turn) = self.turns.iter_mut().find(|t| t.id == id) { turn.text = claimed.text; }
            match claimed.done {
                Some(result) => self.finish_turn(id, result, cx),
                None => self.stream_replies(id, claimed.generation, claimed.replies, cx),
            }
            return;
        }
        let settings = self.store.value.clone();
        let point = self.native.and_then(platform::center);
        let prepared = self.prepared_shot.fresh(Instant::now());
        let action = action.to_string();
        let conversation = self.conversation().render();
        cx.spawn_in(window, async move |this, cx| {
            let screenshot = match (settings.screen_on_send, prepared, point) {
                (false, ..) | (true, None, None) => None,
                (true, Some(jpeg), _) => Some(jpeg),
                (true, None, Some((x, y))) => cx.background_executor().spawn(async move { crate::capture::screen_jpeg(x, y).ok() }).await,
            };
            let request = reasoning::Request { action, question, history, conversation, screenshot: screenshot.clone(), utterance };
            let started = this.update(cx, |this, cx| {
                if let Some(turn) = this.turns.iter_mut().find(|t| t.id == id) { turn.screenshot = screenshot; }
                // Live ended while the screenshot was taken: nothing to answer any more.
                let Some(session) = this.reasoning.as_mut() else { this.finish_turn(id, Err("Live session ended.".into()), cx); return None };
                match session.ask(&settings, request) {
                    Ok(started) => Some(started),
                    Err(reason) => { this.finish_turn(id, Err(reason), cx); None }
                }
            });
            let Ok(Some((generation, replies))) = started else { return };
            let _ = this.update(cx, |this, cx| this.stream_replies(id, generation, replies, cx));
        }).detach();
    }

    /// Finished exchanges of this session, oldest first.
    fn history(&self) -> Vec<Exchange> {
        self.turns.iter().filter(|t| t.status == Status::Done)
            .map(|t| Exchange { action: t.action.to_string(), question: t.question.clone(), answer: t.text.clone() }).collect()
    }

    /// Show a request's replies in turn `id` while it is the current one.
    fn stream_replies(&mut self, id: u64, generation: reasoning::Generation, mut replies: UnboundedReceiver<reasoning::Reply>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            while let Some(reply) = replies.next().await {
                let finished = matches!(reply.event, reasoning::session::Event::Done(_));
                let alive = this.update(cx, |this, cx| {
                    // Replies from a request that was cancelled or superseded never reach the screen.
                    if !this.reasoning.as_ref().is_some_and(|session| session.is_current(generation)) { return; }
                    match reply.event {
                        reasoning::session::Event::Delta(delta) => {
                            if let Some(turn) = this.turns.iter_mut().find(|t| t.id == id && t.status == Status::Streaming) { turn.text.push_str(&delta); }
                            this.scroll.scroll_to_bottom();
                            cx.notify();
                        }
                        reasoning::session::Event::Done(result) => this.finish_turn(id, result, cx),
                    }
                }).is_ok();
                if finished || !alive { break; }
            }
        }).detach();
    }

    /// The speculative answer for exactly what is being asked, if one is running or ready. One
    /// that doesn't match is cancelled (a miss). Assist and What do I say? on a heard question
    /// qualify; typed questions, Follow-ups and Recap never do.
    fn claim_prepared(&mut self, action: &str, question: &str, exchanges: usize) -> Option<reasoning::Claimed> {
        let action = ["Assist", "What do I say?"].into_iter().find(|known| *known == action && question.trim().is_empty());
        if let Some(action) = action { self.answer_action = action; }
        // A new question already being asked means the prepared answer is for an older one.
        let newer_question = self.provisional[1].as_ref().is_some_and(|line| line.question);
        let key = action.filter(|_| !newer_question).map(|action| self.context_key(action, exchanges));
        let session = self.reasoning.as_mut()?;
        session.speculating()?;
        // A key no speculation has: anything running is a miss.
        let claimed = session.claim(key.unwrap_or(0));
        if claimed.is_some() { self.planner.shown() } else { self.planner.finished() }
        claimed
    }

    /// The quick action whose answer is already being written for the current context.
    fn answer_ready(&self) -> Option<&'static str> {
        let ready = self.reasoning.as_ref()?.speculation_ready()?;
        if self.provisional[1].as_ref().is_some_and(|line| line.question) { return None; }
        (ready == self.context_key(self.answer_action, self.history().len())).then_some(self.answer_action)
    }

    /// Settings → Show answers automatically: the speculative answer just started becomes a
    /// turn of its own, unless an answer the user asked for is still coming in.
    fn show_automatically(&mut self, key: u64, utterance: UtteranceId, cx: &mut Context<Self>) {
        if self.turns.iter().any(|turn| turn.status == Status::Streaming) { return; }
        let Some(claimed) = self.reasoning.as_mut().and_then(|session| session.claim_automatically(key)) else { return };
        self.planner.shown();
        self.next_turn += 1;
        let id = self.next_turn;
        let at_ms = self.live_since.map(|start| start.elapsed().as_millis() as u64).unwrap_or(0);
        self.turns.push(Turn { id, action: self.answer_action.into(), question: String::new(), text: claimed.text, status: Status::Streaming,
            at_ms, screenshot: None, auto: Some((utterance, at_ms)) });
        self.scroll.scroll_to_bottom();
        match claimed.done {
            Some(result) => self.finish_turn(id, result, cx),
            None => self.stream_replies(id, claimed.generation, claimed.replies, cx),
        }
        cx.notify();
    }

    /// Stop an answer that was shown automatically, keeping what it has written.
    fn stop_turn(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.reasoning { session.cancel(); }
        let text = self.turns.iter().find(|turn| turn.id == id).map(|turn| turn.text.clone()).unwrap_or_default();
        self.finish_turn(id, Ok(text), cx);
        cx.notify();
    }

    fn context_key(&self, action: &str, exchanges: usize) -> u64 {
        speculation::context_key(&self.conversation().lines, self.latest_heard_utterance(), action, exchanges, &self.store.value)
    }

    /// Transcript updates drive answer preparation (`reasoning::speculation`).
    pub(crate) fn plan(&mut self, update: &crate::transcript::live::Update) -> Vec<Step> {
        use crate::transcript::live::Update;
        if self.reasoning.is_none() { return Vec::new(); }
        self.planner.enabled = self.store.value.speculative_answers;
        self.planner.budget.early_start = Budget::early_start_for(self.store.value.provider);
        let now = Instant::now();
        match update {
            Update::Provisional { source, id, stable, unstable } => self.planner.on_provisional(*source, *id, stable, unstable, now),
            Update::Committed { utterance, .. } => self.planner.on_committed(utterance.source, utterance.id, &utterance.text, now),
            Update::QuestionLikely { .. } | Update::Error { .. } => Vec::new(),
        }
    }

    /// Carry out what the planner decided, after the update itself was applied.
    pub(crate) fn run_steps(&mut self, steps: Vec<Step>, cx: &mut Context<Self>) {
        for step in steps {
            match step {
                Step::Prepare { .. } => {
                    // Preparation, no model requests: a screenshot now, so the answer doesn't
                    // wait for one, and a warm thread or process for a speculative answer.
                    if self.store.value.screen_on_send && let Some((x, y)) = self.native.and_then(platform::center) {
                        cx.spawn(async move |this, cx| {
                            let taken = Instant::now();
                            let jpeg = cx.background_executor().spawn(async move { crate::capture::screen_jpeg(x, y).ok() }).await;
                            if let Some(jpeg) = jpeg { let _ = this.update(cx, |this, _| this.prepared_shot.store(taken, jpeg)); }
                        }).detach();
                    }
                    if self.store.value.speculative_answers && let Some(session) = &self.reasoning { session.prewarm_speculation(&self.store.value); }
                }
                Step::Speculate { utterance } => self.speculate(utterance, cx),
                Step::Cancel => {
                    self.speculation_attempt += 1;
                    if let Some(session) = &mut self.reasoning { session.cancel_speculation() }
                }
                Step::Rekey => {
                    let key = self.context_key(self.answer_action, self.history().len());
                    if let Some(session) = &mut self.reasoning { session.rekey(key); }
                }
            }
        }
    }

    /// Start the answer the user will most likely ask for next, into a hidden buffer.
    fn speculate(&mut self, utterance: UtteranceId, cx: &mut Context<Self>) {
        let history = self.history();
        let (action, settings) = (self.answer_action, self.store.value.clone());
        self.speculation_attempt += 1;
        let attempt = self.speculation_attempt;
        let conversation = self.conversation().render();
        let point = if settings.screen_on_send { self.native.and_then(platform::center) } else { None };
        let prepared = if settings.screen_on_send { self.prepared_shot.fresh(Instant::now()) } else { None };
        cx.spawn(async move |this, cx| {
            let screenshot = match (prepared, point) {
                (Some(jpeg), _) => Some(jpeg),
                (None, Some((x, y))) => cx.background_executor().spawn(async move { crate::capture::screen_jpeg(x, y).ok() }).await,
                (None, None) => None,
            };
            let request = reasoning::Request { action: action.into(), question: String::new(), history, conversation, screenshot, utterance: Some(utterance) };
            let _ = this.update(cx, |this, cx| {
                // Cancelled or replaced while the screenshot was taken. Otherwise the context is
                // read now: its question may have been committed meanwhile.
                if this.speculation_attempt != attempt || !this.planner.wants(utterance) { return; }
                let key = this.context_key(action, request.history.len());
                let Some(session) = this.reasoning.as_mut() else { return };
                if let Err(reason) = session.speculate(&settings, request, key) {
                    eprintln!("speculative answer skipped: {reason}");
                    this.planner.finished();
                    return;
                }
                if settings.auto_answer { this.show_automatically(key, utterance, cx); }
            });
        }).detach();
    }

    fn finish_turn(&mut self, id: u64, result: Result<String, String>, cx: &mut Context<Self>) {
        let Some(turn) = self.turns.iter_mut().find(|t| t.id == id && t.status == Status::Streaming) else { return };
        match result {
            Ok(text) => {
                turn.text = text;
                turn.status = Status::Done;
                save_turn(&mut self.recorder, self.store.value.save_screenshots, turn, false);
            }
            Err(reason) => turn.status = Status::Failed(reason.into()),
        }
        self.remember_claude_models();
        cx.notify();
    }

    /// Which provider the key field belongs to: the transcription provider on the Listening
    /// tab, the API-key answer provider on the Model tab.
    pub(crate) fn key_target(&self) -> String {
        if self.settings_tab == Some(Tab::Listening) { crate::stt::deepgram::PROVIDER_ID.to_string() } else { self.store.value.api_provider.clone() }
    }

    /// Keep the Claude versions the CLI has reported (e.g. "opus" → claude-opus-5-5), so the pickers
    /// show real version numbers, also on the next launch before any answer.
    fn remember_claude_models(&mut self) {
        let mut changed = false;
        for (alias, id) in crate::claude_cli::resolved_models() {
            if self.store.value.claude_models.get(&alias) != Some(&id) {
                self.store.value.claude_models.insert(alias, id);
                changed = true;
            }
        }
        if changed { self.store.save(); }
    }

    /// Save the typed key for the tab's provider, then mark it saved (with its masked hint) in
    /// Settings. The credential store is used on a background thread: it can block, and on macOS
    /// show a Keychain prompt.
    pub fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).text().trim().to_string();
        if key.is_empty() { return; }
        let provider = self.key_target();
        self.key_notice = Some("Saving…".into());
        cx.notify();
        let target = provider.clone();
        cx.spawn_in(window, async move |this, cx| {
            let saved = cx.background_executor()
                .spawn(async move { crate::secrets::set(&target, &key).map(|()| crate::secrets::masked(&key).unwrap_or_default()) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match saved {
                    Ok(hint) => {
                        this.key_input.update(cx, |input, cx| input.clear(cx));
                        this.set_key_marker(&provider, Some(hint));
                        this.key_notice = Some(format!("Saved to {}.", crate::secrets::STORE_NAME).into());
                        #[cfg(target_os = "macos")]
                        this.key_saved(&provider, window, cx);
                    }
                    Err(error) => this.key_notice = Some(format!("Couldn't save the key: {error}").into()),
                }
                // Only macOS loads models from here.
                #[cfg(not(target_os = "macos"))]
                let _ = window;
                cx.notify();
            });
        }).detach();
    }

    /// Delete the tab's provider key, on a background thread, and its marker in Settings.
    pub fn remove_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.key_reveal.hide();
        let provider = self.key_target();
        let target = provider.clone();
        cx.spawn_in(window, async move |this, cx| {
            let removed = cx.background_executor().spawn(async move { crate::secrets::remove(&target) }).await;
            let _ = this.update(cx, |this, cx| {
                this.key_notice = Some(match removed {
                    Ok(()) => { this.set_key_marker(&provider, None); "Key removed.".into() }
                    Err(_) => "No saved key to remove.".into(),
                });
                cx.notify();
            });
        }).detach();
    }

    /// Keys saved before `Settings::saved_keys` existed have no marker. Look for them once at
    /// startup, in the background (on macOS by attributes only, which never shows a Keychain
    /// prompt), and mark the ones found.
    fn mark_existing_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let unmarked = self.store.value.unmarked_key_providers();
        if unmarked.is_empty() { return; }
        cx.spawn_in(window, async move |this, cx| {
            let found = cx.background_executor().spawn(async move {
                unmarked.into_iter().filter_map(|id| crate::secrets::find_marker(id).map(|hint| (id.to_string(), hint))).collect::<Vec<_>>()
            }).await;
            let _ = this.update(cx, |this, cx| {
                if this.store.value.mark_found_keys(found) {
                    this.store.save();
                    cx.notify();
                }
            });
        }).detach();
    }

    /// Record in Settings whether `provider` has a saved key, with its masked hint.
    pub(crate) fn set_key_marker(&mut self, provider: &str, hint: Option<String>) {
        let markers = &mut self.store.value.saved_keys;
        let changed = match hint {
            Some(hint) => markers.insert(provider.to_string(), hint.clone()).as_ref() != Some(&hint),
            None => markers.remove(provider).is_some(),
        };
        if changed { self.store.save(); }
    }

    /// Fetch the provider's model list off the UI thread.
    pub fn load_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = crate::providers::preset(&self.store.value.api_provider) else { return };
        let base = if preset.base_url.is_empty() { self.store.value.custom_base_url.trim().to_string() } else { preset.base_url.to_string() };
        let wire = preset.wire;
        self.models_loading = true;
        self.key_notice = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            // The key is read here, off the UI thread (the credential store can block or prompt).
            let (result, found) = cx.background_executor().spawn(async move {
                let key = crate::secrets::get(preset.id);
                let found = key.as_deref().and_then(crate::secrets::masked);
                (crate::providers::list_models(wire, &base, key.as_deref()), found)
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.models_loading = false;
                // A key saved before markers existed is marked once it has been read. A key that
                // can't be read leaves the marker alone: a denied Keychain prompt reads as no key.
                if found.is_some() { this.set_key_marker(preset.id, found); }
                // The provider changed while this list loaded: it belongs to the previous one.
                if this.store.value.api_provider != preset.id {
                    #[cfg(target_os = "macos")]
                    this.models_stale(window, cx);
                } else {
                    #[cfg(target_os = "macos")]
                    this.models_loaded(preset, result);
                    #[cfg(not(target_os = "macos"))]
                    match result {
                        Ok(models) => this.loaded_models = models,
                        Err(error) => this.key_notice = Some(format!("Couldn't load models: {error}").into()),
                    }
                }
                // Only macOS starts another load from here.
                #[cfg(not(target_os = "macos"))]
                let _ = window;
                cx.notify();
            });
        }).detach();
    }

    pub fn choose_model(&mut self, model: String, window: &mut Window, cx: &mut Context<Self>) {
        self.model_input.update(cx, |input, cx| input.set_text(model.clone(), cx));
        self.update_settings(|s| { s.api_models.insert(s.api_provider.clone(), model); }, window, cx);
    }

    fn toggle_visible(&mut self) {
        let Some(native) = self.native else { return };
        let visible = !platform::is_visible(native);
        if !visible { self.key_reveal.hide(); }
        platform::set_visible(native, visible);
        self.hotkeys.set_overlay_visible(visible);
        // A hidden panel must not keep claiming Esc.
        self.hotkeys.set_panel_open(visible && self.settings_tab.is_some());
    }

    fn show(&mut self) {
        if let Some(native) = self.native.filter(|native| !platform::is_visible(*native)) {
            platform::set_visible(native, true);
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
        let Some((dx, dy)) = platform::held_direction(kind == Motion::Scroll) else {
            self.motion = None;
            return false;
        };
        let cruise = if kind == Motion::Move { MOVE_CRUISE } else { SCROLL_CRUISE };
        let speed = (MOVE_START + frame as f32 * EASE_PER_FRAME).min(cruise);
        match kind {
            Motion::Move => if let Some(native) = self.native {
                let _ = platform::move_by(native, (dx as f32 * speed).round() as i32, (dy as f32 * speed).round() as i32);
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

    /// How far Settings has collapsed into the conversation (eased, 0 to 1) while it does. Each
    /// frame asks for the next; at the end Settings closes and the window shrinks once.
    fn collapse_progress(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<f32> {
        let started = self.collapse?;
        let progress = card::collapse_eased(started.elapsed(), COLLAPSE);
        if progress >= 1.0 { cx.defer_in(window, |this, window, cx| this.close_panels(window, cx)); }
        else { window.request_animation_frame(); }
        Some(progress)
    }

    fn elapsed(&self) -> String {
        let seconds = self.live_since.map(|start| start.elapsed().as_secs()).unwrap_or(0);
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}


impl Overlay {
    /// The conversation shown above the card's text box: transcript strip, answer thread and quick
    /// actions.
    fn live_parts(&self, cx: &mut Context<Self>) -> (gpui::Div, gpui::Stateful<gpui::Div>, gpui::Div) {
        let ticker = self.transcript_block();
        let mut thread = div().id("thread").flex().flex_col().gap(px(14.0)).px(px(18.0)).py(px(14.0))
            .flex_1().overflow_y_scroll().track_scroll(&self.scroll);
        if self.turns.is_empty() {
            thread = thread.child(div().text_size(px(13.0)).text_color(theme::muted())
                .child(format!("Press {} to answer from your screen and the conversation.", self.hotkeys.label(Action::Assist))));
        }
        for turn in &self.turns {
            let asked = if turn.question.is_empty() { turn.action.to_string() } else { turn.question.clone() };
            let header: AnyElement = match turn.auto {
                Some((utterance, shown_ms)) => {
                    // An early start is shown before its line is committed; then the line's time counts.
                    let heard = self.transcript.iter().find(|line| line.id == utterance).map_or(shown_ms, |line| line.at_ms);
                    auto_header(turn.id, heard, turn.status == Status::Streaming, self.hits.mark(), cx).into_any_element()
                }
                None => div().flex().justify_end().child(div().max_w(px(420.0)).text_size(px(13.0)).text_color(theme::text())
                    .px(px(12.0)).py(px(6.0)).rounded(px(12.0)).border_1().border_color(theme::bubble_border()).bg(theme::bubble())
                    .child(asked)).into_any_element(),
            };
            let body: AnyElement = match &turn.status {
                Status::Failed(reason) => div().text_size(px(13.0)).line_height(px(20.0)).text_color(gpui::rgb(0xffb4a8)).child(reason.clone()).into_any_element(),
                Status::Streaming if turn.text.is_empty() => div().text_size(px(13.0)).text_color(theme::muted()).child("Thinking…").into_any_element(),
                _ => div().id(("answer", turn.id as usize)).min_w_0().text_color(theme::text()).child(crate::markdown::render(&turn.text, self.store.value.text_size.answer())).into_any_element(),
            };
            thread = thread.child(header).child(body);
        }
        let ready = self.answer_ready();
        let mut actions = div().flex().items_center().gap(px(4.0)).px(px(12.0)).pt(px(8.0)).pb(px(8.0));
        for (index, label) in answer::ACTIONS.into_iter().enumerate() {
            let action = div().id(("action", index)).relative().flex().items_center().gap(px(6.0)).rounded(px(8.0)).cursor_pointer()
                .text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).hover(|b| b.bg(theme::raised()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.send(label, String::new(), window, cx)))
                .child(label).child(self.hits.mark());
            // A speculative answer for this action and context is already being written.
            actions = actions.child(if ready == Some(label) {
                action.px(px(9.0)).py(px(5.0)).bg(gpui::rgb(0x0d1830)).border_1().border_color(theme::bubble_border()).text_color(gpui::rgb(0xdce7ff))
                    .child(div().size(px(12.0)).flex_none().rounded_full().bg(gpui::rgba(0x4c8dff2e)).flex().items_center().justify_center()
                        .child(div().size(px(6.0)).rounded_full().bg(theme::accent())))
            } else { action.px(px(10.0)).py(px(6.0)) });
        }
        if ready.is_some() {
            actions = actions.child(div().flex_1().flex().justify_end().pr(px(4.0)).text_size(px(12.0)).text_color(theme::muted())
                .child("Answer ready for their last question"));
        }
        (ticker, thread, actions)
    }
}

/// The line under the idle overlay saying what Start does.
fn idle_hint(text: &'static str) -> impl IntoElement {
    div().text_size(px(12.0)).text_color(theme::body()).px(px(12.0)).py(px(5.0)).rounded_full().bg(gpui::rgba(0x0f1012b3)).child(text)
}

mod card;
mod update_notice;
mod switcher;
pub(crate) use card::CARD_WIDTH;
/// How many audio levels the Live waveform keeps (one per bar from the middle out).
pub(crate) const LEVEL_HISTORY: usize = 3;

/// What Start does, under the idle card.
#[cfg(target_os = "macos")]
const IDLE_HINT: &str = "Start listens to your Mac's audio + mic and attaches your screen to every message.";
#[cfg(not(target_os = "macos"))]
const IDLE_HINT: &str = "Start listens to your desktop + mic and attaches your screen to every message.";

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.live_since.is_some();
        self.hits.clear();
        let collapse = self.collapse_progress(window, cx);
        let mut root = div().size_full().flex().flex_col().items_center().gap(px(10.0)).pt(px(8.0)).pb(px(12.0))
            .font_family(theme::FONT).text_color(theme::text())
            .track_focus(&self.focus)
            .on_children_prepainted(click_through(self.native, self.hits.clone(), self.shape.clone(), self.settings_tab.is_some()))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                // Tab in the text box opens the mode switcher.
                let modifiers = event.keystroke.modifiers;
                if event.keystroke.key == "tab" && !modifiers.modified() && !this.switcher_open() && this.composer.focus_handle(cx).is_focused(window) {
                    cx.stop_propagation();
                    this.open_switcher(window, cx);
                    return;
                }
                if event.keystroke.key != "escape" { return; }
                if this.settings_tab.is_some() { this.close_panels(window, cx); }
                else if this.composer.focus_handle(cx).is_focused(window) {
                    this.composer.update(cx, |input, cx| input.clear(cx));
                    this.return_to_previous_app();
                }
            }));
        root = root.child(self.card(collapse, cx));
        // Settings as a panel under the card (Windows; macOS opens its own window and never sets
        // `settings_tab`). While collapsing it shrinks from its laid-out height and fades out.
        if let Some(tab) = self.settings_tab {
            let panel = div().relative().flex_none().child(self.settings_panel(tab, cx));
            root = root.child(match collapse {
                Some(progress) => panel.h(px(self.settings_height.get() * (1.0 - progress))).overflow_hidden().opacity(1.0 - progress),
                None => {
                    let height = self.settings_height.clone();
                    panel.child(gpui::canvas(move |bounds, _, _| height.set(f32::from(bounds.size.height)), |_, _, _, _| {})
                        .absolute().top_0().left_0().size_full())
                }
            });
        } else if !live {
            root = root.child(idle_hint(IDLE_HINT));
        }
        root
    }
}

/// Bounds of the controls that should catch the mouse, collected while laying out a frame.
#[derive(Clone, Default)]
pub struct Hits(Rc<RefCell<Vec<Hit>>>, Rc<std::cell::Cell<f32>>, Rc<RefCell<Vec<Hit>>>);

type Hit = gpui::Bounds<gpui::Pixels>;

impl Hits {
    fn clear(&self) { self.0.borrow_mut().clear(); self.2.borrow_mut().clear(); }

    /// Areas reserved this frame for content painted deferred (an open dropdown list).
    fn floating(&self) -> Vec<Hit> { self.2.borrow().clone() }

    /// Reserve `bounds` in the window region for content that is painted deferred, so it is
    /// visible in the very frame it appears. Deferred content lays out after the region is
    /// computed, so the element that opens it reserves the area during the main pass.
    pub fn reserve(&self, bounds: Hit) { self.2.borrow_mut().push(bounds); }

    /// Whether a window-relative physical point lies on an interactive control.
    fn contains(&self, (x, y): (i32, i32)) -> bool {
        let scale = self.1.get().max(0.5);
        let point = gpui::point(gpui::px(x as f32 / scale), gpui::px(y as f32 / scale));
        self.0.borrow().iter().any(|bounds| bounds.contains(&point))
    }

    /// An invisible child that records its parent's bounds. The parent must be `relative()`, and
    /// the mark must be its *last* child: the layout engine still applies the parent's `gap`
    /// after an absolutely positioned first child, which pushed the real children past the
    /// window region (the pill's right end was cut off).
    pub fn mark(&self) -> impl IntoElement {
        let hits = self.clone();
        gpui::canvas(move |bounds, _, _| hits.0.borrow_mut().push(bounds), |_, _, _, _| {}).absolute().top_0().left_0().size_full()
    }
}

/// Shape the window to what is visible (the card, Settings, the hint, open popovers), so the space
/// around them is click-through; inside them `update_passthrough` decides per cursor position.
/// While Settings is open (`settings_open`) the shape is held at one rectangle around everything
/// shown since it opened, so collapsing Settings into the conversation never reshapes the window:
/// changing the window region every frame stalled each frame for 20–80 ms.
fn click_through(native: Option<NativeWindow>, hits: Hits, applied: Rc<RefCell<Vec<platform::Shape>>>, settings_open: bool) -> impl Fn(Vec<gpui::Bounds<gpui::Pixels>>, &mut Window, &mut gpui::App) + 'static {
    move |children, window, _| {
        let Some(native) = native else { return };
        let scale = window.scale_factor();
        hits.1.set(scale);
        let shapes = region(&children, &hits.floating(), scale);
        let shapes = if settings_open { vec![enclosing(applied.borrow().iter().chain(&shapes), (18.0 * scale).round() as i32)] } else { shapes };
        if *applied.borrow() != shapes {
            platform::set_shape(native, &shapes);
            *applied.borrow_mut() = shapes;
        }
    }
}

/// The window region, in physical pixels, for the laid-out `children` of the overlay and the
/// `floating` areas reserved for deferred content (open lists and popovers), each a rounded
/// rectangle a pixel larger than its bounds. The same on every platform; only Windows applies it.
fn region(children: &[Hit], floating: &[Hit], scale: f32) -> Vec<platform::Shape> {
    let physical = |value: gpui::Pixels| (f32::from(value) * scale).round() as i32;
    children.iter().map(|bounds| (bounds, false)).chain(floating.iter().map(|bounds| (bounds, true))).map(|(bounds, float)| {
        let height = f32::from(bounds.size.height);
        let radius = if float { 10.0 } else if height <= 44.0 { height / 2.0 } else { 18.0 };
        (physical(bounds.left()) - 1, physical(bounds.top()) - 1, physical(bounds.right()) + 1, physical(bounds.bottom()) + 1,
            (radius * scale).round() as i32)
    }).collect()
}

/// The text box's placeholder says what an answer will see: the screen only while Screen on send is on.
fn composer_placeholder(settings: &Settings) -> &'static str {
    if settings.screen_on_send { "Ask about your screen or conversation…" } else { "Ask about your conversation…" }
}

/// One rounded rectangle around `shapes`.
fn enclosing<'a>(shapes: impl Iterator<Item = &'a platform::Shape>, radius: i32) -> platform::Shape {
    shapes.fold((i32::MAX, i32::MAX, i32::MIN, i32::MIN, radius), |(left, top, right, bottom, radius), &(l, t, r, b, _)| {
        (left.min(l), top.min(t), right.max(r), bottom.max(b), radius)
    })
}

/// Whether the overlay and Settings are kept out of screen captures right now.
pub(crate) fn capture_excluded(settings: &Settings) -> bool {
    // CLUELYRS_ALLOW_CAPTURE=1 is a development switch for taking screenshots of the overlay.
    let dev_override = std::env::var_os("CLUELYRS_ALLOW_CAPTURE").is_some_and(|value| value == "1");
    settings.hide_from_capture && !dev_override
}

pub(crate) fn apply_capture_setting(native: NativeWindow, settings: &Settings) {
    if let Err(error) = platform::set_capture_hidden(native, capture_excluded(settings)) {
        eprintln!("capture exclusion unavailable: {error}");
    }
}

/// The header of an answer shown without a press: what it is, which question it answers, and
/// Stop while it is still being written.
fn auto_header(id: u64, heard_ms: u64, streaming: bool, hit: impl IntoElement, cx: &mut Context<Overlay>) -> impl IntoElement {
    let label = gpui::rgb(0x7fa8ff);
    div().flex().items_center().gap(px(8.0))
        .child(ui::icon("icons/sparkle.svg", 13.0, label))
        .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(label).child("SHOWN AUTOMATICALLY"))
        .child(div().flex_1().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(format!("for their question at {}", archive::clock(heard_ms))))
        .when(streaming, |row| row.child(div().id(("stop-auto", id as usize)).px(px(10.0)).py(px(4.0)).rounded(px(8.0)).border_1().border_color(theme::hairline())
            .cursor_pointer().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(gpui::rgb(0xd9d5cc)).relative().child("Stop").child(hit)
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.stop_turn(id, cx)))))
}

#[cfg(test)]
mod tests {
    use super::{Status, Turn, composer_placeholder, enclosing, save_turn};
    use crate::archive::Archive;
    use crate::settings::Settings;

    /// Live stopping while an answer is written: History gets the partial answer marked as
    /// stopped (the question and screenshot too); a stopped Assist that wrote nothing is left out.
    #[test]
    fn an_answer_stopped_with_live_is_saved_with_its_partial_text() {
        let root = std::env::temp_dir().join(format!("cluelyrs-overlay-stopped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let archive = Archive::at(root.clone());
        let mut recorder = Some(archive.start(100, "ended".into()).unwrap());
        let turn = |action: &str, question: &str, text: &str| Turn { id: 1, action: action.to_string().into(), question: question.into(), text: text.into(),
            status: Status::Streaming, at_ms: 4_000, screenshot: Some(b"jpeg".to_vec()), auto: None };
        save_turn(&mut recorder, true, &turn("Assist", "", "Done answer"), false);
        save_turn(&mut recorder, true, &turn("Ask", "How do I scale it?", "Shard by tenant and"), true);
        save_turn(&mut recorder, true, &turn("Assist", "", " "), true);
        recorder.take().unwrap().finish(200).unwrap();
        let session = archive.load("ended").unwrap();
        assert_eq!(session.turns.len(), 2);
        assert!(!session.turns[0].stopped);
        let stopped = &session.turns[1];
        assert_eq!((stopped.question.as_str(), stopped.answer.as_str(), stopped.stopped), ("How do I scale it?", "Shard by tenant and", true));
        assert_eq!(stopped.screenshot.as_deref(), Some("002.jpg"));
        // Complete answers are written as before: no `stopped` field.
        let json = std::fs::read_to_string(root.join("ended").join("session.json")).unwrap();
        assert_eq!(json.matches("\"stopped\"").count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_placeholder_mentions_the_screen_only_while_it_is_sent() {
        let on = Settings { screen_on_send: true, ..Settings::default() };
        let off = Settings { screen_on_send: false, ..Settings::default() };
        assert_eq!(composer_placeholder(&on), "Ask about your screen or conversation…");
        assert_eq!(composer_placeholder(&off), "Ask about your conversation…");
    }

    #[test]
    fn open_settings_hold_one_rectangle_around_every_shape() {
        let shapes = [(10, 0, 650, 104, 36), (10, 114, 650, 700, 36)];
        assert_eq!(enclosing(shapes.iter(), 36), (10, 0, 650, 700, 36));
        // Held across frames: once it encloses everything, a smaller frame doesn't shrink it.
        let held = enclosing(shapes.iter(), 36);
        assert_eq!(enclosing([held, (10, 0, 650, 300, 36)].iter(), 36), held);
    }
}
