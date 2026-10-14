//! macOS first-run setup: a normal window, "Set up CluelyRS", stepping through the choices that matter
//! before the first Live session. Layout follows the Paper "Menu bar + macOS onboarding" artboards.
//! The pages are the overlay's own settings (`settings_view::onboarding`), so every choice saves and
//! applies as it does in Settings: skipping a step keeps what is set, and closing the window or Skip
//! setup counts as finishing. The overlay stays hidden, and Live waits, until a first run finishes.

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Render,
    ScrollHandle, Styled, Subscription, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*, px, rgb, size,
};

use crate::overlay::Overlay;
use crate::platform::{self, NativeWindow};
use crate::providers::{Message, Part, Role};
use crate::settings::{ONBOARDING_VERSION, OnboardingStep as Step, Provider, Settings};
use crate::settings_view::button;
use crate::settings_window::{CloseSettings, sync_capture};
use crate::theme;

/// The steps this build shows, in order. Permissions and Shortcuts join when their pages exist.
pub(crate) const STEPS: [Step; 5] = [Step::Welcome, Step::Model, Step::Transcription, Step::Privacy, Step::Done];

const WIDTH: f32 = 760.0;
const HEIGHT: f32 = 700.0;
/// Room the window always leaves on the screen.
const SCREEN_MARGIN: f32 = 80.0;
/// The test request: one short question, no screenshot.
const TEST_SYSTEM: &str = "You are checking that a connection works.";
const TEST_QUESTION: &str = "Reply with OK.";

/// What the footer and the summary's Change links do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Nav { Back, Next, Go(Step), Finish }

/// Where `nav` leads from `step`: another step, or `None` once setup is finished (Continue on the
/// last step, Skip setup).
fn next_step(step: Step, nav: Nav) -> Option<Step> {
    let index = position(step);
    match nav {
        Nav::Back => Some(STEPS[index.saturating_sub(1)]),
        Nav::Next => STEPS.get(index + 1).copied(),
        Nav::Go(step) => Some(shown(step)),
        Nav::Finish => None,
    }
}

fn position(step: Step) -> usize { STEPS.iter().position(|shown| *shown == step).unwrap_or(0) }

/// A step this build shows: one saved by a newer build that this one doesn't have starts over.
pub(crate) fn shown(step: Step) -> Step { STEPS[position(step)] }

/// Settings as setup leaves them: open on `step`, so a relaunch resumes there, or finished (`None`).
pub(crate) fn record(settings: &mut Settings, step: Option<Step>) {
    settings.onboarding_step = step;
    if step.is_none() { settings.onboarding_version = ONBOARDING_VERSION; }
}

/// Setup › Model's test request, for the provider and model it was sent with.
#[derive(Clone)]
pub(crate) struct TestRun {
    pub provider: Provider,
    pub model: String,
    pub state: TestState,
}

#[derive(Clone)]
pub(crate) enum TestState { Sending, Answered(Duration), Failed(String) }

