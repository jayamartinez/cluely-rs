//! Windows: first-run setup in a window of its own, from the Paper "Redesign v3 · Onboarding"
//! Windows artboards: Welcome → Answer model → Listening, in a fixed 760 × 560 window with a drawn
//! title bar (progress bars in the middle), a ghost Back button and one primary button at the bottom.
//! It opens at launch until it is finished (`Settings::setup_done`), and again from Settings › About ›
//! Run setup again. The overlay stays hidden while it is open, until Assist (or Show / hide) brings it
//! up. Like Settings it hides from screen capture while the overlay does; its pages are drawn by the
//! overlay, whose settings they change. What setup no longer asks (privacy, shortcuts, the look) keeps
//! its defaults and lives in Settings.

mod answer;
mod hearing;
mod steps;

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Div, Entity, FontWeight, MouseButton, ParentElement, Render, Styled, Subscription, TitlebarOptions,
    Transformation, Window, WindowBounds, WindowControlArea, WindowHandle, WindowKind, WindowOptions, div, prelude::*, px, radians, rgb, size,
};

use crate::overlay::{self, Overlay};
use crate::platform::{self, NativeWindow};
use crate::settings::SttProvider;
use crate::settings_view::listen;
use crate::{theme, ui};

const WIDTH: f32 = 760.0;
const HEIGHT: f32 = 560.0;
const TITLE: &str = "Set up CluelyRS";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Step { Welcome, AnswerModel, Listening }

impl Step {
    const ALL: [Step; 3] = [Step::Welcome, Step::AnswerModel, Step::Listening];

    fn offset(self, by: isize) -> Option<Step> { Step::ALL.get((self as usize).checked_add_signed(by)?).copied() }
}

/// The dropdowns on the setup pages; at most one is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu { ApiProvider, Mic, Desktop }

/// Where setup is, kept on the overlay while its window is open.
pub(crate) struct Setup {
    window: Option<WindowHandle<SetupWindow>>,
    pub(crate) step: Step,
    menu: Option<Menu>,
    answer: answer::AnswerState,
    /// The saved key was clicked: its field takes a new one.
    replacing_key: bool,
    /// Listening's meters, while that step is open.
    audio: Option<hearing::AudioCheck>,
    /// The speech model picked on Listening. Nothing is picked at first, whatever Settings holds, and
    /// Start waits for a pick (`hearing::speech_ready`).
    speech: Option<SttProvider>,
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
        self.setup = Some(Setup { window: None, step: Step::Welcome, menu: None, answer: answer::AnswerState::default(), replacing_key: false,
            audio: None, speech: None });
        self.find_clis(window, cx);
        self.show_model_example(cx);
        let overlay = cx.weak_entity();
        // Deferred: the window reads the overlay while it draws, and the overlay is being updated now.
        cx.defer(move |cx| {
            let Some(overlay) = overlay.upgrade() else { return };
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx))),
                titlebar: Some(TitlebarOptions { title: Some(TITLE.into()), appears_transparent: true, traffic_light_position: None }),
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

    /// End setup: Start CluelyRS saves it as done; closing the window before that leaves it to open
    /// again at the next launch. The overlay shows either way; a download carries on.
    fn close_setup(&mut self, done: bool, window: &mut Window, cx: &mut Context<Self>) {
        if done { self.update_settings(|s| s.setup_done = true, window, cx); }
        if let Some(setup) = self.setup.take() {
            setup.answer.cancel_sign_in();
            if let Some(handle) = setup.window {
                cx.defer(move |cx| { let _ = handle.update(cx, |_, window, _| window.remove_window()); });
            }
        }
        self.key_input.update(cx, |input, cx| input.clear(cx));
        self.model_input.update(cx, |input, cx| input.set_placeholder(overlay::MODEL_PLACEHOLDER, cx));
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
        setup.menu = None;
        if left == step { return; }
        setup.replacing_key = false;
        if left == Step::Listening { setup.audio = None; }
        // The key field is shared by the answer and Deepgram keys.
        self.key_input.update(cx, |input, cx| input.clear(cx));
        self.key_notice = None;
        match step {
            Step::AnswerModel => self.choose_ready_path(window, cx),
            Step::Listening => { self.refresh_model_status(); self.load_devices(window, cx); self.start_audio_check(window, cx); }
            Step::Welcome => {}
        }
        cx.notify();
    }

    fn set_menu(&mut self, menu: Option<Menu>, cx: &mut Context<Self>) {
        if let Some(setup) = &mut self.setup && setup.menu != menu { setup.menu = menu; cx.notify(); }
    }

    /// The whole window: the glow, the title bar, the step, and Back from the second step on.
    fn setup_page(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let Some(setup) = &self.setup else { return div() };
        let back = setup.step.offset(-1).map(|previous| {
            div().id("setup-back").absolute().left(px(16.0)).top(px(52.0)).size(px(32.0)).flex().items_center().justify_center()
                .rounded(px(4.0)).cursor_pointer().hover(|button| button.bg(theme::hover()))
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.go_to(previous, window, cx)))
                // No left arrow asset: setup-arrow turned around.
                .child(gpui::svg().path("icons/setup-arrow.svg").size(px(18.0)).flex_none().text_color(theme::muted())
                    .with_transformation(Transformation::rotate(radians(std::f32::consts::PI))))
        });
        div().relative().size_full().flex().flex_col()
            .child(glow())
            .child(title_bar(setup.step))
            .child(self.setup_step(setup, window, cx))
            .children(back)
    }

    /// A step's body: its content centred in the space above the actions (the primary button, and
    /// what goes under it), which sit at the bottom.
    fn step_frame(top: f32, bottom: f32, gap: f32, content: Vec<AnyElement>, actions: impl IntoElement) -> Div {
        div().flex_1().min_h_0().flex().flex_col().items_center().justify_between().pt(px(top)).pb(px(bottom)).px(px(64.0))
            .child(div().flex_1().min_h_0().flex().flex_col().items_center().justify_center().gap(px(gap)).children(content))
            .child(actions)
    }

    /// The single 280 × 44 button at the bottom. Disabled, it is the quiet secondary look.
    fn primary(&self, id: &'static str, label: &'static str, enabled: bool, next: Option<Step>, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let button = div().id(id).w(px(280.0)).h(px(44.0)).flex_none().flex().items_center().justify_center().rounded(px(4.0))
            .text_size(px(14.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).child(label);
        if !enabled { return button.bg(theme::raised()).border_1().border_color(theme::hairline()).text_color(theme::placeholder()); }
        button.bg(theme::accent()).text_color(theme::accent_ink()).cursor_pointer().hover(|button| button.opacity(0.92))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| match next {
                Some(next) => this.go_to(next, window, cx),
                None => this.close_setup(true, window, cx),
            }))
    }
}

