//! Windows: the tray icon's menu and the questions it asks, drawn by CluelyRS in its own dark style
//! rather than as a native Win32 menu, which stays light and shows up in a shared screen (Paper "Tray
//! v2 · Windows · 2 Right-click menu · Hide tray icon…", "Tray · Windows · 2 Right-click menu" and
//! "Tray · Windows · 4 Quit while Live"). Each is a pop-up window of its own, like the overlay: above
//! other windows, kept out of screen capture while the overlay is, and closed by a choice, Esc or a
//! click anywhere else (which answers a question with its Cancel). The menu opens above the tray
//! icon with its right edge at the click; the Mode submenu opens beside it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, DisplayId, Div, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, SharedString, Styled, Subscription, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, div, point, prelude::*, px, rgb, size,
};

use super::Overlay;
use super::menu_bar::{Command, Entry, HideDialog, entries, hide_dialog};
use crate::platform::{self, MenuEntry};
use crate::{theme, ui};

const MENU_WIDTH: f32 = 252.0;
const SUBMENU_WIDTH: f32 = 212.0;
const HIDE_WIDTH: f32 = 330.0;
const QUIT_WIDTH: f32 = 300.0;
/// A menu's lines are of fixed heights, so its window is sized before it is drawn.
const STATUS_HEIGHT: f32 = 53.0;
const ROW_HEIGHT: f32 = 30.0;
const SEPARATOR_HEIGHT: f32 = 9.0;
const GROUP_HEIGHT: f32 = 26.0;
/// A menu's padding and border, above and below its lines.
const MENU_CHROME: f32 = 10.0;
/// How far pop-ups keep from the screen's edges and the taskbar.
const MARGIN: f32 = 8.0;
/// The row under the pointer or the arrow keys.
const HIGHLIGHT: u32 = 0x2a2d31;

/// The open pop-ups: the menu and its submenu, or a question. They close together.
#[derive(Clone, Default)]
pub(super) struct Popups(Rc<RefCell<Vec<AnyWindowHandle>>>);

impl Popups {
    /// Close them all once the event being handled is done (one of them may be handling it).
    fn close_all(&self, cx: &mut App) {
        let open = std::mem::take(&mut *self.0.borrow_mut());
        cx.defer(move |cx| for handle in open { let _ = handle.update(cx, |_, window, _| window.remove_window()); });
    }

    /// Close any that are open, saying whether there were. While Settings is open, Esc is a global
    /// shortcut (`Action::Close`) that reaches the overlay instead of the pop-up, which it closes first.
    pub(super) fn close_open(&self, cx: &mut App) -> bool {
        let open = !self.0.borrow().is_empty();
        if open { self.close_all(cx); }
        open
    }

    fn close(&self, handle: AnyWindowHandle, cx: &mut App) {
        self.0.borrow_mut().retain(|open| *open != handle);
        cx.defer(move |cx| { let _ = handle.update(cx, |_, window, _| window.remove_window()); });
    }

    /// A pop-up stopped being the active window: unless another one now is (its submenu, say), the
    /// click went elsewhere, so they all close.
    fn close_unless_active(&self, cx: &mut App) {
        if cx.active_window().is_some_and(|active| self.0.borrow().contains(&active)) { return; }
        self.close_all(cx);
    }
}

/// The screen a pop-up opens on, in GPUI's logical pixels: the click (or the pointer), the work area
/// beside the taskbar, and which display that is.
#[derive(Clone, Copy)]
struct Screen { point: (f32, f32), work: (f32, f32, f32, f32), display: Option<DisplayId> }

fn screen_at(point: Option<(i32, i32)>, cx: &App) -> Option<Screen> {
    let found = platform::pointer_screen(point)?;
    let logical = |value: i32| value as f32 / found.scale;
    let (left, top, right, bottom) = found.work;
    let point = (logical(found.point.0), logical(found.point.1));
    let display = cx.displays().into_iter().find(|display| display.bounds().contains(&gpui::point(px(point.0), px(point.1)))).map(|display| display.id());
    Some(Screen { point, work: (logical(left), logical(top), logical(right), logical(bottom)), display })
}

