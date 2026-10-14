//! Welcome, and the pieces the setup pages share: the heading, panels and tiles, the small buttons,
//! the dropdown lists, and the row of shortcuts under Start. Answer model is in `answer`, Listening in
//! `hearing`.

use gpui::{
    AnyElement, BoxShadow, Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Stateful, Styled, Window, deferred, div, point,
    prelude::*, px,
};

use super::{Menu, Setup, Step};
use crate::hotkeys::Action;
use crate::overlay::Overlay;
use crate::settings_view::fluent::{app_tile, flyout, flyout_item};
use crate::settings_view::listen;
use crate::{theme, ui};

/// A step's headline (34/40 700) and the one line under it.
pub(super) fn heading(title: &'static str, detail: &'static str) -> AnyElement {
    div().flex().flex_col().items_center().gap(px(10.0))
        .child(div().text_size(px(34.0)).line_height(px(40.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(title))
        .child(div().text_size(px(15.0)).line_height(px(22.0)).text_color(theme::muted()).child(detail))
        .into_any_element()
}

/// A panel or an unselected card: #131416 with a divider edge.
pub(super) fn panel() -> Div { div().rounded(px(6.0)).bg(theme::setup_panel()).border_1().border_color(theme::divider()) }

/// An icon's square tile; the selected card's is blue.
pub(super) fn tile(size: f32, selected: bool) -> Div {
    let tile = div().size(px(size)).flex_none().flex().items_center().justify_center().rounded(px(6.0)).border_1();
    if selected { tile.bg(theme::bubble()).border_color(theme::bubble_border()) } else { tile.bg(theme::raised()).border_color(theme::hairline()) }
}

/// The 18 px accent check in a selected card's top right corner.
pub(super) fn check_badge() -> Div {
    div().absolute().top(px(10.0)).right(px(10.0)).size(px(18.0)).flex().items_center().justify_center().rounded(px(9.0)).bg(theme::accent())
        .child(ui::icon("icons/check.svg", 11.0, theme::accent_ink()))
}

/// A 28 px button inside a panel ("Privacy settings ↗", "Sign in"): secondary or primary.
pub(super) fn small_button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    let button = div().id(id).flex_none().h(px(28.0)).px(px(10.0)).flex().items_center().gap(px(6.0)).rounded(px(4.0)).cursor_pointer()
        .text_size(px(12.0)).line_height(px(16.0)).child(label.into());
    if primary { button.bg(theme::accent()).text_color(theme::accent_ink()).font_weight(FontWeight::SEMIBOLD) }
    else { button.bg(theme::raised()).border_1().border_color(theme::keycap_border()).text_color(theme::text()).hover(|button| button.bg(theme::fluent_selected())) }
}

/// A quiet text action ("Sign out", "Cancel", "Locate…").
pub(super) fn link(id: &'static str, label: &'static str) -> Stateful<Div> {
    div().id(id).flex_none().cursor_pointer().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted())
        .hover(|link| link.text_color(theme::body())).child(label)
}

/// The turning spinner, while something is being checked or waited for.
pub(super) fn spinner(id: &'static str) -> impl IntoElement {
    use gpui::{Animation, AnimationExt, Transformation, percentage};
    gpui::svg().path("icons/spinner.svg").size(px(16.0)).flex_none().text_color(theme::accent_soft())
        .with_animation(id, Animation::new(std::time::Duration::from_millis(900)).repeat(),
            |spinner, delta| spinner.with_transformation(Transformation::rotate(percentage(delta))))
}

