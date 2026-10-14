//! Windows Settings › Modes (Paper "Settings · Modes" and "Settings · Modes › Coding Interview"):
//! a list page with one card per mode by group, and each mode's own page under a breadcrumb — name
//! and icon, group, whether it's active, its context and files, and a ⋯ flyout. The state and every
//! action are the Modes page's own (`ModesUi`); only the drawing is Fluent (`settings_view::fluent`).

use gpui::{
    AnyElement, Context, Div, ExternalPaths, Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    Stateful, Styled, Window, deferred, div, prelude::*, px, rgb,
};

use super::{DANGER, Popover, on_click, tokens_label};
use crate::hotkeys::Action;
use crate::modes::{self, FileError, Mode, extract};
use crate::overlay::Overlay;
use crate::settings_view::Picker;
use crate::settings_view::fluent::{
    button, card, card_icon, card_label, cards_column, chevron_right, combo_face, flyout, flyout_item, in_use, keycaps, pill, section, status, text_box,
};
use crate::{theme, ui};

impl Overlay {
    /// Modes: the list, or one mode's page.
    pub(crate) fn fluent_modes(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        match self.fluent.mode_page.as_deref().and_then(|id| self.modes.get(id)) {
            Some(mode) => self.fluent_mode_page(mode, window, cx),
            None => self.fluent_mode_list(cx),
        }
    }

    /// Open `id`'s page from the list.
    fn open_mode_page(&mut self, id: &str, cx: &mut Context<Self>) {
        self.select_mode(id, cx);
        self.fluent.mode_page = Some(self.modes_ui.selected.clone());
        self.fluent.scroll_to_top();
        cx.notify();
    }