pub struct OnboardingWindow {
    overlay: Entity<Overlay>,
    step: Step,
    focus: FocusHandle,
    native: Option<NativeWindow>,
    capture_hidden: Option<bool>,
    scroll: ScrollHandle,
    test: Option<TestRun>,
    _overlay_changed: Subscription,
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
        let height = cx.primary_display().map_or(HEIGHT, |display| HEIGHT.min(f32::from(display.bounds().size.height) - SCREEN_MARGIN));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(height)), cx))),
            titlebar: Some(TitlebarOptions { title: Some("Set up CluelyRS".into()), appears_transparent: true, traffic_light_position: Some(point(px(14.0), px(14.0))) }),
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
        // The red button finishes setup like Skip setup: nothing chosen is lost.
        let closing = overlay.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            if let Some(overlay) = closing.upgrade() { finish(&overlay, cx); }
            true
        });
        let _overlay_changed = cx.observe(&overlay, |_, _, cx| cx.notify());
        Self { overlay, step, focus, native: platform::native_window(window), capture_hidden: None, scroll: ScrollHandle::new(), test: None, _overlay_changed }
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
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// Ask the selected provider a one-word question off the UI thread and time the answer. A
    /// subscription's CLI or a saved API key is used exactly as for an answer; nothing waits on it.
    pub(crate) fn send_test(&mut self, cx: &mut Context<Self>) {
        let overlay = self.overlay.read(cx);
        let (provider, model) = (overlay.store.value.provider, overlay.model_choice().value);
        let messages = vec![Message { role: Role::User, parts: vec![Part::Text(TEST_QUESTION.into())] }];
        let target = crate::answer::target(&overlay.store.value, &overlay.codex, TEST_SYSTEM.into(), messages, 16);
        let run = |state| Some(TestRun { provider, model: model.clone(), state });
        let target = match target {
            Ok(target) => target,
            Err(reason) => { self.test = run(TestState::Failed(reason)); cx.notify(); return; }
        };
        self.test = run(TestState::Sending);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = cx.background_executor().spawn(async move { crate::answer::complete(target) }).await;
            let elapsed = started.elapsed();
            let _ = this.update(cx, |this, cx| {
                // Only the latest test reports; one for another provider or model was replaced.
                let Some(test) = this.test.as_mut().filter(|test| test.provider == provider && test.model == model) else { return };
                test.state = match result { Ok(_) => TestState::Answered(elapsed), Err(reason) => TestState::Failed(reason) };
                cx.notify();
            });
        }).detach();
    }

    /// The title and the step indicator: done steps outlined in blue, the current one filled.
    fn header(&self) -> impl IntoElement {
        const CELL: f32 = 80.0;
        let current = position(self.step);
        // One track behind the dots, from the first to the last. The row has no gap: GPUI would add
        // one after this absolutely placed first child.
        let track = div().absolute().top(px(14.0)).left(px(CELL / 2.0)).right(px(CELL / 2.0)).h(px(1.0)).bg(theme::hairline());
        let steps = STEPS.iter().enumerate().fold(div().relative().flex().justify_center().pt(px(4.0)).child(track), |row, (index, step)| {
            let (dot, number, label) = match index.cmp(&current) {
                std::cmp::Ordering::Less => (div().bg(theme::bubble()).border_1().border_color(theme::bubble_border()), theme::accent_soft(), theme::body()),
                std::cmp::Ordering::Equal => (div().bg(theme::accent()), theme::accent_ink(), theme::text()),
                std::cmp::Ordering::Greater => (div().bg(rgb(0x17181b)).border_1().border_color(theme::keycap_border()), theme::muted(), theme::muted()),
            };
            row.child(div().w(px(CELL)).flex().flex_col().items_center().gap(px(5.0))
                .child(dot.size(px(20.0)).rounded_full().flex().items_center().justify_center()
                    .text_size(px(11.0)).font_weight(if index > current { FontWeight::SEMIBOLD } else { FontWeight::BOLD }).text_color(number).child((index + 1).to_string()))
                .child(div().text_size(px(11.0)).text_color(label).when(index == current, |text| text.font_weight(FontWeight::SEMIBOLD)).child(label_of(*step))))
        });
        // The title sits beside the traffic lights, in the transparent title bar.
        div().flex().flex_col().gap(px(8.0)).px(px(14.0)).pt(px(10.0)).pb(px(10.0)).bg(rgb(0x17181b)).border_b_1().border_color(theme::divider())
            .child(div().h(px(16.0)).flex().justify_center().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child("Set up CluelyRS"))
            .child(steps)
    }

    /// Skip (Skip setup on the first step; a reminder on the last) on the left; Back and Continue
    /// (Open CluelyRS on the last step) on the right.
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (first, last) = (position(self.step) == 0, self.step == Step::Done);
        let go = |nav: Nav| cx.listener(move |this, _, window, cx| this.navigate(nav, window, cx));
        let left = if last { div().px(px(10.0)).text_size(px(12.0)).text_color(theme::muted()).child("Setup can be run again any time").into_any_element() }
            else { div().id("skip").px(px(10.0)).py(px(7.0)).rounded(px(9.0)).cursor_pointer().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body())
                .hover(|skip| skip.text_color(theme::text()))
                .on_mouse_down(MouseButton::Left, go(if first { Nav::Finish } else { Nav::Next }))
                .child(if first { "Skip setup" } else { "Skip" }).into_any_element() };
        let back = button("back", "Back", false).px(px(14.0)).py(px(7.0)).when(first, |back| back.opacity(0.4).cursor_default())
            .when(!first, |back| back.on_mouse_down(MouseButton::Left, go(Nav::Back)));
        let primary = button("continue", if last { "Open CluelyRS" } else { "Continue" }, true).px(px(16.0)).py(px(7.0)).on_mouse_down(MouseButton::Left, go(Nav::Next));
        div().flex().items_center().justify_between().px(px(16.0)).py(px(12.0)).bg(rgb(0x131416)).border_t_1().border_color(theme::divider())
            .child(left)
            .child(div().flex().items_center().gap(px(8.0)).child(back).child(primary))
    }
}

