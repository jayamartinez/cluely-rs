//! Windows: first-run setup in a window of its own, from the Paper "Redesign v3 · Onboarding" Windows
//! artboards: Welcome, Answer model, Listening. It opens at launch until it is finished or skipped
//! (`Settings::setup_done`), and again from Settings › About › Run setup again. The overlay stays
//! hidden while it is open, until Assist (or Show / hide) brings it up. Like the Modes window it is a
//! normal window that draws its own title bar and hides from screen capture while the overlay does;
//! its pages are drawn by the overlay, whose settings they change.

mod answer;
mod hearing;
mod steps;

use gpui::{
    App, AppContext, Bounds, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement, MouseButton, ObjectFit, ParentElement, Render, Styled,
    StyledImage, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, img, px, size,
};

use crate::overlay::{self, Overlay};
use crate::platform::{self, NativeWindow};
use crate::settings::SttProvider;
use crate::settings_view::listen;
use crate::{theme, ui, ui_v3};

/// The steps, in order. Listening is `Transcription`, the name the overlay knows it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Step { Welcome, AnswerModel, Transcription }

impl Step {
    const ALL: [Step; 3] = [Step::Welcome, Step::AnswerModel, Step::Transcription];

    fn offset(self, by: isize) -> Option<Step> { Step::ALL.get((self as usize).checked_add_signed(by)?).copied() }
}

const WIDTH: f32 = 760.0;
const HEIGHT: f32 = 560.0;

/// The dropdowns on the setup pages; at most one is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu { ApiProvider, Mic, Desktop }

/// Where setup is, kept on the overlay while its window is open.
pub(crate) struct Setup {
    window: Option<WindowHandle<SetupWindow>>,
    pub(crate) step: Step,
    /// Assist was pressed while setup was open (`Overlay::handle` routes it here).
    pub(crate) tried_assist: bool,
    menu: Option<Menu>,
    answer: answer::AnswerState,
    /// "Replace" was pressed beside a saved key: its field takes a new one.
    replacing_key: bool,
    /// Listening's meters, while that step is open.
    audio: Option<hearing::AudioCheck>,
    /// Listening's choice of speech model; nothing is chosen until one is clicked.
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
        self.setup = Some(Setup { window: None, step: Step::Welcome, tried_assist: false, menu: None, answer: answer::AnswerState::default(),
            replacing_key: false, audio: None, speech: None });
        self.find_clis(window, cx);
        self.show_model_example(cx);
        let overlay = cx.weak_entity();
        // Deferred: the window reads the overlay while it draws, and the overlay is being updated now.
        cx.defer(move |cx| {
            let Some(overlay) = overlay.upgrade() else { return };
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx))),
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

    /// End setup: Start CluelyRS saves it as done; closing the window before the last step leaves it
    /// to open again at the next launch. The overlay shows either way; a download carries on.
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

    /// Show `step`, starting what it needs (the account check, devices and meters, the model's
    /// status) and stopping what the one left had running.
    fn go_to(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        let left = setup.step;
        setup.step = step;
        setup.menu = None;
        if left == step { return; }
        setup.replacing_key = false;
        if left == Step::Transcription { setup.audio = None; }
        // The key field is shared by the answer and Deepgram keys.
        self.key_input.update(cx, |input, cx| input.clear(cx));
        self.key_notice = None;
        match step {
            Step::AnswerModel => self.choose_ready_path(window, cx),
            Step::Transcription => { self.refresh_model_status(); self.load_devices(window, cx); self.start_audio_check(window, cx); }
            Step::Welcome => {}
        }
        cx.notify();
    }

    fn set_menu(&mut self, menu: Option<Menu>, cx: &mut Context<Self>) {
        if let Some(setup) = &mut self.setup && setup.menu != menu { setup.menu = menu; cx.notify(); }
    }

    /// The window's contents: the title bar with the step indicator, the page, and the back arrow.
    fn setup_page(&self, cx: &mut Context<Self>) -> Div {
        let Some(setup) = &self.setup else { return div() };
        let back = setup.step.offset(-1).map(|previous| div().id("setup-back").absolute().left(px(16.0)).top(px(52.0)).size(px(32.0))
            .flex().items_center().justify_center().rounded(px(ui_v3::RADII.small)).cursor_pointer().hover(|back| back.bg(theme::raised()))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.go_to(previous, window, cx)))
            .child(ui_v3::back_arrow()));
        let glow = img(gpui::ImageSource::Resource(gpui::Resource::Embedded("app-icon/setup-glow.svg".into()))).absolute().top_0().left_0().size_full()
            .object_fit(ObjectFit::Fill);
        div().relative().size_full().flex().flex_col()
            .child(glow)
            .child(title_bar(setup.step))
            .child(self.setup_step(setup, cx))
            .children(back)
    }
}

/// The title bar: the mark and the window's name, the step indicator, and the caption buttons.
fn title_bar(step: Step) -> impl IntoElement {
    let caption = |id: &'static str, glyph: &'static str| div().id(id).w(px(46.0)).h(px(40.0)).flex().items_center().justify_center()
        .child(ui::icon(glyph, 10.0, theme::body()));
    let (drag, minimize, close) = (
        div().id("title-drag").w(px(200.0)).flex_none().h_full().flex().items_center().gap(px(10.0)).pl(px(14.0))
            .child(ui_v3::mark(16.0))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::body()).child("Set up CluelyRS")),
        caption("minimize", "icons/minimize.svg").hover(|button| button.bg(theme::raised())),
        caption("close-window", "icons/window-close.svg").hover(|button| button.bg(gpui::rgb(0xc42b1c))),
    );
    // Windows has no window-move call: the drag area and buttons report themselves to the system's
    // hit test instead (Aero Snap, Snap Layouts), so they have no click handlers.
    let (drag, minimize, close) = (
        drag.window_control_area(gpui::WindowControlArea::Drag),
        minimize.window_control_area(gpui::WindowControlArea::Min),
        close.window_control_area(gpui::WindowControlArea::Close),
    );
    div().h(px(40.0)).flex_none().flex().items_center()
        .child(drag)
        .child(div().id("title-middle").flex_1().h_full().flex().items_center().justify_center()
            .child(ui_v3::progress_dashes(Step::ALL.len(), step as usize)))
        // Setup doesn't resize, so Maximize shows but is dimmed.
        .child(div().w(px(200.0)).flex_none().flex().justify_end()
            .child(minimize)
            .child(div().w(px(46.0)).h(px(40.0)).flex().items_center().justify_center().opacity(0.35).child(ui::icon("icons/maximize.svg", 10.0, theme::body())))
            .child(close))
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
        // The title bar's ×: before the last step, setup opens again next launch.
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_capture(cx);
        let page = self.overlay.update(cx, |overlay, cx| overlay.setup_page(cx));
        div().size_full().flex().flex_col().bg(theme::window_bg()).font_family(theme::FONT).text_color(theme::text())
            .font_weight(FontWeight::NORMAL)
            .child(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steps_run_welcome_answer_model_listening() {
        assert_eq!(Step::Welcome.offset(1), Some(Step::AnswerModel));
        assert_eq!(Step::AnswerModel.offset(1), Some(Step::Transcription));
        assert_eq!(Step::Transcription.offset(1), None, "Start CluelyRS finishes");
        assert_eq!(Step::Welcome.offset(-1), None, "no back arrow on Welcome");
    }
}