    fn fluent_mode_list(&self, cx: &mut Context<Self>) -> Div {
        let new_mode = div().id("new-mode").flex().flex_none().items_center().gap(px(8.0)).h(px(32.0)).px(px(12.0)).rounded(px(4.0)).cursor_pointer()
            .bg(theme::accent()).text_size(px(13.0)).line_height(px(16.0)).text_color(theme::accent_ink())
            .child(ui::icon("icons/plus.svg", 12.0, theme::accent_ink())).child("New mode")
            .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.name.focus_handle(cx)), |this, cx| {
                this.new_mode(cx);
                let id = this.modes_ui.selected.clone();
                this.fluent.mode_page = Some(id);
                this.fluent.scroll_to_top();
            }));
        let header = div().flex().items_center().gap(px(8.0))
            .child(div().flex_1().flex().items_center().gap(px(8.0))
                .child(div().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::body()).child("Switch mode"))
                .child(keycaps(&self.hotkeys.label(Action::SwitchMode))))
            .child(new_mode);
        let active = self.modes.active().id.clone();
        let mut page = div().flex().flex_col().gap(px(24.0)).child(crate::settings_view::fluent::title("Modes")).child(header);
        for (group_index, (group, members)) in self.modes.sections().into_iter().enumerate() {
            let cards = members.into_iter().enumerate().map(|(index, mode)| self.fluent_mode_card(mode, mode.id == active, group_index * 1000 + index, cx));
            page = page.child(section(group.unwrap_or("Built in").to_string(), cards));
        }
        page
    }

    /// A mode in the list: its icon, name and files, ✓ Active, and › to its page.
    fn fluent_mode_card(&self, mode: &Mode, active: bool, key: usize, cx: &mut Context<Self>) -> AnyElement {
        let id = mode.id.clone();
        let files = match mode.files.as_slice() {
            [] => None,
            [file] => Some(format!("{} · {} tokens", file.name, tokens_label(file.tokens))),
            many => Some(format!("{} files · {} tokens", many.len(), tokens_label(mode.file_tokens()))),
        };
        let card = card(("mode-card", key)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.focus.clone()), move |this, cx| this.open_mode_page(&id, cx)))
            .child(ui::icon(mode.icon.path(), 20.0, if active { theme::accent_soft() } else { theme::body() }))
            .child(card_label(mode.name.clone(), files.map(|files| status(files).into_any_element())))
            .when(active, |card| card.child(pill("Active")))
            .child(chevron_right(12.0, theme::body()));
        if active { in_use(card).into_any_element() } else { card.hover(|card| card.bg(theme::raised())).into_any_element() }
    }

    fn fluent_mode_page(&self, mode: &Mode, window: &Window, cx: &mut Context<Self>) -> Div {
        let menu_open = self.modes_ui.popover == Some(Popover::Menu);
        let back = div().id("modes-crumb").cursor_pointer().text_color(theme::muted()).hover(|crumb| crumb.text_color(theme::body()))
            .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| {
                this.flush_context(cx);
                this.modes_ui.popover = None;
                this.modes_ui.renaming = false;
                this.fluent.mode_page = None;
                this.fluent.scroll_to_top();
                cx.notify();
            }))
            .child("Modes");
        let more = div().id("mode-more").size(px(32.0)).flex_none().flex().items_center().justify_center().rounded(px(4.0)).cursor_pointer()
            .bg(theme::fluent_selected()).border_1().border_color(if menu_open { theme::accent() } else { theme::keycap_border() })
            .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| {
                this.modes_ui.popover = if this.modes_ui.popover == Some(Popover::Menu) { None } else { Some(Popover::Menu) };
                cx.notify();
            }))
            .child(ui::icon("icons/more.svg", 14.0, theme::body()));
        let breadcrumb = div().flex().items_center().gap(px(12.0)).text_size(px(28.0)).line_height(px(36.0)).font_weight(FontWeight::SEMIBOLD)
            .child(back)
            .child(chevron_right(16.0, theme::muted()))
            .child(div().flex_1().min_w_0().truncate().text_color(theme::text()).child(mode.name.clone()))
            .child(more);
        let active = self.modes.active().id == mode.id;
        let id = mode.id.clone();
        let active_card = card("mode-active").child(ui::icon("icons/setup-circle-check.svg", 20.0, if active { theme::accent_soft() } else { theme::body() }))
            .child(card_label("Active mode", None))
            .child(if active { pill("Active").into_any_element() } else {
                button("set-active", "Set active", true).on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| this.activate_mode(&id, cx))).into_any_element()
            });
        let cards = vec![
            self.fluent_name_card(mode, window, cx),
            self.fluent_group_card(mode, cx),
            if active { in_use(active_card).into_any_element() } else { active_card.into_any_element() },
        ];
        let focus = self.modes_ui.focus.clone();
        let context = self.modes_ui.context.clone();
        let context_focus = context.clone();
        let context_box = div().id("mode-context").w_full().min_w_0().overflow_hidden().min_h(px(120.0)).px(px(12.0)).py(px(10.0)).rounded(px(4.0)).bg(theme::raised())
            .border_1().border_color(theme::hairline()).cursor_text().text_size(px(13.0)).line_height(px(20.0)).text_color(theme::body())
            .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&context_focus.focus_handle(cx)))
            .child(context);
        div().flex().flex_col().gap(px(24.0)).track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.modes_key(event, window, cx)))
            .child(breadcrumb)
            .child(cards_column(cards))
            .child(section("Context", vec![context_box.into_any_element()]))
            .child(self.fluent_files(mode, cx))
    }

    /// The icon picker (the user's modes) and the name: a TextBox while renaming, else the name.
    fn fluent_name_card(&self, mode: &Mode, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let custom = mode.builtin.is_none();
        let renaming = self.modes_ui.renaming && custom;
        let look_open = self.modes_ui.popover == Some(Popover::Look);
        let mut picker = div().id("mode-icon-picker").relative().size(px(36.0)).flex_none().flex().items_center().justify_center().rounded(px(4.0))
            .bg(theme::bubble()).border_1().border_color(theme::bubble_border())
            .child(ui::icon(mode.icon.path(), 20.0, theme::accent_soft()));
        if custom {
            picker = picker.cursor_pointer().on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| {
                this.modes_ui.popover = if this.modes_ui.popover == Some(Popover::Look) { None } else { Some(Popover::Look) };
                cx.notify();
            }));
        }
        if look_open {
            picker = picker.child(deferred(div().absolute().top(px(40.0)).left_0().child(self.look_picker(mode, cx))).with_priority(1));
        }
        let name: Stateful<Div> = if renaming {
            text_box("mode-name", &self.modes_ui.name, window, cx)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" { cx.stop_propagation(); this.cancel_rename(cx); }
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.commit_rename(cx)))
        } else {
            let field = div().id("mode-name").flex().items_center().h(px(32.0)).px(px(11.0)).rounded(px(4.0)).bg(theme::raised())
                .border_1().border_color(theme::hairline()).text_size(px(13.0)).line_height(px(16.0))
                .child(div().min_w_0().truncate().child(mode.name.clone()));
            if custom {
                field.cursor_text().text_color(theme::text())
                    .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.name.focus_handle(cx)), |this, cx| this.start_rename(cx)))
            } else {
                // Built-ins keep their names.
                field.text_color(theme::muted())
            }
        };
        card("mode-name-card").child(picker).child(card_label("Name and icon", None)).child(name.w(px(240.0))).into_any_element()
    }

    /// The mode's group: a ComboBox of the groups in use, with a field for a new one.
    fn fluent_group_card(&self, mode: &Mode, cx: &mut Context<Self>) -> AnyElement {
        let custom = mode.builtin.is_none();
        let current = mode.group.clone().unwrap_or(if custom { modes::YOUR_MODES.into() } else { "Built in".into() });
        let open = custom && self.open_picker == Some(Picker::ModeGroup);
        let mut face = combo_face("mode-group", None, current.clone().into(), 240.0);
        face = if custom {
            face.on_mouse_down(MouseButton::Left, crate::settings_view::listen(cx, |this, _, _, cx| { cx.stop_propagation(); this.toggle_picker(Picker::ModeGroup, cx) }))
        } else { face.opacity(0.5).cursor_default() };
        let face_bounds = self.picker_face.clone();
        if open {
            face = face.child(gpui::canvas(move |bounds, _, _| face_bounds.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        }
        let mut wrapper = div().relative().flex_none().child(face);
        if open {
            let face_bounds = self.picker_face.clone();
            let mut list = flyout().id("mode-group-list").w(px(240.0))
                .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if face_bounds.get().is_some_and(|face| face.contains(&event.position)) { return; }
                    this.close_picker(cx);
                }));
            for (index, group) in self.modes.groups().into_iter().enumerate() {
                let name = group.clone();
                list = list.child(flyout_item(("mode-group-option", index), group == current, true)
                    .on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| { this.close_picker(cx); this.set_group(&name, cx); }))
                    .child(div().flex_1().min_w_0().truncate().child(group)));
            }
            let new_group = self.modes_ui.new_group.clone();
            let focus = new_group.clone();
            list = list.child(div().h(px(1.0)).bg(theme::hairline()))
                .child(div().id("mode-new-group").flex().items_center().gap(px(8.0)).h(px(32.0)).px(px(10.0)).rounded(px(4.0)).cursor_text()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| { cx.stop_propagation(); window.focus(&focus.focus_handle(cx)) })
                    .child(ui::icon("icons/plus.svg", 10.0, theme::accent_soft()))
                    .child(div().flex_1().min_w_0().text_size(px(13.0)).child(new_group)));
            wrapper = wrapper.child(deferred(div().absolute().top(px(36.0)).left_0().child(list)).with_priority(1));
        }
        card("mode-group-card").child(card_icon("icons/mode-folder.svg")).child(card_label("Group", None)).child(wrapper).into_any_element()
    }

    /// Files: the budget meter, a card per file (and per file being read or refused), and the drop zone.
    fn fluent_files(&self, mode: &Mode, cx: &mut Context<Self>) -> Div {
        let meter = modes::meter(mode, modes::file_budget(&self.store.value));
        let over = meter.used > meter.budget;
        let mut header = div().flex().items_center().gap(px(12.0))
            .child(div().flex_1().text_size(px(14.0)).line_height(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Files"));
        header = if mode.files.is_empty() { header.child(status("PDF, DOCX, TXT, MD · up to 5 files")) } else {
            let fill = (meter.used as f32 / meter.budget.max(1) as f32).min(1.0);
            header.child(div().w(px(96.0)).h(px(4.0)).rounded(px(2.0)).bg(theme::keycap_border())
                    .child(div().h_full().rounded(px(2.0)).w(px(96.0 * fill)).bg(theme::accent())))
                .child(status(format!("{} of {} tokens", tokens_label(meter.used), tokens_label(meter.budget))))
        };
        let mut cards: Vec<AnyElement> = Vec::new();
        if let (true, Some(cut)) = (over, meter.cut) {
            cards.push(div().px(px(16.0)).py(px(8.0)).text_size(px(12.0)).line_height(px(16.0)).text_color(theme::body())
                .child(format!("The end of {cut} is left out with {}. Remove a file or switch to a model with more room.", super::provider_name(&self.store.value)))
                .into_any_element());
        }
        let close = |id: (&'static str, usize)| div().id(id).flex_none().size(px(24.0)).flex().items_center().justify_center().rounded(px(4.0)).cursor_pointer()
            .hover(|close| close.bg(theme::fluent_selected())).child(ui::icon("icons/close.svg", 12.0, theme::muted()));
        let file_card = |id: (&'static str, usize)| card(id).min_h(px(52.0)).child(card_icon("icons/file-drop.svg"));
        for (index, file) in mode.files.iter().enumerate() {
            let file_id = file.id.clone();
            cards.push(file_card(("mode-file", index)).child(card_label(file.name.clone(), None))
                .child(status(format!("{} tokens", tokens_label(file.tokens))))
                .child(close(("mode-file-remove", index)).on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| this.remove_file(&file_id, cx))))
                .into_any_element());
        }
        for (index, (_, owner, name)) in self.modes_ui.reading.iter().enumerate().filter(|(_, (_, owner, _))| *owner == mode.id) {
            let _ = owner;
            cards.push(file_card(("mode-file-reading", index)).child(card_label(name.clone(), Some(status("Reading…").into_any_element()))).into_any_element());
        }
        for (index, notice) in self.modes_ui.notices.iter().enumerate() {
            let id = notice.id;
            cards.push(file_card(("mode-file-notice", index))
                .child(card_label(notice.name.clone(), Some(div().text_size(px(12.0)).line_height(px(16.0)).text_color(rgb(DANGER)).child(notice.error.to_string()).into_any_element())))
                .child(close(("mode-file-dismiss", index)).on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| {
                    this.modes_ui.notices.retain(|notice| notice.id != id);
                    cx.notify();
                })))
                .into_any_element());
        }
        let full = mode.files.len() >= extract::MAX_FILES;
        let zone = div().id("mode-drop").flex().items_center().justify_center().gap(px(10.0)).h(px(64.0)).rounded(px(6.0))
            .border_1().border_dashed().border_color(theme::keycap_border()).text_size(px(13.0)).line_height(px(18.0)).text_color(theme::muted());
        let zone = if full { zone.child(FileError::LimitReached.to_string()) } else {
            let (id, this) = (mode.id.clone(), cx.weak_entity());
            zone.cursor_pointer()
                .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(theme::bubble()).border_color(theme::accent()))
                .on_drop(move |paths: &ExternalPaths, _, cx| {
                    let (id, paths) = (id.clone(), paths.paths().to_vec());
                    this.update(cx, |this, cx| this.add_files(&id, paths, cx)).ok();
                })
                .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| this.browse_files(cx)))
                .child(ui::icon("icons/setup-download.svg", 18.0, theme::muted()))
                .child(div().flex().gap(px(4.0)).child("Drop files here or").child(div().text_color(theme::accent_soft()).child("Browse…")))
        };
        cards.push(zone.into_any_element());
        div().flex().flex_col().gap(px(8.0)).child(header).child(cards_column(cards))
    }

    /// The ⋯ flyout (Rename, Duplicate, Reset to built-in, Delete) and the delete confirmation, over
    /// the Settings window's page.
    pub(crate) fn fluent_mode_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.fluent.mode_page.as_ref()?;
        let mode = self.modes.get(&self.modes_ui.selected)?;
        match self.modes_ui.popover {
            Some(Popover::ConfirmDelete) => Some(self.delete_confirmation(mode, cx).into_any_element()),
            Some(Popover::Menu) => {
                let custom = mode.builtin.is_none();
                let item = |id: &'static str, label: &'static str, enabled: bool| flyout_item(id, false, enabled).child(label);
                let mut menu = flyout().id("mode-menu").w(px(200.0))
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.modes_ui.popover = None; cx.notify(); }))
                    .child(item("mode-menu-rename", "Rename", custom).when(custom, |row| row
                        .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.name.focus_handle(cx)), |this, cx| this.start_rename(cx)))))
                    .child(item("mode-menu-duplicate", "Duplicate", true)
                        .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| {
                            this.duplicate_mode(cx);
                            this.fluent.mode_page = Some(this.modes_ui.selected.clone());
                            this.fluent.scroll_to_top();
                        })));
                let resettable = mode.is_edited();
                menu = menu.child(item("mode-menu-reset", "Reset to built-in", resettable).when(resettable, |row| row
                        .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = None; this.reset_prompt(cx); }))))
                    .child(div().h(px(1.0)).flex_none().bg(theme::hairline()))
                    .child(item("mode-menu-delete", "Delete", custom).when(custom, |row| row
                        .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = Some(Popover::ConfirmDelete); cx.notify(); }))));
                Some(div().absolute().top(px(62.0)).right(px(32.0)).child(menu).into_any_element())
            }
            _ => None,
        }
    }

    /// Close the ⋯ flyout, the icon picker or the delete confirmation (Esc, the window closing).
    /// Whether one was open.
    pub(crate) fn close_mode_menus(&mut self, cx: &mut Context<Self>) -> bool {
        if self.modes_ui.popover.take().is_none() { return false; }
        cx.notify();
        true
    }

}
