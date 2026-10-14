//! Windows: first-run setup in a window of its own, from the Paper "Onboarding v2 · Windows"
//! artboards. It opens at launch until it is finished or skipped (`Settings::setup_done`), and again
//! from Settings › About › Run setup again. The overlay stays hidden while it is open, until Assist
//! (or Show / hide) brings it up to see what Customize changes. Like the Modes window it is a normal
//! window that draws its own title bar and hides from screen capture while the overlay does; its
//! pages are drawn by the overlay, whose settings they change.

mod answer;
mod hearing;
mod steps;

use gpui::{
    App, AppContext, Bounds, Context, Div, Entity, FontWeight, MouseButton, ParentElement, Render, Styled, Subscription, TitlebarOptions,
    Window, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, prelude::*, px, rgb, size,
};

use crate::overlay::{self, Overlay};
use crate::platform::{self, NativeWindow};
use crate::settings_view::listen;
use crate::{theme, ui};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Step { Welcome, AnswerModel, Transcription, AudioCheck, Privacy, Shortcuts, Customize, Done }

impl Step {
    const ALL: [Step; 8] = [Step::Welcome, Step::AnswerModel, Step::Transcription, Step::AudioCheck, Step::Privacy, Step::Shortcuts, Step::Customize, Step::Done];

    fn label(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome", Step::AnswerModel => "Answer model", Step::Transcription => "Transcription", Step::AudioCheck => "Audio check",
            Step::Privacy => "Privacy", Step::Shortcuts => "Shortcuts", Step::Customize => "Customize", Step::Done => "Done",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Step::Welcome => "icons/setup-sparkle.svg", Step::AnswerModel => "icons/setup-chat.svg", Step::Transcription => "icons/setup-wave.svg",
            Step::AudioCheck => "icons/setup-mic.svg", Step::Privacy => "icons/setup-shield.svg", Step::Shortcuts => "icons/setup-keyboard.svg",
            Step::Customize => "icons/setup-sliders.svg", Step::Done => "icons/setup-flag.svg",
        }
    }

    fn offset(self, by: isize) -> Option<Step> { Step::ALL.get((self as usize).checked_add_signed(by)?).copied() }
}

/// The dropdowns on the setup pages; at most one is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu { Mode, ApiProvider, Model, Mic, Desktop }

/// Where setup is, kept on the overlay while its window is open.
pub(crate) struct Setup {
    window: Option<WindowHandle<SetupWindow>>,
    pub(crate) step: Step,
    /// The furthest step reached; the steps before it are marked done.
    furthest: Step,
    /// Assist was pressed while setup was open (`Overlay::handle` routes it here).
    pub(crate) tried_assist: bool,
    menu: Option<Menu>,
    /// Customize's "Open at sign-in", as the registry has it.
    launch_at_login: bool,
    answer: answer::AnswerState,
    /// "Replace" was pressed beside a saved key: its field takes a new one.
    replacing_key: bool,
    /// Audio check's meters, while that step is open.
    audio: Option<hearing::AudioCheck>,
    /// What Audio check has heard (microphone, desktop audio), for the Done step.
    heard: [bool; 2],
}

impl Overlay {
    /// Bring the setup window to the front if it is open (CluelyRS launched again during setup).
    pub(crate) fn raise_setup(&self, cx: &mut Context<Self>) -> bool {
        let Some(handle) = self.setup.as_ref().and_then(|setup| setup.window) else { return false };
        cx.defer(move |cx| { let _ = handle.update(cx, |_, window, _| window.activate_window()); });
        true
    }