/// Where a pop-up of `size` opens for a click at `point` in the work area `work` (left, top, right,
/// bottom): its right edge at the click and, from a taskbar at the bottom, `MARGIN` above it (below
/// one at the top); from anywhere else it ends at the click. Always `MARGIN` inside the work area.
fn place(point: (f32, f32), size: (f32, f32), work: (f32, f32, f32, f32)) -> (f32, f32) {
    let (left, top, right, bottom) = work;
    let y = if point.1 >= bottom { bottom - MARGIN - size.1 } else if point.1 < top { top + MARGIN } else { point.1 - size.1 };
    (inside(point.0 - size.0, size.0, left, right), inside(y, size.1, top, bottom))
}

/// The middle of the work area.
fn centre(size: (f32, f32), work: (f32, f32, f32, f32)) -> (f32, f32) { ((work.0 + work.2 - size.0) / 2.0, (work.1 + work.3 - size.1) / 2.0) }

/// The submenu beside the menu (`menu`: left, top), level with the row that opened it (`row_top`
/// inside the menu): on the menu's left, as the menu sits by the tray at the screen's right, or on
/// its right when there is no room.
fn beside(menu: (f32, f32), row_top: f32, size: (f32, f32), work: (f32, f32, f32, f32)) -> (f32, f32) {
    let left = if menu.0 - 4.0 - size.0 >= work.0 + MARGIN { menu.0 - 4.0 - size.0 } else { menu.0 + MENU_WIDTH + 4.0 };
    (left, inside(menu.1 + row_top - MENU_CHROME / 2.0, size.1, work.1, work.3))
}

/// `start` moved so `start..start + length` lies `MARGIN` inside `low..high` (at its start when too long).
fn inside(start: f32, length: f32, low: f32, high: f32) -> f32 { start.min(high - MARGIN - length).max(low + MARGIN) }

fn line_height(entry: &Entry) -> f32 {
    match entry { MenuEntry::Label(_) => STATUS_HEIGHT, MenuEntry::Separator => SEPARATOR_HEIGHT, MenuEntry::Header(_) => GROUP_HEIGHT, _ => ROW_HEIGHT }
}

fn menu_height(entries: &[Entry]) -> f32 { MENU_CHROME + entries.iter().map(line_height).sum::<f32>() }

/// How far down the menu its line `index` starts.
fn row_top(entries: &[Entry], index: usize) -> f32 { MENU_CHROME / 2.0 + entries[..index].iter().map(line_height).sum::<f32>() }

/// The hide question's height: two ways back, and the warning when the shortcut is taken.
fn hide_height(dialog: &HideDialog) -> f32 { if dialog.warning.is_some() { 244.0 } else { 210.0 } }

/// The quit question's text: the session is saved only when Settings › History keeps sessions.
fn quit_text(saved: bool) -> &'static str {
    if saved { "Quitting ends this session. It's saved to Sessions, transcript and answers included." } else { "Quitting ends this session." }
}

enum Content {
    /// The menu, or its submenu (`parent` is the menu then). `origin` is where it is on screen.
    Menu { entries: Vec<Entry>, live: bool, highlighted: Option<usize>, submenu: Option<WindowHandle<Popup>>, parent: Option<WindowHandle<Popup>>, origin: (f32, f32) },
    Hide(HideDialog),
    Quit { since: Instant, saved: bool },
}

/// What every pop-up needs: the others, the way to the overlay and its menu's commands, and where it is.
#[derive(Clone)]
struct Shared {
    popups: Popups,
    overlay: WeakEntity<Overlay>,
    /// The overlay's window, which its handlers resize and tie work to (as `settings_view::listen`).
    own: AnyWindowHandle,
    commands: Option<UnboundedSender<Command>>,
    hide_from_capture: bool,
    screen: Screen,
}

pub(super) struct Popup {
    content: Content,
    shared: Shared,
    focus: FocusHandle,
    _activation: Subscription,
}

impl Overlay {
    fn popup_shared(&self, screen: Screen, cx: &Context<Self>) -> Shared {
        Shared { popups: self.popups.clone(), overlay: cx.weak_entity(), own: self.own_window, commands: self.menu_bar.as_ref().map(|bar| bar.commands.clone()),
            hide_from_capture: super::capture_excluded(&self.store.value), screen }
    }

