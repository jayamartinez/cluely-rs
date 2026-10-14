//! Settings › Shortcuts (Paper "Settings · Shortcuts"): the global shortcuts from `hotkeys`, grouped
//! Live and Overlay, each as its keycaps. They can't be changed yet, so the keys sit in a plain box
//! that hugs them; one another app already has shows "Used by another app".

use gpui::{AnyElement, Div, IntoElement, ParentElement, Styled, div, prelude::*, px};

use super::{card, card_icon, card_label, keycaps, section, status, title};
use crate::hotkeys::Action;
use crate::overlay::Overlay;
use crate::theme;

impl Overlay {
    pub(super) fn fluent_shortcuts(&self) -> Div {
        let label = |action| self.hotkeys.label(action);
        // The four move (and two scroll) shortcuts share their modifiers: one row each, with every arrow.
        let modifiers = |action| { let label = label(action); label.rsplit_once(' ').map_or(String::new(), |(keys, _)| keys.to_string()) };
        let moves = format!("{} ← ↑ ↓ →", modifiers(Action::MoveLeft));
        let scrolls = format!("{} ↑ ↓", modifiers(Action::ScrollUp));
        let row = |id: &'static str, icon: &'static str, name: &'static str, keys: String, actions: &[Action]| -> AnyElement {
            let taken = actions.iter().any(|action| self.hotkeys.unavailable.contains(action));
            card(id).child(card_icon(icon))
                .child(card_label(name, taken.then(|| status("Used by another app").into_any_element())))
                .child(div().flex_none().p(px(4.0)).rounded(px(4.0)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
                    .when(taken, |keys| keys.opacity(0.4))
                    .child(keycaps(&keys)))
                .into_any_element()
        };
        let live = vec![
            row("assist", "icons/setup-sparkle.svg", "Assist", label(Action::Assist), &[Action::Assist]),
            row("type", "icons/setup-field.svg", "Type a question", label(Action::Focus), &[Action::Focus]),
            row("switch-mode", "icons/setup-grid.svg", "Switch mode", label(Action::SwitchMode), &[Action::SwitchMode]),
        ];
        let overlay = vec![
            row("toggle", "icons/panel-bottom.svg", "Show or hide overlay", label(Action::Toggle), &[Action::Toggle]),
            row("move", "icons/setup-move.svg", "Move overlay", moves, &[Action::MoveUp, Action::MoveDown, Action::MoveLeft, Action::MoveRight]),
            row("scroll", "icons/setup-scroll.svg", "Scroll answer", scrolls, &[Action::ScrollUp, Action::ScrollDown]),
            row("close", "icons/close.svg", "Close settings", label(Action::Close), &[]),
        ];
        div().flex().flex_col().gap(px(24.0))
            .child(title("Shortcuts"))
            .child(section("Live", live))
            .child(section("Overlay", overlay))
    }
}