fn label_of(step: Step) -> &'static str {
    match step {
        Step::Welcome => "Welcome", Step::Model => "Model", Step::Transcription => "Transcription", Step::Permissions => "Permissions",
        Step::Privacy => "Privacy", Step::Shortcuts => "Shortcuts", Step::Done => "Done",
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
        let (step, test, setup) = (self.step, self.test.clone(), cx.weak_entity());
        let page = self.overlay.update(cx, |overlay, cx| overlay.in_window(cx, |overlay, cx| overlay.onboarding_page(step, test.as_ref(), &setup, cx)));
        div().size_full().flex().flex_col().bg(rgb(0x0f1012)).font_family(theme::FONT).text_color(theme::text())
            // ⌘W closes it, as it does Settings.
            .key_context("SettingsWindow").track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseSettings, window, cx| this.navigate(Nav::Finish, window, cx)))
            .child(self.header())
            .child(div().id("onboarding-page").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll)
                .child(div().w(px(600.0)).mx_auto().pt(px(30.0)).pb(px(28.0)).child(page)))
            .child(self.footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_go_forward_and_back_and_finish_after_the_last() {
        assert_eq!(next_step(Step::Welcome, Nav::Next), Some(Step::Model));
        assert_eq!(next_step(Step::Model, Nav::Next), Some(Step::Transcription));
        assert_eq!(next_step(Step::Transcription, Nav::Next), Some(Step::Privacy), "Permissions isn't shown yet");
        assert_eq!(next_step(Step::Privacy, Nav::Back), Some(Step::Transcription));
        assert_eq!(next_step(Step::Welcome, Nav::Back), Some(Step::Welcome));
        assert_eq!(next_step(Step::Done, Nav::Next), None, "Open CluelyRS finishes");
        assert_eq!(next_step(Step::Model, Nav::Finish), None, "Skip setup finishes");
        assert_eq!(next_step(Step::Done, Nav::Go(Step::Model)), Some(Step::Model), "a summary's Change link");
    }

    #[test]
    fn a_step_this_build_does_not_show_starts_over() {
        assert_eq!(shown(Step::Transcription), Step::Transcription);
        assert_eq!(shown(Step::Shortcuts), Step::Welcome);
        assert_eq!(next_step(Step::Permissions, Nav::Next), Some(Step::Model));
    }

    #[test]
    fn each_step_is_saved_and_finishing_records_the_version() {
        let mut settings = Settings::default();
        record(&mut settings, Some(Step::Privacy));
        assert_eq!((settings.onboarding_step, settings.onboarding_version), (Some(Step::Privacy), 0));
        record(&mut settings, None);
        assert_eq!((settings.onboarding_step, settings.onboarding_version), (None, ONBOARDING_VERSION));
    }
}
