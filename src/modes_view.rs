//! The Modes page, laid out like the Paper "Modes v2" and "Modes v3" artboards: the modes in a
//! sidebar by group, and the selected one's meeting context and files beside it, with a footer to
//! make it active. macOS shows it in the Settings window ("macOS · Settings window · Modes");
//! Windows in its own Modes window (`modes_window`, "Modes v3 · Windows · Modes window"), with a
//! short summary in the overlay's Settings panel.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Div, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, PathPromptOptions, Rgba, SharedString, Stateful, Styled, Task, Window, deferred, div,
    prelude::*, px, rgb, rgba,
};

use crate::input::{InputEvent, TextInput};
use crate::modes::{self, FileError, Icon, Mode, ModeStore, extract};
use crate::overlay::Overlay;
use crate::settings::{Provider, Settings};
use crate::text_area::TextArea;
use crate::{theme, ui};

/// Context edits are saved after typing pauses this long.
const SAVE_DELAY: Duration = Duration::from_millis(600);
const DANGER: u32 = 0xffb4a8;
const AMBER: u32 = 0xf2b95c;
const SELECTED_TILE: u32 = 0x2a2d31;
const MENU_HOVER: u32 = 0x2a2d31;

/// Sizes that differ between the Windows panel and the macOS window.
struct Look {
    sidebar: f32,
    sidebar_bg: u32,
    sidebar_pad: (f32, f32, f32),
    sidebar_gap: f32,
    row_selected: u32,
    detail_pad: (f32, f32, f32),
    detail_gap: f32,
    title: (f32, f32),
    /// The big icon tile beside the title (macOS).
    title_icon: f32,
    more: f32,
    field_text: (f32, f32),
    field_pad: (f32, f32),
    field_radius: f32,
    label_gap: f32,
    file_row_pad: (f32, f32),
    file_tile: f32,
    file_text: (f32, f32),
    footer_pad: (f32, f32, f32),
    footer_bg: u32,
    context_hint: &'static str,
}

/// The macOS Settings window and the Windows Modes window share one layout.
const LOOK: Look = Look {
    sidebar: 240.0, sidebar_bg: 0x121316, sidebar_pad: (12.0, 8.0, 12.0), sidebar_gap: 3.0, row_selected: 0x1f2124,
    detail_pad: (22.0, 28.0, 20.0), detail_gap: 20.0, title: (20.0, 24.0), title_icon: 40.0, more: 30.0,
    field_text: (13.0, 20.0), field_pad: (12.0, 14.0), field_radius: 10.0, label_gap: 7.0,
    file_row_pad: (9.0, 12.0), file_tile: 28.0, file_text: (13.0, 12.0),
    footer_pad: (12.0, 28.0, 20.0), footer_bg: 0x121316, context_hint: "Added to every answer while this mode is active",
};

const CUSTOM_PLACEHOLDER: &str = "Tell CluelyRS what this meeting is and how to answer. For example: \"Renewal call with Acme. I'm the account exec. Keep answers short, concrete and on price.\"";

/// Which popover is open over the tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Popover { Menu, Look, ConfirmDelete }

/// A file that couldn't be added, shown in the list until dismissed.
struct Notice { id: u64, name: String, error: FileError }

/// State of the Modes tab. The modes themselves live in `Overlay::modes`.
pub struct ModesUi {
    /// The mode shown on the right (not necessarily the active one).
    pub selected: String,
    popover: Option<Popover>,
    renaming: bool,
    notices: Vec<Notice>,
    /// Files being read: (id, mode id, name).
    reading: Vec<(u64, String, String)>,
    next_id: u64,
    pending_save: Option<Task<()>>,
    /// Takes the keyboard when a row is clicked, for the menu's shortcuts.
    focus: FocusHandle,
    context: Entity<TextArea>,
    name: Entity<TextInput>,
    new_group: Entity<TextInput>,
}

impl ModesUi {
    pub fn new(store: &ModeStore, window: &mut Window, cx: &mut Context<Overlay>) -> Self {
        let context = cx.new(|cx| TextArea::new(CUSTOM_PLACEHOLDER, cx).with_max_chars(modes::MAX_CONTEXT_CHARS));
        let name = cx.new(|cx| TextInput::new("Mode name", cx).with_text_size(LOOK.title.0, LOOK.title.1));
        let new_group = cx.new(|cx| TextInput::new("New group…", cx));
        cx.subscribe_in(&context, window, |this, _, event, _, cx| if matches!(event, InputEvent::Changed) { this.context_edited(cx) }).detach();
        cx.subscribe_in(&name, window, |this, _, event, _, cx| if matches!(event, InputEvent::Submit) { this.commit_rename(cx) }).detach();
        cx.subscribe_in(&new_group, window, |this, input, event, _, cx| {
            if !matches!(event, InputEvent::Submit) { return; }
            let group = input.read(cx).text().to_string();
            input.update(cx, |input, cx| input.clear(cx));
            this.set_group(&group, cx);
        }).detach();
        let selected = store.active().id.clone();
        let text = store.active().context.clone();
        context.update(cx, |area, cx| area.set_text(&text, cx));
        Self { selected, popover: None, renaming: false, notices: Vec::new(), reading: Vec::new(), next_id: 0, pending_save: None,
            focus: cx.focus_handle(), context, name, new_group }
    }
}

/// "7.2k", "30k", "950".
pub fn tokens_label(tokens: usize) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..10_000 => format!("{:.1}k", tokens as f64 / 1_000.0).replace(".0k", "k"),
        _ => format!("{}k", (tokens as f64 / 1_000.0).round()),
    }
}

