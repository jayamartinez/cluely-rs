//! macOS first-run setup: a normal window, "Set up CluelyRS", four short steps before the first Live
//! session (Welcome, Permissions, Answer model, Listening). Layout follows the Paper "Redesign v3 ·
//! Onboarding" macOS artboards. The pages are the overlay's own settings (`settings_view::onboarding`),
//! so every choice saves and applies as it does in Settings, and closing the window counts as
//! finishing. The overlay stays hidden, and Live waits, until a first run finishes.

pub(crate) mod audio_check;

use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, ImageSource, InteractiveElement, IntoElement, MouseButton, ObjectFit, ParentElement, Render,
    Resource, Styled, StyledImage, Subscription, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions, div, img, point, px, size,
};

pub(crate) use self::audio_check::{AudioCheck, Meter, MeterState, Metered, Permissions};
use crate::overlay::Overlay;
use crate::platform::{self, NativeWindow, PrivacyPane};
use crate::settings::{ONBOARDING_VERSION, OnboardingStep as Step, Settings, SttProvider};
use crate::settings_window::{CloseSettings, sync_capture};
use crate::{theme, ui_v3};

/// The steps this build shows, in order. Answer model is `Model` and Listening is `Transcription`,
/// the names they're saved under.
pub(crate) const STEPS: [Step; 4] = [Step::Welcome, Step::Permissions, Step::Model, Step::Transcription];

const WIDTH: f32 = 760.0;
const HEIGHT: f32 = 560.0;
/// How long the keys light up after Assist is pressed as practice (the overlay's `practice_assist`).
pub(crate) const PRACTICE_FLASH: Duration = Duration::from_millis(700);

/// Where the back arrow and the main button lead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Nav { Back, Next, Finish }

/// Where `nav` leads from `step`: another step, or `None` once setup is finished (the last step's
/// Start CluelyRS).
fn next_step(step: Step, nav: Nav) -> Option<Step> {
    let index = position(step);
    match nav {
        Nav::Back => Some(STEPS[index.saturating_sub(1)]),
        Nav::Next => STEPS.get(index + 1).copied(),
        Nav::Finish => None,
    }
}

fn position(step: Step) -> usize { STEPS.iter().position(|shown| *shown == step).unwrap_or(0) }

/// A step this build shows: one saved by a newer or older build that this one doesn't have starts over.
pub(crate) fn shown(step: Step) -> Step { STEPS[position(step)] }

/// Settings as setup leaves them: open on `step`, so a relaunch resumes there, or finished (`None`).
pub(crate) fn record(settings: &mut Settings, step: Option<Step>) {
    settings.onboarding_step = step;
    if step.is_none() { settings.onboarding_version = ONBOARDING_VERSION; }
}

/// What the setup window itself holds for its pages (the rest is the overlay's).
pub(crate) struct PageState<'a> {
    pub permissions: &'a Permissions,
    pub audio: &'a AudioCheck,
    /// The speech model chosen on Listening; nothing is chosen until one is clicked.
    pub speech: Option<SttProvider>,
}

pub struct OnboardingWindow {
    overlay: Entity<Overlay>,
    step: Step,
    focus: FocusHandle,
    native: Option<NativeWindow>,
    capture_hidden: Option<bool>,
    permissions: Permissions,
    audio: AudioCheck,
    speech: Option<SttProvider>,
    _overlay_changed: Subscription,
    _activation: Subscription,
}

impl Metered for OnboardingWindow {
    fn meters(&mut self) -> &mut AudioCheck { &mut self.audio }
}

/// Show setup on `step`, opening it if needed; it becomes key only once CluelyRS is active, as
/// Settings does (`platform::activate_then`).
pub fn open(step: Step, cx: &mut Context<Overlay>) {
    let entity = cx.entity();
    cx.defer(move |cx| platform::activate_then(cx, move |active, cx| {
        if let Some(handle) = entity.read(cx).onboarding_window
            && handle.update(cx, |this, window, cx| {
                this.show_step(step, cx);
                if active { window.activate_window(); } else if let Some(native) = this.native { platform::order_front(native); }
            }).is_ok() {
            return;
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx))),
            titlebar: Some(TitlebarOptions { title: Some("Set up CluelyRS".into()), appears_transparent: true, traffic_light_position: Some(point(px(14.0), px(15.0))) }),
            kind: WindowKind::Normal,
            focus: active,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        };
        match cx.open_window(options, |window, cx| cx.new(|cx| OnboardingWindow::new(entity.clone(), step, window, cx))) {
            Ok(handle) => entity.update(cx, |overlay, _| { overlay.onboarding_window = Some(handle); overlay.update_dock(); }),
            Err(error) => eprintln!("setup window could not open: {error}"),
        }
    }));
}

