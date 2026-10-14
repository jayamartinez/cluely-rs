//! macOS Settings › Modes, laid out like the Paper "Redesign v3" Modes artboard: the modes in a
//! sidebar by group (the active one checked), and the selected one's name, group, context and
//! files beside it, with Switch mode's keys and Set active in the footer. The state and every
//! action are `modes_view`'s; the Windows Modes window keeps its own layout.

use gpui::{AnyElement, Context, Div, ExternalPaths, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Stateful, Styled, deferred, div, prelude::*, px, relative};

use super::{FileError, Mode, Popover, extract, modes, on_click, tokens_label};
use crate::hotkeys::Action;
use crate::overlay::Overlay;
use crate::ui_v3::{self, Kind, RADII};
use crate::{theme, ui};

const SIDEBAR: f32 = 260.0;

impl Overlay {
    pub(super) fn mac_modes_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.modes.get(&self.modes_ui.selected).unwrap_or_else(|| self.modes.active());
        let focus = self.modes_ui.focus.clone();
        div().id("modes-tab").relative().flex().size_full().min_h_0()
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.modes_key(event, window, cx)))
            .child(self.mac_modes_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().flex().flex_col()
                .child(div().flex_1().min_h_0().flex().flex_col().gap(px(22.0)).px(px(28.0)).py(px(20.0))
                    .child(self.mac_mode_header(mode, cx))
                    .child(self.mac_mode_context())
                    .child(self.mac_mode_files(mode, cx)))
                .child(self.mac_mode_footer(mode, cx)))
            .when(self.modes_ui.popover == Some(Popover::ConfirmDelete), |tab| tab.child(self.delete_confirmation(mode, cx)))
            .into_any_element()
    }

    fn mac_modes_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let new_mode = div().id("new-mode").flex().items_center().gap(px(10.0)).px(px(8.0)).py(px(5.0)).mb(px(4.0)).rounded(px(RADII.card)).cursor_pointer()
            .bg(theme::raised()).border_1().border_color(theme::hairline())
            .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.name.focus_handle(cx)), |this, cx| this.new_mode(cx)))
            .child(div().size(px(28.0)).flex_none().flex().items_center().justify_center().rounded(px(RADII.small)).bg(theme::accent())
                .child(ui::icon("icons/plus.svg", 16.0, theme::accent_ink())))
            .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("New mode"));
        let mut list = div().id("modes-sidebar").size_full().track_scroll(&self.modes_ui.sidebar_scroll).flex().flex_col().gap(px(2.0)).px(px(12.0)).py(px(14.0)).overflow_y_scroll().child(new_mode);
        let active = self.modes.active().id.clone();
        for (index, (group, members)) in self.modes.sections().into_iter().enumerate() {
            if let Some(group) = group {
                list = list.child(div().flex().items_center().gap(px(8.0)).px(px(8.0)).pt(px(12.0)).pb(px(4.0))
                    .child(div().flex_none().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::placeholder())
                        .child(group.to_uppercase()))
                    .child(div().flex_1().h(px(1.0)).bg(theme::divider())));
            }
            for (position, mode) in members.into_iter().enumerate() {
                list = list.child(self.mac_mode_row(mode, mode.id == active, (index, position), cx));
            }
        }
        div().relative().w(px(SIDEBAR)).flex_none().h_full().bg(theme::panel()).border_r_1().border_color(theme::divider()).child(list)
            .children(ui_v3::scroll_thumb(&self.modes_ui.sidebar_scroll, 3.0))
    }

    fn mac_mode_row(&self, mode: &Mode, active: bool, key: (usize, usize), cx: &mut Context<Self>) -> impl IntoElement {
        let selected = mode.id == self.modes_ui.selected;
        let id = mode.id.clone();
        div().id(("mode-row", key.0 * 1000 + key.1)).flex().items_center().gap(px(10.0)).px(px(8.0)).py(px(5.0)).rounded(px(RADII.card)).cursor_pointer()
            .border_1().border_color(if selected { theme::bubble_border() } else { gpui::rgba(0x00000000) })
            .when(selected, |row| row.bg(theme::chosen()))
            .when(!selected, |row| row.hover(|row| row.bg(gpui::rgba(0xffffff08))))
            .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.focus.clone()), move |this, cx| this.select_mode(&id, cx)))
            .child(ui_v3::tile(28.0, selected).child(ui::icon(mode.icon.path(), 15.0, ui_v3::tile_ink(selected))))
            .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(18.0))
                .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .text_color(if selected { theme::text() } else { theme::body() })
                .child(mode.name.clone()))
            .when(active, |row| row.child(ui_v3::check_badge(12.0)))
    }

    /// The mode's tile, its name (a field: click to rename your own modes), its group and ⋯.
    fn mac_mode_header(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let custom = mode.builtin.is_none();
        let renaming = self.modes_ui.renaming && custom;
        let field = div().id("mode-name").flex_1().min_w_0().h(px(48.0)).flex().items_center().px(px(14.0)).rounded(px(RADII.control))
            .bg(theme::field()).border_1().border_color(if renaming { theme::accent() } else { theme::hairline() })
            .text_size(px(22.0)).line_height(px(28.0)).font_weight(FontWeight::BOLD).text_color(theme::text());
        let field = if renaming {
            field.on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" { cx.stop_propagation(); this.cancel_rename(cx); }
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.commit_rename(cx)))
                .child(div().flex_1().min_w_0().child(self.modes_ui.name.clone()))
        } else {
            field.when(custom, |field| field.cursor_text()
                    .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.name.focus_handle(cx)), |this, cx| this.start_rename(cx))))
                .child(div().truncate().child(mode.name.clone()))
        };
        // Built-ins outside a group (General) show none.
        let group = (custom || mode.group.is_some()).then(|| div().id("mode-group").flex().flex_none().items_center().gap(px(8.0)).pl(px(11.0)).pr(px(9.0)).py(px(5.0)).rounded(px(RADII.control))
            .bg(theme::field()).border_1().border_color(theme::hairline())
            .child(ui::icon("icons/mode-folder.svg", 13.0, theme::muted()))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::MEDIUM).text_color(theme::body())
                .child(mode.group.clone().unwrap_or(modes::YOUR_MODES.into())))
            // Your own modes move between groups; built-ins stay in theirs.
            .when(custom, |group| group.cursor_pointer().child(ui::icon("icons/chevron-down.svg", 12.0, theme::muted()))
                .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| {
                    this.modes_ui.popover = if this.modes_ui.popover == Some(Popover::Look) { None } else { Some(Popover::Look) };
                    cx.notify();
                }))));
        let more = div().id("mode-more").size(px(32.0)).flex_none().flex().items_center().justify_center().rounded_full().cursor_pointer()
            .bg(theme::selected_fill()).border_1().border_color(theme::keycap_border())
            .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| {
                this.modes_ui.popover = if this.modes_ui.popover == Some(Popover::Menu) { None } else { Some(Popover::Menu) };
                cx.notify();
            }))
            .child(ui::icon("icons/more.svg", 14.0, theme::body()));
        let popover: Option<AnyElement> = match self.modes_ui.popover {
            Some(Popover::Menu) => Some(self.mac_mode_menu(mode, cx).into_any_element()),
            Some(Popover::Look) => Some(self.look_picker(mode, cx).into_any_element()),
            _ => None,
        };
        div().relative().flex().items_center().gap(px(12.0))
            .child(ui_v3::tile(48.0, true).child(ui::icon(mode.icon.path(), 22.0, theme::accent_soft())))
            .child(field)
            .children(group)
            .child(more)
            .children(popover.map(|popover| deferred(div().absolute().top(px(54.0)).right_0().child(popover))))
    }

    fn mac_mode_menu(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |id: &'static str, text: &'static str, muted: bool| div().id(id).flex().items_center().px(px(9.0)).py(px(6.0)).rounded(px(RADII.small)).cursor_pointer()
            .text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::MEDIUM).text_color(if muted { theme::muted() } else { theme::text() })
            .hover(|row| row.bg(theme::selected_fill()))
            .child(text);
        let mut menu = crate::settings_view::mac_menu().id("mode-menu").w(px(196.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.modes_ui.popover = None; cx.notify(); }));
        if mode.builtin.is_none() {
            menu = menu.child(item("menu-rename", "Rename", false)
                .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.name.focus_handle(cx)), |this, cx| this.start_rename(cx))));
        }
        menu = menu.child(item("menu-duplicate", "Duplicate", false).on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| this.duplicate_mode(cx))));
        match mode.builtin {
            Some(_) => menu.child(item("menu-reset", "Reset to built-in", false).when(!mode.is_edited(), |row| row.opacity(0.45))
                .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = None; this.reset_prompt(cx); }))),
            None => menu
                .child(item("menu-look", "Change icon & group…", false)
                    .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = Some(Popover::Look); cx.notify(); })))
                .child(div().h(px(1.0)).my(px(3.0)).bg(theme::hairline()))
                .child(item("menu-delete", "Delete", true)
                    .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = Some(Popover::ConfirmDelete); cx.notify(); }))),
        }
    }

    /// The meeting context: fills the space the files leave, and scrolls.
    fn mac_mode_context(&self) -> impl IntoElement {
        div().flex_1().min_h(px(96.0)).flex().flex_col().gap(px(10.0))
            .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Context"))
            .child(div().id("mode-context").flex_1().min_h_0().overflow_y_scroll().px(px(14.0)).py(px(12.0)).rounded(px(RADII.control))
                .bg(theme::field()).border_1().border_color(theme::hairline()).cursor_text()
                .text_size(px(13.0)).line_height(px(20.0)).text_color(theme::body())
                .child(self.modes_ui.context.clone()))
    }

    fn mac_mode_files(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let meter = modes::meter(mode, modes::file_budget(&self.store.value));
        let over = meter.used > meter.budget;
        let mut header = div().flex().items_center().gap(px(12.0))
            .child(div().flex_1().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Files"));
        if !mode.files.is_empty() {
            let fill = (meter.used as f32 / meter.budget.max(1) as f32).min(1.0);
            header = header.child(div().flex().items_center().gap(px(8.0))
                .child(div().w(px(96.0)).h(px(4.0)).rounded(px(2.0)).bg(theme::divider())
                    .child(div().h_full().rounded(px(2.0)).w(relative(fill)).bg(if over { theme::danger() } else { theme::accent() })))
                .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(if over { theme::danger() } else { theme::muted() })
                    .child(format!("{} of {} tokens", tokens_label(meter.used), tokens_label(meter.budget)))));
        }
        let row = |id: (&'static str, usize)| div().id(id).flex().items_center().gap(px(10.0)).px(px(10.0)).py(px(8.0)).rounded(px(RADII.card))
            .bg(theme::field()).border_1().border_color(theme::hairline());
        let name = |text: String, color| div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::MEDIUM).text_color(color).child(text);
        let meta = |text: String, color| div().flex_none().text_size(px(12.0)).line_height(px(16.0)).text_color(color).child(text);
        let close = |id: (&'static str, usize)| div().id(id).flex_none().cursor_pointer().child(ui::icon("icons/close.svg", 14.0, theme::muted()));
        let mut list = div().flex().flex_col().gap(px(6.0));
        for (index, file) in mode.files.iter().enumerate() {
            let file_id = file.id.clone();
            list = list.child(row(("file", index))
                .child(ui_v3::tile(28.0, false).child(ui::icon("icons/mode-document.svg", 15.0, theme::body())))
                .child(name(file.name.clone(), theme::text()))
                .child(meta(format!("{} tokens", tokens_label(file.tokens)), theme::muted()))
                .child(close(("remove-file", index)).on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| this.remove_file(&file_id, cx)))));
        }
        for (index, (_, _, file)) in self.modes_ui.reading.iter().filter(|(_, owner, _)| *owner == mode.id).enumerate() {
            list = list.child(row(("reading", index))
                .child(ui_v3::tile(28.0, false).child(ui::icon("icons/mode-document.svg", 15.0, theme::muted())))
                .child(name(file.clone(), theme::text()))
                .child(meta("Reading…".into(), theme::accent_soft())));
        }
        for (index, notice) in self.modes_ui.notices.iter().enumerate() {
            let id = notice.id;
            list = list.child(row(("notice", index))
                .child(ui_v3::tile(28.0, false).child(ui::icon("icons/alert-circle.svg", 15.0, theme::danger())))
                .child(name(notice.name.clone(), theme::text()))
                .child(meta(notice.error.to_string(), theme::danger()))
                .child(close(("dismiss", index)).on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| {
                    this.modes_ui.notices.retain(|notice| notice.id != id);
                    cx.notify();
                }))));
        }
        let full = mode.files.len() >= extract::MAX_FILES;
        let zone = self.mac_drop_zone(mode, full, cx);
        div().flex_none().flex().flex_col().gap(px(10.0)).child(header).child(list).child(zone)
    }

    /// Where files are dropped, or browsed for.
    fn mac_drop_zone(&self, mode: &Mode, full: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let zone = div().id("files-drop").h(px(72.0)).flex_none().flex().items_center().justify_center().gap(px(10.0)).rounded(px(RADII.card))
            .border_1().border_dashed().border_color(theme::keycap_border());
        if full {
            return zone.child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::MEDIUM).text_color(theme::muted()).child(FileError::LimitReached.to_string()));
        }
        let id = mode.id.clone();
        let this = cx.weak_entity();
        zone.cursor_pointer()
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(theme::bubble()).border_color(theme::accent()))
            .on_drop(move |paths: &ExternalPaths, _, cx| {
                let (id, paths) = (id.clone(), paths.paths().to_vec());
                this.update(cx, |this, cx| this.add_files(&id, paths, cx)).ok();
            })
            .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| this.browse_files(cx)))
            .child(ui::icon("icons/setup-download.svg", 18.0, theme::muted()))
            .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::MEDIUM).text_color(theme::muted()).child("Drop files here or"))
            .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()).child("Browse…"))
    }

    /// Switch mode's keys, and Set active (Active once it is).
    fn mac_mode_footer(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let action: AnyElement = if self.modes.active().id == mode.id {
            div().flex().flex_none().items_center().gap(px(6.0)).pl(px(13.0)).pr(px(16.0)).py(px(7.0)).rounded(px(RADII.control)).bg(theme::accent())
                .child(ui::icon("icons/check.svg", 13.0, theme::accent_ink()))
                .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::BOLD).text_color(theme::accent_ink()).child("Active"))
                .into_any_element()
        } else {
            let id = mode.id.clone();
            ui_v3::button("set-active", "Set active", Kind::Secondary).py(px(7.0))
                .on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| this.activate_mode(&id, cx))).into_any_element()
        };
        div().flex().flex_none().items_center().gap(px(8.0)).pl(px(28.0)).pr(px(20.0)).py(px(12.0)).bg(theme::panel()).border_t_1().border_color(theme::divider())
            .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::MEDIUM).text_color(theme::muted()).child("Switch mode"))
            .child(ui_v3::keycaps(&self.hotkeys.label(Action::SwitchMode), true))
            .child(div().flex_1())
            .child(action)
    }
}
