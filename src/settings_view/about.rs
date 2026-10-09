//! Settings › About's Updates group, from the Paper "Update check · macOS / Windows · Settings ›
//! About" artboards: where the last check stands (with Check for updates, or the newer version's
//! notes and Download), and the automatic-check switch. macOS shows it in its Settings window under
//! the app's name and version; on Windows it's the panel's About tab.

use gpui::{Animation, AnimationExt, AnyElement, Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, Styled, Transformation, div,
    percentage, prelude::*, px, rgb};

use super::{BOX_PADDING, button, listen, switch, switch_row};
use crate::overlay::Overlay;
use crate::update::{self, Release, Status};
use crate::{archive, theme, ui};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Release notes shown before "and N more changes".
const NOTE_LINES: usize = 3;

impl Overlay {
    /// The Windows panel's About tab: the app's name and version over the Updates group.
    pub(crate) fn about_tab(&self, cx: &mut Context<Self>) -> Div {
        div().flex().flex_col().gap(px(18.0))
            .child(div().flex().items_center().gap(px(12.0))
                .child(ui::mark(36.0))
                .child(div().flex().flex_col().gap(px(2.0))
                    .child(div().text_size(px(15.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("CluelyRS"))
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child(format!("Version {VERSION}")))))
            .child(self.updates_section(cx))
    }

    /// UPDATES: the status row and "Check for updates automatically".
    pub(crate) fn updates_section(&self, cx: &mut Context<Self>) -> Div {
        let automatic = switch_row("Check for updates automatically", "At launch and every 6 hours. Only asks GitHub which version is the latest.",
            switch("check-for-updates", self.store.value.check_for_updates, |s, v| s.check_for_updates = v, cx));
        div().flex().flex_col().gap(px(8.0))
            .child(ui::section_label("UPDATES"))
            .child(div().flex().flex_col().rounded(px(12.0)).border_1().border_color(theme::hairline()).overflow_hidden()
                .child(self.update_status(cx))
                .child(automatic.px(px(BOX_PADDING)).py(px(12.0))))
    }

    fn update_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.store.value;
        let checked = settings.last_update_check.map(|then| update::ago_label(then, archive::unix_now()));
        let checked_detail = |what: &str| match &checked { Some(ago) => format!("{what} · Checked {ago}"), None => what.to_string() };
        if self.updates.status == Status::Available && let Some(release) = &self.updates.latest {
            return self.available(release, checked, cx);
        }
        let ok = || ui::icon("icons/check-circle.svg", 16.0, theme::ok()).into_any_element();
        let (icon, title, detail, action) = match self.updates.status {
            Status::NotChecked => (ui::icon("icons/tab-about.svg", 16.0, theme::muted()).into_any_element(), format!("CluelyRS {VERSION}"),
                checked.as_ref().map_or_else(|| "Not checked yet".to_string(), |ago| format!("Last checked {ago}")), "Check for updates"),
            Status::Checking => (spinner(), "Checking for updates…".to_string(), format!("You have {VERSION}"), "Check for updates"),
            Status::UpToDate | Status::Available => (ok(), "You're up to date".to_string(), checked_detail(&format!("{VERSION} is the latest")), "Check for updates"),
            Status::NoReleases => (ok(), "You're up to date".to_string(), checked_detail("No releases yet"), "Check for updates"),
            Status::Failed => (ui::icon("icons/alert-circle.svg", 16.0, theme::muted()).into_any_element(), "Couldn't check for updates".to_string(),
                if settings.check_for_updates { "You may be offline. It tries again later." } else { "You may be offline." }.to_string(), "Retry"),
        };
        let checking = self.updates.status == Status::Checking;
        let action = button("check-updates", action, false)
            .when(checking, |button| button.opacity(0.45).cursor_default())
            .when(!checking, |button| button.on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.check_for_updates(window, cx))));
        div().flex().items_center().gap(px(10.0)).px(px(BOX_PADDING)).py(px(12.0)).border_b_1().border_color(theme::divider())
            .child(icon)
            .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
                .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child(detail)))
            .child(action)
            .into_any_element()
    }

    /// A newer version: its notes, Download (today the release page; later "Download and restart")
    /// and View release.
    fn available(&self, release: &Release, checked: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let released = release.published.as_deref().and_then(update::released_label).map(|date| format!(" · Released {date}")).unwrap_or_default();
        let (lines, more) = update::note_lines(&release.notes, NOTE_LINES);
        let mut notes = div().flex().flex_col().gap(px(4.0)).pl(px(26.0)).text_size(px(12.0)).line_height(px(17.0));
        for line in lines { notes = notes.child(div().text_color(rgb(0xd9d5cc)).child(line)); }
        if more > 0 {
            notes = notes.child(div().text_color(theme::muted()).child(format!("and {more} more {} in the release notes", if more == 1 { "change" } else { "changes" })));
        }
        let open = |id: &'static str, label: &'static str, primary: bool| {
            let page = release.page.clone();
            button(id, label, primary).on_mouse_down(MouseButton::Left, move |_, _, cx| cx.open_url(&page))
        };
        let actions = div().flex().items_center().gap(px(8.0)).pl(px(26.0)).pt(px(2.0))
            .child(open("update-download", "Download ↗", true))
            .child(open("update-release", "View release", false))
            .child(div().flex_1())
            .when_some(checked, |row, ago| row.child(div().text_size(px(12.0)).text_color(theme::muted()).child(format!("Checked {ago}"))))
            .child(div().id("check-again").cursor_pointer().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft())
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.check_for_updates(window, cx)))
                .child("Check again"));
        div().flex().flex_col().gap(px(12.0)).px(px(BOX_PADDING)).py(px(14.0)).bg(theme::bubble()).border_b_1().border_color(theme::bubble_border())
            .child(div().flex().items_start().gap(px(10.0))
                .child(div().pt(px(1.0)).child(ui::icon("icons/update.svg", 16.0, theme::accent_soft())))
                .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text())
                        .child(format!("CluelyRS {} is available", release.version)))
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child(format!("You have {VERSION}{released}")))))
            .child(notes)
            .child(actions)
            .into_any_element()
    }
}

fn spinner() -> AnyElement {
    gpui::svg().path("icons/spinner.svg").size(px(16.0)).flex_none().text_color(theme::accent_soft())
        .with_animation("update-checking", Animation::new(std::time::Duration::from_millis(900)).repeat(),
            |spinner, delta| spinner.with_transformation(Transformation::rotate(percentage(delta))))
        .into_any_element()
}