impl OnboardingWindow {
    fn new(overlay: Entity<Overlay>, step: Step, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        // The red button finishes setup: nothing chosen is lost.
        let closing = overlay.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            if let Some(overlay) = closing.upgrade() { finish(&overlay, cx); }
            true
        });
        // A settings change can change what the audio check captures (its devices).
        let _overlay_changed = cx.observe(&overlay, |this, _, cx| this.sync_audio(cx));
        // Coming back from System Settings (or a permission prompt) is when permissions change.
        let _activation = cx.observe_window_activation(window, |this, window, cx| if window.is_window_active() { this.refresh_permissions(cx) });
        let mut setup = Self { overlay, step, focus, native: platform::native_window(window), capture_hidden: None,
            permissions: Permissions::read(), audio: AudioCheck::default(), speech: None, _overlay_changed, _activation };
        setup.sync_audio(cx);
        setup
    }

    pub(crate) fn navigate(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        match next_step(self.step, nav) {
            Some(step) => {
                self.show_step(step, cx);
                with_overlay(&self.overlay, cx, move |overlay, window, cx| overlay.prepare_onboarding(step, window, cx));
            }
            None => {
                finish(&self.overlay, cx);
                window.remove_window();
            }
        }
    }

    fn show_step(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.refresh_permissions(cx);
    }

    /// Read both permissions again (they change in System Settings), then match the audio check to them.
    fn refresh_permissions(&mut self, cx: &mut Context<Self>) {
        let read = Permissions::read();
        self.permissions.mic = read.mic;
        self.permissions.screen_allowed = read.screen_allowed;
        self.sync_audio(cx);
    }

    /// The meters run on the Listening step only.
    fn sync_audio(&mut self, cx: &mut Context<Self>) {
        let overlay = self.overlay.read(cx);
        let wanted = audio_check::wanted(self.step == Step::Transcription, &overlay.store.value, &self.permissions, overlay.live_since.is_some());
        audio_check::sync(self, wanted, cx);
    }

    /// Grant for the microphone: macOS's prompt, then the answer. Once denied, macOS doesn't ask
    /// again, so Grant opens its pane in System Settings.
    pub(crate) fn allow_microphone(&mut self, cx: &mut Context<Self>) {
        if self.permissions.mic == platform::Access::Denied { return self.open_privacy(PrivacyPane::Microphone, cx); }
        let (answered, answer) = futures::channel::oneshot::channel();
        platform::request_microphone(move |_| { let _ = answered.send(()); });
        cx.spawn(async move |this, cx| {
            let _ = answer.await;
            let _ = this.update(cx, |this, cx| this.refresh_permissions(cx));
        }).detach();
    }

    /// Grant for Screen & System Audio Recording. macOS prompts only the first time it's ever asked,
    /// and what is allowed applies after a relaunch, so from here the row offers Quit & Reopen.
    pub(crate) fn allow_screen_recording(&mut self, cx: &mut Context<Self>) {
        platform::request_screen_recording();
        self.permissions.screen_asked = true;
        self.refresh_permissions(cx);
    }

    pub(crate) fn open_privacy(&mut self, pane: PrivacyPane, cx: &mut Context<Self>) {
        if let Err(error) = platform::open_privacy_pane(pane) { eprintln!("System Settings could not open: {error}"); }
        cx.notify();
    }

    /// Quit, and start again on this step once this process is gone, with Screen Recording applied.
    /// Setup isn't finished: it was saved on this step, so the relaunch resumes here.
    pub(crate) fn quit_and_reopen(&mut self, cx: &mut Context<Self>) {
        match platform::relaunch_after_exit() {
            Ok(()) => cx.quit(),
            Err(error) => { self.permissions.relaunch_error = Some(format!("CluelyRS couldn't reopen itself ({error}). Quit and open it again.")); cx.notify(); }
        }
    }

    /// Listening's choice of speech model. On this Mac starts the download if the model isn't here
    /// yet: it only ever starts from this click.
    pub(crate) fn choose_speech(&mut self, provider: SttProvider, cx: &mut Context<Self>) {
        self.speech = Some(provider);
        cx.notify();
        with_overlay(&self.overlay, cx, move |overlay, window, cx| {
            overlay.update_settings(|s| { s.transcribe = true; s.stt_provider = provider; }, window, cx);
            if provider == SttProvider::Parakeet && !overlay.model_installed && overlay.download_progress().is_none() { overlay.download_model(window, cx); }
        });
    }

    /// The step indicator in the title bar, between the traffic lights and an equal space.
    fn top_bar(&self) -> impl IntoElement {
        div().h(px(44.0)).flex_none().flex().items_center().px(px(14.0))
            .child(div().w(px(80.0)).flex_none())
            .child(div().flex_1().child(ui_v3::progress_dashes(STEPS.len(), position(self.step))))
            .child(div().w(px(80.0)).flex_none())
    }
}

