//! Multi-line text area with soft wrapping, for a mode's meeting context. It grows with its text
//! (the surrounding panel scrolls) and shares the single-line input's grapheme handling.
//! The element follows the structure of Zed's gpui input example, as `input.rs` does.

use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, Font, GlobalElementId, Hsla, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, SharedString, Size, Style, Task, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};

use crate::input::{InputEvent, next_boundary, previous_boundary, utf16_range_in};
use crate::theme;

actions!(text_area, [Backspace, Delete, Left, Right, Up, Down, SelectLeft, SelectRight, SelectUp, SelectDown, SelectAll, Home, End,
    Newline, Paste, Cut, Copy]);

const CONTEXT: &str = "TextArea";
const CURSOR_WIDTH: f32 = 2.0;
/// How long the caret stays shown, then hidden, while it blinks.
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// Register key bindings once at startup (context "TextArea"). `secondary` is ⌘ on macOS and
/// Ctrl elsewhere.
pub fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("enter", Newline, context),
        KeyBinding::new("shift-enter", Newline, context),
        KeyBinding::new("secondary-a", SelectAll, context),
        KeyBinding::new("secondary-c", Copy, context),
        KeyBinding::new("secondary-v", Paste, context),
        KeyBinding::new("secondary-x", Cut, context),
    ]);
}

pub struct TextArea {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    max_chars: usize,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    layout: Option<Layout>,
    is_selecting: bool,
    /// Where Up and Down aim, so moving through short lines keeps the column.
    goal_x: Option<Pixels>,
    cursor_visible: bool,
    blink: Option<Task<()>>,
}

/// The text as last painted.
struct Layout {
    lines: Vec<LaidLine>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
}

/// One paragraph (text between line breaks), wrapped to the area's width.
struct LaidLine {
    start: usize,
    len: usize,
    wrapped: WrappedLine,
    /// From the top of the area.
    top: Pixels,
    height: Pixels,
}

impl Layout {
    /// Where `offset` is drawn, relative to the area's top left.
    fn position(&self, offset: usize) -> Point<Pixels> {
        let line = self.lines.iter().rev().find(|line| line.start <= offset).or(self.lines.first());
        let Some(line) = line else { return point(px(0.0), px(0.0)) };
        let within = offset.saturating_sub(line.start).min(line.len);
        let at = line.wrapped.position_for_index(within, self.line_height).unwrap_or_default();
        point(at.x, at.y + line.top)
    }

    /// The text offset nearest to `position` (relative to the area's top left).
    fn offset(&self, position: Point<Pixels>) -> usize {
        let Some(last) = self.lines.last() else { return 0 };
        if position.y < px(0.0) { return 0; }
        if position.y >= last.top + last.height { return last.start + last.len; }
        let line = self.lines.iter().find(|line| position.y < line.top + line.height).unwrap_or(last);
        let local = point(position.x.max(px(0.0)), position.y - line.top);
        let index = match line.wrapped.closest_index_for_position(local, self.line_height) { Ok(index) | Err(index) => index };
        line.start + index.min(line.len)
    }

    fn height(&self) -> Pixels { self.lines.last().map_or(px(0.0), |line| line.top + line.height) }
}

impl EventEmitter<InputEvent> for TextArea {}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle { self.focus_handle.clone() }
}

