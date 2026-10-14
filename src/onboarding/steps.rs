//! The pages of first-run setup and the pieces they share, from the Paper "Onboarding v2 · Windows"
//! artboards. Every control changes the setting it shows at once and saves it, as in Settings.
//! Answer model is in `answer`, Transcription and Audio check in `hearing`.

use gpui::{
    AnyElement, Context, Div, Entity, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Rgba, SharedString, Stateful, Styled, Window, deferred, div,
    prelude::*, px, relative, rgb,
};

use super::{Menu, Setup, Step};
use crate::appearance::{CardWidth, TextSize};
use crate::archive::Retention;
use crate::hotkeys::Action;
use crate::input::TextInput;
use crate::overlay::Overlay;
use crate::settings::{AnswerStyle, Provider, SttProvider};
use crate::settings_view::{choice, listen, switch};
use crate::{theme, ui};

/// Notes about cost and shortcuts that are taken.
pub(super) const CAUTION: u32 = 0xf0c674;
pub(super) const ERROR: u32 = 0xf08a7e;
pub(super) const LIGHT: u32 = 0xe6e3dc;
pub(super) const FAINT: u32 = 0x8a867e;
pub(super) const DISABLED: u32 = 0x6e6a63;

/// The global shortcuts, as setup lists them: movement and scrolling as one row each.
const SHORTCUTS: [(&str, &str, &[Action]); 6] = [
    ("icons/setup-eye.svg", "Show / hide", &[Action::Toggle]),
    ("icons/setup-bolt.svg", "Assist", &[Action::Assist]),
    ("icons/setup-field.svg", "Type a question", &[Action::Focus]),
    ("icons/setup-grid.svg", "Switch mode", &[Action::SwitchMode]),
    ("icons/setup-move.svg", "Move the overlay", &[Action::MoveLeft, Action::MoveUp, Action::MoveDown, Action::MoveRight]),
    ("icons/setup-scroll.svg", "Scroll the answer", &[Action::ScrollUp, Action::ScrollDown]),
];

pub(super) fn page() -> Div { div().flex().flex_col().gap(px(20.0)) }