/// "212 KB", "1.2 MB".
pub fn bytes_label(bytes: u64) -> String {
    if bytes < 1024 * 1024 { format!("{} KB", bytes.div_ceil(1024).max(1)) } else { format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)) }
}

/// "1.4" (MB, one decimal, for the "of 50 MB" counter).
fn megabytes(bytes: u64) -> String { format!("{:.1}", bytes as f64 / (1024.0 * 1024.0)) }

/// The selected model's provider as the budget note names it.
fn provider_name(settings: &Settings) -> String {
    match settings.provider {
        Provider::Codex => "ChatGPT".into(),
        Provider::Claude => "Claude".into(),
        Provider::ApiKey => crate::providers::preset(&settings.api_provider).map(|preset| preset.label.to_string()).unwrap_or("this model".into()),
    }
}

/// What the empty file area invites, by kind of mode.
fn drop_title(mode: &Mode) -> &'static str {
    match mode.builtin.map(|builtin| builtin.id) {
        Some("lecture") => "Drop a syllabus, slides or readings",
        Some("team-meeting") => "Drop plans, docs or notes",
        Some("customer") => "Drop product docs, FAQs or notes",
        Some("general") => "Drop notes or reference docs",
        Some(_) if mode.group.as_deref() == Some(modes::builtins::LOOKING_FOR_WORK) => "Drop a résumé, job post or notes",
        _ => "Drop a pricing sheet, brief or notes",
    }
}

/// The mode's kind and group under its name.
fn subtitle(mode: &Mode) -> String {
    match mode.builtin {
        Some(_) => {
            let mut text = "Built-in".to_string();
            if let Some(group) = &mode.group { text.push_str(&format!(" · {group}")); }
            if mode.is_edited() { text.push_str(" · edited"); }
            text
        }
        None => format!("Custom · {}", mode.group.as_deref().unwrap_or(modes::YOUR_MODES)),
    }
}

fn label(text: &'static str) -> Div {
    div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(text)
}

fn hint(text: impl Into<SharedString>) -> Div {
    div().text_size(px(11.0)).line_height(px(14.0)).text_color(theme::placeholder()).child(text.into())
}

fn mode_icon(icon: Icon, size: f32, color: Rgba) -> impl IntoElement { ui::icon(icon.path(), size, color) }

/// A small text tag in a file row's tile ("PDF", "MD").
fn file_tag(kind: modes::FileKind) -> impl IntoElement {
    let color = match kind { modes::FileKind::Pdf => rgb(DANGER), modes::FileKind::Docx => theme::accent_soft(), _ => theme::ok() };
    div().text_size(px(if kind == modes::FileKind::Docx { 7.5 } else { 9.0 })).line_height(px(12.0)).font_weight(FontWeight::BOLD).text_color(color).child(kind.tag())
}

fn tile(size: f32, bg: u32) -> Div {
    div().size(px(size)).flex_none().flex().items_center().justify_center().rounded(px(7.0)).bg(rgb(bg))
}

fn primary_button(id: &'static str, text: &'static str) -> Stateful<Div> {
    div().id(id).flex_none().px(px(12.0)).py(px(6.0)).rounded(px(8.0)).cursor_pointer().bg(theme::accent())
        .text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink()).child(text)
}