impl TextArea {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(), content: String::new(), placeholder: placeholder.into(), max_chars: usize::MAX,
            selected_range: 0..0, selection_reversed: false, marked_range: None, layout: None, is_selecting: false, goal_x: None,
            cursor_visible: true, blink: None,
        }
    }

    /// Typing past `max` characters is cut off.
    pub fn with_max_chars(mut self, max: usize) -> Self {
        self.max_chars = max;
        self
    }

    pub fn text(&self) -> &str { &self.content }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// Replace the text programmatically (no `Changed`), with the caret at the end.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = normalize(text).chars().take(self.max_chars).collect();
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.goal_x = None;
        cx.notify();
    }

    fn show_cursor(&mut self) {
        self.cursor_visible = true;
        self.blink = None;
    }

    fn has_keyboard(&self, window: &Window) -> bool { self.focus_handle.is_focused(window) && window.is_window_active() }

    fn keep_blinking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_keyboard(window) {
            self.show_cursor();
            return;
        }
        if self.blink.is_some() { return; }
        self.blink = Some(cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(BLINK_INTERVAL).await;
            if this.update(cx, |this, cx| { this.cursor_visible = !this.cursor_visible; cx.notify(); }).is_err() { break; }
        }));
    }

    fn cursor_offset(&self) -> usize { if self.selection_reversed { self.selected_range.start } else { self.selected_range.end } }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.show_cursor();
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.show_cursor();
        if self.selection_reversed { self.selected_range.start = offset } else { self.selected_range.end = offset }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    /// The offset one visual row above (`-1`) or below (`1`) the caret, keeping the column.
    fn vertical(&mut self, rows: f32) -> usize {
        let Some(layout) = &self.layout else { return self.cursor_offset() };
        let at = layout.position(self.cursor_offset());
        let x = *self.goal_x.get_or_insert(at.x);
        let y = at.y + layout.line_height * (rows + 0.5);
        if y < px(0.0) { return 0; }
        if y >= layout.height() { return self.content.len(); }
        layout.offset(point(x, y))
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        let target = if self.selected_range.is_empty() { previous_boundary(&self.content, self.cursor_offset()) } else { self.selected_range.start };
        self.move_to(target, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        let target = if self.selected_range.is_empty() { next_boundary(&self.content, self.cursor_offset()) } else { self.selected_range.end };
        self.move_to(target, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) { let target = self.vertical(-1.0); self.move_to(target, cx); }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) { let target = self.vertical(1.0); self.move_to(target, cx); }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.select_to(previous_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.select_to(next_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) { let target = self.vertical(-1.0); self.select_to(target, cx); }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) { let target = self.vertical(1.0); self.select_to(target, cx); }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(line_start(&self.content, self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(line_end(&self.content, self.cursor_offset()), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() { self.select_to(previous_boundary(&self.content, self.cursor_offset()), cx); }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() { self.select_to(next_boundary(&self.content, self.cursor_offset()), cx); }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) { self.replace_text_in_range(None, "\n", window, cx); }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) { self.replace_text_in_range(None, &text, window, cx); }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() { cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string())); }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() { return; }
        cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        self.replace_text_in_range(None, "", window, cx);
    }

    fn offset_for_mouse(&self, position: Point<Pixels>) -> usize {
        self.layout.as_ref().map_or(0, |layout| layout.offset(position - layout.bounds.origin))
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        self.goal_x = None;
        let offset = self.offset_for_mouse(event.position);
        if event.modifiers.shift { self.select_to(offset, cx) } else { self.move_to(offset, cx) }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) { self.is_selecting = false; }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting { let offset = self.offset_for_mouse(event.position); self.select_to(offset, cx); }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize { utf16_range_in(&self.content, &(0..offset)).end }

    fn offset_to_utf16(&self, offset: usize) -> usize { self.content[..offset.min(self.content.len())].encode_utf16().count() }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> { self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end) }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> { self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end) }

    /// Insert `text` over `range`, cut so the whole stays within `max_chars`. Returns what went in.
    fn splice(&mut self, range: &Range<usize>, text: &str) -> String {
        let room = self.max_chars.saturating_sub(self.content.chars().count() - self.content[range.clone()].chars().count());
        let text: String = normalize(text).chars().take(room).collect();
        self.content.replace_range(range.clone(), &text);
        text
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(&mut self, range_utf16: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) { self.marked_range = None; }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.show_cursor();
        self.goal_x = None;
        let range = range_utf16.as_ref().map(|range| self.range_from_utf16(range)).or(self.marked_range.clone()).unwrap_or(self.selected_range.clone());
        let inserted = self.splice(&range, new_text);
        let changed = !(range.is_empty() && inserted.is_empty());
        self.selected_range = range.start + inserted.len()..range.start + inserted.len();
        self.selection_reversed = false;
        self.marked_range = None;
        if changed { cx.emit(InputEvent::Changed); }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, selected_utf16: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        self.show_cursor();
        let range = range_utf16.as_ref().map(|range| self.range_from_utf16(range)).or(self.marked_range.clone()).unwrap_or(self.selected_range.clone());
        let inserted = self.splice(&range, new_text);
        self.marked_range = (!inserted.is_empty()).then(|| range.start..range.start + inserted.len());
        self.selected_range = selected_utf16.as_ref().map(|selection| utf16_range_in(&inserted, selection))
            .map(|selection| range.start + selection.start..range.start + selection.end)
            .unwrap_or_else(|| range.start + inserted.len()..range.start + inserted.len());
        self.selection_reversed = false;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position(range.start);
        let origin = layout.bounds.origin + start;
        Some(Bounds::new(origin, size(px(1.0), layout.line_height)))
    }

    fn character_index_for_point(&mut self, point: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let offset = self.offset_for_mouse(point);
        Some(self.offset_to_utf16(offset))
    }
}