/// A step's title and the line under it.
pub(super) fn heading(title: &'static str, detail: &'static str) -> Div {
    div().flex().flex_col().gap(px(6.0))
        .child(div().text_size(px(22.0)).line_height(px(28.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(title))
        .child(div().text_size(px(13.0)).line_height(px(18.0)).text_color(theme::muted()).child(detail))
}

/// Rows in a rounded box, a hairline between each.
pub(super) fn boxed(rows: Vec<Div>) -> Div {
    let last = rows.len().saturating_sub(1);
    div().flex().flex_col().rounded(px(12.0)).border_1().border_color(theme::hairline()).overflow_hidden()
        .children(rows.into_iter().enumerate().map(|(index, row)| row.when(index < last, |row| row.border_b_1().border_color(theme::divider()))))
}

/// A section label over a box of rows.
fn group(label: &'static str, rows: Vec<Div>) -> Div {
    div().flex().flex_col().gap(px(8.0)).child(div().pl(px(2.0)).child(ui::section_label(label))).child(boxed(rows))
}

/// Icon, label and whatever follows on the right.
fn line(icon: &'static str, size: f32, label: impl Into<SharedString>, trailing: impl IntoElement) -> Div {
    div().flex().items_center().gap(px(12.0))
        .child(ui::icon(icon, size, theme::body()))
        .child(div().flex_1().min_w_0().text_size(px(13.0)).line_height(px(16.0)).text_color(theme::text()).child(label.into()))
        .child(trailing)
}

/// A muted line with an icon: where something is set, or where to find setup again.
pub(super) fn hint(icon: &'static str, text: impl Into<SharedString>) -> Div {
    div().flex().items_center().gap(px(8.0)).pl(px(2.0))
        .child(ui::icon(icon, 14.0, theme::muted()))
        .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(text.into()))
}

/// A key drawn larger than `ui::keycap`, for the keys the pages ask to press.
fn cap(label: impl Into<SharedString>, large: bool, border: Rgba, color: Rgba) -> Div {
    let (y, x, radius, size) = if large { (5.0, 9.0, 7.0, 13.0) } else { (2.0, 7.0, 6.0, 12.0) };
    div().py(px(y)).px(px(x)).rounded(px(radius)).border_1().border_color(border)
        .font_family(theme::MONO).text_size(px(size)).line_height(px(16.0)).text_color(color).child(label.into())
}

/// "Ctrl ↵" → ["Ctrl", "Enter"]: one cap per key, with Enter spelled out as the setup pages write it.
fn key_names(label: &str) -> Vec<String> {
    label.split(' ').map(|key| if key == "↵" { "Enter".to_string() } else { key.to_string() }).collect()
}

/// One of three choices side by side: an icon and its status on top, the name and a line under it.
pub(super) fn option_card(id: (&'static str, usize), selected: bool, icon: AnyElement, status: AnyElement, name: &'static str, detail: AnyElement) -> Stateful<Div> {
    div().id(id).flex_1().min_w_0().flex().flex_col().gap(px(14.0)).p(px(14.0)).rounded(px(12.0)).border_1().cursor_pointer()
        .border_color(if selected { theme::bubble_border() } else { theme::hairline() })
        .when(selected, |card| card.bg(rgb(0x0d1830)))
        .child(div().h(px(22.0)).flex().items_center().justify_between().child(icon).child(status))
        .child(div().flex().flex_col().gap(px(2.0))
            .child(div().text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(name))
            .child(detail))
}

pub(super) fn detail_text(text: &'static str) -> AnyElement {
    div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(text).into_any_element()
}

pub(super) fn status_text(text: impl Into<SharedString>, color: Rgba) -> AnyElement {
    div().flex_none().text_size(px(12.0)).line_height(px(16.0)).text_color(color).child(text.into()).into_any_element()
}

pub(super) fn muted_text(text: &'static str) -> Div { div().text_size(px(13.0)).text_color(theme::muted()).child(text) }

/// A row in a box of settings: icon, an 88 px label and the value; callers add a check or an action.
pub(super) fn labelled(icon: &'static str, label: &'static str, value: impl IntoElement) -> Div {
    labelled_with(ui::icon(icon, 16.0, theme::muted()), label, value)
}

/// [`labelled`] with an icon of its own (a spinner while waiting).
pub(super) fn labelled_with(icon: impl IntoElement, label: &'static str, value: impl IntoElement) -> Div {
    div().flex().items_center().gap(px(12.0)).px(px(12.0)).py(px(8.0)).min_h(px(48.0))
        .child(icon)
        .child(div().w(px(88.0)).flex_none().text_size(px(13.0)).text_color(theme::muted()).child(label))
        .child(div().flex_1().min_w_0().child(value))
}

/// The fixed lane at a row's right edge, so actions line up from row to row.
pub(super) fn trailing(width: f32, content: impl IntoElement) -> Div { div().w(px(width)).flex_none().flex().justify_end().child(content) }

/// A quiet text action ("Locate…", "Sign out", "Cancel").
pub(super) fn link(id: &'static str, label: &'static str) -> Stateful<Div> {
    div().id(id).cursor_pointer().text_size(px(12.0)).text_color(theme::muted()).hover(|link| link.text_color(theme::body())).child(label)
}

/// A link that opens a web page ("Get a key ↗").
pub(super) fn external(id: &'static str, label: &'static str) -> Stateful<Div> {
    div().id(id).flex_none().flex().items_center().gap(px(4.0)).cursor_pointer()
        .child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child(label))
        .child(ui::icon("icons/setup-external.svg", 9.0, theme::accent_soft()))
}

pub(super) fn small_button(id: &'static str, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    let button = div().id(id).flex_none().h(px(30.0)).px(px(12.0)).flex().flex_row_reverse().items_center().gap(px(6.0)).rounded(px(8.0)).cursor_pointer()
        .text_size(px(12.0)).child(label.into());
    if primary { button.bg(theme::accent()).text_color(theme::accent_ink()).font_weight(FontWeight::SEMIBOLD) }
    else { button.bg(theme::field()).border_1().border_color(theme::hairline()).text_color(rgb(LIGHT)) }
}

/// A result under a box of rows, tucked close to it.
pub(super) fn status_line(icon: &'static str, text: impl Into<SharedString>, color: Rgba) -> Div {
    div().mt(px(-8.0)).pl(px(12.0)).flex().items_center().gap(px(8.0))
        .child(ui::icon(icon, 14.0, color))
        .child(div().flex_1().min_w_0().text_size(px(12.0)).line_height(px(16.0)).text_color(color).child(text.into()))
}

impl Overlay {
    pub(super) fn setup_step(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        match setup.step {
            Step::Welcome => welcome(),
            Step::AnswerModel => self.answer_model(setup, cx),
            Step::Transcription => self.transcription(setup, cx),
            Step::AudioCheck => self.audio_check(setup, cx),
            Step::Privacy => self.privacy(cx),
            Step::Shortcuts => self.shortcuts(setup),
            Step::Customize => self.customize(setup, cx),
            Step::Done => self.summary(setup, cx),
        }
    }

    /// A dropdown on a setup page: its face, and the list under it while open. `pick` gets the
    /// chosen option's id. Shorter than 32 px, it is the compact kind with smaller text.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn setup_dropdown(&self, setup: &Setup, menu: Menu, value: impl Into<SharedString>, options: Vec<(String, String)>, selected: &str, height: f32,
        pick: impl Fn(&mut Overlay, String, &mut Window, &mut Context<Overlay>) + Clone + 'static, cx: &mut Context<Self>) -> Div {
        let open = setup.menu == Some(menu);
        let compact = height < 32.0;
        let face = div().id(("setup-menu", menu as usize)).w_full().h(px(height)).px(px(if height > 32.0 { 12.0 } else { 10.0 }))
            .flex().items_center().justify_between().gap(px(6.0)).rounded(px(if height > 32.0 { 9.0 } else { 8.0 })).cursor_pointer()
            .bg(theme::field()).border_1().border_color(if open { theme::accent() } else { theme::hairline() })
            .child(div().min_w_0().truncate().text_size(px(if compact { 12.0 } else { 13.0 })).text_color(theme::text()).child(value.into()))
            .child(ui::icon("icons/chevron-down.svg", if compact { 10.0 } else { 12.0 }, theme::muted()))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| this.set_menu(Some(menu), cx)));
        let mut dropdown = div().relative().w_full().child(face);
        if open {
            let mut list = ui::menu().id(("setup-list", menu as usize)).w_full().min_w(px(180.0)).max_h(px(260.0)).overflow_y_scroll()
                .on_mouse_down_out(listen(cx, |this, _, _, cx| this.set_menu(None, cx)));
            for (index, (id, label)) in options.into_iter().enumerate() {
                let (chosen, pick) = (id.clone(), pick.clone());
                list = list.child(ui::menu_item(("setup-option", index), label, id == selected)
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.set_menu(None, cx);
                        pick(this, chosen.clone(), window, cx);
                    })));
            }
            dropdown = dropdown.child(deferred(div().absolute().top(px(height + 4.0)).right_0().w_full().child(list)).with_priority(1));
        }
        dropdown
    }

    /// A text box on a setup page. Focused with a plain handler: `listen` would hand it the
    /// overlay's window, which can't take the setup window's keyboard focus.
    pub(super) fn text_field(&self, id: &'static str, input: Entity<TextInput>) -> Div {
        let focus = input.clone();
        div().flex_1().min_w_0().child(div().id(id).h(px(32.0)).px(px(10.0)).flex().items_center().rounded(px(8.0)).cursor_text()
            .bg(theme::field()).border_1().border_color(theme::hairline()).text_size(px(12.0))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&focus.focus_handle(cx)))
            .child(input))
    }

    /// An API key: the saved one, masked, with Replace; or the box to paste one, with Save. A key the
    /// provider refused gets a red edge.
    pub(super) fn key_field(&self, setup: &Setup, saved: Option<&str>, refused: bool, cx: &mut Context<Self>) -> (Div, Stateful<Div>) {
        match saved.filter(|_| !setup.replacing_key) {
            Some(hint) => {
                let field = div().h(px(32.0)).px(px(10.0)).flex().items_center().justify_between().rounded(px(8.0)).bg(theme::field()).border_1()
                    .border_color(if refused { rgb(0x6b3a36) } else { theme::hairline() })
                    .child(div().font_family(theme::MONO).text_size(px(12.0)).text_color(theme::body()).child(if hint.is_empty() { "Saved".to_string() } else { hint.to_string() }))
                    .child(ui::icon("icons/setup-lock.svg", 14.0, theme::muted()));
                let replace = small_button("setup-replace-key", "Replace", false).h(px(32.0))
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| if let Some(setup) = &mut this.setup { setup.replacing_key = true; cx.notify(); }));
                (field, replace)
            }
            None => (self.text_field("setup-key", self.key_input.clone()),
                small_button("setup-save-key", "Save", true).h(px(32.0)).on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.save_key(window, cx)))),
        }
    }

    /// The keys for a shortcuts row: the shared modifiers, then each arrow ("Ctrl Alt ← ↑ ↓ →").
    fn shortcut_keys(&self, actions: &[Action]) -> Vec<String> {
        let mut keys = key_names(&self.hotkeys.label(actions[0]));
        if actions.len() > 1 {
            keys.pop();
            keys.push(actions.iter().filter_map(|action| self.hotkeys.label(*action).rsplit(' ').next().map(str::to_string)).collect::<Vec<_>>().join(" "));
        }
        keys
    }

    fn shortcut_taken(&self, actions: &[Action]) -> bool { actions.iter().any(|action| self.hotkeys.unavailable.contains(action)) }

    fn privacy(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let row = |icon, label, control| line(icon, 18.0, label, control).h(px(48.0)).px(px(14.0));
        let shown = vec![
            row("icons/eye-off.svg", "Hide from screen sharing", switch("setup-hide", s.hide_from_capture, |s, v| s.hide_from_capture = v, cx).into_any_element()),
            row("icons/image.svg", "Attach my screen to each question", switch("setup-screen", s.screen_on_send, |s, v| s.screen_on_send = v, cx).into_any_element()),
        ];
        let mut kept = vec![row("icons/setup-history.svg", "Save sessions", switch("setup-save", s.save_sessions, |s, v| s.save_sessions = v, cx).into_any_element())];
        // As in Settings › History, the rest only applies while sessions are saved.
        if s.save_sessions {
            kept.push(row("icons/setup-pictures.svg", "Include screenshots",
                switch("setup-screenshots", s.save_screenshots, |s, v| s.save_screenshots = v, cx).into_any_element()));
            kept.push(row("icons/setup-calendar.svg", "Keep for", choice("setup-keep", &[(Retention::Days7, "7 days"), (Retention::Days30, "30 days"),
                (Retention::Forever, "Forever")], s.keep_sessions, |s, v| s.keep_sessions = v, cx).into_any_element()));
        }
        page().child(heading("Privacy defaults", "All on. Change any of them later."))
            .child(group("WHAT OTHERS SEE AND WHAT'S SENT", shown))
            .child(group("KEPT ON THIS PC", kept))
    }

    /// Every global shortcut, with any another app holds marked Taken, and a place to try Assist:
    /// while setup is open, Assist only shows the overlay.
    fn shortcuts(&self, setup: &Setup) -> Div {
        let mut rows = Vec::new();
        for (icon, label, actions) in SHORTCUTS {
            let assist = actions[0] == Action::Assist;
            let taken = self.shortcut_taken(actions);
            let badge = div().flex().items_center().gap(px(5.0)).px(px(8.0)).py(px(2.0)).rounded_full().bg(rgb(0x2a2416))
                .child(ui::icon("icons/alert-circle.svg", 12.0, rgb(CAUTION)))
                .child(div().text_size(px(12.0)).text_color(rgb(CAUTION)).child("Taken"));
            let keys = div().flex().gap(px(4.0)).when(taken, |keys| keys.opacity(0.5).line_through())
                .children(self.shortcut_keys(actions).into_iter().map(ui::keycap));
            rows.push(div().flex().items_center().gap(px(12.0)).h(px(40.0))
                .child(ui::icon(icon, 16.0, if assist { theme::accent_soft() } else { theme::body() }))
                .child(div().flex_1().text_size(px(13.0)).text_color(theme::text()).when(assist, |label| label.font_weight(FontWeight::SEMIBOLD)).child(label))
                .when(taken, |row| row.child(badge))
                .child(keys));
        }
        let assist = self.shortcut_keys(&[Action::Assist]);
        let tried = setup.tried_assist;
        let (border, key_color) = if tried { (rgb(0x24503a), theme::ok()) } else { (theme::bubble_border(), rgb(0xc9daff)) };
        let status = if tried { ui::icon("icons/setup-circle-check.svg", 18.0, theme::ok()).into_any_element() } else {
            div().flex().items_center().gap(px(6.0))
                .child(div().size(px(6.0)).rounded_full().bg(theme::accent_soft()))
                .child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child("Waiting…"))
                .into_any_element()
        };
        let try_it = div().flex().items_center().gap(px(14.0)).px(px(14.0)).py(px(12.0)).rounded(px(12.0)).border_1().border_color(border)
            .bg(if tried { rgb(0x0f1a14) } else { rgb(0x0d1830) })
            .when(!tried, |row| row.border_dashed())
            .child(div().flex().gap(px(6.0)).children(assist.into_iter().map(|key| cap(key, true, border, key_color).when(!tried, |cap| cap.bg(rgb(0x101d38))))))
            .child(div().flex_1().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(if tried { "That's Assist" } else { "Try it now" }))
            .child(status);
        let toggle = self.shortcut_keys(&[Action::Toggle]).join(" ");
        page().child(heading("Shortcuts that work anywhere", "Even with another app in front."))
            .child(boxed(rows).px(px(14.0)))
            .child(try_it)
            .child(hint("icons/info.svg", if tried { format!("That's the overlay. Nothing was sent; {toggle} hides it again.") }
                else { "During setup Assist only shows the overlay. Nothing is sent.".to_string() }))
    }

    fn customize(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let row = |icon, label, control: AnyElement| line(icon, 16.0, label, control).gap(px(10.0)).h(px(36.0));
        let title = |label, top: f32| div().pt(px(top)).pb(px(6.0)).child(ui::section_label(label));
        let sizes = |name, current: usize, set: fn(&mut crate::settings::Settings, usize), cx: &mut Context<Self>|
            choice(name, &[(0, "S"), (1, "M"), (2, "L")], current, set, cx).into_any_element();

        let modes: Vec<(String, String)> = self.modes.modes().iter().map(|mode| (mode.id.clone(), mode.name.clone())).collect();
        let active = self.modes.active();
        let mode_picker = div().w(px(150.0)).child(self.setup_dropdown(setup, Menu::Mode, active.name.clone(), modes, &active.id, 28.0,
            |this, id, _, cx| this.activate_mode(&id, cx), cx));
        let mut answers = div().flex_1().flex().flex_col().gap(px(4.0))
            .child(title("ANSWERS", 0.0))
            .child(row("icons/setup-chat.svg", "Style", choice("setup-style", &[(AnswerStyle::Spoken, "Spoken"), (AnswerStyle::Standard, "Standard")],
                s.answer_style, |s, v| s.answer_style = v, cx).into_any_element()))
            .child(row("icons/setup-grid.svg", "Start in mode", mode_picker.into_any_element()))
            .child(row("icons/setup-bulb.svg", "Smart mode", switch("setup-smart", s.smart_mode, |s, v| s.smart_mode = v, cx).into_any_element()))
            .child(row("icons/setup-bolt.svg", "Answer early", switch("setup-early", s.speculative_answers, |s, v| s.speculative_answers = v, cx).into_any_element()))
            .child(row("icons/setup-eye.svg", "Show automatically", switch("setup-auto", s.auto_answer, |s, v| s.auto_answer = v, cx).into_any_element()));
        if s.speculative_answers || s.auto_answer {
            answers = answers.child(div().mt(px(6.0)).flex().items_center().gap(px(8.0)).px(px(10.0)).py(px(8.0)).rounded(px(8.0))
                .bg(rgb(0x1c1912)).border_1().border_color(rgb(0x3d3420))
                .child(ui::icon("icons/alert-circle.svg", 14.0, rgb(CAUTION)))
                .child(div().text_size(px(12.0)).text_color(rgb(LIGHT)).child("Early answers use your plan or credits")));
        }

        let login = div().id("setup-login").flex_none().cursor_pointer().child(ui::switch(setup.launch_at_login))
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.toggle_launch_at_login(cx)));
        let percent = s.background_opacity.min(100);
        let opacity = div().flex().items_center().gap(px(10.0)).h(px(36.0))
            .child(ui::icon("icons/setup-contrast.svg", 16.0, theme::body()))
            .child(div().w(px(62.0)).flex_none().text_size(px(13.0)).text_color(theme::text()).child("Opacity"))
            .child(self.opacity_slider(percent, cx))
            .child(div().w(px(30.0)).flex_none().flex().justify_end().text_size(px(12.0)).text_color(theme::muted()).child(format!("{percent}%")));
        let text_size = [TextSize::Small, TextSize::Standard, TextSize::Large].iter().position(|size| *size == s.text_size).unwrap_or(1);
        let card_width = [CardWidth::Narrow, CardWidth::Standard, CardWidth::Wide].iter().position(|width| *width == s.card_width).unwrap_or(1);
        let startup = div().flex_1().flex().flex_col().gap(px(4.0))
            .child(title("STARTUP", 0.0))
            .child(row("icons/setup-power.svg", "Open at sign-in", login.into_any_element()))
            .child(row("icons/setup-wave.svg", "Start Live on open", switch("setup-live", s.start_live_on_launch, |s, v| s.start_live_on_launch = v, cx).into_any_element()))
            .child(row("icons/setup-refresh.svg", "Check for updates", switch("setup-updates", s.check_for_updates, |s, v| s.check_for_updates = v, cx).into_any_element()))
            .child(title("LOOK", 14.0))
            .child(opacity)
            .child(row("icons/setup-text-size.svg", "Text", sizes("setup-text", text_size,
                |s, v| s.text_size = [TextSize::Small, TextSize::Standard, TextSize::Large][v], cx)))
            .child(row("icons/setup-width.svg", "Card width", sizes("setup-width", card_width,
                |s, v| s.card_width = [CardWidth::Narrow, CardWidth::Standard, CardWidth::Wide][v], cx)));
        let toggle = self.shortcut_keys(&[Action::Toggle]).join(" ");
        page().child(heading("Make it yours", "Optional. Defaults are fine."))
            .child(div().flex().gap(px(28.0)).child(answers).child(startup))
            .child(hint("icons/setup-eye.svg", format!("The overlay is showing, so you can see each change. {toggle} hides it.")))
    }

    /// Add or remove CluelyRS from what starts at sign-in, then show what the registry now says.
    fn toggle_launch_at_login(&mut self, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        if let Err(error) = crate::platform::set_launch_at_login(!setup.launch_at_login) { eprintln!("could not change sign-in startup: {error}"); }
        setup.launch_at_login = crate::platform::launch_at_login();
        cx.notify();
    }

    /// What was chosen, a row per step (each goes back to it), and where to find setup again.
    fn summary(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let keys = |action| div().flex().gap(px(8.0)).children(self.shortcut_keys(&[action]).into_iter().map(|key| cap(key, false, theme::keycap_border(), rgb(LIGHT))));
        let said = |text| div().text_size(px(13.0)).line_height(px(18.0)).text_color(theme::muted()).child(text);
        let reminder = div().flex().items_center().gap(px(8.0))
            .child(keys(Action::Toggle)).child(said("shows the overlay"))
            .child(div().w(px(12.0)).flex_none())
            .child(keys(Action::Assist)).child(said("answers"));

        let model = self.model_choice().value;
        let (model_icon, model_color, model) = match s.provider {
            Provider::Codex => ("icons/logo-openai.svg", theme::body(), format!("ChatGPT plan · {model}")),
            Provider::Claude => ("icons/logo-claude.svg", rgb(0xd97757), format!("Claude plan · {model}")),
            Provider::ApiKey => ("icons/setup-key.svg", theme::body(), match crate::providers::preset(&s.api_provider) {
                Some(preset) => format!("{} · {model}", preset.label),
                None => "Your API key".to_string(),
            }),
        };
        let transcription = match (s.transcribe, s.stt_provider) {
            (false, _) => "Transcription off", (true, SttProvider::Parakeet) => "On this PC · Parakeet", (true, SttProvider::Deepgram) => "Deepgram",
        };
        let device = |name: &str, default: &str| if name.trim().is_empty() { default.to_string() } else { name.trim().to_string() };
        let kept = if !s.save_sessions { "not saved" } else { match s.keep_sessions { Retention::Days7 => "7 days", Retention::Days30 => "30 days", Retention::Forever => "kept" } };
        let privacy = format!("{} · {} · {kept}", if s.hide_from_capture { "Hidden" } else { "Shown when sharing" },
            if s.screen_on_send { "screen on send" } else { "no screenshots" });
        let taken: Vec<&str> = SHORTCUTS.iter().filter(|(_, _, actions)| self.shortcut_taken(actions)).map(|(_, label, _)| *label).collect();
        let note = |text: String, color: Rgba| status_text(text, color);
        let heard = match setup.heard { [true, true] => Some("Both heard"), [true, false] => Some("Mic heard"), [false, true] => Some("Desktop heard"), _ => None };
        let rows: [(Step, AnyElement, String, Option<AnyElement>); 5] = [
            (Step::AnswerModel, ui::icon(model_icon, 16.0, model_color).into_any_element(), model,
                Self::answer_tested(setup).then(|| note("Tested".into(), theme::ok()))),
            (Step::Transcription, ui::icon("icons/setup-wave.svg", 16.0, theme::body()).into_any_element(), transcription.to_string(),
                (s.transcribe && s.stt_provider == SttProvider::Parakeet).then(|| self.download_note()).flatten()),
            (Step::AudioCheck, ui::icon("icons/setup-mic.svg", 16.0, theme::body()).into_any_element(),
                format!("{} · {}", device(&s.mic_device, "Default mic"), device(&s.desktop_device, "Default speakers")),
                heard.map(|heard| note(heard.into(), theme::ok()))),
            (Step::Privacy, ui::icon("icons/setup-shield.svg", 16.0, theme::body()).into_any_element(), privacy, None),
            (Step::Shortcuts, ui::icon("icons/setup-keyboard.svg", 16.0, theme::body()).into_any_element(),
                format!("{} of {} shortcuts", SHORTCUTS.len() - taken.len(), SHORTCUTS.len()),
                taken.first().map(|label| note(format!("{label} taken"), rgb(CAUTION)))),
        ];
        let rows = rows.into_iter().map(|(step, icon, label, trailing)| {
            div().id(("setup-summary", step as usize)).flex().items_center().gap(px(12.0)).h(px(42.0)).cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.go_to(step, window, cx)))
                .child(icon)
                .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).text_color(theme::text()).child(label))
                .children(trailing)
        }).map(|row| div().child(row)).collect();
        page()
            .child(div().flex().flex_col().gap(px(10.0))
                .child(div().text_size(px(22.0)).line_height(px(28.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("You're all set"))
                .child(reminder))
            .child(boxed(rows).px(px(14.0)))
            .child(hint("icons/setup-refresh.svg", "Settings › About › Run setup again"))
    }
}

