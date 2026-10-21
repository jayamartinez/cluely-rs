//! Attaching files to the question being typed (Paper "Features Oct 2026" F3): the paperclip's file
//! picker, a drop anywhere on the card, or Ctrl V (⌘V) while the text box has the keyboard. Chips sit
//! above the text in the field, each with its name, type and size and ✕; a refused file keeps its
//! chip, outlined, with the reason, and the line under the chips says what is allowed. While files
//! are dragged over the card, the text row becomes the drop target. What is attached goes with the
//! next question only (`attachments`).
//!
//! Nothing here takes the keyboard or activates the overlay. Windows sends drag and drop to the
//! window under the pointer without activating it (and GPUI's activate is a no-op there); the card
//! is a control area, so it isn't click-through while the pointer is over it. Pasting is a capture
//! listener on the text box, so it only runs when the box already has the keyboard.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    ClipboardEntry, Context, Div, DragMoveEvent, ExternalPaths, FontWeight, ImageFormat, InteractiveElement, IntoElement, MouseButton,
    ParentElement, PathPromptOptions, Rgba, Styled, StyledImage, Window, div, img, px,
};

use super::Overlay;
use crate::attachments::{self, Attachment, Content, MAX_FILES, Refusal};
use crate::{platform, theme, ui};

/// Room the window keeps under the card for chips (up to three rows) and the line under them.
pub(super) const ROOM: f32 = 150.0;

pub(super) enum Chip {
    Reading { id: u64, name: String },
    Ready { id: u64, attachment: Attachment },
    Refused { id: u64, name: String, refusal: Refusal },
}

impl Chip {
    fn id(&self) -> u64 { match self { Self::Reading { id, .. } | Self::Ready { id, .. } | Self::Refused { id, .. } => *id } }
}

/// The question being typed: its chips, and what is being dragged over the card.
#[derive(Default)]
pub(super) struct Attached {
    chips: Vec<Chip>,
    next: u64,
    /// "notes.docx · 1 file" while files are dragged over the card.
    pub(super) drop_over: Option<String>,
}

impl Attached {
    pub(super) fn is_empty(&self) -> bool { self.chips.is_empty() }

    /// A file is being read; the question waits for it.
    pub(super) fn reading(&self) -> bool { self.chips.iter().any(|chip| matches!(chip, Chip::Reading { .. })) }

    pub(super) fn has_ready(&self) -> bool { self.chips.iter().any(|chip| matches!(chip, Chip::Ready { .. })) }

    /// A chip for `name`, being read, unless the question already has its five files: then it is
    /// refused at once. Returns the id to finish it with.
    fn admit(&mut self, name: String) -> Option<u64> {
        self.next += 1;
        let id = self.next;
        let kept = self.chips.iter().filter(|chip| !matches!(chip, Chip::Refused { .. })).count();
        if kept >= MAX_FILES {
            self.chips.push(Chip::Refused { id, name, refusal: Refusal::TooMany });
            return None;
        }
        self.chips.push(Chip::Reading { id, name });
        Some(id)
    }

    /// The file behind chip `id` was read (or refused). A chip removed meanwhile stays removed.
    fn finish(&mut self, id: u64, result: Result<Attachment, Refusal>) {
        let Some(chip) = self.chips.iter_mut().find(|chip| chip.id() == id) else { return };
        let name = match chip { Chip::Reading { name, .. } => std::mem::take(name), _ => return };
        *chip = match result {
            Ok(attachment) => Chip::Ready { id, attachment },
            Err(refusal) => Chip::Refused { id, name, refusal },
        };
    }

    fn remove(&mut self, id: u64) { self.chips.retain(|chip| chip.id() != id); }

    /// Sending: the files read go with the question; refused chips are cleared too.
    pub(super) fn take(&mut self) -> Arc<[Attachment]> {
        std::mem::take(&mut self.chips).into_iter().filter_map(|chip| match chip { Chip::Ready { attachment, .. } => Some(attachment), _ => None }).collect()
    }