/// The radial #13203A glow at the top centre, 70 % of the window wide and 55 % tall, fading to the
/// window's own background.
fn glow() -> impl IntoElement {
    gpui::svg().path("icons/setup-glow.svg").absolute().top_0().left(px((WIDTH - 1064.0) / 2.0)).w(px(1064.0)).h(px(308.0))
        .text_color(theme::selected_row())
}

/// 16 px viewfinder and the title on the left, the progress bars centred, ─ □ ✕ on the right (46 px
/// each, □ dimmed: the window has one size). The drag area and caption buttons report themselves to
/// the system's hit test, as in Settings.
fn title_bar(step: Step) -> impl IntoElement {
    let control = |id: &'static str, icon: &'static str| div().id(id).w(px(46.0)).h_full().flex().items_center().justify_center()
        .child(ui::icon(icon, 10.0, theme::body()));
    let progress = div().flex_1().flex().items_center().justify_center().gap(px(6.0)).children(Step::ALL.map(|each| {
        let (width, color) = if each == step { (22.0, theme::accent()) } else if each < step { (6.0, theme::bubble_border()) } else { (6.0, theme::keycap_border()) };
        div().w(px(width)).h(px(4.0)).flex_none().rounded(px(2.0)).bg(color)
    }));
    div().id("setup-title-bar").flex_none().h(px(40.0)).flex().items_center()
        .child(div().id("setup-title-drag").flex_1().h_full().flex().items_center().window_control_area(WindowControlArea::Drag)
            .child(div().w(px(200.0)).flex_none().flex().items_center().gap(px(10.0)).pl(px(14.0))
                .child(ui::viewfinder(16.0))
                .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::body()).child(TITLE)))
            .child(progress)
            // Balances the left side, so the bars sit in the middle of the window.
            .child(div().w(px(200.0 - 3.0 * 46.0)).flex_none()))
        .child(div().flex().h_full()
            .child(control("setup-minimize", "icons/minimize.svg").hover(|button| button.bg(theme::raised())).window_control_area(WindowControlArea::Min))
            .child(control("setup-maximize", "icons/maximize.svg").opacity(0.35))
            .child(control("setup-close", "icons/window-close.svg").hover(|button| button.bg(rgb(0xc42b1c))).window_control_area(WindowControlArea::Close)))
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
        // The title bar's ✕ (and Alt+F4): setup opens again at the next launch.
        window.on_window_should_close(cx, move |_, cx| {
            let Some(overlay) = closing.upgrade() else { return true };
            let own = overlay.read(cx).own_window;
            let _ = own.update(cx, |_, window, cx| overlay.update(cx, |overlay, cx| {
                let Some(setup) = &mut overlay.setup else { return };
                setup.window = None;
                overlay.close_setup(false, window, cx);
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
        let page = self.overlay.update(cx, |overlay, cx| overlay.in_window(cx, |this, cx| this.setup_page(window, cx)));
        div().size_full().bg(theme::fluent_window()).border_1().border_color(theme::keycap_border()).overflow_hidden()
            .font_family(theme::FONT).text_color(theme::text())
            .child(page)
    }
}