    /// Open setup (or bring its window forward), hiding the overlay until it ends.
    pub(crate) fn open_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_panels(window, cx);
        self.hide();
        self.refresh_subscriptions(window, cx);
        if self.raise_setup(cx) { return; }
        self.setup = Some(Setup { window: None, step: Step::Welcome, furthest: Step::Welcome, tried_assist: false, menu: None,
            launch_at_login: platform::launch_at_login(), answer: answer::AnswerState::default(), replacing_key: false, audio: None, heard: [false; 2] });
        self.find_clis(window, cx);
        let overlay = cx.weak_entity();
        // Deferred: the window reads the overlay while it draws, and the overlay is being updated now.
        cx.defer(move |cx| {
            let Some(overlay) = overlay.upgrade() else { return };
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(880.0), px(642.0)), cx))),
                titlebar: Some(TitlebarOptions { title: Some("Set up CluelyRS".into()), appears_transparent: true, traffic_light_position: None }),
                kind: WindowKind::Normal,
                focus: true,
                show: true,
                is_resizable: false,
                is_minimizable: true,
                ..Default::default()
            };
            match cx.open_window(options, |window, cx| cx.new(|cx| SetupWindow::new(overlay.clone(), window, cx))) {
                Ok(handle) => overlay.update(cx, |overlay, _| if let Some(setup) = &mut overlay.setup { setup.window = Some(handle) }),
                Err(error) => {
                    eprintln!("setup window could not open: {error}");
                    overlay.update(cx, |overlay, _| { overlay.setup = None; overlay.show(); });
                }
            }
        });
    }

    /// End setup: Finish and Skip save it as done; closing the window before Done leaves it to
    /// open again at the next launch. The overlay shows either way; a download carries on.
    fn close_setup(&mut self, done: bool, window: &mut Window, cx: &mut Context<Self>) {
        if done { self.update_settings(|s| s.setup_done = true, window, cx); }
        if let Some(setup) = self.setup.take() {
            setup.answer.cancel_sign_in();
            if let Some(handle) = setup.window {
                cx.defer(move |cx| { let _ = handle.update(cx, |_, window, _| window.remove_window()); });
            }
        }
        self.key_input.update(cx, |input, cx| input.clear(cx));
        self.key_notice = None;
        self.show();
        cx.notify();
    }

    /// Show `step`, starting what it needs (devices and meters, the model's status) and stopping
    /// what the one left had running.
    fn go_to(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        let left = setup.step;
        setup.step = step;
        setup.furthest = setup.furthest.max(step);
        setup.menu = None;
        if left == step { return; }
        setup.replacing_key = false;
        if left == Step::AudioCheck { setup.audio = None; }
        // The key field is shared by the answer and Deepgram keys.
        self.key_input.update(cx, |input, cx| input.clear(cx));
        self.key_notice = None;
        match step {
            Step::AnswerModel => self.choose_ready_path(window, cx),
            Step::Transcription => self.refresh_model_status(),
            Step::AudioCheck => { self.load_devices(window, cx); self.start_audio_check(window, cx); }
            // What Customize changes shows on the overlay.
            Step::Customize => self.show(),
            _ => {}
        }
        cx.notify();
    }

    fn set_menu(&mut self, menu: Option<Menu>, cx: &mut Context<Self>) {
        if let Some(setup) = &mut self.setup && setup.menu != menu { setup.menu = menu; cx.notify(); }
    }

    /// The window's contents: steps on the left, the current step and its footer on the right.
    fn setup_page(&self, cx: &mut Context<Self>) -> Div {
        let Some(setup) = &self.setup else { return div() };
        let mut rail = div().flex().flex_col().gap(px(3.0))
            .child(div().px(px(8.0)).pb(px(8.0)).child(ui::section_label("SETUP")));
        for step in Step::ALL {
            let current = step == setup.step;
            let done = !current && step < setup.furthest;
            let trailing = if done { Some(ui::icon("icons/setup-check.svg", 14.0, theme::ok()).into_any_element()) }
                else if step == Step::Customize { Some(div().text_size(px(11.0)).text_color(rgb(steps::FAINT)).child("Optional").into_any_element()) }
                else { None };
            rail = rail.child(div().id(("setup-step", step as usize)).flex().items_center().gap(px(10.0)).px(px(8.0)).py(px(7.0)).rounded(px(8.0)).cursor_pointer()
                .when(current, |row| row.bg(rgb(0x1f2124)))
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.go_to(step, window, cx)))
                .child(div().size(px(20.0)).flex_none().flex().items_center().justify_center()
                    .child(ui::icon(step.icon(), 16.0, if current { theme::accent_soft() } else { theme::muted() })))
                .child(div().flex_1().text_size(px(13.0)).line_height(px(16.0))
                    .when(current, |label| label.font_weight(FontWeight::SEMIBOLD).text_color(theme::text()))
                    .when(!current, |label| label.text_color(theme::body()))
                    .child(step.label()))
                .children(trailing));
        }
        let rail = div().w(px(220.0)).flex_none().py(px(16.0)).px(px(8.0)).bg(rgb(0x121316)).border_r_1().border_color(theme::divider()).child(rail);
        let body = div().id("setup-body").flex_1().min_h_0().overflow_y_scroll()
            .child(div().px(px(36.0)).pt(px(30.0)).pb(px(20.0)).child(self.setup_step(setup, cx)));
        div().size_full().flex()
            .child(rail)
            .child(div().flex_1().min_w_0().flex().flex_col().child(body).child(self.setup_footer(setup, cx)))
    }

    /// Skip setup on the left (kept but invisible on Done), Back and the step's main button on the
    /// right. Continue waits only while a sign-in is running in the terminal.
    fn setup_footer(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let step = setup.step;
        let button = |id: &'static str, label: &'static str| div().id(id).h(px(34.0)).flex().items_center().rounded(px(9.0))
            .text_size(px(13.0)).child(label);
        let primary = match step { Step::Welcome => "Get started", Step::Done => "Open CluelyRS", _ => "Continue" };
        let waiting = step == Step::AnswerModel && setup.answer.signing_in().is_some();
        let next = step.offset(1);
        let mut main = button("setup-next", primary).px(px(16.0)).font_weight(FontWeight::SEMIBOLD);
        main = if waiting { main.bg(theme::divider()).text_color(rgb(steps::DISABLED)) } else {
            main.bg(theme::accent()).text_color(theme::accent_ink()).cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| match next {
                    Some(next) => this.go_to(next, window, cx),
                    None => this.close_setup(true, window, cx),
                }))
        };
        let nav = div().flex().items_center().gap(px(8.0))
            .when_some(step.offset(-1), |nav, previous| nav.child(button("setup-back", "Back").px(px(14.0)).cursor_pointer().bg(theme::field())
                .border_1().border_color(theme::hairline()).text_color(rgb(steps::LIGHT))
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.go_to(previous, window, cx)))))
            .child(main);
        let skip = div().id("setup-skip").text_size(px(13.0)).text_color(theme::muted()).child("Skip setup");
        let skip = if step == Step::Done { skip.opacity(0.0) } else {
            skip.cursor_pointer().on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.close_setup(true, window, cx)))
        };
        div().flex_none().flex().items_center().justify_between().py(px(12.0)).pl(px(36.0)).pr(px(20.0))
            .bg(rgb(0x121316)).border_t_1().border_color(theme::divider())
            .child(skip)
            .child(nav)
    }
}

