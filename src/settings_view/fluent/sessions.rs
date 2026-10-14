//! Settings › Sessions (Paper "Settings · Sessions"): what's saved and for how long, then the saved
//! sessions themselves: how many, their size, and the folder and the Sessions window.

use gpui::{Context, Div, IntoElement, MouseButton, ParentElement, Styled, Window, div, prelude::*, px};

use super::{button, card, card_icon, card_label, section, status, title};
use crate::archive::{Archive, Retention};
use crate::overlay::Overlay;
use crate::settings_view::{Picker, listen};

const RETENTION: [(Retention, &str); 3] = [(Retention::Days7, "7 days"), (Retention::Days30, "30 days"), (Retention::Forever, "Forever")];

/// "128 sessions", "1 session", "No sessions yet".
fn count_label(count: usize) -> String {
    match count { 0 => "No sessions yet".into(), 1 => "1 session".into(), n => format!("{n} sessions") }
}

/// "412 MB on this PC" (`folder_bytes`).
fn size_label(bytes: u64) -> String {
    match bytes {
        0 => "Nothing saved yet".into(),
        b if b < 1_000_000 => format!("{} KB on this PC", (b as f64 / 1e3).ceil()),
        b => format!("{:.0} MB on this PC", b as f64 / 1e6),
    }
}

impl Overlay {
    pub(super) fn fluent_sessions(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut saving = vec![self.toggle_card("save-sessions", "icons/setup-history.svg", "Save sessions", s.save_sessions, |s, v| s.save_sessions = v, cx)];
        if s.save_sessions {
            let id = |retention: Retention| format!("{retention:?}");
            let current = RETENTION.iter().find(|(retention, _)| *retention == s.keep_sessions).map_or("30 days", |(_, label)| *label);
            saving.push(self.toggle_card("save-screenshots", "icons/setup-pictures.svg", "Save screenshots", s.save_screenshots, |s, v| s.save_screenshots = v, cx));
            saving.push(card("keep-for").child(card_icon("icons/setup-calendar.svg")).child(card_label("Keep for", None))
                .child(self.fluent_combo(Picker::Retention, None, current, RETENTION.iter().map(|(retention, label)| (id(*retention), label.to_string())).collect(),
                    &id(s.keep_sessions), |s, v| s.keep_sessions = RETENTION.iter().find(|(retention, _)| format!("{retention:?}") == v).map_or(s.keep_sessions, |(retention, _)| *retention),
                    200.0, cx))
                .into_any_element());
        }
        let yours = card("your-sessions").child(card_icon("icons/archive.svg"))
            .child(card_label(self.fluent.session_count.map_or_else(|| "Sessions".to_string(), count_label),
                self.archive_bytes.map(|bytes| status(size_label(bytes)).into_any_element())))
            .child(button("open-archive", "Open folder", false).on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, _| this.reveal_archive())))
            .child(button("view-history", "Open sessions", true).on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.open_sessions(cx))))
            .into_any_element();
        div().flex().flex_col().gap(px(24.0))
            .child(title("Sessions"))
            .child(section("Saving", saving))
            .child(section("Your sessions", vec![yours]))
    }

    /// Count the saved sessions off the UI thread (it reads each one's record).
    pub(super) fn fluent_count_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.archive.as_ref().map(|archive| archive.root().to_path_buf()) else { return };
        cx.spawn_in(window, async move |this, cx| {
            let count = cx.background_executor().spawn(async move { Archive::at(root).list().len() }).await;
            let _ = this.update(cx, |this, cx| { this.fluent.session_count = Some(count); cx.notify(); });
        }).detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_sizes_read_as_drawn() {
        assert_eq!((count_label(0), count_label(1), count_label(128)), ("No sessions yet".into(), "1 session".into(), "128 sessions".into()));
        assert_eq!(size_label(412_000_000), "412 MB on this PC");
        assert_eq!(size_label(1_500), "2 KB on this PC");
    }
}