/// Run `f` on the overlay from a click in any window (the overlay, the macOS Settings window or the
/// Windows Modes window), giving the keyboard to `focus` in the window that was clicked.
fn on_click(cx: &Context<Overlay>, focus: Option<FocusHandle>, f: impl Fn(&mut Overlay, &mut Context<Overlay>) + 'static)
    -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static {
    let this = cx.weak_entity();
    move |_, window, cx| {
        cx.stop_propagation();
        if let Some(focus) = &focus { window.focus(focus); }
        this.update(cx, |this, cx| f(this, cx)).ok();
    }
}

impl Overlay {
    /// The whole tab: sidebar, detail, footer and any open popover.
    pub(crate) fn modes_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.modes.get(&self.modes_ui.selected).unwrap_or_else(|| self.modes.active());
        let focus = self.modes_ui.focus.clone();
        let mut tab = div().id("modes-tab").relative().flex().size_full().min_h_0()
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.modes_key(event, window, cx)))
            .child(self.modes_sidebar(cx))
            .child(self.mode_detail(mode, cx));
        if self.modes_ui.popover == Some(Popover::ConfirmDelete) { tab = tab.child(self.delete_confirmation(mode, cx)); }
        tab.into_any_element()
    }

    fn modes_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (top, x, bottom) = LOOK.sidebar_pad;
        let mut list = div().id("modes-sidebar").flex_none().w(px(LOOK.sidebar)).h_full().flex().flex_col().gap(px(LOOK.sidebar_gap))
            .pt(px(top)).px(px(x)).pb(px(bottom)).bg(rgb(LOOK.sidebar_bg)).border_r_1().border_color(theme::divider()).overflow_y_scroll();
        list = list.child(div().id("new-mode").flex().items_center().gap(px(8.0)).px(px(8.0)).py(px(6.0)).rounded(px(8.0)).cursor_pointer()
            .hover(|row| row.bg(rgb(LOOK.row_selected)))
            .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.name.focus_handle(cx)), |this, cx| this.new_mode(cx)))
            .child(div().size(px(22.0)).flex_none().flex().items_center().justify_center().rounded(px(6.0)).bg(theme::bubble())
                .border_1().border_color(theme::bubble_border()).child(ui::icon("icons/plus.svg", 10.0, theme::accent_soft())))
            .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()).child("New mode")));
        let active = self.modes.active().id.clone();
        for (index, (group, members)) in self.modes.sections().into_iter().enumerate() {
            if let Some(group) = group {
                list = list.child(div().flex().items_center().gap(px(8.0)).px(px(8.0)).pt(px(12.0)).pb(px(4.0))
                    .child(div().flex_none().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted())
                        .child(group.to_uppercase()))
                    .child(div().flex_1().h(px(1.0)).bg(theme::divider())));
            }
            for (position, mode) in members.into_iter().enumerate() {
                list = list.child(self.mode_row(mode, mode.id == active, (index, position), cx));
            }
        }
        list
    }

    fn mode_row(&self, mode: &Mode, active: bool, key: (usize, usize), cx: &mut Context<Self>) -> impl IntoElement {
        let selected = mode.id == self.modes_ui.selected;
        let id = mode.id.clone();
        let mut name = div().flex().flex_col().flex_1().min_w_0()
            .child(div().truncate().text_size(px(13.0)).line_height(px(16.0)).text_color(theme::text())
                .when(selected, |name| name.font_weight(FontWeight::SEMIBOLD)).child(mode.name.clone()));
        if mode.id == modes::GENERAL {
            name = name.child(div().text_size(px(11.0)).line_height(px(14.0)).text_color(theme::placeholder()).child("Default"));
        }
        let trailing = div().size(px(16.0)).flex_none().when(active, |slot| slot.flex().items_center().justify_center().rounded_full().bg(theme::accent())
            .child(ui::icon("icons/check.svg", 9.0, theme::accent_ink())));
        div().id(("mode-row", key.0 * 1000 + key.1)).flex().items_center().gap(px(8.0)).px(px(8.0)).py(px(6.0)).rounded(px(8.0)).cursor_pointer()
            .when(selected, |row| row.bg(rgb(LOOK.row_selected)))
            .when(!selected, |row| row.hover(|row| row.bg(rgba(0xffffff08))))
            .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.focus.clone()), move |this, cx| this.select_mode(&id, cx)))
            .child(div().size(px(22.0)).flex_none().flex().items_center().justify_center().rounded(px(6.0))
                .bg(rgb(if selected { SELECTED_TILE } else { 0x1b1d20 }))
                .child(mode_icon(mode.icon, 12.0, if selected { theme::text() } else { theme::body() })))
            .child(name)
            .child(trailing)
    }

    fn mode_detail(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let (top, x, bottom) = LOOK.detail_pad;
        let content = div().id("mode-detail").flex_1().min_h_0().overflow_y_scroll()
            .child(div().flex().flex_col().gap(px(LOOK.detail_gap)).pt(px(top)).px(px(x)).pb(px(bottom))
                .child(self.mode_header(mode, cx))
                .child(self.mode_context())
                .child(self.mode_files(mode, cx)));
        div().flex_1().min_w_0().h_full().flex().flex_col().child(content).child(self.mode_footer(mode, cx))
    }

    fn mode_header(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let renaming = self.modes_ui.renaming && mode.builtin.is_none();
        let title: AnyElement = if renaming {
            let name = self.modes_ui.name.clone();
            div().id("rename").flex().items_center().px(px(6.0)).py(px(2.0)).rounded(px(7.0)).bg(theme::field()).border_1().border_color(theme::bubble_border())
                .font_weight(FontWeight::BOLD)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" { cx.stop_propagation(); this.cancel_rename(cx); }
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.commit_rename(cx)))
                .child(name).into_any_element()
        } else {
            div().truncate().text_size(px(LOOK.title.0)).line_height(px(LOOK.title.1)).font_weight(FontWeight::BOLD).text_color(theme::text())
                .child(mode.name.clone()).into_any_element()
        };
        let mut sub = subtitle(mode);
        if renaming { sub.push_str(" · ↵ to keep, Esc to cancel"); }
        let mut row = div().flex().items_center().gap(px(12.0))
            .child(div().size(px(LOOK.title_icon)).flex_none().flex().items_center().justify_center().rounded(px(10.0)).bg(theme::raised())
                .border_1().border_color(theme::hairline()).child(mode_icon(mode.icon, 18.0, theme::text())))
            .child(div().flex().flex_col().gap(px(if renaming { 4.0 } else { 2.0 })).flex_1().min_w_0()
                .child(title)
                .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(sub)));
        if mode.is_edited() {
            row = row.child(div().id("reset-prompt").pr(px(6.0)).cursor_pointer().text_size(px(12.0)).text_color(theme::muted())
                .hover(|text| text.text_color(theme::body()))
                .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| this.reset_prompt(cx)))
                .child("Reset prompt"));
        }
        row.child(self.more_button(mode, cx))
    }

    /// The ⋯ button and, while open, its menu or the icon & group picker under it.
    fn more_button(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let open = matches!(self.modes_ui.popover, Some(Popover::Menu | Popover::Look));
        let button = div().id("mode-more").size(px(LOOK.more)).flex_none().flex().items_center().justify_center().rounded(px(8.0)).cursor_pointer()
            .border_1().border_color(if open { rgb(0x3a3e44) } else { theme::hairline() }).when(open, |button| button.bg(theme::raised()))
            .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| {
                this.modes_ui.popover = if this.modes_ui.popover == Some(Popover::Menu) { None } else { Some(Popover::Menu) };
                cx.notify();
            }))
            .child(ui::icon("icons/more.svg", 14.0, theme::body()));
        let mut wrapper = div().relative().flex_none().child(button);
        let popover = match self.modes_ui.popover {
            Some(Popover::Menu) => Some(self.mode_menu(mode, cx).into_any_element()),
            Some(Popover::Look) => Some(self.look_picker(mode, cx).into_any_element()),
            _ => None,
        };
        if let Some(popover) = popover {
            wrapper = wrapper.child(deferred(div().absolute().top(px(LOOK.more + 6.0)).right_0().child(popover)));
        }
        wrapper
    }

    fn mode_menu(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let mac = cfg!(target_os = "macos");
        let item = |id: &'static str, text: &'static str, keys: &'static str, danger: bool| {
            div().id(id).flex().items_center().justify_between().gap(px(12.0)).px(px(10.0)).py(px(7.0)).rounded(px(6.0)).cursor_pointer()
                .text_size(px(13.0)).line_height(px(16.0)).text_color(if danger { rgb(DANGER) } else { theme::text() })
                .hover(move |row| if mac { row.bg(theme::accent()).text_color(gpui::white()) } else { row.bg(rgb(MENU_HOVER)) })
                .child(text)
                .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::placeholder()).child(keys))
        };
        let mut menu = div().id("mode-menu").w(px(if mac { 200.0 } else { 176.0 })).flex().flex_col().p(px(4.0)).rounded(px(10.0))
            .bg(theme::raised()).border_1().border_color(theme::keycap_border())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.modes_ui.popover = None; cx.notify(); }));
        if mode.builtin.is_none() {
            menu = menu.child(item("menu-rename", "Rename", if mac { "↵" } else { "F2" }, false)
                .on_mouse_down(MouseButton::Left, on_click(cx, Some(self.modes_ui.name.focus_handle(cx)), |this, cx| this.start_rename(cx))));
        }
        menu = menu.child(item("menu-duplicate", "Duplicate", if mac { "⌘D" } else { "Ctrl D" }, false)
            .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| this.duplicate_mode(cx))));
        match mode.builtin {
            Some(_) => {
                menu = menu.child(item("menu-reset", if mac { "Reset Prompt" } else { "Reset prompt" }, "", false)
                    .when(!mode.is_edited(), |row| row.opacity(0.45))
                    .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = None; this.reset_prompt(cx); })));
            }
            None => {
                menu = menu
                    .child(item("menu-look", if mac { "Change Icon & Group…" } else { "Change icon & group" }, "", false)
                        .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = Some(Popover::Look); cx.notify(); })))
                    .child(div().h(px(1.0)).my(px(3.0)).bg(theme::hairline()))
                    .child(item("menu-delete", if mac { "Delete Mode…" } else { "Delete mode" }, if mac { "⌘⌫" } else { "Del" }, true)
                        .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = Some(Popover::ConfirmDelete); cx.notify(); })));
            }
        }
        menu
    }

    fn look_picker(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let mut grid = div().flex().flex_wrap().gap(px(4.0)).px(px(6.0)).pt(px(2.0)).pb(px(8.0));
        for (index, icon) in Icon::ALL.into_iter().enumerate() {
            let current = icon == mode.icon;
            grid = grid.child(div().id(("icon", index)).size(px(26.0)).flex().items_center().justify_center().rounded(px(7.0)).cursor_pointer()
                .bg(if current { theme::bubble() } else { rgb(0x232528) })
                .when(current, |tile| tile.border(px(1.5)).border_color(theme::accent()))
                .on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| this.set_icon(icon, cx)))
                .child(mode_icon(icon, 13.0, if current { theme::accent_soft() } else { theme::body() })));
        }
        let current_group = mode.group.clone().unwrap_or(modes::YOUR_MODES.into());
        let mut groups = div().flex().flex_col();
        for (index, group) in self.modes.groups().into_iter().enumerate() {
            let chosen = group == current_group;
            let name = group.clone();
            groups = groups.child(div().id(("group", index)).flex().items_center().justify_between().h(px(30.0)).px(px(10.0)).rounded(px(6.0))
                .cursor_pointer().text_size(px(13.0)).text_color(theme::text()).hover(|row| row.bg(rgb(MENU_HOVER)))
                .when(chosen, |row| row.bg(rgb(MENU_HOVER)))
                .on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| this.set_group(&name, cx)))
                .child(group)
                .when(chosen, |row| row.child(ui::icon("icons/check.svg", 14.0, theme::accent()))));
        }
        let new_group = self.modes_ui.new_group.clone();
        div().id("look-picker").w(px(258.0)).flex().flex_col().p(px(4.0)).rounded(px(10.0)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.modes_ui.popover = None; cx.notify(); }))
            .child(div().px(px(8.0)).pt(px(7.0)).pb(px(5.0)).child(label("ICON")))
            .child(grid)
            .child(div().h(px(1.0)).bg(theme::hairline()))
            .child(div().px(px(8.0)).pt(px(9.0)).pb(px(5.0)).child(label("GROUP")))
            .child(groups)
            .child(div().p(px(4.0)).child(div().id("new-group").flex().items_center().gap(px(6.0)).h(px(28.0)).px(px(8.0)).rounded(px(7.0))
                .bg(theme::field()).border_1().border_color(theme::hairline()).cursor_text()
                .child(ui::icon("icons/plus.svg", 10.0, theme::accent_soft()))
                .child(div().flex_1().min_w_0().child(new_group))))
    }

    fn mode_context(&self) -> impl IntoElement {
        let area = self.modes_ui.context.clone();
        let (pad_y, pad_x) = LOOK.field_pad;
        let field = div().id("mode-context").px(px(pad_x)).py(px(pad_y)).rounded(px(LOOK.field_radius)).bg(theme::field())
            .border_1().border_color(theme::hairline()).cursor_text()
            .text_size(px(LOOK.field_text.0)).line_height(px(LOOK.field_text.1)).text_color(theme::body())
            .child(area);
        div().flex().flex_col().gap(px(LOOK.label_gap))
            .child(div().flex().items_baseline().justify_between().child(label("MEETING CONTEXT")).child(hint(LOOK.context_hint)))
            .child(field)
    }

    fn mode_files(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let reading: Vec<&(u64, String, String)> = self.modes_ui.reading.iter().filter(|(_, owner, _)| *owner == mode.id).collect();
        let empty = mode.files.is_empty() && reading.is_empty() && self.modes_ui.notices.is_empty();
        let meter = modes::meter(mode, modes::file_budget(&self.store.value));
        let over = meter.used > meter.budget;
        let header_hint = if mode.files.is_empty() {
            hint("PDF, DOCX, TXT, MD · up to 5 files, 50 MB").into_any_element()
        } else {
            let text = format!("≈ {} of {} tokens", tokens_label(meter.used), tokens_label(meter.budget));
            if over { hint(text).font_weight(FontWeight::SEMIBOLD).text_color(rgb(AMBER)).into_any_element() } else { hint(text).into_any_element() }
        };
        let mut section = div().flex().flex_col().gap(px(8.0))
            .child(div().flex().items_baseline().justify_between().child(label("FILES")).child(header_hint));
        if !mode.files.is_empty() {
            let fill = (meter.used as f32 / meter.budget.max(1) as f32).min(1.0);
            section = section.child(div().h(px(4.0)).flex_none().rounded(px(2.0)).bg(theme::divider())
                .child(div().h_full().rounded(px(2.0)).w(gpui::relative(fill)).bg(if over { rgb(AMBER) } else { theme::accent() })));
        }
        if let (true, Some(cut)) = (over, meter.cut) {
            section = section.child(div().flex().gap(px(8.0)).px(px(10.0)).py(px(8.0)).rounded(px(8.0)).bg(rgb(0x221c10)).border_1().border_color(rgb(0x4a3a18))
                .child(div().pt(px(1.0)).child(ui::icon("icons/warning.svg", 13.0, rgb(AMBER))))
                .child(div().flex_1().min_w_0().text_size(px(11.5)).line_height(px(16.0)).text_color(rgb(0xe9d3a8))
                    .child(format!("The end of {cut} is left out with {}. Remove a file or switch to a model with more room.", provider_name(&self.store.value)))));
        }
        let mode_id = mode.id.clone();
        let drop_target = |zone: Stateful<Div>, cx: &mut Context<Self>| {
            let id = mode_id.clone();
            let this = cx.weak_entity();
            zone.drag_over::<ExternalPaths>(|style, _, _, _| style.bg(theme::bubble()).border_color(theme::accent()))
                .on_drop(move |paths: &ExternalPaths, _, cx| {
                    let (id, paths) = (id.clone(), paths.paths().to_vec());
                    this.update(cx, |this, cx| this.add_files(&id, paths, cx)).ok();
                })
        };
        if empty {
            let zone = div().id("files-drop").flex().items_center().gap(px(12.0)).p(px(14.0)).rounded(px(10.0)).border_1().border_dashed()
                .border_color(rgb(0x3a3e44)).cursor_pointer()
                .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| this.browse_files(cx)))
                .child(tile(32.0, 0x1b1d20).rounded(px(8.0)).child(ui::icon("icons/file-drop.svg", 16.0, theme::body())))
                .child(div().flex().flex_col().gap(px(2.0)).min_w_0()
                    .child(div().text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(drop_title(mode)))
                    .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child("or browse files · read as extra context")));
            return section.child(drop_target(zone, cx));
        }
        let (pad_y, pad_x) = LOOK.file_row_pad;
        let row = |id: (&'static str, usize)| div().id(id).flex().items_center().gap(px(10.0)).py(px(pad_y)).px(px(pad_x)).border_b_1().border_color(theme::divider());
        let text = |name: String, meta: String, meta_color: Rgba| div().flex().flex_col().flex_1().min_w_0()
            .child(div().truncate().text_size(px(LOOK.file_text.0)).line_height(px(16.0)).text_color(theme::text()).child(name))
            .child(div().text_size(px(LOOK.file_text.1)).line_height(px(15.0)).text_color(meta_color).child(meta));
        let close = |id: (&'static str, usize)| div().id(id).flex_none().cursor_pointer().p(px(2.0)).child(ui::icon("icons/close.svg", 10.0, theme::muted()));
        let mut list = div().id("files-list").flex().flex_col().rounded(px(10.0)).border_1().border_color(theme::hairline()).overflow_hidden();
        for (index, file) in mode.files.iter().enumerate() {
            let mut meta = Vec::new();
            if let Some(pages) = file.pages { meta.push(format!("{pages} page{}", if pages == 1 { "" } else { "s" })); }
            meta.push(bytes_label(file.bytes));
            meta.push(format!("≈ {} tokens", tokens_label(file.tokens)));
            let file_id = file.id.clone();
            list = list.child(row(("file", index))
                .child(tile(LOOK.file_tile, 0x1b1d20).child(file_tag(file.kind)))
                .child(text(file.name.clone(), meta.join(" · "), theme::muted()))
                .child(close(("remove-file", index)).on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| this.remove_file(&file_id, cx)))));
        }
        for (index, (_, _, name)) in reading.iter().enumerate() {
            list = list.child(row(("reading", index))
                .child(tile(LOOK.file_tile, 0x1b1d20).child(div().text_size(px(11.0)).text_color(theme::muted()).child("…")))
                .child(text(name.clone(), "Reading…".into(), theme::accent_soft())));
        }
        for (index, notice) in self.modes_ui.notices.iter().enumerate() {
            let id = notice.id;
            list = list.child(row(("notice", index)).bg(rgb(0x1a1312))
                .child(tile(LOOK.file_tile, 0x2e1a18).child(div().text_size(px(12.0)).font_weight(FontWeight::BOLD).text_color(rgb(DANGER)).child("!")))
                .child(text(notice.name.clone(), notice.error.to_string(), rgb(DANGER)))
                .child(close(("dismiss", index)).on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| {
                    this.modes_ui.notices.retain(|notice| notice.id != id);
                    cx.notify();
                }))));
        }
        let full = mode.files.len() >= extract::MAX_FILES;
        let counts = format!("{} of {} · {} MB of 50 MB", mode.files.len(), extract::MAX_FILES, megabytes(mode.file_bytes()));
        let add = div().id("files-add").flex().items_center().gap(px(10.0)).py(px(pad_y + 1.0)).px(px(pad_x)).bg(rgb(0x121316));
        let add = if full {
            add.child(ui::icon("icons/plus.svg", 12.0, theme::placeholder()))
                .child(div().flex_1().text_size(px(12.0)).text_color(theme::placeholder()).child(FileError::LimitReached.to_string()))
        } else {
            add.cursor_pointer().on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| this.browse_files(cx)))
                .child(ui::icon("icons/plus.svg", 12.0, theme::accent_soft()))
                .child(div().flex_1().text_size(px(12.0)).text_color(theme::muted())
                    .child("Drop files here or click to browse · PDF, DOCX, TXT, MD"))
                .child(hint(counts))
        };
        section.child(drop_target(list.child(add), cx))
    }

    fn mode_footer(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.modes.active();
        let (pad_y, left, right) = LOOK.footer_pad;
        let status = if cfg!(target_os = "macos") {
            div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child("Switch anytime from the mode chip in the overlay")
        } else {
            div().flex().items_center().gap(px(6.0))
                .child(div().size(px(6.0)).rounded_full().bg(theme::accent()))
                .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(format!("{} is active", active.name)))
        };
        let action: AnyElement = if active.id == mode.id {
            div().flex().flex_none().items_center().gap(px(6.0)).px(px(12.0)).py(px(6.0)).rounded(px(8.0)).bg(theme::bubble()).border_1().border_color(theme::bubble_border())
                .child(ui::icon("icons/check.svg", 12.0, theme::accent_soft()))
                .child(div().text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()).child("Active"))
                .into_any_element()
        } else {
            let id = mode.id.clone();
            primary_button("set-active", "Set active")
                .on_mouse_down(MouseButton::Left, on_click(cx, None, move |this, cx| this.activate_mode(&id, cx))).into_any_element()
        };
        div().flex().flex_none().items_center().justify_between().py(px(pad_y)).pl(px(left)).pr(px(right)).border_t_1().border_color(theme::divider())
            .bg(rgb(LOOK.footer_bg))
            .child(status).child(action)
    }

    fn delete_confirmation(&self, mode: &Mode, cx: &mut Context<Self>) -> impl IntoElement {
        let files = mode.files.len();
        let body = match files {
            0 => "Its meeting context is removed. This can't be undone.".to_string(),
            1 => "Its meeting context and 1 file are removed. This can't be undone.".to_string(),
            n => format!("Its meeting context and {n} files are removed. This can't be undone."),
        };
        let card = div().id("delete-card").w(px(320.0)).flex().flex_col().gap(px(16.0)).p(px(18.0)).rounded(px(14.0)).bg(theme::raised())
            .border_1().border_color(theme::keycap_border())
            .child(div().flex().flex_col().gap(px(6.0))
                .child(div().text_size(px(15.0)).line_height(px(20.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(format!("Delete “{}”?", mode.name)))
                .child(div().text_size(px(12.5)).line_height(px(18.0)).text_color(theme::body()).child(body)))
            .child(div().flex().justify_end().gap(px(8.0))
                .child(div().id("delete-cancel").px(px(12.0)).py(px(6.0)).rounded(px(8.0)).border_1().border_color(theme::hairline()).cursor_pointer()
                    .text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body())
                    .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = None; cx.notify(); }))
                    .child("Cancel"))
                .child(div().id("delete-confirm").px(px(12.0)).py(px(6.0)).rounded(px(8.0)).bg(rgb(0x3a1c1a)).border_1().border_color(rgb(0x6b302b)).cursor_pointer()
                    .text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(DANGER))
                    .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| this.delete_mode(cx)))
                    .child("Delete")));
        deferred(div().id("delete-scrim").absolute().inset_0().flex().items_center().justify_center().bg(rgba(0x0000008c))
            .on_mouse_down(MouseButton::Left, on_click(cx, None, |this, cx| { this.modes_ui.popover = None; cx.notify(); }))
            .child(card.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())))
    }

    /// Windows: Settings › Modes in the overlay's panel is a summary; the Modes window manages them.
    pub(crate) fn modes_summary(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.modes.active();
        let mut detail = subtitle(active);
        if !active.files.is_empty() { detail.push_str(&format!(" · {} file{}", active.files.len(), if active.files.len() == 1 { "" } else { "s" })); }
        let custom = self.modes.modes().iter().filter(|mode| mode.builtin.is_none()).count();
        let count = format!("{} built-in modes and {custom} of yours · switch anytime from the mode chip", modes::BUILTINS.len());
        div().flex().flex_col().gap(px(12.0))
            .child(label("ACTIVE MODE"))
            .child(div().flex().items_center().gap(px(12.0)).px(px(14.0)).py(px(12.0)).rounded(px(12.0)).bg(theme::field()).border_1().border_color(theme::hairline())
                .child(div().size(px(40.0)).flex_none().flex().items_center().justify_center().rounded(px(10.0)).bg(theme::raised())
                    .border_1().border_color(theme::hairline()).child(mode_icon(active.icon, 18.0, theme::text())))
                .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
                    .child(div().truncate().text_size(px(15.0)).line_height(px(20.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(active.name.clone()))
                    .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(detail)))
                .child(div().flex().flex_none().items_center().gap(px(6.0)).px(px(10.0)).py(px(4.0)).rounded_full().bg(theme::bubble())
                    .border_1().border_color(theme::bubble_border())
                    .child(ui::icon("icons/check.svg", 11.0, theme::accent_soft()))
                    .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()).child("Active"))))
            .child(div().flex().items_center().justify_between().gap(px(12.0)).pt(px(4.0))
                .child(div().flex_1().min_w_0().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(count))
                .child(primary_button("open-modes", "Open Modes…")
                    .on_mouse_down(MouseButton::Left, crate::settings_view::listen(cx, |this, _, window, cx| {
                        cx.stop_propagation();
                        // The settings panel stays above every window, where it would cover the Modes window.
                        this.close_panels(window, cx);
                        this.open_modes_window(None, cx);
                    }))))
            .into_any_element()
    }

    /// Open the Windows Modes window on `mode` (the active one when `None`), or bring it forward.
    /// An open window keeps what it shows, such as a rename or a menu, unless asked for another mode.
    pub(crate) fn open_modes_window(&mut self, mode: Option<&str>, cx: &mut Context<Self>) {
        // Closing it from its own title bar removes the window without clearing the handle.
        let open = self.modes_window.is_some_and(|handle| cx.windows().iter().any(|window| window.window_id() == handle.window_id()));
        let id = mode.map(str::to_string).or_else(|| (!open).then(|| self.modes.active().id.clone()));
        if let Some(id) = id.filter(|id| !open || *id != self.modes_ui.selected) { self.select_mode(&id, cx); }
        crate::modes_window::open(self, cx);
    }

    /// The menu's shortcuts, while the tab itself (not a text field) has the keyboard.
    fn modes_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.modes_ui.focus.is_focused(window) { return; }
        let keys = &event.keystroke;
        let custom = self.modes.get(&self.modes_ui.selected).is_some_and(|mode| mode.builtin.is_none());
        let secondary = if cfg!(target_os = "macos") { keys.modifiers.platform } else { keys.modifiers.control };
        let rename = if cfg!(target_os = "macos") { "enter" } else { "f2" };
        match keys.key.as_str() {
            key if key == rename && custom => { window.focus(&self.modes_ui.name.focus_handle(cx)); self.start_rename(cx) }
            "d" if secondary => self.duplicate_mode(cx),
            "delete" if custom && !cfg!(target_os = "macos") => { self.modes_ui.popover = Some(Popover::ConfirmDelete); cx.notify(); }
            "backspace" if custom && secondary => { self.modes_ui.popover = Some(Popover::ConfirmDelete); cx.notify(); }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Show `id` on the right.
    pub(crate) fn select_mode(&mut self, id: &str, cx: &mut Context<Self>) {
        self.flush_context(cx);
        let Some(mode) = self.modes.get(id) else { return };
        let (text, placeholder_custom) = (mode.context.clone(), mode.builtin.is_none() || mode.id == modes::GENERAL);
        self.modes_ui.selected = id.to_string();
        self.modes_ui.popover = None;
        self.modes_ui.renaming = false;
        self.modes_ui.notices.clear();
        self.modes_ui.context.update(cx, |area, cx| {
            area.set_text(&text, cx);
            area.set_placeholder(if placeholder_custom { CUSTOM_PLACEHOLDER } else { "Describe this meeting and how to answer." }, cx);
        });
        cx.notify();
    }

    /// Settings › Modes opens on the active mode.
    pub(crate) fn prepare_modes_tab(&mut self, cx: &mut Context<Self>) {
        let active = self.modes.active().id.clone();
        self.select_mode(&active, cx);
    }

    fn context_edited(&mut self, cx: &mut Context<Self>) {
        self.modes_ui.pending_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            this.update(cx, |this, cx| this.flush_context(cx)).ok();
        }));
    }

    /// Save a context edit that is waiting for typing to pause.
    fn flush_context(&mut self, cx: &mut Context<Self>) {
        if self.modes_ui.pending_save.take().is_none() { return; }
        let id = self.modes_ui.selected.clone();
        let text = self.modes_ui.context.read(cx).text().to_string();
        if self.modes.set_context(&id, &text) { self.mode_changed(&id, cx); }
    }

    /// The selected mode's prompt or files changed: if it's the active one, answers use the change.
    fn mode_changed(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.modes.active().id == id { self.apply_mode(); }
        cx.notify();
    }

    pub(crate) fn activate_mode(&mut self, id: &str, cx: &mut Context<Self>) {
        self.flush_context(cx);
        if self.modes.set_active(id) { self.apply_mode(); }
        cx.notify();
    }

    fn new_mode(&mut self, cx: &mut Context<Self>) {
        let id = self.modes.create();
        self.select_mode(&id, cx);
        self.start_rename(cx);
    }

    fn duplicate_mode(&mut self, cx: &mut Context<Self>) {
        self.flush_context(cx);
        self.modes_ui.popover = None;
        if let Some(id) = self.modes.duplicate(&self.modes_ui.selected.clone()) { self.select_mode(&id, cx); }
    }

    fn start_rename(&mut self, cx: &mut Context<Self>) {
        let Some(mode) = self.modes.get(&self.modes_ui.selected).filter(|mode| mode.builtin.is_none()) else { return };
        let name = mode.name.clone();
        self.modes_ui.popover = None;
        self.modes_ui.renaming = true;
        // Callers give the field the keyboard, in the window that was clicked.
        // The name starts selected, so typing replaces it.
        self.modes_ui.name.update(cx, |input, cx| { input.set_text(name, cx); input.select_all_text(cx); });
        cx.notify();
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        if !self.modes_ui.renaming { return; }
        self.modes_ui.renaming = false;
        let (id, name) = (self.modes_ui.selected.clone(), self.modes_ui.name.read(cx).text().to_string());
        if self.modes.rename(&id, &name) { self.mode_changed(&id, cx); }
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.modes_ui.renaming = false;
        cx.notify();
    }

    fn set_icon(&mut self, icon: Icon, cx: &mut Context<Self>) {
        let id = self.modes_ui.selected.clone();
        let group = self.modes.get(&id).and_then(|mode| mode.group.clone()).unwrap_or_default();
        self.modes.set_look(&id, icon, &group);
        cx.notify();
    }

    fn set_group(&mut self, group: &str, cx: &mut Context<Self>) {
        let id = self.modes_ui.selected.clone();
        let Some(icon) = self.modes.get(&id).map(|mode| mode.icon) else { return };
        if !group.trim().is_empty() { self.modes.set_look(&id, icon, group); }
        self.modes_ui.popover = None;
        cx.notify();
    }

    fn delete_mode(&mut self, cx: &mut Context<Self>) {
        let id = self.modes_ui.selected.clone();
        self.modes_ui.popover = None;
        self.modes_ui.pending_save = None;
        let was_active = self.modes.active().id == id;
        if self.modes.delete(&id) {
            if was_active { self.apply_mode(); }
            let next = self.modes.active().id.clone();
            self.select_mode(&next, cx);
        }
    }

    fn reset_prompt(&mut self, cx: &mut Context<Self>) {
        self.modes_ui.pending_save = None;
        let id = self.modes_ui.selected.clone();
        if !self.modes.reset_context(&id) { return; }
        let text = self.modes.get(&id).map(|mode| mode.context.clone()).unwrap_or_default();
        self.modes_ui.context.update(cx, |area, cx| area.set_text(&text, cx));
        self.mode_changed(&id, cx);
    }

    fn browse_files(&mut self, cx: &mut Context<Self>) {
        let id = self.modes_ui.selected.clone();
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            this.update(cx, |this, cx| this.add_files(&id, paths, cx)).ok();
        }).detach();
    }

    /// Read each file in its own process, off the UI thread, and add the ones that work.
    pub(crate) fn add_files(&mut self, id: &str, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            let busy = self.modes_ui.reading.iter().filter(|(_, owner, _)| owner == id).count();
            let check = if extract::FileKind::from_path(&path).is_none() { Err(FileError::Unsupported) } else { self.modes.room_for_more(id, size, busy) };
            self.modes_ui.next_id += 1;
            let ticket = self.modes_ui.next_id;
            if let Err(error) = check {
                self.modes_ui.notices.push(Notice { id: ticket, name, error });
                continue;
            }
            self.modes_ui.reading.push((ticket, id.to_string(), name.clone()));
            let room = self.modes.bytes_left(id);
            let read = cx.background_executor().spawn(async move { extract::extract_isolated(&path, room) });
            let owner = id.to_string();
            cx.spawn(async move |this, cx| {
                let result = read.await;
                this.update(cx, |this, cx| {
                    this.modes_ui.reading.retain(|(reading, _, _)| *reading != ticket);
                    match result.and_then(|file| this.modes.add_file(&owner, file)) {
                        Ok(_) => this.mode_changed(&owner, cx),
                        Err(error) => this.modes_ui.notices.push(Notice { id: ticket, name, error }),
                    }
                    cx.notify();
                }).ok();
            }).detach();
        }
        cx.notify();
    }

    fn remove_file(&mut self, file: &str, cx: &mut Context<Self>) {
        let id = self.modes_ui.selected.clone();
        if self.modes.remove_file(&id, file) { self.mode_changed(&id, cx); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_and_size_labels_read_like_the_mockups() {
        assert_eq!((tokens_label(950), tokens_label(1_900), tokens_label(7_240), tokens_label(30_000), tokens_label(41_400)), ("950".into(), "1.9k".into(), "7.2k".into(), "30k".into(), "41k".into()));
        assert_eq!(tokens_label(2_000), "2k");
        assert_eq!((bytes_label(212 * 1024), bytes_label(1_258_291), bytes_label(10)), ("212 KB".into(), "1.2 MB".into(), "1 KB".into()));
        assert_eq!(megabytes(1_468_006), "1.4");
    }

    #[test]
    fn subtitles_and_drop_hints_follow_the_kind_of_mode() {
        let store = ModeStore::at(None);
        let coding = store.get("coding").unwrap();
        assert_eq!(subtitle(coding), "Built-in · Looking for work");
        let mut edited = coding.clone();
        edited.context.push('!');
        assert_eq!(subtitle(&edited), "Built-in · Looking for work · edited");
        assert_eq!(subtitle(store.get("general").unwrap()), "Built-in");
        assert_eq!(drop_title(coding), "Drop a résumé, job post or notes");
        assert_eq!(drop_title(store.get("lecture").unwrap()), "Drop a syllabus, slides or readings");
        let custom = Mode { builtin: None, group: None, ..coding.clone() };
        assert_eq!(subtitle(&custom), "Custom · Your modes");
        assert_eq!(drop_title(&custom), "Drop a pricing sheet, brief or notes");
        assert_eq!(provider_name(&Settings { provider: Provider::ApiKey, api_provider: "groq".into(), ..Settings::default() }), "Groq");
    }
}