    /// Open the menu for a right click on the tray icon at `click` (physical pixels), as the overlay is now.
    pub(super) fn open_tray_menu(&mut self, click: (i32, i32), cx: &mut Context<Self>) {
        let Some(screen) = screen_at(Some(click), cx) else { return };
        let entries = entries(&self.menu_state(), self.native.is_some_and(platform::is_visible), Instant::now());
        let size = (MENU_WIDTH, menu_height(&entries));
        let origin = place(screen.point, size, screen.work);
        let content = Content::Menu { entries, live: self.live_since.is_some(), highlighted: None, submenu: None, parent: None, origin };
        self.open_popup(content, origin, size, screen, cx);
    }

    /// "Hide tray icon…", from the menu or Settings › Window: ask in the middle of the pointer's screen.
    pub(super) fn ask_hide_tray_icon(&mut self, point: Option<(i32, i32)>, cx: &mut Context<Self>) {
        let Some(screen) = screen_at(point, cx) else { return };
        let dialog = hide_dialog(&self.menu_state());
        let size = (HIDE_WIDTH, hide_height(&dialog));
        self.open_popup(Content::Hide(dialog), centre(size, screen.work), size, screen, cx);
    }

    /// Quit while Live is on: ask at the pointer (where the menu or Settings › About's Quit was).
    pub(crate) fn ask_quit(&mut self, cx: &mut Context<Self>) {
        let (Some(since), Some(screen)) = (self.live_since, screen_at(None, cx)) else { return };
        let saved = self.store.value.save_sessions && self.archive.is_some();
        let size = (QUIT_WIDTH, if saved { 158.0 } else { 141.0 });
        self.open_popup(Content::Quit { since, saved }, place(screen.point, size, screen.work), size, screen, cx);
    }

    /// Replace whatever pop-up is open. Deferred: a new window draws at once, outside this update.
    fn open_popup(&mut self, content: Content, origin: (f32, f32), size: (f32, f32), screen: Screen, cx: &mut Context<Self>) {
        self.popups.close_all(cx);
        let shared = self.popup_shared(screen, cx);
        cx.defer(move |cx| { open(content, origin, size, shared, cx); });
    }
}

fn open(content: Content, origin: (f32, f32), (width, height): (f32, f32), shared: Shared, cx: &mut App) -> Option<WindowHandle<Popup>> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(origin.0), px(origin.1)), size(px(width), px(height))))),
        titlebar: None,
        window_background: WindowBackgroundAppearance::Transparent,
        kind: WindowKind::PopUp,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: shared.screen.display,
        ..Default::default()
    };
    let popups = shared.popups.clone();
    match cx.open_window(options, |window, cx| cx.new(|cx| Popup::new(content, shared, window, cx))) {
        Ok(handle) => {
            popups.0.borrow_mut().push(handle.into());
            // Takes the keyboard, so Esc works and a click elsewhere deactivates it.
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            Some(handle)
        }
        Err(error) => { eprintln!("tray pop-up could not open: {error}"); None }
    }
}