/// The speech model's download as a small bar and its megabytes, or that it isn't downloaded.
impl Overlay {
    fn download_note(&self) -> Option<AnyElement> {
        if let Some(progress) = self.download_progress() {
            return Some(div().flex().items_center().gap(px(10.0))
                .child(div().w(px(64.0)).h(px(4.0)).rounded_full().bg(theme::divider())
                    .child(div().h_full().rounded_full().bg(theme::accent()).w(relative(progress.clamp(0.0, 1.0) as f32))))
                .child(status_text(super::hearing::megabytes(progress), theme::muted()))
                .into_any_element());
        }
        (!self.model_installed).then(|| status_text("Not downloaded", theme::muted()))
    }
}

fn welcome() -> Div {
    let feature = |icon, label| div().flex_1().flex().flex_col().gap(px(12.0)).p(px(16.0)).rounded(px(12.0)).bg(rgb(0x15171a)).border_1().border_color(theme::hairline())
        .child(ui::icon(icon, 24.0, theme::accent_soft()))
        .child(div().text_size(px(14.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(label));
    let column = |icon, color, title, items: [(&'static str, &'static str); 3]| div().flex_1().flex().flex_col().gap(px(10.0))
        .child(div().flex().items_center().gap(px(8.0)).pb(px(2.0)).child(ui::icon(icon, 14.0, color)).child(ui::section_label(title)))
        .children(items.map(|(icon, label)| div().flex().items_center().gap(px(10.0))
            .child(ui::icon(icon, 16.0, theme::body()))
            .child(div().text_size(px(13.0)).line_height(px(16.0)).text_color(rgb(LIGHT)).child(label))));
    page()
        .child(div().flex().flex_col().gap(px(8.0)).pt(px(8.0))
            .child(div().text_size(px(28.0)).line_height(px(34.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("Welcome to CluelyRS"))
            .child(div().text_size(px(13.0)).line_height(px(20.0)).text_color(theme::body()).child("Answers about your call, in a card only you can see.")))
        .child(div().flex().gap(px(12.0))
            .child(feature("icons/setup-wave.svg", "Hears the call"))
            .child(feature("icons/image.svg", "Sees your screen"))
            .child(feature("icons/eye-off.svg", "Hidden from sharing")))
        .child(div().flex().gap(px(28.0)).pt(px(4.0))
            .child(column("icons/setup-monitor.svg", theme::ok(), "STAYS ON THIS PC",
                [("icons/setup-wave.svg", "Transcription"), ("icons/setup-history.svg", "Saved sessions"), ("icons/setup-key.svg", "API keys · Credential Manager")]))
            .child(column("icons/setup-arrow.svg", theme::accent_soft(), "SENT TO YOUR MODEL",
                [("icons/setup-chat.svg", "Questions and transcript"), ("icons/image.svg", "A screenshot, if on"), ("icons/setup-cloud.svg", "Audio, only with Deepgram")])))
}