pub struct SetupWindow {
    overlay: Entity<Overlay>,
    native: Option<NativeWindow>,
    /// The capture exclusion last applied, so it is only set when the setting changes.
    capture_hidden: Option<bool>,
    _overlay_changed: Subscription,
}

impl SetupWindow {
    fn new(overlay: Entity<Overlay>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let closing = overlay.downgrade();
        // The title bar's ×: finishes setup on Done; before that, setup opens again next launch.
        window.on_window_should_close(cx, move |_, cx| {
            let Some(overlay) = closing.upgrade() else { return true };
            let own = overlay.read(cx).own_window;
            let _ = own.update(cx, |_, window, cx| overlay.update(cx, |overlay, cx| {
                let Some(setup) = &mut overlay.setup else { return };
                setup.window = None;
                let done = setup.step == Step::Done;
                overlay.close_setup(done, window, cx);
            }));
            true
        });
        let _overlay_changed = cx.observe(&overlay, |_, _, cx| cx.notify());
        Self { overlay, native: platform::native_window(window), capture_hidden: None, _overlay_changed }
    }

    fn apply_capture(&mut self, cx: &App) {
        let settings = &self.overlay.read(cx).store.value;
        let Some(native) = self.native else { return };
        if self.capture_hidden == Some(settings.hide_from_capture) { return; }
        self.capture_hidden = Some(settings.hide_from_capture);
        overlay::apply_capture_setting(native, settings);
    }
}

impl Render for SetupWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_capture(cx);
        let page = self.overlay.update(cx, |overlay, cx| overlay.setup_page(cx));
        div().size_full().flex().flex_col().bg(rgb(0x0f1012)).font_family(theme::FONT).text_color(theme::text())
            .child(crate::sessions_window::title_bar("Set up CluelyRS", window))
            .child(div().flex_1().min_h_0().flex().child(page))
    }
}
