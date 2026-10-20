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
use crate::attachments::{self, Attachment, Content};
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
use crate::shortcut_keys::{self, Typed};
use crate::theme;
use crate::ui;
use crate::platform::{self, NativeWindow, PreviousFocus};

/// The overlay window; the card is 40 px narrower.
pub const WIDTH: f32 = 640.0;
/// The idle card and room for a tooltip below it.
pub const IDLE_HEIGHT: f32 = 160.0;
/// The card at its tallest (about 520 px) and room for a tooltip below it.
const LIVE_HEIGHT: f32 = 680.0;
/// macOS sign-in in Terminal: how often the account is checked, and for how long.
#[cfg(target_os = "macos")]
const SIGN_IN_POLL: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
/// Every this many sign-in polls (30 s), Codex is checked fully whatever `codex login status` says.
#[cfg(target_os = "macos")]
const CODEX_FULL_CHECK: u32 = 15;
/// Held movement eases in so a tap nudges, then cruises. Pixels per ~16 ms frame.
const MOVE_START: f32 = 4.0;
const MOVE_CRUISE: f32 = 22.0;
const SCROLL_CRUISE: f32 = 26.0;
const EASE_PER_FRAME: f32 = 0.6;
const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Motion { Move, Scroll }

#[derive(Clone, PartialEq)]
enum Status {
    Streaming,
    Done,
    Failed(SharedString),
    /// Live stopped while it was being written; its text is what it had by then.
    Stopped,
}

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
    /// The Live session it belongs to (`Overlay::session`). Earlier sessions stay on the card,
    /// but only the current one's turns go to the model.
    session: u64,
    /// Files attached to the question (`attach`), and those of them cut to fit the prompt budget.
    attachments: Arc<[Attachment]>,
    cut: Vec<String>,
}

/// Add an answer to the session's History record, when one is being saved. A `stopped` answer
/// (Live ended while it was being written) keeps the text it had, unless nothing was asked or written.
fn save_turn(recorder: &mut Option<Recorder>, screenshots: bool, turn: &Turn, stopped: bool) {
    let Some(recorder) = recorder else { return };
    if stopped && turn.text.trim().is_empty() && turn.question.trim().is_empty() { return; }
    let shot = if screenshots { turn.screenshot.as_deref() } else { None };
    // Attached files are kept by name and type, with what the model read from each: a document's
    // text, and an image when screenshots are saved too.
    let attachments = turn.attachments.iter().map(|file| archive::Attached { name: file.name.clone(), kind: file.kind.tag().into(), bytes: file.bytes, file: None }).collect();
    let kept: Vec<Option<archive::Kept>> = turn.attachments.iter().map(|file| match &file.content {
        Content::Text(text) => Some(archive::Kept::Text(text)),
        Content::Jpeg(jpeg) => screenshots.then_some(archive::Kept::Jpeg(jpeg)),
    }).collect();
    let saved = archive::Turn { at_ms: turn.at_ms, action: turn.action.to_string(), question: turn.question.clone(),
        answer: turn.text.clone(), screenshot: None, stopped, attachments };
    if let Err(error) = recorder.add_turn(saved, shot, &kept) { eprintln!("answer could not be saved: {error}"); }
}

pub struct Overlay {
    native: Option<NativeWindow>,
    /// The overlay's own window. Settings handlers run here even when macOS shows the settings
    /// in their own window (see `settings_view::listen`).
    pub(crate) own_window: gpui::AnyWindowHandle,
    pub(crate) hotkeys: Hotkeys,
    pub(crate) store: Store,
    /// The Sessions review window, while it's open.
    sessions_window: Option<gpui::WindowHandle<SessionsWindow>>,
    /// macOS shows settings in their own window, while it's open.
    #[cfg(target_os = "macos")]
    pub(crate) settings_window: Option<gpui::WindowHandle<crate::settings_window::SettingsWindow>>,
    /// The tab the macOS Settings window last drew, so the shared key field knows its provider.
    #[cfg(target_os = "macos")]
    pub(crate) settings_window_tab: Option<Tab>,
    /// macOS first-run setup, while it's open (`onboarding_window`).
    #[cfg(target_os = "macos")]
    pub(crate) onboarding_window: Option<gpui::WindowHandle<crate::onboarding_window::OnboardingWindow>>,
    /// The overlay is hidden, and Live hasn't started, until the first-run setup finishes.
    #[cfg(target_os = "macos")]
    hidden_for_onboarding: bool,
    /// When Assist was last pressed as setup's Shortcuts practice. While setup is open, Assist asks nothing.
    #[cfg(target_os = "macos")]
    pub(crate) assist_practiced: Option<Instant>,
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
    /// Windows' Load models is fetching the list (macOS has the model typed instead).
    #[cfg(not(target_os = "macos"))]
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
    /// The subscription being signed in to, while its sign-in runs (macOS: while it waits for Terminal).
    pub(crate) signing_in: Option<Provider>,
    /// Bumped by every macOS sign-in and by cancelling one, so a replaced wait stops polling.
    #[cfg(target_os = "macos")]
    sign_in_attempt: u64,
    /// Why the file picked for a subscription's CLI can't be used (Settings › Model).
    pub(crate) cli_error: Option<(Provider, SharedString)>,
    /// The Live session being saved to History, when saving is on.
    pub(crate) recorder: Option<Recorder>,
    /// The held-key loop that is currently running, if any.
    motion: Option<Motion>,
    pub(crate) live_since: Option<Instant>,
    /// Every turn on the card since launch, across Live sessions (see `chat`).
    turns: Vec<Turn>,
    /// The current (or last) Live session, counted from launch; 0 before the first.
    session: u64,
    /// Where each later session starts in `turns`, with its "New session" label.
    dividers: Vec<chat::Divider>,
    /// Files attached to the question being typed (`attach`).
    attached: attach::Attached,
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
    /// The toolbar control under the pointer whose tooltip shows under the card (`toggles`).
    pub(crate) tip: Option<crate::toggles::Tip>,
    /// The modes and which one is active (`modes`); its context and files are on `store.value.mode`.
    pub(crate) modes: crate::modes::ModeStore,
    /// Settings › Modes (`modes_view`).
    pub(crate) modes_ui: crate::modes_view::ModesUi,
    /// Update checks: what Settings › About shows and the notice above the text box (`update_notice`).
    pub(crate) updates: crate::update::Tracker,
    /// The mode switcher under the card, while it's open.
    switcher: Option<switcher::Switcher>,
    /// The Sessions panel in place of the card, while it's open.
    sessions_panel: Option<sessions_panel::SessionsPanel>,
    /// Windows: the Settings window and what it shows (`settings_view::fluent`).
    #[cfg(windows)]
    pub(crate) fluent: crate::settings_view::fluent::Fluent,
    /// Windows: first-run setup, while its window is open; the overlay stays hidden meanwhile.
    #[cfg(windows)]
    pub(crate) setup: Option<crate::onboarding::Setup>,
    /// The menu bar icon (macOS) or tray icon (Windows) and its menu (`menu_bar`).
    #[cfg(any(target_os = "macos", windows))]
    menu_bar: Option<menu_bar::MenuBar>,
    /// Windows: the tray menu or a question it asked, while one is open (`tray_menu`).
    #[cfg(windows)]
    popups: tray_menu::Popups,
    /// Windows: created hidden for first-run setup, so not yet where `main` asked (a hidden window
    /// starts where Windows puts new windows). The first `show` puts it top centre.
    #[cfg(windows)]
    unplaced: bool,
}