/// Run `f` on the overlay in its own window, where its handlers start their work (`settings_view::listen`).
fn with_overlay(overlay: &Entity<Overlay>, cx: &mut App, f: impl FnOnce(&mut Overlay, &mut Window, &mut Context<Overlay>)) {
    let own = overlay.read(cx).own_window;
    let _ = own.update(cx, |_, window, cx| overlay.update(cx, |overlay, cx| f(overlay, window, cx)));
}

/// Setup ends here however it ends; the window is closing or closed by the caller.
fn finish(overlay: &Entity<Overlay>, cx: &mut App) {
    with_overlay(overlay, cx, |overlay, window, cx| overlay.finish_onboarding(window, cx));
}

impl Render for OnboardingWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        sync_capture(self.native, &mut self.capture_hidden, &self.overlay.read(cx).store.value);
        let (step, setup) = (self.step, cx.weak_entity());
        let state = PageState { permissions: &self.permissions, audio: &self.audio, speech: self.speech };
        let page = self.overlay.update(cx, |overlay, cx| overlay.in_window(cx, |overlay, cx| overlay.onboarding_page(step, &state, &setup, cx)));
        let back = (position(step) > 0).then(|| div().id("back").absolute().left(px(16.0)).top(px(56.0)).size(px(32.0)).flex().items_center().justify_center()
            .rounded(px(ui_v3::RADII.small)).cursor_pointer().hover(|back| back.bg(theme::raised()))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.navigate(Nav::Back, window, cx)))
            .child(ui_v3::back_arrow()));
        let glow = img(ImageSource::Resource(Resource::Embedded("app-icon/setup-glow.svg".into()))).absolute().top_0().left_0().size_full().object_fit(ObjectFit::Fill);
        div().relative().size_full().flex().flex_col().bg(theme::window_bg()).font_family(theme::FONT).text_color(theme::text())
            // ⌘W closes it, as it does Settings.
            .key_context("SettingsWindow").track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseSettings, window, cx| this.navigate(Nav::Finish, window, cx)))
            .child(glow)
            .child(self.top_bar())
            .child(page)
            .children(back)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_go_forward_and_back_and_finish_after_the_last() {
        assert_eq!(next_step(Step::Welcome, Nav::Next), Some(Step::Permissions));
        assert_eq!(next_step(Step::Permissions, Nav::Next), Some(Step::Model));
        assert_eq!(next_step(Step::Model, Nav::Next), Some(Step::Transcription));
        assert_eq!(next_step(Step::Transcription, Nav::Next), None, "Start CluelyRS finishes");
        assert_eq!(next_step(Step::Model, Nav::Back), Some(Step::Permissions));
        assert_eq!(next_step(Step::Welcome, Nav::Back), Some(Step::Welcome));
        assert_eq!(next_step(Step::Permissions, Nav::Finish), None, "closing finishes");
    }

    #[test]
    fn every_step_is_shown_in_order_and_retired_steps_start_over() {
        assert_eq!(STEPS.map(shown), STEPS);
        for retired in [Step::Privacy, Step::Shortcuts, Step::Done] { assert_eq!(shown(retired), Step::Welcome, "{retired:?}"); }
        assert_eq!(position(Step::Transcription), STEPS.len() - 1);
    }

    #[test]
    fn each_step_is_saved_and_finishing_records_the_version() {
        let mut settings = Settings::default();
        record(&mut settings, Some(Step::Permissions));
        assert_eq!((settings.onboarding_step, settings.onboarding_version), (Some(Step::Permissions), 0));
        record(&mut settings, None);
        assert_eq!((settings.onboarding_step, settings.onboarding_version), (None, ONBOARDING_VERSION));
    }
}