/// Unix line breaks only.
fn normalize(text: &str) -> String {
    if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.to_string() }
}

fn line_start(text: &str, offset: usize) -> usize { text[..offset].rfind('\n').map_or(0, |at| at + 1) }

fn line_end(text: &str, offset: usize) -> usize { text[offset..].find('\n').map_or(text.len(), |at| offset + at) }

/// Shape `text` paragraph by paragraph, wrapped to `width`.
fn shape(text: &str, font: &Font, font_size: Pixels, color: Hsla, line_height: Pixels, width: Pixels, window: &Window) -> Vec<LaidLine> {
    let mut lines = Vec::new();
    let (mut start, mut top) = (0, px(0.0));
    for paragraph in text.split('\n') {
        let run = TextRun { len: paragraph.len(), font: font.clone(), color, background_color: None, underline: None, strikethrough: None };
        let wrapped = window.text_system().shape_text(SharedString::from(paragraph.to_string()), font_size, &[run], Some(width), None)
            .ok().and_then(|mut shaped| (!shaped.is_empty()).then(|| shaped.remove(0))).unwrap_or_default();
        let height = wrapped.size(line_height).height.max(line_height);
        lines.push(LaidLine { start, len: paragraph.len(), wrapped, top, height });
        top += height;
        start += paragraph.len() + 1;
    }
    lines
}

struct TextAreaElement {
    area: Entity<TextArea>,
    min_lines: usize,
}

struct Prepaint {
    lines: Vec<LaidLine>,
    selection: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    line_height: Pixels,
}

impl IntoElement for TextAreaElement {
    type Element = Self;
    fn into_element(self) -> Self::Element { self }
}