impl Overlay {
    /// `relaunches` yields each time CluelyRS is launched again while this copy runs (`platform::single_instance`).
    pub fn new(hotkeys: Hotkeys, presses: UnboundedReceiver<Action>, relaunches: UnboundedReceiver<()>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let native = platform::native_window(window);
        let mut store = Store::load();
        // A first run (or one that quit during setup) opens setup with the overlay hidden; main.rs
        // opens the window unshown.
        let onboarding = store.onboarding_pending();
        let modes = crate::modes::ModeStore::load();
        store.value.mode = modes.active_material();
        if let Some(native) = native {
            // Failures here leave a visible, usable window; they are not fatal.
            apply_capture_setting(native, &store.value);
            platform::remove_frame(native);
            let _ = platform::set_topmost(native);
        }
        // Windows opens first-run setup instead of the overlay (`main` keeps the overlay hidden).
        let setup = cfg!(windows) && store.setup_needed();
        let mut hotkeys = hotkeys;
        hotkeys.set_overlay_visible(!setup && !onboarding);
        if !hotkeys.unavailable.is_empty() { eprintln!("shortcuts in use by another app: {:?}", hotkeys.unavailable); }

        cx.spawn_in(window, async move |this, cx| {
            let mut presses = presses;
            while let Some(action) = presses.next().await {
                if this.update_in(cx, |this, window, cx| this.handle(action, window, cx)).is_err() { break; }
            }
        }).detach();
        // What is typed into the overlay while the app in front keeps the keyboard (`take_keyboard`).
        // Dispatched as the window's own key presses would be, outside any update of the overlay,
        // whose key handlers update it themselves.
        let typed = shortcut_keys::typed_keys();
        cx.spawn_in(window, async move |this, cx| {
            let mut typed = typed;
            while let Some(typed) = typed.next().await {
                let open = match typed {
                    Typed::Key(keystroke) => cx.update(|window, cx| { window.dispatch_keystroke(keystroke, cx); }).is_ok(),
                    Typed::Lost => this.update_in(cx, |this, window, cx| this.keyboard_lost(window, cx)).is_ok(),
                };
                if !open { break; }
            }
        }).detach();
        cx.spawn_in(window, async move |this, cx| {
            let mut relaunches = relaunches;
            while relaunches.next().await.is_some() {
                if this.update(cx, |this, cx| this.relaunched(cx)).is_err() { break; }
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

        // Every quit (⌘Q, the menu, Settings › About, a quit Apple Event; Settings › About on Windows)
        // passes here: stop transcribing and free parakeet.cpp's backend before exit. With Metal ggml
        // aborts otherwise; on Windows (CPU) it's an orderly stop of the transcriber before exit.
        cx.on_app_quit(|this, _| {
            this.stop_listening();
            crate::stt::parakeet::shutdown();
            // Windows leaves an exited app's tray icon behind until the pointer passes over it.
            #[cfg(windows)]
            { this.menu_bar = None; }
            async {}
        }).detach();

        let archive = Archive::default_location();
        if let Some(archive) = &archive {
            // A previous run that was killed mid-session leaves an unfinished record.
            archive.recover();
            if store.value.save_sessions { archive.prune(store.value.keep_sessions, archive::unix_now()); }
        }
        // Live on launch waits for setup to finish (Windows `onboarding`, macOS `finish_onboarding`).
        let start_live = store.value.start_live_on_launch && !setup && !onboarding;
        let composer_hint = crate::appearance::legible(theme::placeholder(), store.value.background_opacity);
        let composer = cx.new(|cx| { let mut input = TextInput::new(composer_placeholder(&store.value), cx).with_text_size(14.0, 20.0); input.set_placeholder_color(composer_hint, cx); input });
        let key_input = cx.new(|cx| TextInput::new("Paste your API key", cx).masked(true));
        // macOS has the model typed, with an example id for the provider; Windows lists models below.
        #[cfg(target_os = "macos")]
        let model_placeholder = crate::settings_view::model_placeholder(&store.value.api_provider);
        #[cfg(not(target_os = "macos"))]
        let model_placeholder = MODEL_PLACEHOLDER;
        let model_input = cx.new(|cx| TextInput::new(model_placeholder, cx));
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
        let model_ui = crate::settings_view::ModelUi::new(&key_input, window, cx);
        let mut overlay = Self { native, own_window: window.window_handle(), hotkeys, store, sessions_window: None,
            #[cfg(target_os = "macos")]
            settings_window: None,
            #[cfg(target_os = "macos")]
            settings_window_tab: None,
            #[cfg(target_os = "macos")]
            onboarding_window: None,
            #[cfg(target_os = "macos")]
            hidden_for_onboarding: onboarding,
            #[cfg(target_os = "macos")]
            assist_practiced: None,
            archive, focus: cx.focus_handle(),
            recorder: None, shape: Rc::default(), hits: Hits::default(), composer, key_input, model_input, base_url_input,
            #[cfg(target_os = "macos")]
            model_ui,
            key_reveal: Default::default(), opacity_drag: false,
            key_notice: None, loaded_models: Vec::new(),
            #[cfg(not(target_os = "macos"))]
            models_loading: false,
            reasoning: None, planner: Planner::new(false, Budget::default()), prepared_shot: PreparedShot::default(), answer_action: "Assist", speculation_attempt: 0,
            metrics: None, next_turn: 0, catching_mouse: true, return_focus: None,
            codex: crate::codex::CodexClient::new(), codex_status: None, claude_status: None, signing_in: None,
            #[cfg(target_os = "macos")]
            sign_in_attempt: 0,
            cli_error: None,
            motion: None, live_since: None, turns: Vec::new(), session: 0, dividers: Vec::new(), attached: Default::default(), scroll: ScrollHandle::new(),
            listening: None, listening_status: None, listening_epoch: 0, transcript: Vec::new(), provisional: Default::default(), levels: Default::default(),
            model_installed: false, model_download: None, model_notice: None,
            open_picker: None, reveal_accounts: false, devices: None, devices_loading: false, archive_bytes: None, picker_face: Rc::default(), tip: None,
            modes_ui, modes, updates: Default::default(), switcher: None, sessions_panel: None,
            #[cfg(windows)]
            fluent: Default::default(),
            #[cfg(windows)]
            setup: None,
            #[cfg(any(target_os = "macos", windows))]
            menu_bar: None,
            #[cfg(windows)]
            popups: Default::default(),
            #[cfg(windows)]
            unplaced: false };
        overlay.refresh_model_status();
        overlay.mark_existing_keys(window, cx);
        #[cfg(target_os = "macos")]
        overlay.update_dock();
        #[cfg(any(target_os = "macos", windows))]
        overlay.start_menu_bar(window, cx);
        if start_live { overlay.set_live(true, window, cx); }
        #[cfg(windows)]
        if setup {
            overlay.unplaced = true;
            overlay.open_setup(window, cx);
        }
        // Deferred: the setup window reads the overlay, which doesn't exist until this returns.
        #[cfg(target_os = "macos")]
        if onboarding {
            let step = overlay.store.value.onboarding_step.unwrap_or(crate::settings::OnboardingStep::Welcome);
            cx.defer_in(window, move |this, window, cx| this.open_onboarding_at(step, window, cx));
        }
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
        if previous.show_in_dock != self.store.value.show_in_dock {
            // Leaving the Dock would send Settings behind other apps (`settings_window::keep_in_front`).
            // While setup is open CluelyRS stays in the Dock, and the switch only records the choice.
            let in_front = self.settings_window.filter(|&handle| !self.in_dock() && crate::settings_window::is_in_front(handle, cx));
            self.update_dock();
            if let Some(handle) = in_front { crate::settings_window::keep_in_front(handle, cx); }
        }
        // Grows and shrinks around the card's centre, keeping its top edge (the height doesn't change).
        if let Some(native) = self.native && previous.card_width != self.store.value.card_width {
            platform::set_width_centred(native, self.store.value.card_width.window());
        }
        if previous.background_opacity != self.store.value.background_opacity { self.dim_placeholders(cx); }
        if previous.screen_on_send != self.store.value.screen_on_send {
            let placeholder = composer_placeholder(&self.store.value);
            self.composer.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
        if previous.api_provider != self.store.value.api_provider {
            // Each provider keeps its own model; show the one saved for the new provider.
            let model = self.store.value.api_model().to_string();
            self.model_input.update(cx, |input, cx| input.set_text(model, cx));
            #[cfg(target_os = "macos")]
            {
                let placeholder = crate::settings_view::model_placeholder(&self.store.value.api_provider);
                self.model_input.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
            }
            self.loaded_models.clear();
            self.key_notice = None;
        }
        let now = &self.store.value;
        if previous.transcribe != now.transcribe || previous.stt_provider != now.stt_provider || previous.listen_mic != now.listen_mic || previous.listen_desktop != now.listen_desktop
            || previous.mic_device != now.mic_device || previous.desktop_device != now.desktop_device || previous.use_gpu != now.use_gpu {
            self.restart_listening_if_live(window, cx);
        }
        cx.notify();
    }

    pub fn open_settings(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_tab(tab, window, cx);
        #[cfg(target_os = "macos")]
        crate::settings_window::open(tab, cx);
        // Windows: the Settings window (`settings_view::fluent`).
        #[cfg(windows)]
        {
            crate::settings_view::fluent::open(self, tab, None, cx);
            self.fluent_prepare(tab, window, cx);
        }
    }

    /// Setup from the start, with the current settings (Settings › About's Run setup again…, the menu
    /// bar's Run Setup Again…).
    #[cfg(target_os = "macos")]
    pub fn open_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_onboarding_at(crate::settings::OnboardingStep::Welcome, window, cx);
    }

    #[cfg(target_os = "macos")]
    fn open_onboarding_at(&mut self, step: crate::settings::OnboardingStep, window: &mut Window, cx: &mut Context<Self>) {
        let step = crate::onboarding_window::shown(step);
        self.assist_practiced = None;
        self.prepare_onboarding(step, window, cx);
        crate::onboarding_window::open(step, cx);
    }

    /// A setup step is opening: save it, so a relaunch resumes there, and refresh what it shows
    /// (accounts, the speech model, audio devices), as for a Settings tab. Nothing here reads the Keychain.
    #[cfg(target_os = "macos")]
    pub(crate) fn prepare_onboarding(&mut self, step: crate::settings::OnboardingStep, window: &mut Window, cx: &mut Context<Self>) {
        use crate::settings::OnboardingStep;
        self.update_settings(|s| crate::onboarding_window::record(s, Some(step)), window, cx);
        // The key field is shared by the Model and Transcription steps.
        self.key_notice = None;
        match step {
            OnboardingStep::Model => self.prepare_tab(Tab::Model, window, cx),
            // Permissions' audio check uses Listening's device pickers.
            OnboardingStep::Transcription | OnboardingStep::Permissions => self.prepare_tab(Tab::Listening, window, cx),
            _ => self.prepare_tab(Tab::Window, window, cx),
        }
    }

    /// Setup finished, was skipped or closed: it isn't shown again, and the overlay appears (with Live,
    /// on a first run, when Start Live when the app opens is on). Runs before the setup window
    /// closes, so CluelyRS has left the Dock (unless "Show in Dock" is on) by the time it does: tools
    /// that quit apps whose last window closes leave apps outside the Dock alone (`update_dock`).
    #[cfg(target_os = "macos")]
    pub(crate) fn finish_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.onboarding_window = None;
        self.update_settings(|s| crate::onboarding_window::record(s, None), window, cx);
        self.key_reveal.hide();
        self.update_dock();
        // Leaving the Dock deactivates CluelyRS; Settings, if it's open, comes back in front. The
        // overlay doesn't need it: it shows without activating anything.
        if let Some(handle) = self.settings_window.filter(|_| !self.in_dock()) { crate::settings_window::keep_in_front(handle, cx); }
        self.show();
        if std::mem::take(&mut self.hidden_for_onboarding) && self.store.value.start_live_on_launch && self.live_since.is_none() {
            self.set_live(true, window, cx);
        }
    }

    /// Assist pressed while setup is open: on the Shortcuts step it is the practice, which flashes the
    /// keys (`onboarding_window::PRACTICE_FLASH`) and then shows it worked. Nothing is sent, and Live
    /// doesn't start, whichever step is open.
    #[cfg(target_os = "macos")]
    fn practice_assist(&mut self, cx: &mut Context<Self>) {
        if self.store.value.onboarding_step != Some(crate::settings::OnboardingStep::Shortcuts) { return; }
        self.assist_practiced = Some(Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::onboarding_window::PRACTICE_FLASH).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }).detach();
    }