/// One key under Start: 20 tall, words 11 px, ↵ a little larger and Space a little smaller, as drawn.
fn key(key: &str) -> Div {
    let size = match key { "↵" => 12.0, "Space" => 10.0, _ => 11.0 };
    div().h(px(20.0)).min_w(px(20.0)).px(px(if key == "Space" { 6.0 } else { 5.0 })).flex().flex_none().items_center().justify_center()
        .rounded(px(theme::radius::KEYCAP)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
        .text_size(px(size)).line_height(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child(SharedString::from(key.to_string()))
}

impl Overlay {
    pub(super) fn setup_step(&self, setup: &Setup, window: &Window, cx: &mut Context<Self>) -> Div {
        match setup.step {
            Step::Welcome => self.welcome(cx),
            Step::AnswerModel => self.answer_model(setup, window, cx),
            Step::Listening => self.listening(setup, window, cx),
        }
    }

    fn welcome(&self, cx: &mut Context<Self>) -> Div {
        let drop = BoxShadow { color: gpui::rgba(0x00000080).into(), offset: point(px(0.0), px(16.0)), blur_radius: px(40.0), spread_radius: px(0.0) };
        // The 8 px halo is a ring of its own: a spread shadow without blur doesn't draw.
        let hero = div().p(px(8.0)).rounded(px(24.0)).bg(theme::setup_halo()).child(app_tile(104.0, 16.0, 68.0).shadow(vec![drop]));
        let hidden = div().flex().items_center().gap(px(7.0)).px(px(12.0)).py(px(6.0)).rounded_full().bg(theme::selected_row()).border_1().border_color(theme::bubble_border())
            .child(ui::icon("icons/eye-off.svg", 14.0, theme::accent_soft()))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child("Hidden from screen sharing by default"));
        let actions = div().flex().flex_col().items_center().gap(px(14.0))
            .child(self.primary("setup-next", "Get started", true, Some(Step::AnswerModel), cx))
            .child(div().text_size(px(11.0)).line_height(px(16.0)).text_color(theme::placeholder())
                .child("An independent open-source project. Not affiliated with or endorsed by Cluely."));
        Self::step_frame(28.0, 36.0, 28.0, vec![
            hero.into_any_element(),
            heading("Welcome to CluelyRS", "Live answers for your calls, in an overlay only you can see."),
            hidden.into_any_element(),
        ], actions)
    }

    /// The one-line row under Start: Assist, Type a question and Show / hide, as the hotkeys have them.
    pub(super) fn start_shortcuts(&self) -> Div {
        let group = |action: Action, what: &'static str| div().flex().items_center().gap(px(3.0))
            .children(self.hotkeys.label(action).split(' ').filter(|key| !key.is_empty()).map(key))
            .child(div().pl(px(4.0)).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::muted()).child(what));
        div().flex().items_center().gap(px(16.0))
            .child(group(Action::Assist, "Assist"))
            .child(group(Action::Focus, "Type"))
            .child(group(Action::Toggle, "Show / hide"))
    }

    /// The list of a dropdown while `menu` is open, `offset` below its anchor (or above it, where there
    /// is more room): each option with its logo, if `logo` maps ids to one. `pick` gets the chosen id.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn setup_menu(&self, setup: &Setup, menu: Menu, options: Vec<(String, String)>, logo: Option<fn(&str) -> &'static str>, selected: &str, offset: f32,
        above: bool, width: f32, pick: impl Fn(&mut Overlay, String, &mut Window, &mut Context<Overlay>) + Clone + 'static, cx: &mut Context<Self>) -> Option<AnyElement> {
        if setup.menu != Some(menu) { return None; }
        let mut list = flyout().id(("setup-list", menu as usize)).w(px(width)).max_h(px(240.0)).overflow_y_scroll()
            .on_mouse_down_out(listen(cx, |this, _, _, cx| this.set_menu(None, cx)));
        for (index, (id, label)) in options.into_iter().enumerate() {
            let (chosen, pick) = (id.clone(), pick.clone());
            list = list.child(flyout_item(("setup-option", index), id == selected, true)
                .when_some(logo, |row, logo| row.child(div().w(px(16.0)).flex_none().flex().justify_center().child(ui::icon(logo(&id), 14.0, theme::body()))))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.set_menu(None, cx);
                    pick(this, chosen.clone(), window, cx);
                })));
        }
        let anchor = if above { div().absolute().bottom(px(offset)) } else { div().absolute().top(px(offset)) };
        Some(deferred(anchor.left_0().child(list)).with_priority(1).into_any_element())
    }
}