    /// The line under the chips: why the last refused file won't be sent.
    fn message(&self) -> Option<String> {
        self.chips.iter().rev().find_map(|chip| match chip { Chip::Refused { name, refusal, .. } => Some(refusal.message(name)), _ => None })
    }
}

/// "notes.docx · 1 file", "notes.docx and 2 more · 3 files".
fn dragged(paths: &[PathBuf]) -> String {
    let first = paths.first().and_then(|path| path.file_name()).map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    match paths.len() {
        0 | 1 => format!("{first} · 1 file"),
        count => format!("{first} and {} more · {count} files", count - 1),
    }
}

impl Overlay {
    /// Attach files by path (picked, dropped or pasted), each read off the UI thread.
    pub(super) fn attach_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let takes_images = attachments::takes_images(&self.store.value);
        for path in paths {
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned());
            let Some(id) = self.attached.admit(name) else { continue };
            let read = cx.background_executor().spawn(async move { attachments::read(&path, takes_images) });
            self.finish_reading(id, read, cx);
        }
        self.fit(window);
        cx.notify();
    }

    /// Attach a pasted image (`attachments::pasted_image` for a bitmap without a file).
    fn attach_pasted(&mut self, data: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let takes_images = attachments::takes_images(&self.store.value);
        if let Some(id) = self.attached.admit(attachments::PASTED.into()) {
            let read = cx.background_executor().spawn(async move { attachments::pasted_image(&data, takes_images) });
            self.finish_reading(id, read, cx);
        }
        self.fit(window);
        cx.notify();
    }

    fn finish_reading(&mut self, id: u64, read: gpui::Task<Result<Attachment, Refusal>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |this, cx| { this.attached.finish(id, result); cx.notify(); });
        }).detach();
    }

    /// The paperclip: the system file picker (a window of its own, which takes the keyboard while
    /// it's open, as the user asked for it).
    fn browse_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update_in(cx, |this, window, cx| this.attach_paths(paths, window, cx));
        }).detach();
    }

    /// Ctrl V / ⌘V in the text box: copied files or an image become chips; text pastes as usual.
    /// Only reached while the box has the keyboard, so a paste meant for another app is never taken.
    fn paste_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer.read(cx).has_keyboard(window) { return; }
        let files = platform::pasted_files();
        if !files.is_empty() {
            cx.stop_propagation();
            self.attach_paths(files, window, cx);
            return;
        }
        let item = cx.read_from_clipboard();
        let image = item.as_ref().and_then(|item| item.entries().iter().find_map(|entry| match entry {
            ClipboardEntry::Image(image) if matches!(image.format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Bmp) => Some(image.bytes.clone()),
            _ => None,
        }));
        // Text on the clipboard pastes as text, even when an app put an image beside it.
        let bitmap = || item.as_ref().and_then(|item| item.text()).is_none().then(platform::pasted_bitmap).flatten();
        if let Some(data) = image.or_else(bitmap) {
            cx.stop_propagation();
            self.attach_pasted(data, window, cx);
        }
    }

    fn remove_chip(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.attached.remove(id);
        self.fit(window);
        cx.notify();
    }

    /// Files are being dragged over the card: the text row turns into the drop target.
    fn drag_over_card(&mut self, event: &DragMoveEvent<ExternalPaths>, cx: &mut Context<Self>) {
        let over = event.bounds.contains(&event.event.position).then(|| dragged(event.drag(cx).paths()));
        if over != self.attached.drop_over {
            self.attached.drop_over = over;
            cx.notify();
        }
    }

    /// The drag left the window or was cancelled: GPUI ends it without telling the card.
    pub(super) fn drag_ended(&mut self, cx: &mut Context<Self>) {
        if self.attached.drop_over.is_some() && !cx.has_active_drag() {
            self.attached.drop_over = None;
            cx.notify();
        }
    }

    /// The card's drop listeners: anything dropped on it is attached.
    pub(super) fn drop_target(&self, card: gpui::Stateful<Div>, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        card.on_drag_move::<ExternalPaths>(cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| this.drag_over_card(event, cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.attached.drop_over = None;
                this.attach_paths(paths.paths().to_vec(), window, cx);
            }))
    }

    /// The text box with its paste listener, the paperclip and the return button (`card::input_row`).
    pub(super) fn text_row(&self, send: impl IntoElement, cx: &mut Context<Self>) -> Div {
        let paperclip = div().id("attach").size(px(28.0)).flex_none().rounded(px(theme::radius::BUTTON)).flex().items_center().justify_center().cursor_pointer()
            .hover(|button| button.bg(theme::pressed()))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.browse_attachments(window, cx); }))
            .child(ui::icon("icons/paperclip.svg", 15.0, theme::body()));
        div().w_full().min_w_0().flex().items_center().gap(px(10.0))
            .child(div().id("composer-input").flex_1().min_w_0().cursor_text()
                .capture_action(cx.listener(|this, _: &crate::input::Paste, window, cx| this.paste_attachments(window, cx)))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus_text_box(window, cx)))
                .child(self.composer.clone()))
            .child(paperclip)
            .child(send)
    }

    /// The chips and the line under them, above the text row; `None` without any.
    pub(super) fn chips(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.attached.is_empty() { return None; }
        let row = div().flex().flex_wrap().gap(px(6.0)).children(self.attached.chips.iter().map(|chip| self.chip(chip, cx)));
        let message = self.attached.message().map(|message| div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::danger()).child(message));
        Some(div().flex().flex_col().gap(px(10.0)).child(row).children(message))
    }

    fn chip(&self, chip: &Chip, cx: &mut Context<Self>) -> Div {
        let id = chip.id();
        let (name, detail, refused): (&str, String, bool) = match chip {
            Chip::Reading { name, .. } => (name, "Reading…".into(), false),
            Chip::Ready { attachment, .. } => (&attachment.name, attachment.detail.clone(), false),
            Chip::Refused { name, refusal, .. } => (name, refusal.reason(), true),
        };
        let thumbnail = match chip { Chip::Ready { attachment, .. } => attachment.jpeg(), _ => None };
        let tile = |icon: &'static str, size: f32, color: Rgba| div().size(px(22.0)).flex_none().rounded(px(4.0)).bg(theme::pressed()).flex().items_center().justify_center()
            .child(ui::icon(icon, size, color));
        let leading = match thumbnail {
            Some(jpeg) => preview(jpeg, 40.0, 28.0).into_any_element(),
            None if refused => tile("icons/alert-circle.svg", 13.0, theme::danger()).into_any_element(),
            None => tile("icons/file.svg", 12.0, theme::body()).into_any_element(),
        };
        let remove = div().id(("remove-attachment", id as usize)).size(px(20.0)).flex_none().rounded(px(4.0)).flex().items_center().justify_center().cursor_pointer()
            .hover(|button| button.bg(theme::pressed()))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { cx.stop_propagation(); this.remove_chip(id, window, cx); }))
            .child(ui::icon("icons/close.svg", 10.0, theme::muted()));
        div().h(px(36.0)).max_w(px(260.0)).flex().items_center().gap(px(8.0)).pl(px(if thumbnail.is_some() { 4.0 } else { 8.0 })).pr(px(6.0))
            .rounded(px(theme::radius::CHIP)).border_1().border_color(if refused { theme::danger() } else { theme::hairline() }).bg(theme::raised())
            .child(leading)
            .child(div().flex().flex_col().min_w_0()
                .child(div().truncate().text_size(px(12.0)).line_height(px(15.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(name.to_string()))
                .child(div().truncate().text_size(px(11.0)).line_height(px(13.0)).text_color(if refused { theme::danger() } else { self.dim(theme::muted()) }).child(detail)))
            .child(remove)
    }

    /// The text row while files are dragged over the card.
    pub(super) fn drop_zone(&self, dragged: &str) -> Div {
        div().h(px(64.0)).flex().items_center().justify_center().gap(px(10.0)).rounded(px(theme::radius::CHIP))
            .border_1().border_dashed().border_color(theme::accent()).bg(theme::bubble())
            .child(ui::icon("icons/drop.svg", 16.0, theme::accent_soft()))
            .child(div().text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Drop to attach"))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::accent_soft()).child(dragged.to_string()))
    }
}

/// An attached image, small, as its chip and the sent question show it.
pub(super) fn preview(jpeg: &[u8], width: f32, height: f32) -> gpui::Img {
    let image = Arc::new(gpui::Image::from_bytes(ImageFormat::Jpeg, jpeg.to_vec()));
    img(image).w(px(width)).h(px(height)).flex_none().rounded(px(if height < 20.0 { 3.0 } else { 4.0 })).object_fit(gpui::ObjectFit::Cover)
}

/// Under a sent question: one small chip per file it took.
pub(super) fn sent(attachments: &[Attachment]) -> Div {
    div().flex().flex_wrap().justify_end().gap(px(6.0)).children(attachments.iter().map(|attachment| {
        let leading = match &attachment.content {
            Content::Jpeg(jpeg) => preview(jpeg, 24.0, 16.0).into_any_element(),
            Content::Text(_) => ui::icon("icons/file.svg", 11.0, theme::muted()).into_any_element(),
        };
        div().h(px(24.0)).max_w(px(220.0)).flex().items_center().gap(px(6.0)).pl(px(if attachment.jpeg().is_some() { 3.0 } else { 8.0 })).pr(px(8.0))
            .rounded(px(theme::radius::CHIP)).border_1().border_color(theme::hairline()).bg(theme::raised())
            .child(leading)
            .child(div().truncate().text_size(px(11.0)).line_height(px(14.0)).text_color(theme::body()).child(attachment.name.clone()))
    }))
}

/// "Read 2 attachments" above the answer to a question that had files.
pub(super) fn read_line(count: usize, cut: &[String], color: Rgba) -> Div {
    div().flex().items_center().gap(px(6.0))
        .child(ui::icon("icons/paperclip.svg", 12.0, color))
        .child(div().text_size(px(11.0)).line_height(px(14.0)).text_color(color).child(attachments::read_label(count, cut)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::Kind;

    fn ready(name: &str) -> Attachment {
        Attachment { name: name.into(), kind: Kind::Txt, bytes: 1, detail: String::new(), content: Content::Text("x".into()) }
    }

    /// Five files per question: a sixth is refused with its own chip, a refused chip doesn't count,
    /// removing one makes room, and sending takes only the files read, clearing every chip.
    #[test]
    fn a_question_takes_five_files_and_sending_clears_the_chips() {
        let mut attached = Attached::default();
        let ids: Vec<u64> = (1..=5).map(|n| attached.admit(format!("{n}.txt")).unwrap()).collect();
        assert!(attached.reading() && !attached.has_ready());
        assert_eq!(attached.admit("6.txt".into()), None);
        assert_eq!(attached.message().unwrap(), "6.txt won't be sent: a question takes up to 5 files.");
        attached.finish(ids[0], Ok(ready("1.txt")));
        attached.finish(ids[1], Err(Refusal::Unsupported("Video isn't supported")));
        assert_eq!(attached.message().unwrap(), "6.txt won't be sent: a question takes up to 5 files.", "the last refused chip speaks");
        // The refused second file no longer counts: one more fits.
        let seventh = attached.admit("7.txt".into()).unwrap();
        attached.remove(ids[2]);
        attached.finish(ids[2], Ok(ready("3.txt")));
        assert!(attached.chips.iter().all(|chip| chip.id() != ids[2]), "a chip removed while reading stays removed");
        for id in [ids[3], ids[4], seventh] { attached.finish(id, Ok(ready("n.txt"))); }
        assert!(!attached.reading() && attached.has_ready());
        let sent = attached.take();
        assert_eq!(sent.len(), 4);
        assert_eq!(sent[0].name, "1.txt");
        assert!(attached.is_empty() && attached.message().is_none());
    }

    #[test]
    fn the_drop_target_says_what_is_dragged() {
        assert_eq!(dragged(&[PathBuf::from("/Users/me/notes.docx")]), "notes.docx · 1 file");
        assert_eq!(dragged(&[PathBuf::from("a.pdf"), PathBuf::from("b.png"), PathBuf::from("c.md")]), "a.pdf and 2 more · 3 files");
    }
}
