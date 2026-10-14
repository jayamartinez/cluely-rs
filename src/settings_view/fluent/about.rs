//! Settings › About (Paper "Settings · About"): the app and its version, Updates (where the last check
//! stands, or the newer version's notes with Download, and checking automatically), then first-run
//! setup, the source code and Quit — without a Dock or menu bar, the tray and this are how to quit.

use gpui::{AnyElement, Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, Styled, div, prelude::*, px};

use crate::settings_view::about::spinner;
use super::{app_tile, button, card, card_icon, card_label, chevron, expander, expander_header, external, section, status, title};
use crate::overlay::Overlay;
use crate::settings_view::listen;
use crate::update::{self, Release, Status};
use crate::{archive, theme, ui};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SOURCE: &str = "https://github.com/jayamartinez/cluely-rs";
/// Release notes shown in the update card.
const NOTE_LINES: usize = 3;

impl Overlay {
    pub(super) fn fluent_about(&self, cx: &mut Context<Self>) -> Div {
        let hero = div().flex().items_center().gap(px(20.0)).p(px(20.0)).rounded(px(6.0)).bg(theme::field()).border_1().border_color(theme::divider())
            .child(app_tile(88.0, 16.0, 58.0))
            .child(div().flex().flex_col().gap(px(2.0))
                .child(div().text_size(px(20.0)).line_height(px(28.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("CluelyRS"))
                .child(status(format!("Version {VERSION} · MIT License"))));
        let updates = vec![
            self.fluent_update_card(cx),
            card("check-for-updates").child(card_icon("icons/setup-refresh.svg")).child(card_label("Check for updates automatically", None))
                .child(self.fluent_switch("check-for-updates-switch", self.store.value.check_for_updates, |s, v| s.check_for_updates = v, cx))
                .into_any_element(),
        ];
        let more = vec![
            card("first-run").child(card_icon("icons/setup-flag.svg")).child(card_label("First-run setup", None))
                .child(button("run-setup", "Run setup again", false)
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| { super::close(this, cx); this.open_setup(window, cx) })))
                .into_any_element(),
            card("source-code").cursor_pointer().hover(|card| card.bg(theme::raised()))
                .on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url(SOURCE)))
                .child(card_icon("icons/setup-link.svg")).child(card_label("Source code", Some(status("MIT License").into_any_element())))
                .child(external(12.0, theme::body()))
                .into_any_element(),
            card("quit").child(card_icon("icons/setup-power.svg")).child(card_label("Quit CluelyRS", None))
                .child(button("quit-button", "Quit", false).on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.quit(window, cx))))
                .into_any_element(),
        ];
        div().flex().flex_col().gap(px(24.0))
            .child(title("About"))
            .child(hero)
            .child(section("Updates", updates))
            .child(section("More", more))
    }

    /// Where the last check stands, with Check for updates; or the newer version as an expander.
    fn fluent_update_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.store.value;
        let checked = settings.last_update_check.map(|then| update::ago_label(then, archive::unix_now()));
        if self.updates.status == Status::Available && let Some(release) = &self.updates.latest {
            return self.fluent_available(release, checked, cx);
        }
        let checked_detail = |what: &str| match &checked { Some(ago) => format!("{what} · Checked {ago}"), None => what.to_string() };
        let ok = || ui::icon("icons/check-circle.svg", 20.0, theme::ok()).into_any_element();
        let (icon, label, detail, action) = match self.updates.status {
            Status::NotChecked => (card_icon("icons/update.svg").into_any_element(), format!("CluelyRS {VERSION}"),
                checked.as_ref().map_or_else(|| "Not checked yet".to_string(), |ago| format!("Last checked {ago}")), "Check for updates"),
            Status::Checking => (spinner(), "Checking for updates…".to_string(), format!("You have {VERSION}"), "Check for updates"),
            Status::UpToDate | Status::Available => (ok(), "You're up to date".to_string(), checked_detail(&format!("{VERSION} is the latest")), "Check for updates"),
            Status::NoReleases => (ok(), "You're up to date".to_string(), checked_detail("No releases yet"), "Check for updates"),
            Status::Failed => (ui::icon("icons/alert-circle.svg", 20.0, theme::muted()).into_any_element(), "Couldn't check for updates".to_string(),
                if settings.check_for_updates { "You may be offline. It tries again later." } else { "You may be offline." }.to_string(), "Retry"),
        };
        let checking = self.updates.status == Status::Checking;
        card("update-status").child(icon).child(card_label(label, Some(status(detail).into_any_element())))
            .child(button("check-updates", action, false)
                .when(checking, |button| button.opacity(0.45).cursor_default())
                .when(!checking, |button| button.on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.check_for_updates(window, cx)))))
            .into_any_element()
    }

    /// A newer version: Download (the release page) and, while open, its notes.
    fn fluent_available(&self, release: &Release, checked: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let open = self.fluent_expanded("update");
        let mut line = release.published.as_deref().and_then(update::released_label).map(|date| format!("Released {date}")).unwrap_or_else(|| format!("You have {VERSION}"));
        if let Some(ago) = checked { line.push_str(&format!(" · checked {ago}")); }
        let page = release.page.clone();
        let header = expander_header().id("update-header").cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.fluent_toggle("update", cx)))
            .child(ui::icon("icons/update.svg", 20.0, theme::accent_soft()))
            .child(card_label(format!("CluelyRS {} is available", release.version), Some(status(line).into_any_element())))
            .child(button("update-download", "Download", true).child(external(10.0, theme::accent_ink()))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| { cx.stop_propagation(); cx.open_url(&page) }))
            .child(chevron(open));
        let body = open.then(|| {
            let (lines, more) = update::note_lines(&release.notes, NOTE_LINES);
            let page = release.page.clone();
            let link = if more > 0 { format!("And {more} more in the release notes") } else { "View all release notes".to_string() };
            vec![div().flex().flex_col().gap(px(4.0)).pt(px(12.0)).pb(px(14.0)).pl(px(52.0)).pr(px(16.0))
                .bg(theme::fluent_expander()).border_t_1().border_color(theme::divider())
                .text_size(px(13.0)).line_height(px(20.0))
                .children(lines.into_iter().map(|line| div().text_color(theme::body()).child(line)))
                .child(div().id("release-notes").flex().items_center().gap(px(6.0)).cursor_pointer().text_color(theme::accent_soft())
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| cx.open_url(&page))
                    .child(link).child(external(10.0, theme::accent_soft())))
                .into_any_element()]
        });
        expander("update", header, body).into_any_element()
    }
}