    /// In the Dock while "Show in Dock" is on, and while setup is open whatever it says (`in_dock`).
    #[cfg(target_os = "macos")]
    pub(crate) fn update_dock(&self) {
        platform::set_in_dock(self.in_dock());
    }

    #[cfg(target_os = "macos")]
    fn in_dock(&self) -> bool { in_dock(&self.store.value, self.onboarding_window.is_some()) }

    /// Refresh what a settings tab shows (accounts, devices, history size) as it opens.
    pub(crate) fn prepare_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.open_picker = None;
        // A key shown while typing, or a saved key shown on request, is hidden again whenever a tab opens.
        self.key_input.update(cx, |input, cx| input.set_revealed(false, cx));
        self.key_reveal.hide();
        if tab == Tab::Model { self.refresh_subscriptions(window, cx); }
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
    pub fn open_sessions(&mut self, cx: &mut Context<Self>) { self.open_sessions_at(None, cx); }

    /// Open (or focus) the Sessions window, showing session `id` when one is given (the overlay's
    /// Sessions panel).
    pub(crate) fn open_sessions_at(&mut self, id: Option<String>, cx: &mut Context<Self>) {
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
                show_session(handle, id, cx);
                overlay.update(cx, |overlay, _| overlay.sessions_window = handle);
            }));
        }
        #[cfg(not(target_os = "macos"))]
        {
            sessions_window::open(&mut self.sessions_window, root, self.codex.clone(), true, cx);
            show_session(self.sessions_window, id, cx);
        }
    }

    /// Close any open dropdown and hide what Settings revealed (accounts, a saved key), as when
    /// setup opens. Settings itself is a window of its own on both platforms.
    pub fn close_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_picker = None;
        self.reveal_accounts = false;
        self.key_reveal.hide();
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
        let mut height = if self.live_since.is_some() || !self.turns.is_empty() { LIVE_HEIGHT }
            else if self.update_notice_shown() { IDLE_HEIGHT + update_notice::NOTICE_HEIGHT } else { IDLE_HEIGHT };
        // The Sessions panel replaces the card, taller than it is idle.
        if self.sessions_panel_open() { height = height.max(SESSIONS_PANEL_WINDOW_HEIGHT); }
        // The mode switcher hangs below the card.
        if self.switcher_open() { height = height.max(switcher::window_height(self.live_since.is_some() || !self.turns.is_empty(), self.update_notice_shown())); }
        // Chips above the text box make the card taller.
        if !self.attached.is_empty() { height += attach::ROOM; }
        platform::resize(window, self.native, size(px(self.store.value.card_width.window()), px(height)));
    }

    fn handle(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        // During setup the overlay is only there to look at: Assist shows it (nothing starts or is
        // sent), Show / hide and the move keys work as usual, and the rest wait until setup ends.
        #[cfg(windows)]
        if self.setup.is_some() {
            match action {
                Action::Assist => { self.show(); cx.notify(); }
                Action::Toggle => self.toggle_visible(),
                Action::MoveUp | Action::MoveDown | Action::MoveLeft | Action::MoveRight => self.start_motion(Motion::Move, window, cx),
                _ => {}
            }
            return;
        }
        #[cfg(windows)]
        if action == Action::Close && self.popups.close_open(cx) { return; }
        match action {
            Action::Toggle => {
                // Hiding closes the mode switcher, handing the keyboard back.
                if self.native.is_some_and(platform::is_visible) { self.close_switcher(window, cx); }
                self.toggle_visible();
            }
            #[cfg(target_os = "macos")]
            Action::Assist if self.onboarding_window.is_some() => self.practice_assist(cx),
            Action::Assist => self.send("Assist", String::new(), window, cx),
            // The text box and the mode chip are on the card, which the Sessions panel replaces.
            Action::Focus => { self.close_sessions_panel(false, window, cx); self.type_shortcut(window, cx) }
            Action::SwitchMode => { self.close_sessions_panel(false, window, cx); self.switch_mode_shortcut(window, cx) }
            Action::MoveUp | Action::MoveDown | Action::MoveLeft | Action::MoveRight => self.start_motion(Motion::Move, window, cx),
            Action::ScrollUp | Action::ScrollDown => self.start_motion(Motion::Scroll, window, cx),
            // Esc, claimed while the Windows Settings panel is open, closes the switcher over it first.
            Action::Close => if self.switcher_open() { self.close_switcher(window, cx) }
                else if self.sessions_panel_open() { self.close_sessions_panel(true, window, cx) } else { self.close_panels(window, cx) },
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
            chat::stop(&mut self.turns);
            // The stopped mark (and the last of the cut-off answer) lands at the bottom: show it.
            if !self.turns.is_empty() { self.scroll.scroll_to_bottom(); }
            // Dropping the session cancels the running request; its late replies are stale.
            self.reasoning = None;
            // The pipeline's own final commits arrive after it's gone and are dropped, so save
            // what each source was still saying from the last provisional text first.
            self.archive_provisional();
            self.stop_listening();
            // The chat and the last heard lines stay on the card until the next session starts.
            self.provisional = Default::default();
            if let Some(metrics) = self.metrics.take() { std::thread::spawn(move || metrics.export()); }
        }
        self.record(live);
        if live {
            // A new session: earlier turns stay on the card under a divider, but the model,
            // speculation and the transcript start empty.
            self.session += 1;
            if chat::begin(self.turns.len(), &mut self.dividers, chat::divider_label(chrono::Local::now())) { self.scroll.scroll_to_bottom(); }
            self.clear_transcript();
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

    /// Quit CluelyRS (Settings › About; the tray menu on Windows). On Windows, quitting while Live is
    /// on asks first (`ask_quit`).
    pub(crate) fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(windows)]
        if self.live_since.is_some() { self.ask_quit(cx); return; }
        self.quit_now(window, cx);
    }

    /// Live ends first, as when it's stopped, so the session is saved to History; `on_app_quit`
    /// then stops the transcriber.
    pub(crate) fn quit_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.live_since.is_some() { self.set_live(false, window, cx); }
        cx.quit();
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
                // The mode gives the session its icon in the Sessions panel.
                Ok(mut recorder) => { recorder.set_mode(&self.modes.active().id); self.recorder = Some(recorder) }
                Err(error) => eprintln!("session recording unavailable: {error}"),
            }
        }
    }

    fn update_passthrough(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(native) = self.native else { return };
        let over_control = platform::cursor_in_window(native).is_some_and(|point| self.hits.contains(point));
        if over_control != self.catching_mouse {
            self.catching_mouse = over_control;
            platform::set_mouse_passthrough(native, !over_control);
        }
        // A press anywhere that isn't one of the overlay's controls (the click-through answer
        // area, or another app) never reaches the overlay, so the mode switcher closes from here;
        // the app clicked takes the keyboard.
        if self.switcher_open() && !over_control && platform::left_button_down() { self.dismiss_switcher(window, cx); }
        // Likewise, leaving a control for the click-through area sends the overlay no "hover
        // ended", so a hover tooltip is cleared here.
        if self.tip.is_some() && !over_control {
            self.tip = None;
            cx.notify();
        }
        self.drag_ended(cx);
    }

    /// Start Live if needed, take the foreground and put the caret in the composer.
    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show();
        if self.live_since.is_none() { self.set_live(true, window, cx); }
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

    /// Typing here ended in the other app (a click there, Alt+Tab), which has the keyboard already.
    /// Unless the overlay took it again meanwhile.
    fn keyboard_lost(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if platform::typing_into(window) { return; }
        self.return_focus = None;
        self.dismiss_switcher(window, cx);
        cx.notify();
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
    #[cfg(not(target_os = "macos"))]
    pub fn sign_in(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        if self.signing_in.is_some() { return; }
        self.signing_in = Some(provider);
        let codex = self.codex.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                match provider { Provider::Codex => codex.login(), Provider::Claude => crate::claude_cli::ClaudeCli::login(), Provider::ApiKey => Ok(()) }
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.signing_in = None;
                if let Err(error) = result { this.key_notice = Some(error.into()); }
                this.refresh_subscriptions(window, cx);
            });
        }).detach();
    }

    /// macOS: open the CLI's own sign-in in a Terminal window (it opens the browser itself), where
    /// the user can see and answer it, then check every few seconds until the account is signed in,
    /// for up to `SIGN_IN_WAIT`. Clicking Sign in again while it waits stops waiting.
    #[cfg(target_os = "macos")]
    pub fn sign_in(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        self.sign_in_attempt += 1;
        self.key_notice = None;
        cx.notify();
        if self.signing_in.take() == Some(provider) { return; }
        let (attempt, codex) = (self.sign_in_attempt, self.codex.clone());
        self.signing_in = Some(provider);
        cx.spawn_in(window, async move |this, cx| {
            // Resolving the CLI reads the disk, and writing the script too: off the UI thread.
            let opened = cx.background_executor().spawn(async move {
                let (exe, args) = match provider {
                    Provider::Codex => (crate::codex::executable()?, &["login"][..]),
                    _ => (crate::claude_cli::ClaudeCli::executable()?, &["auth", "login", "--claudeai"][..]),
                };
                platform::run_in_terminal(&exe, args).map_err(|error| format!("Terminal couldn't open: {error}"))
            }).await;
            let started = Instant::now();
            let mut failed = opened.err();
            let mut poll = 0;
            loop {
                if failed.is_none() { cx.background_executor().timer(SIGN_IN_POLL).await; }
                if this.update(cx, |this, _| this.sign_in_attempt != attempt).unwrap_or(true) { return; }
                let codex = codex.clone();
                poll += 1;
                // A running Codex app-server keeps the signed-out account it read at start, so a check
                // starts a fresh one, but only once `codex login status` stops saying signed out.
                // `None` is a poll that changed nothing.
                let status = if failed.is_some() { None } else {
                    cx.background_executor().spawn(async move {
                        match provider {
                            Provider::Codex if !codex_restart_due(crate::codex::login_status(), poll) => None,
                            Provider::Codex => { codex.shutdown(); Some(codex.status()) }
                            _ => Some(crate::claude_cli::ClaudeCli::status()),
                        }
                    }).await
                };
                let done = this.update(cx, |this, cx| {
                    if this.sign_in_attempt != attempt { return true; }
                    let signed_in = status.as_ref().is_some_and(|status| status.signed_in);
                    let timed_out = started.elapsed() >= SIGN_IN_WAIT;
                    if let Some(status) = status { if provider == Provider::Codex { this.codex_status = Some(status) } else { this.claude_status = Some(status) } }
                    if signed_in || timed_out || failed.is_some() {
                        this.signing_in = None;
                        this.key_notice = failed.take().or((timed_out && !signed_in).then(|| "Sign-in didn't finish. Try again.".to_string())).map(Into::into);
                    }
                    cx.notify();
                    this.signing_in.is_none()
                }).unwrap_or(true);
                if done { return; }
            }
        }).detach();
    }

    /// Send what is typed, with the files attached to it. A file still being read is waited for
    /// (the return button stays grey); refused files are cleared, never sent.
    pub(crate) fn send_composer(&mut self, input: Entity<TextInput>, window: &mut Window, cx: &mut Context<Self>) {
        let question = input.read(cx).text().trim().to_string();
        if (question.is_empty() && !self.attached.has_ready()) || self.attached.reading() { return; }
        input.update(cx, |input, cx| input.clear(cx));
        let attachments = self.attached.take();
        self.send_with("Ask", question, attachments, window, cx);
        self.return_to_previous_app();
    }

    /// Ask the selected provider: optional screenshot, recent exchanges as context, streamed reply.
    /// A speculative answer prepared for exactly this context is shown at once instead.
    fn send(&mut self, action: &str, question: String, window: &mut Window, cx: &mut Context<Self>) {
        self.send_with(action, question, Arc::default(), window, cx);
    }

    /// `send`, with files attached to the question.
    fn send_with(&mut self, action: &str, question: String, attachments: Arc<[Attachment]>, window: &mut Window, cx: &mut Context<Self>) {
        self.show();
        // The answer shows on the card, which the Sessions panel replaces.
        self.close_sessions_panel(false, window, cx);
        if self.live_since.is_none() { self.set_live(true, window, cx); }
        let utterance = self.latest_heard_utterance();
        if let Some(session) = &self.reasoning { session.note_requested(utterance); }
        // The session cancels the previous request when the new one starts (below).
        for turn in &mut self.turns { if turn.status == Status::Streaming { turn.status = Status::Failed("Stopped for a newer request.".into()); } }
        let history = self.history();
        self.next_turn += 1;
        let id = self.next_turn;
        let at_ms = self.live_since.map(|start| start.elapsed().as_millis() as u64).unwrap_or(0);
        let cut = attachments::parts(&attachments, attachments::budget(&self.store.value), false).cut;
        self.turns.push(Turn { id, action: action.to_string().into(), question: question.clone(), text: String::new(), status: Status::Streaming, at_ms, screenshot: None, auto: None,
            session: self.session, attachments: attachments.clone(), cut });
        self.scroll.scroll_to_bottom();
        self.fit(window);
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
        let conversation = self.conversation();
        cx.spawn_in(window, async move |this, cx| {
            let screenshot = match (settings.screen_on_send, prepared, point) {
                (false, ..) | (true, None, None) => None,
                (true, Some(jpeg), _) => Some(jpeg),
                (true, None, Some((x, y))) => cx.background_executor().spawn(async move { crate::capture::screen_jpeg(x, y).ok() }).await,
            };
            let conversation = conversation.render(screenshot.is_some());
            let request = reasoning::Request { action, question, history, conversation, screenshot: screenshot.clone(), attachments, utterance };
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

    /// Finished exchanges of this session, oldest first. Earlier sessions' turns are only shown.
    fn history(&self) -> Vec<Exchange> { chat::history(&self.turns, self.session) }

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
            at_ms, screenshot: None, auto: Some((utterance, at_ms)), session: self.session, attachments: Arc::default(), cut: Vec::new() });
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
        let conversation = self.conversation();
        let point = if settings.screen_on_send { self.native.and_then(platform::center) } else { None };
        let prepared = if settings.screen_on_send { self.prepared_shot.fresh(Instant::now()) } else { None };
        cx.spawn(async move |this, cx| {
            let screenshot = match (prepared, point) {
                (Some(jpeg), _) => Some(jpeg),
                (None, Some((x, y))) => cx.background_executor().spawn(async move { crate::capture::screen_jpeg(x, y).ok() }).await,
                (None, None) => None,
            };
            let conversation = conversation.render(screenshot.is_some());
            let request = reasoning::Request { action: action.into(), question: String::new(), history, conversation, screenshot, attachments: Arc::default(), utterance: Some(utterance) };
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

    /// Which provider the key field belongs to: the transcription provider on the Listening tab
    /// (macOS: in the Settings window, or setup's Transcription step), the API-key answer provider
    /// on the Model tab.
    pub(crate) fn key_target(&self) -> String {
        #[cfg(windows)]
        if self.setup.as_ref().is_some_and(|setup| setup.step == crate::onboarding::Step::Listening) {
            return crate::stt::deepgram::PROVIDER_ID.to_string();
        }
        #[cfg(target_os = "macos")]
        let listening = match self.onboarding_window {
            Some(_) => self.store.value.onboarding_step == Some(crate::settings::OnboardingStep::Transcription),
            None => self.settings_window_tab == Some(Tab::Listening),
        };
        #[cfg(windows)]
        let listening = self.fluent.tab == Some(Tab::Listening);
        if listening { crate::stt::deepgram::PROVIDER_ID.to_string() } else { self.store.value.api_provider.clone() }
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
    /// Settings. On macOS the provider checks the key first (`key_check`). The network and the
    /// credential store are used on a background thread: they can block, and the Keychain can prompt.
    pub fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).text().trim().to_string();
        if key.is_empty() { return; }
        let provider = self.key_target();
        self.key_notice = Some(if cfg!(target_os = "macos") { "Checking the key…" } else { "Saving…" }.into());
        cx.notify();
        let target = provider.clone();
        #[cfg(target_os = "macos")]
        let base_url = self.store.value.custom_base_url.clone();
        cx.spawn_in(window, async move |this, cx| {
            let saved = cx.background_executor().spawn(async move {
                // macOS asks the provider first, so a key it turns down never reaches the Keychain.
                #[cfg(target_os = "macos")]
                if let crate::key_check::Verdict::Refuse(reason) = crate::key_check::check(&target, &key, &base_url) { return Err(reason); }
                crate::secrets::set(&target, &key).map(|()| crate::secrets::masked(&key).unwrap_or_default())
                    .map_err(|error| format!("Couldn't save the key: {error}"))
            }).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                match saved {
                    Ok(hint) => {
                        this.key_input.update(cx, |input, cx| input.clear(cx));
                        this.set_key_marker(&provider, Some(hint));
                        this.key_notice = Some(format!("Saved to {}.", crate::secrets::STORE_NAME).into());
                        #[cfg(target_os = "macos")]
                        this.key_saved(&provider);
                        #[cfg(windows)]
                        this.setup_key_saved(&provider, _window, cx);
                    }
                    Err(notice) => this.key_notice = Some(notice.into()),
                }
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
                    // Nothing stored counts as removed, so this is a refusal or a store error.
                    Err(error) => error.to_string().into(),
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

    /// Fetch the provider's model list off the UI thread (Windows' Load models; macOS has the model typed).
    #[cfg(not(target_os = "macos"))]
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
            let _ = this.update_in(cx, |this, _, cx| {
                this.models_loading = false;
                // A key saved before markers existed is marked once it has been read. A key that
                // can't be read leaves the marker alone: a denied Keychain prompt reads as no key.
                if found.is_some() { this.set_key_marker(preset.id, found); }
                // The provider changed while this list loaded: it belongs to the previous one.
                if this.store.value.api_provider == preset.id {
                    match result {
                        Ok(models) => this.loaded_models = models,
                        Err(error) => this.key_notice = Some(format!("Couldn't load models: {error}").into()),
                    }
                }
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
        // Typing into a hidden overlay would swallow keys out of sight.
        if !visible { self.key_reveal.hide(); self.return_to_previous_app(); }
        platform::set_visible(native, visible);
        self.hotkeys.set_overlay_visible(visible);
    }

    pub(crate) fn show(&mut self) {
        if let Some(native) = self.native.filter(|native| !platform::is_visible(*native)) {
            #[cfg(windows)]
            if std::mem::take(&mut self.unplaced) { platform::place_top_centre(native); }
            platform::set_visible(native, true);
            self.hotkeys.set_overlay_visible(true);
        }
    }

    #[cfg(windows)]
    pub(crate) fn hide(&mut self) {
        if let Some(native) = self.native.filter(|native| platform::is_visible(*native)) {
            self.key_reveal.hide();
            self.return_to_previous_app();
            platform::set_visible(native, false);
            self.hotkeys.set_overlay_visible(false);
        }
    }

    /// CluelyRS was launched again: show the overlay, kept on screen in case it was left on a screen
    /// that's gone. Moved before showing, because the pop-in holds the position it starts from.
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn relaunched(&mut self, cx: &mut Context<Self>) {
        // During first-run setup the overlay stays hidden: bring the setup window forward instead.
        #[cfg(windows)]
        if self.raise_setup(cx) { return; }
        if let Some(native) = self.native { let _ = platform::move_by(native, 0, 0, self.content_bottom()); }
        self.show();
    }

    /// The window is taller than the card (room for popovers below it); only the card has to stay on
    /// screen, so the empty space under it may go past the bottom edge (`platform::move_by`).
    fn content_bottom(&self) -> Option<i32> { self.shape.borrow().iter().map(|&(_, _, _, bottom, _)| bottom).max() }

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
                let _ = platform::move_by(native, (dx as f32 * speed).round() as i32, (dy as f32 * speed).round() as i32, self.content_bottom());
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
    /// The conversation shown above the card's text box: what was heard, the answer thread (each
    /// turn's bubble, "Viewed screen" when it had a screenshot, the answer as written) and the quick
    /// actions.
    fn live_parts(&self, cx: &mut Context<Self>) -> (gpui::Div, gpui::Stateful<gpui::Div>, gpui::Div) {
        let heard = self.transcript_block();
        let muted = self.dim(theme::muted());
        let mut thread = div().id("thread").flex().flex_col().gap(px(12.0)).flex_1().overflow_y_scroll().track_scroll(&self.scroll);
        if self.turns.is_empty() {
            thread = thread.child(div().text_size(px(13.0)).text_color(muted)
                .child(format!("Press {} to answer from your screen and the conversation.", self.hotkeys.label(Action::Assist))));
        }
        for (index, turn) in self.turns.iter().enumerate() {
            for divider in self.dividers.iter().filter(|divider| divider.before == index) { thread = thread.child(chat::divider(&divider.label, muted)); }
            let header: AnyElement = match turn.auto {
                Some((utterance, shown_ms)) => {
                    // An early start is shown before its line is committed; then the line's time counts.
                    let heard = self.transcript.iter().find(|line| line.id == utterance).map_or(shown_ms, |line| line.at_ms);
                    auto_header(turn.id, heard, turn.status == Status::Streaming, muted, self.hits.mark(), cx).into_any_element()
                }
                None => chat::bubble(turn).into_any_element(),
            };
            let size = self.store.value.text_size.answer();
            let body: AnyElement = match &turn.status {
                Status::Failed(reason) => div().text_size(px(13.0)).line_height(px(20.0)).text_color(gpui::rgb(0xffb4a8)).child(reason.clone()).into_any_element(),
                Status::Streaming if turn.text.is_empty() => div().text_size(px(13.0)).text_color(muted).child("Thinking…").into_any_element(),
                _ => div().id(("answer", turn.id as usize)).min_w_0().child(chat::answer(&turn.text, size))
                    .when(turn.status == Status::Stopped, |answer| answer.child(chat::stopped_mark(muted))).into_any_element(),
            };
            // "Viewed screen" and "Read 2 attachments" sit with the answer, above it.
            let failed = matches!(turn.status, Status::Failed(_));
            let (viewed, read) = (turn.screenshot.is_some() && !failed, !turn.attachments.is_empty() && !failed);
            let note = self.dim(theme::placeholder());
            let answer = div().flex().flex_col().gap(px(6.0)).min_w_0()
                .when(viewed, |answer| answer.child(chat::viewed_screen(note)))
                .when(read, |answer| answer.child(attach::read_line(turn.attachments.len(), &turn.cut, note)))
                .child(body);
            thread = thread.child(header).child(answer);
        }
        // A session that has started but asked nothing yet.
        for divider in self.dividers.iter().filter(|divider| divider.before == self.turns.len()) { thread = thread.child(chat::divider(&divider.label, muted)); }
        let ready = self.answer_ready();
        let mut actions = div().flex().items_center().gap(px(6.0));
        for (index, label) in answer::ACTIONS.into_iter().enumerate() {
            let prepared = ready == Some(label);
            let action = div().id(("action", index)).relative().flex().flex_none().items_center().gap(px(6.0)).px(px(10.0)).py(px(5.0))
                .rounded(px(theme::radius::QUICK_ACTION)).border_1().cursor_pointer().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.send(label, String::new(), window, cx)));
            // A speculative answer for this action and context is already being written: the pill
            // turns accent, with a dot in place of its icon.
            let action = if prepared {
                action.bg(theme::bubble()).border_color(theme::bubble_border()).text_color(theme::accent_soft())
                    .child(div().size(px(12.0)).flex_none().rounded_full().bg(gpui::rgba(0x4c8dff2e)).flex().items_center().justify_center()
                        .child(div().size(px(6.0)).rounded_full().bg(theme::accent())))
            } else {
                action.bg(theme::raised()).border_color(theme::hairline()).text_color(theme::body()).hover(|action| action.bg(theme::pressed()))
                    .children(chat::action_icon(label).map(|icon| ui::icon(icon, 12.0, theme::body())))
            };
            actions = actions.child(action.child(label).child(self.hits.mark()));
        }
        if ready.is_some() {
            actions = actions.child(div().flex_1().min_w_0().flex().justify_end().truncate().text_size(px(12.0)).text_color(muted)
                .child("Answer ready for their last question"));
        }
        (heard, thread, actions)
    }
}

/// After the Sessions window opens, show session `id` in it.
fn show_session(handle: Option<gpui::WindowHandle<SessionsWindow>>, id: Option<String>, cx: &mut gpui::App) {
    let (Some(handle), Some(id)) = (handle, id) else { return };
    let _ = handle.update(cx, |window, _, cx| window.select(&id, cx));
}

mod attach;
mod card;
mod chat;
mod update_notice;
mod switcher;
mod sessions_panel;
#[cfg(any(target_os = "macos", windows))]
mod menu_bar;
#[cfg(windows)]
mod tray_menu;
pub(crate) use card::CARD_WIDTH;
/// How many audio levels the Live waveform keeps (one per bar from the middle out).
pub(crate) const LEVEL_HISTORY: usize = 3;

/// The window while the Sessions panel replaces the card: the overlay's top padding, the panel
/// and room under it.
const SESSIONS_PANEL_WINDOW_HEIGHT: f32 = 8.0 + sessions_panel::PANEL_HEIGHT + 20.0;

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.hits.clear();
        div().size_full().flex().flex_col().items_center().gap(px(10.0)).pt(px(8.0)).pb(px(12.0))
            .font_family(theme::FONT).text_color(theme::text())
            .track_focus(&self.focus)
            .on_children_prepainted(click_through(self.native, self.hits.clone(), self.shape.clone()))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                // Tab in the text box opens the mode switcher.
                let modifiers = event.keystroke.modifiers;
                if event.keystroke.key == "tab" && !modifiers.modified() && !this.switcher_open() && this.composer.focus_handle(cx).is_focused(window) {
                    cx.stop_propagation();
                    this.open_switcher(window, cx);
                    return;
                }
                if event.keystroke.key != "escape" { return; }
                if this.sessions_panel_open() { this.close_sessions_panel(true, window, cx); }
                else if this.composer.focus_handle(cx).is_focused(window) {
                    this.composer.update(cx, |input, cx| input.clear(cx));
                    this.return_to_previous_app();
                }
            }))
            .child(self.card(cx))
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

