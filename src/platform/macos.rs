//! macOS. Folders open in Finder. The overlay's native window behaviour (level, click-through,
//! focus, capture exclusion, keyboard-driven movement) is not implemented yet: `native_window`
//! returns `None`, so the overlay runs as a plain GPUI window and the window functions below
//! are unreachable.

use std::path::Path;
use std::process::Command;

use gpui::Window;

/// The overlay's own window. No values exist until macOS window handling is implemented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeWindow {}
/// The app that had the keyboard before the overlay took it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviousFocus {}

/// A rounded rectangle in physical pixels relative to the window: left, top, right, bottom, radius.
pub type Shape = (i32, i32, i32, i32, i32);

pub fn native_window(_window: &Window) -> Option<NativeWindow> { None }

pub fn set_capture_hidden(window: NativeWindow, _hidden: bool) -> std::io::Result<()> { match window {} }

pub fn remove_frame(window: NativeWindow) { match window {} }

pub fn set_topmost(window: NativeWindow) -> std::io::Result<()> { match window {} }

pub fn move_by(window: NativeWindow, _dx: i32, _dy: i32) -> std::io::Result<()> { match window {} }

pub fn held_direction(_with_shift: bool) -> Option<(i32, i32)> { None }

pub fn left_button_down() -> bool { false }

pub fn set_shape(window: NativeWindow, _shapes: &[Shape]) { match window {} }

pub fn center(window: NativeWindow) -> Option<(i32, i32)> { match window {} }

pub fn enable_passthrough(window: NativeWindow) { match window {} }

pub fn set_mouse_passthrough(window: NativeWindow, _on: bool) { match window {} }

pub fn cursor_in_window(window: NativeWindow) -> Option<(i32, i32)> { match window {} }

pub fn take_focus(window: NativeWindow) -> Option<PreviousFocus> { match window {} }

pub fn return_focus(previous: PreviousFocus) { match previous {} }

pub fn is_visible(window: NativeWindow) -> bool { match window {} }

pub fn set_visible(window: NativeWindow, _visible: bool) { match window {} }

/// Open a folder in Finder.
pub fn open_folder(path: &Path) -> std::io::Result<()> { Command::new("open").arg(path).spawn().map(drop) }

/// Open Finder at a file's folder with the file selected.
pub fn reveal_file(path: &Path) -> std::io::Result<()> { Command::new("open").arg("-R").arg(path).spawn().map(drop) }
