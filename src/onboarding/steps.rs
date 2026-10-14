//! The pages of first-run setup and the pieces they share, from the Paper "Redesign v3 · Onboarding"
//! Windows artboards. Every control changes the setting it shows at once and saves it, as in
//! Settings. Answer model is in `answer`, Listening in `hearing`.

use gpui::{
    AnyElement, Context, Div, Entity, Focusable, IntoElement, MouseButton, ParentElement, SharedString, Stateful, Styled, Window, deferred, div,
    prelude::*, px,
};

use super::{Menu, Setup, Step};
use crate::input::TextInput;
use crate::overlay::Overlay;
use crate::settings_view::listen;
use crate::ui_v3::{self, Kind, RADII};
use crate::{theme, ui};

pub(super) fn text(size: f32, line: f32, color: gpui::Rgba, content: impl Into<SharedString>) -> Div {
    div().text_size(px(size)).line_height(px(line)).text_color(color).child(content.into())
}

/// A quiet text action ("Sign out", "Cancel").
pub(super) fn link(id: &'static str, label: &'static str) -> Stateful<Div> {
    div().id(id).flex_none().cursor_pointer().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted())
        .hover(|link| link.text_color(theme::body())).child(label)
}

/// A link that opens a web page ("How to install ↗").
pub(super) fn external(id: &'static str, label: &'static str, url: &'static str, cx: &Context<Overlay>) -> Stateful<Div> {
    div().id(id).flex_none().flex().items_center().gap(px(4.0)).cursor_pointer()
        .on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(url)))
        .child(text(12.0, 16.0, theme::accent_soft(), label))
        .child(ui::icon("icons/setup-external.svg", 9.0, theme::accent_soft()))
}

impl Overlay {
    pub(super) fn setup_step(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        match setup.step {
            Step::Welcome => welcome(cx),
            Step::AnswerModel => self.answer_model(setup, cx),
            Step::Transcription => self.listening(setup, cx),
        }
    }

    /// The page's main button: on to the next step, or the end of setup after the last.
    pub(super) fn next_button(&self, step: Step, label: &'static str, kind: Kind, cx: &mut Context<Self>) -> Stateful<Div> {
        let button = ui_v3::big_button("setup-next", label, kind);
        if kind == Kind::Disabled { return button; }
        button.on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| match step.offset(1) {
            Some(next) => this.go_to(next, window, cx),
            None => this.close_setup(true, window, cx),
        }))
    }

    /// `face` opening a list of `options` `top` px below it, at least `width` wide. `pick` gets the
    /// chosen option's id; `logo` maps an id to the icon before its name.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn setup_dropdown(&self, setup: &Setup, menu: Menu, face: Stateful<Div>, width: f32, top: f32, options: Vec<(String, String)>,
        logo: Option<fn(&str) -> &'static str>, selected: &str, pick: impl Fn(&mut Overlay, String, &mut Window, &mut Context<Overlay>) + Clone + 'static,
        cx: &mut Context<Self>) -> Div {
        let open = setup.menu == Some(menu);
        let face = face.on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| this.set_menu(if open { None } else { Some(menu) }, cx)));
        let mut dropdown = div().relative().child(face);
        if open {
            let mut list = div().id(("setup-list", menu as usize)).flex().flex_col().gap(px(1.0)).p(px(5.0)).rounded(px(RADII.card)).bg(theme::raised())
                .border_1().border_color(theme::keycap_border()).shadow(ui_v3::drop_shadow(12.0, 32.0, 0.5)).w(px(width.max(220.0))).max_h(px(260.0)).overflow_y_scroll()
                .on_mouse_down_out(listen(cx, |this, _, _, cx| this.set_menu(None, cx)));
            for (index, (id, label)) in options.into_iter().enumerate() {
                let (chosen, pick) = (id.clone(), pick.clone());
                let selected = id == selected;
                list = list.child(div().id(("setup-option", index)).flex().items_center().gap(px(8.0)).px(px(9.0)).py(px(6.0)).rounded(px(RADII.small)).cursor_pointer()
                    .text_size(px(13.0)).line_height(px(18.0)).text_color(theme::text())
                    .hover(|row| row.bg(theme::selected_fill())).when(selected, |row| row.bg(theme::selected_fill()))
                    .when_some(logo, |row, logo| row.child(div().w(px(16.0)).flex_none().flex().justify_center().child(ui::icon(logo(&id), 14.0, theme::text()))))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when(selected, |row| row.child(ui::icon("icons/check.svg", 12.0, theme::accent())))
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.set_menu(None, cx);
                        pick(this, chosen.clone(), window, cx);
                    })));
            }
            dropdown = dropdown.child(deferred(div().absolute().top(px(top)).left_0().child(list)).with_priority(1));
        }
        dropdown
    }

    /// A text field on a setup page: an optional icon, the input, and a word or action on the right.
    /// Focused with a plain handler: `listen` would hand it the overlay's window, which can't take the
    /// setup window's keyboard focus.
    pub(super) fn text_field(&self, id: &'static str, input: Entity<TextInput>, icon: Option<&'static str>, trailing: Option<AnyElement>, height: f32) -> Stateful<Div> {
        let focus = input.clone();
        div().id(id).flex_1().min_w_0().h(px(height)).flex().items_center().gap(px(9.0)).px(px(12.0)).rounded(px(RADII.control)).cursor_text()
            .bg(theme::field()).border_1().border_color(theme::hairline())
            .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&focus.focus_handle(cx)))
            .when_some(icon, |field, icon| field.child(ui::icon(icon, 15.0, theme::placeholder())))
            .child(div().flex_1().min_w_0().text_size(px(12.0)).child(input))
            .children(trailing)
    }

    /// An API key: the saved one, masked, which a click replaces; or the box to paste one, saved with
    /// Enter or Save. A key the provider refused gets a red edge.
    pub(super) fn key_field(&self, setup: &Setup, saved: Option<&str>, refused: bool, height: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        match saved.filter(|_| !setup.replacing_key) {
            Some(hint) => div().id("setup-saved-key").flex_1().min_w_0().h(px(height)).flex().items_center().gap(px(9.0)).px(px(12.0)).rounded(px(RADII.control))
                .cursor_pointer().bg(theme::field()).border_1().border_color(if refused { theme::danger() } else { theme::hairline() })
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| if let Some(setup) = &mut this.setup { setup.replacing_key = true; cx.notify(); }))
                .child(text(12.0, 16.0, theme::text(), if hint.is_empty() { "••••••••".to_string() } else { hint.to_string() }).flex_1().min_w_0().truncate())
                .when(!refused, |field| field.child(ui::icon("icons/setup-circle-check.svg", 14.0, theme::ok())).child(text(12.0, 16.0, theme::ok(), "Saved"))),
            None => {
                let save = link("setup-save-key", "Save").text_color(theme::accent_soft())
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.save_key(window, cx)));
                self.text_field("setup-key", self.key_input.clone(), None, Some(save.into_any_element()), height)
            }
        }
    }
}

/// Welcome: the app, what it does, and Get started.
fn welcome(cx: &mut Context<Overlay>) -> Div {
    ui_v3::welcome(16.0, ui_v3::big_button("setup-next", "Get started", Kind::Primary)
        .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.go_to(Step::AnswerModel, window, cx))))
}