/// Shape the window to what is visible (the card, open popovers and tooltips), so the space around
/// them is click-through; inside them `update_passthrough` decides per cursor position.
fn click_through(native: Option<NativeWindow>, hits: Hits, applied: Rc<RefCell<Vec<platform::Shape>>>) -> impl Fn(Vec<gpui::Bounds<gpui::Pixels>>, &mut Window, &mut gpui::App) + 'static {
    move |children, window, _| {
        let Some(native) = native else { return };
        let scale = window.scale_factor();
        hits.1.set(scale);
        let shapes = region(&children, &hits.floating(), scale);
        if *applied.borrow() != shapes {
            platform::set_shape(native, &shapes);
            *applied.borrow_mut() = shapes;
        }
    }
}

/// The window region, in physical pixels, for the laid-out `children` of the overlay and the
/// `floating` areas reserved for deferred content (open lists and popovers), each a rounded
/// rectangle a pixel larger than its bounds, with the card's corner (the overlay's own children:
/// the card, the Windows Settings panel) or the popovers' (the mode switcher, tooltips). The same
/// on every platform; only Windows applies it.
fn region(children: &[Hit], floating: &[Hit], scale: f32) -> Vec<platform::Shape> {
    let physical = |value: gpui::Pixels| (f32::from(value) * scale).round() as i32;
    children.iter().map(|bounds| (bounds, false)).chain(floating.iter().map(|bounds| (bounds, true))).map(|(bounds, float)| {
        let radius = if float { theme::radius::POPOVER } else { theme::radius::CARD };
        (physical(bounds.left()) - 1, physical(bounds.top()) - 1, physical(bounds.right()) + 1, physical(bounds.bottom()) + 1,
            (radius * scale).round() as i32)
    }).collect()
}