impl Element for TextAreaElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> { None }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let area = self.area.read(cx);
        let text = if area.content.is_empty() { area.placeholder.to_string() } else { area.content.clone() };
        let style = window.text_style();
        let (font, font_size, line_height) = (style.font(), style.font_size.to_pixels(window.rem_size()), window.line_height());
        let min_height = line_height * self.min_lines as f32;
        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.).into();
        let id = window.request_measured_layout(layout_style, move |known: Size<Option<Pixels>>, available: Size<AvailableSpace>, window, _| {
            let width = known.width.unwrap_or(match available.width { AvailableSpace::Definite(width) => width, _ => px(10_000.0) });
            let lines = shape(&text, &font, font_size, Hsla::default(), line_height, width, window);
            let height = lines.last().map_or(px(0.0), |line| line.top + line.height);
            size(width, height.max(min_height))
        });
        (id, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Prepaint {
        let area = self.area.read(cx);
        let style = window.text_style();
        let (font_size, line_height) = (style.font_size.to_pixels(window.rem_size()), window.line_height());
        let empty = area.content.is_empty();
        let (text, color) = if empty { (area.placeholder.to_string(), theme::placeholder().into()) } else { (area.content.clone(), style.color) };
        let mut lines = shape(&text, &style.font(), font_size, color, line_height, bounds.size.width, window);
        // IME composition is underlined.
        if let Some(marked) = area.marked_range.clone().filter(|_| !empty) {
            for line in &mut lines {
                let (start, end) = (marked.start.max(line.start), marked.end.min(line.start + line.len));
                if start >= end { continue; }
                let underline = UnderlineStyle { color: Some(color), thickness: px(1.0), wavy: false };
                let base = TextRun { len: 0, font: style.font(), color, background_color: None, underline: None, strikethrough: None };
                let runs: Vec<TextRun> = [(start - line.start, None), (end - start, Some(underline)), (line.start + line.len - end, None)]
                    .into_iter().filter(|(len, _)| *len > 0).map(|(len, underline)| TextRun { len, underline, ..base.clone() }).collect();
                let paragraph = SharedString::from(text[line.start..line.start + line.len].to_string());
                if let Some(wrapped) = window.text_system().shape_text(paragraph, font_size, &runs, Some(bounds.size.width), None).ok().and_then(|mut shaped| (!shaped.is_empty()).then(|| shaped.remove(0))) {
                    line.wrapped = wrapped;
                }
            }
        }
        let laid = Layout { lines, bounds, line_height };
        let (selection, cursor) = if empty {
            (Vec::new(), Some(fill(Bounds::new(bounds.origin, size(px(CURSOR_WIDTH), line_height)), theme::accent())))
        } else if area.selected_range.is_empty() {
            let at = laid.position(area.cursor_offset());
            (Vec::new(), Some(fill(Bounds::new(bounds.origin + at, size(px(CURSOR_WIDTH), line_height)), theme::accent())))
        } else {
            (selection_quads(&laid, area.selected_range.clone()), None)
        };
        Prepaint { lines: laid.lines, selection, cursor, line_height }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), prepaint: &mut Prepaint, window: &mut Window, cx: &mut App) {
        let focus = self.area.read(cx).focus_handle.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.area.clone()), cx);
        for quad in prepaint.selection.drain(..) { window.paint_quad(quad); }
        for line in &prepaint.lines {
            // A paint failure only drops this frame's glyphs; never panic in the render path.
            let _ = line.wrapped.paint(point(bounds.left(), bounds.top() + line.top), prepaint.line_height, TextAlign::Left, Some(bounds), window, cx);
        }
        let area = self.area.read(cx);
        if area.has_keyboard(window) && area.cursor_visible && let Some(cursor) = prepaint.cursor.take() { window.paint_quad(cursor); }
        let lines = std::mem::take(&mut prepaint.lines);
        let line_height = prepaint.line_height;
        self.area.update(cx, |area, _| area.layout = Some(Layout { lines, bounds, line_height }));
    }
}

/// Highlight rectangles for `range`, one per visual row it spans.
fn selection_quads(layout: &Layout, range: Range<usize>) -> Vec<PaintQuad> {
    let (start, end) = (layout.position(range.start), layout.position(range.end));
    let (origin, width, height) = (layout.bounds.origin, layout.bounds.size.width, layout.line_height);
    let rect = |x0: Pixels, x1: Pixels, y: Pixels| fill(Bounds::from_corners(origin + point(x0, y), origin + point(x1.max(x0 + px(4.0)), y + height)), theme::bubble());
    if start.y == end.y { return vec![rect(start.x, end.x, start.y)]; }
    let mut quads = vec![rect(start.x, width, start.y)];
    let mut y = start.y + height;
    while y < end.y {
        quads.push(rect(px(0.0), width, y));
        y += height;
    }
    quads.push(rect(px(0.0), end.x, end.y));
    quads
}

impl Render for TextArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.keep_blinking(window, cx);
        div().w_full().key_context(CONTEXT).track_focus(&self.focus_handle(cx)).cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace)).on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left)).on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up)).on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left)).on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up)).on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all)).on_action(cx.listener(Self::home)).on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::newline)).on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut)).on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextAreaElement { area: cx.entity(), min_lines: 4 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_and_ends_follow_hard_breaks() {
        let text = "one\ntwo words\n\nfour";
        assert_eq!((line_start(text, 0), line_end(text, 0)), (0, 3));
        assert_eq!((line_start(text, 6), line_end(text, 6)), (4, 13));
        assert_eq!((line_start(text, 14), line_end(text, 14)), (14, 14));
        assert_eq!((line_start(text, text.len()), line_end(text, text.len())), (15, text.len()));
    }

    #[test]
    fn line_breaks_are_normalized() {
        assert_eq!(normalize("a\r\nb\rc\n"), "a\nb\nc\n");
        assert_eq!(normalize("plain"), "plain");
    }
}
