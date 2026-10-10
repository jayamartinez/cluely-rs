//! Single-line text input.
//!
//! Adapted from Zed's gpui examples/input.rs, Apache-2.0
//! (https://github.com/zed-industries/zed, crates/gpui/examples/input.rs).
//! Changes: themed rendering, masking for secrets, horizontal scrolling, Enter/Changed
//! events, Windows key bindings, and dependency-free grapheme boundaries.

use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, Task, TextRun,
    UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative, size,
};

use crate::theme;

actions!(text_input, [Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, Paste, Cut, Copy, Submit]);

const CONTEXT: &str = "TextInput";
const MASK: &str = "•";
const TEXT_SIZE: f32 = 13.0;
const LINE_HEIGHT: f32 = 20.0;
const CURSOR_WIDTH: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Changed,
    Submit,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    masked: bool,
    /// Text size and line height, in pixels.
    text_size: (f32, f32),
    /// A masked input showing its text for the user to check. Copy, cut and the platform text
    /// services still treat the text as secret.
    revealed: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
    is_selecting: bool,
    /// The caret blinks while the box has focus; this is whether it is drawn right now.
    cursor_visible: bool,
    blink: Option<Task<()>>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// How long the caret stays shown, then hidden, while it blinks (the macOS and Windows default).
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// Register key bindings once at startup (context "TextInput").
pub fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("ctrl-a", SelectAll, context),
        KeyBinding::new("ctrl-c", Copy, context),
        KeyBinding::new("ctrl-v", Paste, context),
        KeyBinding::new("ctrl-x", Cut, context),
        KeyBinding::new("enter", Submit, context),
    ]);
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            masked: false,
            text_size: (TEXT_SIZE, LINE_HEIGHT),
            revealed: false,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.0),
            is_selecting: false,
            cursor_visible: true,
            blink: None,
        }
    }

    /// Typing or moving the caret shows it and restarts the blink, so it never vanishes mid-edit.
    fn show_cursor(&mut self) {
        self.cursor_visible = true;
        self.blink = None;
    }

    /// Whether typing goes here: the box has focus in a window that has the keyboard. The overlay
    /// keeps the box focused after handing the keyboard back to another app, so focus alone isn't enough.
    pub(crate) fn has_keyboard(&self, window: &Window) -> bool { self.focus_handle.is_focused(window) && window.is_window_active() }

    /// Blink while it has the keyboard; stop (ready to show at once next time) when it doesn't.
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

    /// Render the text as bullets (for API keys). Copy and cut are disabled while masked.
    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// Larger or smaller text than the default 13 px on 20 px lines (a title being renamed).
    pub fn with_text_size(mut self, size: f32, line_height: f32) -> Self {
        self.text_size = (size, line_height);
        self
    }

    pub fn masked(mut self, masked: bool) -> Self {
        self.masked = masked;
        self
    }

    pub fn text(&self) -> SharedString {
        self.content.clone()
    }

    /// Replaces the content programmatically (does not emit `Changed`) and moves the cursor to the end.
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text: SharedString = text.into();
        self.content = single_line(&text).into();
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    /// Empties the input programmatically (does not emit `Changed`).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_x = px(0.0);
        self.is_selecting = false;
        self.revealed = false;
        cx.notify();
    }

    /// Shows or hides a masked input's text. Emptying the input hides it again.
    pub fn set_revealed(&mut self, revealed: bool, cx: &mut Context<Self>) {
        if self.revealed != revealed {
            self.revealed = revealed;
            cx.notify();
        }
    }

    pub fn is_revealed(&self) -> bool {
        self.revealed
    }

    /// Whether the text is drawn as bullets.
    fn shows_bullets(&self) -> bool {
        self.masked && !self.revealed
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(previous_boundary(&self.content, self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(next_boundary(&self.content, self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(previous_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_text(cx)
    }

    /// Selects the whole content, so typing replaces it.
    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(previous_boundary(&self.content, self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(next_boundary(&self.content, self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit);
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &single_line(&text), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.show_cursor();
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    /// Content offset to the offset in the rendered (possibly masked) line.
    fn display_offset(&self, offset: usize) -> usize {
        if self.shows_bullets() { masked_display_offset(&self.content, offset) } else { offset }
    }

    /// Offset in the rendered line back to a content offset.
    fn content_offset(&self, display_offset: usize) -> usize {
        if self.shows_bullets() { masked_content_offset(&self.content, display_offset) } else { display_offset }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.content_offset(line.closest_index_for_x(position.x - bounds.left() + self.scroll_x))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.show_cursor();
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn splice(&mut self, range: &Range<usize>, new_text: &str) {
        self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(&mut self, range_utf16: Range<usize>, actual_range: &mut Option<Range<usize>>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        // Never hand secret text to the platform text services.
        if self.masked {
            return Some(MASK.repeat(self.content[range].chars().count()));
        }
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore_disabled_input: bool, _window: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.show_cursor();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = single_line(new_text);
        let changed = !(range.is_empty() && new_text.is_empty());
        self.splice(&range, &new_text);
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        if changed {
            cx.emit(InputEvent::Changed);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, new_selected_range_utf16: Option<Range<usize>>, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_cursor();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = single_line(new_text);
        self.splice(&range, &new_text);
        self.marked_range = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        // The new selection is relative to the inserted text (UTF-16 within `new_text`).
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|selection| utf16_range_in(&new_text, selection))
            .map(|selection| range.start + selection.start..range.start + selection.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let left = bounds.left() - self.scroll_x;
        Some(Bounds::from_corners(
            point(left + last_layout.x_for_index(self.display_offset(range.start)), bounds.top()),
            point(left + last_layout.x_for_index(self.display_offset(range.end)), bounds.bottom()),
        ))
    }

    fn character_index_for_point(&mut self, point: Point<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line_point = bounds.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        let display_index = last_layout.index_for_x(line_point.x + self.scroll_x)?;
        Some(self.offset_to_utf16(self.content_offset(display_index)))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll_x: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _request_layout: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let showing_placeholder = input.content.is_empty();
        let (display_text, text_color): (SharedString, _) = if showing_placeholder {
            (input.placeholder.clone(), theme::placeholder().into())
        } else if input.shows_bullets() {
            (MASK.repeat(input.content.chars().count()).into(), style.color)
        } else {
            (input.content.clone(), style.color)
        };

        let run = TextRun { len: display_text.len(), font: style.font(), color: text_color, background_color: None, underline: None, strikethrough: None };
        let runs = match input.marked_range.as_ref().filter(|_| !showing_placeholder) {
            Some(marked) => {
                let (start, end) = (input.display_offset(marked.start), input.display_offset(marked.end));
                let underline = UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false };
                vec![
                    TextRun { len: start, ..run.clone() },
                    TextRun { len: end - start, underline: Some(underline), ..run.clone() },
                    TextRun { len: display_text.len() - end, ..run },
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect()
            }
            None => vec![run],
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(display_text, font_size, &runs, None);

        let (selection_range, cursor) = if showing_placeholder {
            (0..0, 0)
        } else {
            let selected = input.selected_range.clone();
            (input.display_offset(selected.start)..input.display_offset(selected.end), input.display_offset(input.cursor_offset()))
        };
        let cursor_x = line.x_for_index(cursor);
        let scroll_x = scroll_to_reveal(input.scroll_x, cursor_x, line.width, bounds.size.width - px(CURSOR_WIDTH));
        let left = bounds.left() - scroll_x;

        let (selection, cursor) = if selection_range.is_empty() {
            let caret = Bounds::new(point(left + cursor_x, bounds.top()), size(px(CURSOR_WIDTH), bounds.size.height));
            (None, Some(fill(caret, theme::accent())))
        } else {
            let highlight = Bounds::from_corners(
                point(left + line.x_for_index(selection_range.start), bounds.top()),
                point(left + line.x_for_index(selection_range.end), bounds.bottom()),
            );
            (Some(fill(highlight, theme::bubble())), None)
        };
        PrepaintState { line: Some(line), cursor, selection, scroll_x }
    }

    fn paint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _request_layout: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let Some(line) = prepaint.line.take() else { return };
        let scroll_x = prepaint.scroll_x;
        let focused = self.input.read(cx).has_keyboard(window) && self.input.read(cx).cursor_visible;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection)
            }
            // A paint failure only drops this frame's glyphs; never panic in the render path.
            let _ = line.paint(point(bounds.left() - scroll_x, bounds.top()), window.line_height(), window, cx);
            if focused && let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        });

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
            input.scroll_x = scroll_x;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.keep_blinking(window, cx);
        div()
            .flex()
            .w_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .font_family(theme::FONT)
            .text_size(px(self.text_size.0))
            .line_height(px(self.text_size.1))
            .text_color(theme::text())
            .overflow_hidden()
            .child(TextElement { input: cx.entity() })
    }
}

/// Collapses line breaks so the input stays single-line.
fn single_line(text: &str) -> String {
    if !text.contains(['\r', '\n']) {
        return text.to_owned();
    }
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

/// Converts a UTF-16 range inside `text` to a byte range (clamped to `text`).
pub(crate) fn utf16_range_in(text: &str, range: &Range<usize>) -> Range<usize> {
    let to_utf8 = |target: usize| {
        let (mut utf8, mut utf16) = (0, 0);
        for ch in text.chars() {
            if utf16 >= target {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    };
    to_utf8(range.start)..to_utf8(range.end)
}

/// Scroll offset that keeps the cursor inside a viewport of `viewport` width.
fn scroll_to_reveal(current: Pixels, cursor_x: Pixels, line_width: Pixels, viewport: Pixels) -> Pixels {
    if viewport <= px(0.0) {
        return px(0.0);
    }
    let mut scroll = current;
    if cursor_x < scroll {
        scroll = cursor_x;
    } else if cursor_x > scroll + viewport {
        scroll = cursor_x - viewport;
    }
    // Do not leave blank space on the right once text shrinks.
    let max_scroll = (line_width - viewport).max(px(0.0));
    scroll.min(max_scroll).max(px(0.0))
}

fn masked_display_offset(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())].chars().count() * MASK.len()
}

fn masked_content_offset(content: &str, display_offset: usize) -> usize {
    let chars = display_offset / MASK.len();
    content.char_indices().nth(chars).map_or(content.len(), |(index, _)| index)
}

/// Characters that attach to the preceding character within one user-perceived character.
fn is_extender(ch: char) -> bool {
    matches!(ch as u32,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F
        | 0x0900..=0x0903 | 0x093A..=0x094F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200C | 0x200D
        | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F
        | 0xE0100..=0xE01EF)
}

fn is_regional_indicator(ch: char) -> bool {
    matches!(ch as u32, 0x1F1E6..=0x1F1FF)
}

/// Start offsets of approximate grapheme clusters: combining marks, variation selectors, emoji
/// modifiers and ZWJ sequences stay with their base; regional indicators pair into flags; CRLF is one cluster.
fn cluster_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut previous: Option<char> = None;
    let mut joined = false;
    let mut indicator_run = 0usize;
    for (index, ch) in text.char_indices() {
        let attach = match previous {
            None => false,
            Some(prev) => {
                joined
                    || is_extender(ch)
                    || (prev == '\r' && ch == '\n')
                    || (is_regional_indicator(ch) && is_regional_indicator(prev) && indicator_run % 2 == 1)
            }
        };
        if !attach {
            starts.push(index);
        }
        indicator_run = if is_regional_indicator(ch) { indicator_run + 1 } else { 0 };
        joined = ch == '\u{200D}';
        previous = Some(ch);
    }
    starts
}

pub(crate) fn previous_boundary(text: &str, offset: usize) -> usize {
    cluster_starts(text).into_iter().rev().find(|&index| index < offset).unwrap_or(0)
}

pub(crate) fn next_boundary(text: &str, offset: usize) -> usize {
    cluster_starts(text).into_iter().find(|&index| index > offset).unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_boundaries_step_one_byte() {
        assert_eq!(previous_boundary("abc", 2), 1);
        assert_eq!(next_boundary("abc", 1), 2);
        assert_eq!(previous_boundary("abc", 0), 0);
        assert_eq!(next_boundary("abc", 3), 3);
    }

    #[test]
    fn combining_marks_and_emoji_sequences_delete_as_one() {
        let accented = "e\u{301}x"; // e + combining acute
        assert_eq!(next_boundary(accented, 0), 3);
        assert_eq!(previous_boundary(accented, 3), 0);

        let family = "a👨\u{200D}👩\u{200D}👧b";
        let start = 1;
        let end = family.len() - 1;
        assert_eq!(next_boundary(family, start), end);
        assert_eq!(previous_boundary(family, end), start);

        let thumbs = "👍🏽";
        assert_eq!(next_boundary(thumbs, 0), thumbs.len());
    }

    #[test]
    fn regional_indicators_pair_into_flags() {
        let flags = "🇺🇸🇫🇷";
        let half = flags.len() / 2;
        assert_eq!(next_boundary(flags, 0), half);
        assert_eq!(previous_boundary(flags, flags.len()), half);
    }

    #[test]
    fn boundaries_are_always_char_boundaries() {
        let text = "日本e\u{301}🇯🇵👍🏽x";
        let mut offset = 0;
        while offset < text.len() {
            let next = next_boundary(text, offset);
            assert!(next > offset && text.is_char_boundary(next));
            assert_eq!(previous_boundary(text, next), offset);
            offset = next;
        }
    }

    #[test]
    fn mask_offsets_round_trip() {
        let content = "aé😀z";
        for (index, _) in content.char_indices().chain([(content.len(), ' ')]) {
            let display = masked_display_offset(content, index);
            assert_eq!(masked_content_offset(content, display), index);
        }
        assert_eq!(masked_display_offset(content, content.len()), 4 * MASK.len());
    }

    #[test]
    fn single_line_flattens_breaks() {
        assert_eq!(single_line("a\r\nb\nc\rd"), "a b c d");
        assert_eq!(single_line("plain"), "plain");
    }

    #[test]
    fn utf16_ranges_convert_to_bytes() {
        assert_eq!(utf16_range_in("a😀b", &(1..3)), 1..5);
        assert_eq!(utf16_range_in("ab", &(0..9)), 0..2);
    }

    #[test]
    fn scroll_keeps_cursor_visible() {
        let viewport = px(100.0);
        assert_eq!(scroll_to_reveal(px(0.0), px(50.0), px(80.0), viewport), px(0.0));
        assert_eq!(scroll_to_reveal(px(0.0), px(250.0), px(300.0), viewport), px(150.0));
        assert_eq!(scroll_to_reveal(px(150.0), px(20.0), px(300.0), viewport), px(20.0));
        // Shrinking text pulls the scroll back so no blank tail remains.
        assert_eq!(scroll_to_reveal(px(150.0), px(120.0), px(120.0), viewport), px(20.0));
    }
}