/// Settings › Model's model box placeholder; setup swaps in an example id while it is open.
#[cfg(not(target_os = "macos"))]
pub(crate) const MODEL_PLACEHOLDER: &str = "Model id, e.g. from the list below";

/// The text box's placeholder says what an answer will see: the screen only while Screen on send is on.
fn composer_placeholder(settings: &Settings) -> &'static str {
    if settings.screen_on_send { "Ask about your screen or conversation" } else { "Ask about your conversation" }
}

/// Whether CluelyRS belongs in the Dock (and ⌘Tab). While setup is open it always does: setup is an
/// ordinary window people look for there, and leaving the Dock while it's in front would make macOS
/// deactivate CluelyRS and send setup behind other apps, so setup's own "Show in Dock" switch takes
/// effect when it finishes. Otherwise only "Show in Dock" puts it there. Settings doesn't: tools that
/// quit apps whose last window closes (and so would quit CluelyRS when Settings closes) leave apps
/// outside the Dock alone. Settings and setup handle ⌘W and ⌘Q themselves.
#[cfg(target_os = "macos")]
fn in_dock(settings: &Settings, setup_open: bool) -> bool { settings.show_in_dock || setup_open }

/// Whether a Codex sign-in check restarts the app-server to read the account: when `codex login
/// status` says signed in or can't say, and every `CODEX_FULL_CHECK` polls in case it's wrong.
#[cfg(target_os = "macos")]
fn codex_restart_due(reported: Option<bool>, poll: u32) -> bool { reported != Some(false) || poll.is_multiple_of(CODEX_FULL_CHECK) }

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
fn auto_header(id: u64, heard_ms: u64, streaming: bool, muted: gpui::Rgba, hit: impl IntoElement, cx: &mut Context<Overlay>) -> impl IntoElement {
    let label = gpui::rgb(0x7fa8ff);
    div().flex().items_center().gap(px(8.0))
        .child(ui::icon("icons/sparkle.svg", 13.0, label))
        .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(label).child("SHOWN AUTOMATICALLY"))
        .child(div().flex_1().text_size(px(12.0)).line_height(px(16.0)).text_color(muted).child(format!("for their question at {}", archive::clock(heard_ms))))
        .when(streaming, |row| row.child(div().id(("stop-auto", id as usize)).px(px(10.0)).py(px(4.0)).rounded(px(8.0)).border_1().border_color(theme::hairline())
            .cursor_pointer().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(gpui::rgb(0xd9d5cc)).relative().child("Stop").child(hit)
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.stop_turn(id, cx)))))
}