impl Popup {
    fn new(content: Content, shared: Shared, window: &mut Window, cx: &mut Context<Self>) -> Self {
        if let Some(native) = platform::native_window(window) {
            if let Err(error) = platform::set_capture_hidden(native, shared.hide_from_capture) { eprintln!("capture exclusion unavailable: {error}"); }
            platform::remove_frame(native);
            let _ = platform::set_topmost(native);
        }
        let focus = cx.focus_handle();
        window.focus(&focus);
        let _activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() { return; }
            // Whatever takes over (the submenu, another app) activates a moment later.
            let popups = this.shared.popups.clone();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let _ = cx.update(|cx| popups.close_unless_active(cx));
            }).detach();
        });
        // The quit question's Live timer keeps counting.
        if matches!(content, Content::Quit { .. }) {
            cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() { break; }
            }).detach();
        }
        Self { content, shared, focus, _activation }
    }

    /// An item was chosen: close the menu and hand its command to the overlay.
    fn choose(&mut self, command: Command, cx: &mut Context<Self>) {
        self.shared.popups.close_all(cx);
        if let Some(commands) = &self.shared.commands { let _ = commands.unbounded_send(command); }
    }

    fn with_overlay(&self, cx: &mut App, f: impl FnOnce(&mut Overlay, &mut Window, &mut Context<Overlay>)) {
        let overlay = self.shared.overlay.clone();
        let _ = self.shared.own.update(cx, |_, window, cx| overlay.update(cx, |this, cx| f(this, window, cx)));
    }

    /// The question's blue button.
    fn confirm(&mut self, cx: &mut Context<Self>) {
        self.shared.popups.close_all(cx);
        match self.content {
            Content::Hide(_) => self.with_overlay(cx, |this, window, cx| this.update_settings(|s| s.show_menu_bar_icon = false, window, cx)),
            // Keep listening is the default.
            Content::Quit { .. } | Content::Menu { .. } => {}
        }
    }

    fn quit(&mut self, cx: &mut Context<Self>) {
        self.shared.popups.close_all(cx);
        self.with_overlay(cx, |this, window, cx| this.quit_now(window, cx));
    }

    /// The pointer or an arrow key moved to line `index`. On the menu's Mode row its submenu opens;
    /// on any other row it closes.
    fn highlight(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Menu { entries, highlighted, parent, .. } = &mut self.content else { return };
        if *highlighted == index { return; }
        *highlighted = index;
        let opens_submenu = index.is_some_and(|index| matches!(entries[index], MenuEntry::Submenu { .. }));
        if parent.is_none() && index.is_some() {
            if opens_submenu { self.open_submenu(window, cx); } else { self.close_submenu(window, cx); }
        }
        cx.notify();
    }

    /// A click or Enter on line `index`.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Menu { entries, .. } = &self.content else { return };
        match &entries[index] {
            MenuEntry::Item { command, enabled: true, .. } => self.choose(command.clone(), cx),
            MenuEntry::Submenu { .. } => { self.highlight(Some(index), window, cx); self.open_submenu(window, cx); }
            _ => {}
        }
    }

    fn open_submenu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Menu { entries, live, highlighted: Some(index), submenu: None, parent: None, origin } = &self.content else { return };
        let MenuEntry::Submenu { entries: modes, .. } = &entries[*index] else { return };
        let size = (SUBMENU_WIDTH, menu_height(modes));
        let at = beside(*origin, row_top(entries, *index), size, self.shared.screen.work);
        let content = Content::Menu { entries: modes.clone(), live: *live, highlighted: None, submenu: None, parent: window.window_handle().downcast(), origin: at };
        let (shared, this) = (self.shared.clone(), cx.weak_entity());
        cx.defer(move |cx| {
            let handle = open(content, at, size, shared, cx);
            let _ = this.update(cx, |this, _| if let Content::Menu { submenu, .. } = &mut this.content { *submenu = handle });
        });
    }

    /// Close the submenu, giving the keyboard back to the menu first so closing it doesn't hand
    /// activation to another app (which would close the menu too).
    fn close_submenu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Menu { submenu, .. } = &mut self.content else { return };
        let Some(handle) = submenu.take() else { return };
        window.activate_window();
        self.shared.popups.close(handle.into(), cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match (&self.content, key) {
            (_, "escape") | (Content::Quit { .. }, "enter") => self.shared.popups.close_all(cx),
            (Content::Hide(_), "enter") => self.confirm(cx),
            (Content::Menu { highlighted: Some(index), .. }, "enter") => self.activate(*index, window, cx),
            (Content::Menu { entries, highlighted, .. }, "up" | "down") => {
                // Only lines that can be chosen; wraps around.
                let choosable: Vec<usize> = entries.iter().enumerate()
                    .filter(|(_, entry)| matches!(entry, MenuEntry::Item { enabled: true, .. } | MenuEntry::Submenu { .. })).map(|(index, _)| index).collect();
                let at = highlighted.and_then(|index| choosable.iter().position(|&choosable| choosable == index));
                let next = match (at, key) { (Some(at), "down") => (at + 1) % choosable.len(), (Some(at), _) => (at + choosable.len() - 1) % choosable.len(),
                    (None, "down") => 0, (None, _) => choosable.len() - 1 };
                self.highlight(choosable.get(next).copied(), window, cx);
            }
            (Content::Menu { entries, highlighted: Some(index), parent: None, .. }, "right") if matches!(entries[*index], MenuEntry::Submenu { .. }) => {
                self.open_submenu(window, cx);
            }
            // Back from the submenu to the menu.
            (Content::Menu { parent: Some(parent), .. }, "left") => {
                let parent = *parent;
                cx.defer(move |cx| { let _ = parent.update(cx, |menu, window, cx| menu.close_submenu(window, cx)); });
            }
            _ => {}
        }
    }

    fn menu(&self, entries: &[Entry], live: bool, highlighted: Option<usize>, submenu: bool, cx: &mut Context<Self>) -> Div {
        let mut menu = div().w(px(if submenu { SUBMENU_WIDTH } else { MENU_WIDTH })).flex().flex_col().p(px(4.0)).rounded(px(10.0))
            .bg(theme::raised()).border_1().border_color(theme::keycap_border());
        for (index, entry) in entries.iter().enumerate() {
            menu = menu.child(match entry {
                MenuEntry::Label(status) => status_header(status, live).into_any_element(),
                MenuEntry::Separator => div().h(px(SEPARATOR_HEIGHT)).flex_none().flex().flex_col().justify_center().px(px(6.0))
                    .child(div().h(px(1.0)).bg(theme::hairline())).into_any_element(),
                MenuEntry::Header(title) => div().h(px(GROUP_HEIGHT)).flex_none().flex().items_end().px(px(10.0)).pb(px(4.0))
                    .text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(title.clone()).into_any_element(),
                _ => self.row(index, entry, highlighted == Some(index), submenu, cx).into_any_element(),
            });
        }
        menu
    }

    /// An item, or the Mode row with the active mode and a chevron. The submenu's items have a
    /// lane for the active mode's check instead of one for shortcuts.
    fn row(&self, index: usize, entry: &Entry, highlighted: bool, submenu: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, enabled, checked, trailing) = match entry {
            MenuEntry::Item { title, enabled, checked, key, command } => {
                let shortcut = div().w(px(64.0)).flex_none().flex().justify_end().font_family(theme::MONO).text_size(px(11.0)).line_height(px(14.0))
                    .text_color(theme::placeholder()).children(key.map(|key| key.label()));
                // "Edit modes…" is quieter than the modes above it.
                let title = div().child(title.clone()).when(*command == Command::EditModes, |title| title.text_color(theme::body()));
                (title, *enabled, *checked, shortcut)
            }
            MenuEntry::Submenu { title, entries } => {
                let active = entries.iter().find_map(|entry| match entry { MenuEntry::Item { title, checked: true, .. } => Some(title.clone()), _ => None });
                let trailing = div().w(px(120.0)).flex_none().flex().items_center().justify_end().gap(px(8.0))
                    .child(div().min_w_0().truncate().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).children(active))
                    .child(ui::icon("icons/chevron-right.svg", 10.0, theme::muted()));
                (div().child(title.clone()), true, false, trailing)
            }
            _ => unreachable!("only items and submenus are rows"),
        };
        div().id(("row", index)).h(px(ROW_HEIGHT)).flex_none().flex().items_center().gap(px(8.0)).px(px(10.0)).rounded(px(6.0))
            .when(highlighted && enabled, |row| row.bg(rgb(HIGHLIGHT)))
            .when(submenu, |row| row.child(div().w(px(14.0)).flex_none().when(checked, |lane| lane.child(ui::icon("icons/check.svg", 14.0, theme::accent_soft())))))
            .child(title.flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(16.0)).text_color(if enabled { theme::text() } else { theme::placeholder() })
                .when(checked, |title| title.font_weight(FontWeight::SEMIBOLD)))
            .when(!submenu, |row| row.child(trailing))
            .on_hover(cx.listener(move |this, hovered: &bool, window, cx| if *hovered { this.highlight(Some(index), window, cx) }))
            .when(enabled, |row| row.cursor_pointer().on_click(cx.listener(move |this, _, window, cx| this.activate(index, window, cx))))
    }

    fn hide_question(&self, dialog: &HideDialog, cx: &mut Context<Self>) -> Div {
        let way_back = |icon: &'static str, text: &'static str| div().h(px(28.0)).flex_none().flex().items_center().gap(px(8.0))
            .child(ui::icon(icon, 16.0, theme::body())).child(div().text_size(px(13.0)).line_height(px(16.0)).text_color(rgb(SOFT_TEXT)).child(text));
        let shortcut = div().h(px(28.0)).flex_none().flex().items_center().gap(px(8.0))
            .children(dialog.keys.iter().map(|key| ui::keycap(key.clone())))
            .child(div().text_size(px(13.0)).line_height(px(16.0)).text_color(rgb(SOFT_TEXT)).child("overlay"))
            .child(ui::icon("icons/setup-arrow.svg", 12.0, theme::muted()))
            .child(ui::icon("icons/gear.svg", 16.0, theme::body()))
            .child(div().text_size(px(13.0)).line_height(px(16.0)).text_color(rgb(SOFT_TEXT)).child("Settings"));
        let warning = dialog.warning.clone().map(|warning| div().h(px(28.0)).flex_none().flex().items_center()
            .child(div().flex().items_center().gap(px(5.0)).px(px(8.0)).py(px(2.0)).rounded_full().bg(rgb(0x2a2416))
                .child(ui::icon("icons/alert-circle.svg", 12.0, rgb(WARNING)))
                .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(rgb(WARNING)).child(warning))));
        question(HIDE_WIDTH, 20.0, 16.0)
            .child(div().flex().items_center().gap(px(10.0)).child(ui::mark(22.0))
                .child(div().text_size(px(15.0)).line_height(px(20.0)).font_weight(FontWeight::BOLD).child(dialog.title)))
            .child(div().flex().flex_col().gap(px(6.0))
                .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child("TO GET BACK"))
                .child(shortcut)
                .child(way_back("icons/setup-grid.svg", "Or open CluelyRS again"))
                .children(warning))
            .child(div().flex().justify_end().gap(px(8.0))
                .child(button("cancel", dialog.cancel, false, 32.0, 14.0).on_click(cx.listener(|this, _, _, cx| this.shared.popups.close_all(cx))))
                .child(button("hide", dialog.hide, true, 32.0, 14.0).on_click(cx.listener(|this, _, _, cx| this.confirm(cx)))))
    }

    fn quit_question(&self, since: Instant, saved: bool, cx: &mut Context<Self>) -> Div {
        let seconds = since.elapsed().as_secs();
        question(QUIT_WIDTH, 16.0, 14.0)
            .child(div().flex().flex_col().gap(px(6.0))
                .child(div().flex().items_center().gap(px(8.0))
                    .child(div().size(px(7.0)).flex_none().rounded_full().bg(theme::accent()))
                    .child(div().font_family(theme::MONO).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::accent_soft())
                        .child(format!("LIVE · {:02}:{:02}", seconds / 60, seconds % 60))))
                .child(div().text_size(px(15.0)).line_height(px(20.0)).font_weight(FontWeight::BOLD).child("Quit while Live is on?"))
                .child(div().text_size(px(12.0)).line_height(px(17.0)).text_color(theme::muted()).child(quit_text(saved))))
            .child(div().flex().justify_end().gap(px(8.0))
                .child(button("keep", "Keep listening", true, 30.0, 12.0).text_size(px(12.0)).on_click(cx.listener(|this, _, _, cx| this.shared.popups.close_all(cx))))
                .child(button("quit", "Quit CluelyRS", false, 30.0, 12.0).text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD)
                    .on_click(cx.listener(|this, _, _, cx| this.quit(cx)))))
    }
}

/// Secondary text in the questions.
const SOFT_TEXT: u32 = 0xe6e3dc;
const WARNING: u32 = 0xf0c674;

/// The menu's first line: the mark, CluelyRS, and what Live is doing (blue while it's on).
fn status_header(status: &str, live: bool) -> Div {
    div().h(px(STATUS_HEIGHT)).flex_none().flex().items_center().gap(px(10.0)).px(px(10.0))
        .child(ui::mark(22.0))
        .child(div().flex().flex_col().gap(px(1.0)).min_w_0()
            .child(div().text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::BOLD).child("CluelyRS"))
            .child(div().truncate().text_size(px(12.0)).line_height(px(16.0)).text_color(if live { theme::accent_soft() } else { theme::muted() })
                .child(SharedString::from(status.to_string()))))
}

fn question(width: f32, padding: f32, gap: f32) -> Div {
    div().w(px(width)).flex().flex_col().gap(px(gap)).p(px(padding)).rounded(px(12.0)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
}

fn button(id: &'static str, label: &'static str, primary: bool, height: f32, padding: f32) -> gpui::Stateful<Div> {
    let button = div().id(id).h(px(height)).px(px(padding)).flex().items_center().rounded(px(8.0)).cursor_pointer().text_size(px(13.0)).line_height(px(16.0)).child(label);
    if primary { button.bg(theme::accent()).text_color(theme::accent_ink()).font_weight(FontWeight::SEMIBOLD) }
    else { button.bg(theme::field()).border_1().border_color(theme::hairline()).text_color(rgb(SOFT_TEXT)).hover(|button| button.border_color(theme::keycap_border())) }
}

impl Render for Popup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.content {
            Content::Menu { entries, live, highlighted, parent, .. } => self.menu(entries, *live, *highlighted, parent.is_some(), cx),
            Content::Hide(dialog) => self.hide_question(dialog, cx),
            Content::Quit { since, saved } => self.quit_question(*since, *saved, cx),
        };
        div().size_full().font_family(theme::FONT).text_color(theme::text()).track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(event, window, cx)))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::super::menu_bar::MenuState;
    use super::*;

    /// The idle menu is as tall as the Paper artboard's (252 × 348).
    #[test]
    fn the_menu_is_sized_from_its_lines_as_drawn() {
        let menu = entries(&MenuState::default(), false, Instant::now());
        assert_eq!(menu_height(&menu), 348.0);
        assert_eq!(row_top(&menu, 0), 5.0);
        assert_eq!(row_top(&menu, 4), 5.0 + STATUS_HEIGHT + SEPARATOR_HEIGHT + 2.0 * ROW_HEIGHT, "the Mode row");
    }

    #[test]
    fn the_menu_opens_above_a_bottom_taskbar_with_its_right_edge_at_the_click() {
        let work = (0.0, 0.0, 1920.0, 1032.0);
        assert_eq!(place((1700.0, 1050.0), (252.0, 348.0), work), (1448.0, 1032.0 - 8.0 - 348.0));
        // By the screen's right edge it stays inside it.
        assert_eq!(place((1915.0, 1050.0), (252.0, 348.0), work).0, 1920.0 - 8.0 - 252.0);
        // A taskbar at the top: below it.
        assert_eq!(place((1700.0, 10.0), (252.0, 348.0), (0.0, 48.0, 1920.0, 1080.0)).1, 56.0);
        // From inside the work area (Settings › About's Quit): ending at the click, kept on screen.
        assert_eq!(place((900.0, 500.0), (300.0, 158.0), work), (600.0, 342.0));
        assert_eq!(place((100.0, 60.0), (300.0, 158.0), work), (8.0, 8.0));
    }

    #[test]
    fn the_submenu_opens_beside_the_menu_level_with_the_mode_row() {
        let work = (0.0, 0.0, 1920.0, 1032.0);
        assert_eq!(beside((1448.0, 500.0), 127.0, (212.0, 300.0), work), (1448.0 - 4.0 - 212.0, 500.0 + 127.0 - 5.0));
        // No room on the left: on the right.
        assert_eq!(beside((10.0, 100.0), 127.0, (212.0, 300.0), work).0, 10.0 + MENU_WIDTH + 4.0);
        // Never past the bottom of the work area.
        assert_eq!(beside((1448.0, 676.0), 127.0, (212.0, 500.0), work).1, 1032.0 - 8.0 - 500.0);
    }

    #[test]
    fn the_questions_are_as_tall_as_drawn() {
        let mut dialog = hide_dialog(&MenuState::default());
        assert_eq!(hide_height(&dialog), 210.0);
        dialog.warning = Some("taken".into());
        assert_eq!(hide_height(&dialog), 210.0 + 6.0 + 28.0, "one more line");
        assert!(quit_text(true).contains("saved to Sessions") && !quit_text(false).contains("saved"), "only says it's saved when it is");
    }
}