#[cfg(test)]
mod tests {
    use super::{Status, Turn, composer_placeholder, save_turn};
    use crate::archive::Archive;
    use crate::settings::Settings;

    /// Codex's app-server restarts only once its CLI stops saying signed out, or every 30 s.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_codex_sign_in_restarts_the_app_server_only_when_it_may_have_changed() {
        assert!(!super::codex_restart_due(Some(false), 1), "still signed out");
        assert!(super::codex_restart_due(Some(true), 1));
        assert!(super::codex_restart_due(None, 1), "the CLI can't say: check as before");
        assert!(super::codex_restart_due(Some(false), super::CODEX_FULL_CHECK), "in case the CLI is wrong");
    }

    /// Setup is in the Dock whatever "Show in Dock" says; its switch decides once setup is over.
    #[cfg(target_os = "macos")]
    #[test]
    fn setup_stays_in_the_dock_and_the_setting_applies_after_it() {
        let (off, on) = (Settings { show_in_dock: false, ..Settings::default() }, Settings { show_in_dock: true, ..Settings::default() });
        assert!(super::in_dock(&off, true) && super::in_dock(&on, true), "while setup is open");
        assert!(!super::in_dock(&off, false), "turned off in setup: CluelyRS leaves the Dock as setup closes");
        assert!(super::in_dock(&on, false));
    }

    /// Live stopping while an answer is written: History gets the partial answer marked as
    /// stopped (the question and screenshot too); a stopped Assist that wrote nothing is left out.
    #[test]
    fn an_answer_stopped_with_live_is_saved_with_its_partial_text() {
        let root = std::env::temp_dir().join(format!("cluelyrs-overlay-stopped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let archive = Archive::at(root.clone());
        let mut recorder = Some(archive.start(100, "ended".into()).unwrap());
        let turn = |action: &str, question: &str, text: &str| Turn { id: 1, action: action.to_string().into(), question: question.into(), text: text.into(),
            status: Status::Streaming, at_ms: 4_000, screenshot: Some(b"jpeg".to_vec()), auto: None, session: 1, attachments: std::sync::Arc::default(), cut: Vec::new() };
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
        assert_eq!(composer_placeholder(&on), "Ask about your screen or conversation");
        assert_eq!(composer_placeholder(&off), "Ask about your conversation");
    }
}
