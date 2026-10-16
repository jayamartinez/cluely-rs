//! Settings › Shortcuts, Window (with the overlay's preview), Sessions and About.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, Div, DragMoveEvent, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement, Pixels,
    Styled, Window, div, linear_color_stop, linear_gradient, prelude::*, px, relative, rgb,
};

use super::{action, choice, label, link, row, section, status, switch};
use crate::appearance::{self, CardWidth, TextSize};
use crate::archive::Retention;
use crate::hotkeys::Action;
use crate::overlay::Overlay;
use crate::settings_view::about::spinner;
use crate::settings_view::listen;
use crate::ui_v3::{self, Kind, RADII};
use crate::update::{self, Status};
use crate::{archive, theme, ui};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SOURCE_CODE: &str = "https://github.com/jayamartinez/cluely-rs";
/// Release notes shown before "and N more changes".
const NOTE_LINES: usize = 3;

/// Settings › Shortcuts' rows: (icon, name, actions). Moving and scrolling are one row each.
const LIVE: [(&str, &str, &[Action]); 3] = [
    ("icons/setup-sparkle.svg", "Assist", &[Action::Assist]),
    ("icons/setup-field.svg", "Type a question", &[Action::Focus]),
    ("icons/setup-grid.svg", "Switch mode", &[Action::SwitchMode]),
];
const OVERLAY: [(&str, &str, &[Action]); 4] = [
    ("icons/setup-eye.svg", "Show or hide overlay", &[Action::Toggle]),
    ("icons/setup-move.svg", "Move overlay", &[Action::MoveLeft, Action::MoveUp, Action::MoveDown, Action::MoveRight]),
    ("icons/setup-scroll.svg", "Scroll answer", &[Action::ScrollUp, Action::ScrollDown]),
    ("icons/close.svg", "Close settings", &[Action::Close]),
];

/// A click handler from the window that draws a page (About's Run setup again).
pub(crate) type WindowAction = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// What GPUI carries while the opacity knob is dragged.
struct OpacityDrag;

/// Chords that differ only in their last key, as one: "⌃ ⌥ ←" and "⌃ ⌥ →" are "⌃ ⌥ ← →".
fn join_keys(labels: &[String]) -> String {
    if labels.len() == 1 { return labels[0].clone(); }
    let modifiers = labels[0].rsplit_once(' ').map_or("", |(modifiers, _)| modifiers);
    let keys: Vec<&str> = labels.iter().filter_map(|label| label.rsplit_once(' ').map(|(_, key)| key)).collect();
    format!("{modifiers} {}", keys.join(" "))
}

impl Overlay {
    /// Each global shortcut as a read-only button that hugs its keys.
    pub(super) fn mac_shortcuts_tab(&self) -> Div {
        let rows = |list: &[(&'static str, &'static str, &[Action])]| -> Vec<AnyElement> {
            list.iter().map(|(icon, title, actions)| {
                let keys = join_keys(&actions.iter().map(|action| self.hotkeys.label(*action)).collect::<Vec<_>>());
                let taken = actions.iter().any(|action| self.hotkeys.unavailable.contains(action));
                let taken = taken.then(|| div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::danger()).child("Used by another app").into_any_element());
                row(icon, *title, taken, div().flex().flex_none().gap(px(4.0)).p(px(4.0)).rounded(px(RADII.control)).bg(theme::field())
                    .border_1().border_color(theme::hairline()).child(ui_v3::keycaps(&keys, true))).into_any_element()
            }).collect()
        };
        div().flex().flex_col().gap(px(30.0))
            .child(section("Live", 12.0, rows(&LIVE)))
            .child(section("Overlay", 12.0, rows(&OVERLAY)))
    }

    pub(super) fn mac_window_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let privacy = vec![
            row("icons/eye-off.svg", "Hide from screen capture", None, switch("hide-capture", s.hide_from_capture, |s, v| s.hide_from_capture = v, cx)).into_any_element(),
            row("icons/setup-pictures.svg", "Attach a screenshot to every message", None, switch("screen-on-send", s.screen_on_send, |s, v| s.screen_on_send = v, cx)).into_any_element(),
            row("icons/setup-power.svg", "Start Live when the app opens", None, switch("live-on-launch", s.start_live_on_launch, |s, v| s.start_live_on_launch = v, cx)).into_any_element(),
        ];
        let percent = appearance::clamp(s.background_opacity);
        let opacity = div().flex().flex_none().items_center().gap(px(12.0))
            .child(self.mac_opacity_slider(percent, cx))
            .child(div().w(px(36.0)).flex().justify_end().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text())
                .child(format!("{percent}%")));
        let appearance = vec![
            div().px(px(12.0)).pb(px(6.0)).child(preview(percent, s.text_size, s.card_width)).into_any_element(),
            row("icons/setup-contrast.svg", "Opacity", None, opacity).into_any_element(),
            row("icons/setup-text-size.svg", "Answer text size", None, choice("text-size", &TextSize::ALL, s.text_size, |s, v| s.text_size = v, cx)).into_any_element(),
            row("icons/setup-width.svg", "Card width", None, choice("card-width", &CardWidth::ALL, s.card_width, |s, v| s.card_width = v, cx)).into_any_element(),
        ];
        // Turning the icon off may ask first (`Overlay::set_menu_bar_icon`).
        let icon_on = s.show_menu_bar_icon;
        let menu_bar_switch = div().id("show-menu-bar-icon").flex_none().cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.set_menu_bar_icon(!icon_on, window, cx) }))
            .child(ui_v3::switch(icon_on));
        let dock = vec![
            row("menu-bar/status-idle.svg", "Show menu bar icon", None, menu_bar_switch).into_any_element(),
            row("icons/panel-bottom.svg", "Show in Dock", None, switch("show-in-dock", s.show_in_dock, |s, v| s.show_in_dock = v, cx)).into_any_element(),
        ];
        div().flex().flex_col().gap(px(30.0))
            .child(section("Privacy", 12.0, privacy))
            .child(section("Appearance", 12.0, appearance))
            .child(section("Menu bar and Dock", 12.0, dock))
    }

    /// The opacity slider: pressing jumps to the pointer and dragging follows it, both shown on the
    /// overlay (and the preview) at once; the release saves.
    fn mac_opacity_slider(&self, percent: u8, cx: &mut Context<Self>) -> impl IntoElement {
        const WIDTH: f32 = 162.0;
        let fraction = appearance::slider_fraction(percent);
        let track: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
        let value_at = {
            let track = track.clone();
            move |x: Pixels| track.get().map(|bounds| appearance::opacity_at(f32::from(x - bounds.left()) / f32::from(bounds.size.width).max(1.0)))
        };
        let pressed = value_at.clone();
        div().id("opacity-slider").relative().w(px(WIDTH)).h(px(20.0)).flex_none().cursor_pointer()
            .child(gpui::canvas(move |bounds, _, _| track.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .child(div().absolute().left_0().top(px(8.0)).w_full().h(px(4.0)).rounded(px(2.0)).bg(theme::hairline()))
            .child(div().absolute().left_0().top(px(8.0)).w(relative(fraction)).h(px(4.0)).rounded(px(2.0)).bg(theme::accent()))
            .child(div().absolute().top(px(2.0)).left(px((WIDTH - 16.0) * fraction)).size(px(16.0)).rounded_full().bg(theme::text())
                .shadow(ui_v3::drop_shadow(1.0, 3.0, 0.5)))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, event: &MouseDownEvent, _, cx| {
                if let Some(value) = pressed(event.position.x) { this.opacity_drag = true; this.preview_opacity(value, cx); }
            }))
            .on_drag(OpacityDrag, |_, _, _, cx| cx.new(|_| gpui::Empty))
            .on_drag_move(listen(cx, move |this, event: &DragMoveEvent<OpacityDrag>, _, cx| {
                if let Some(value) = value_at(event.event.position.x) { this.preview_opacity(value, cx); }
            }))
            .on_mouse_up(MouseButton::Left, listen(cx, |this, _: &MouseUpEvent, _, cx| this.finish_opacity_drag(cx)))
            .on_mouse_up_out(MouseButton::Left, listen(cx, |this, _: &MouseUpEvent, _, cx| this.finish_opacity_drag(cx)))
    }

    pub(super) fn mac_sessions_tab(&self, sessions: Option<usize>, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut saving = vec![row("icons/setup-history.svg", "Save sessions", None, switch("save-sessions", s.save_sessions, |s, v| s.save_sessions = v, cx)).into_any_element()];
        if s.save_sessions {
            saving.push(row("icons/setup-pictures.svg", "Save screenshots", None, switch("save-screenshots", s.save_screenshots, |s, v| s.save_screenshots = v, cx)).into_any_element());
            saving.push(row("icons/setup-calendar.svg", "Keep for", None, choice("retention",
                &[(Retention::Days7, "7 days"), (Retention::Days30, "30 days"), (Retention::Forever, "Forever")], s.keep_sessions, |s, v| s.keep_sessions = v, cx)).into_any_element());
        }
        let title = match sessions { Some(0) => "No saved sessions".to_string(), Some(1) => "1 session".to_string(), Some(count) => format!("{count} sessions"), None => "Saved sessions".to_string() };
        let size = self.archive_bytes.filter(|bytes| *bytes > 0).map(|bytes| match bytes {
            b if b < 1_000_000 => format!("{} KB on this Mac", (b as f64 / 1e3).ceil()),
            b => format!("{:.0} MB on this Mac", b as f64 / 1e6),
        });
        let buttons = div().flex().flex_none().items_center().gap(px(8.0))
            .child(action("open-archive", "Open folder", Kind::Secondary, cx, |this, _, _| this.reveal_archive()))
            .child(action("view-history", "Open sessions", Kind::Primary, cx, |this, _, cx| this.open_sessions(cx)));
        div().flex().flex_col().gap(px(30.0))
            .child(section("Saving", 12.0, saving))
            .child(section("Your sessions", 12.0, vec![row("icons/archive.svg", title, size.map(status), buttons).into_any_element()]))
    }

    /// The app's name and version, where updates stand, setup, the source and Quit. `run_setup` is
    /// the window's (it closes as setup opens); `None` while Live is on.
    pub(crate) fn mac_about(&self, run_setup: Option<WindowAction>, cx: &mut Context<Self>) -> Div {
        let header = div().flex().items_center().gap(px(18.0)).px(px(12.0))
            .child(ui_v3::app_tile(88.0, 22.0, 58.0))
            .child(div().flex().flex_col().gap(px(2.0))
                .child(div().text_size(px(22.0)).line_height(px(28.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("CluelyRS"))
                .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(format!("Version {VERSION}"))));
        let automatic = row("icons/setup-refresh.svg", "Check for updates automatically", None,
            switch("check-for-updates", self.store.value.check_for_updates, |s, v| s.check_for_updates = v, cx));
        let setup = match run_setup {
            Some(run) => ui_v3::button("run-setup", "Run setup again", Kind::Secondary).on_mouse_down(MouseButton::Left, run),
            None => ui_v3::button("run-setup", "Run setup again", Kind::Disabled),
        };
        let more = vec![
            row("icons/setup-flag.svg", "First-run setup", (self.live_since.is_some()).then(|| status("Turn Live off first")), setup).into_any_element(),
            row("icons/setup-link.svg", "Source code", Some(status("MIT License")),
                ui_v3::button("source-code", "Open ↗", Kind::Secondary).on_mouse_down(MouseButton::Left, |_, _, cx| cx.open_url(SOURCE_CODE))).into_any_element(),
            row("icons/setup-power.svg", "Quit CluelyRS", None, action("quit", "Quit", Kind::Secondary, cx, |this, window, cx| this.quit(window, cx))).into_any_element(),
        ];
        div().flex().flex_col().gap(px(30.0))
            .child(div().flex().flex_col().gap(px(16.0)).child(header).child(self.update_card(cx)).child(automatic.mt(px(-6.0))))
            .child(section("More", 12.0, more))
    }

    /// Where the update check stands: a newer version with its notes and Download, or the latest
    /// check with Check for updates.
    fn update_card(&self, cx: &mut Context<Self>) -> Div {
        let checked = self.store.value.last_update_check.map(|then| update::ago_label(then, archive::unix_now()));
        if self.updates.status == Status::Available && let Some(release) = &self.updates.latest {
            let released = release.published.as_deref().and_then(update::released_label).map(|date| format!(" · Released {date}")).unwrap_or_default();
            let (lines, more) = update::note_lines(&release.notes, NOTE_LINES);
            let line = |text: String, color| div().text_size(px(12.0)).line_height(px(17.0)).text_color(color).child(text);
            let mut notes = div().flex().flex_col().gap(px(4.0)).pl(px(28.0));
            for text in lines { notes = notes.child(line(text, theme::body())); }
            if more > 0 { notes = notes.child(line(format!("and {more} more {} in the release notes", if more == 1 { "change" } else { "changes" }), theme::muted())); }
            let open = |id: &'static str, text: &'static str, kind| {
                let page = release.page.clone();
                ui_v3::button(id, text, kind).on_mouse_down(MouseButton::Left, move |_, _, cx| cx.open_url(&page))
            };
            return div().flex().flex_col().gap(px(12.0)).px(px(16.0)).py(px(14.0)).rounded(px(RADII.card)).bg(theme::bubble()).border_1().border_color(theme::bubble_border())
                .child(div().flex().items_start().gap(px(10.0))
                    .child(ui::icon("icons/update.svg", 18.0, theme::accent_soft()))
                    .child(label(format!("CluelyRS {} is available", release.version), Some(status(format!("You have {VERSION}{released}"))))))
                .child(notes)
                .child(div().flex().items_center().gap(px(8.0)).pl(px(28.0))
                    .child(open("update-download", "Download ↗", Kind::Primary))
                    .child(open("update-release", "View release", Kind::Secondary))
                    .child(div().flex_1())
                    .children(checked.map(|ago| div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(format!("Checked {ago}"))))
                    .child(link("check-again", "Check again", true).font_weight(FontWeight::SEMIBOLD)
                        .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.check_for_updates(window, cx)))));
        }
        let settings = &self.store.value;
        let checked_detail = |what: &str| match &checked { Some(ago) => format!("{what} · Checked {ago}"), None => what.to_string() };
        let ok = || ui::icon("icons/check-circle.svg", 20.0, theme::ok()).into_any_element();
        let (icon, title, detail, button) = match self.updates.status {
            Status::NotChecked => (ui::icon("icons/update.svg", 20.0, theme::body()).into_any_element(), format!("CluelyRS {VERSION}"),
                checked.as_ref().map_or_else(|| "Not checked yet".to_string(), |ago| format!("Last checked {ago}")), "Check for updates"),
            Status::Checking => (spinner(), "Checking for updates…".to_string(), format!("You have {VERSION}"), "Check for updates"),
            Status::UpToDate | Status::Available => (ok(), "You're up to date".to_string(), checked_detail(&format!("{VERSION} is the latest")), "Check for updates"),
            Status::NoReleases => (ok(), "You're up to date".to_string(), checked_detail("No releases yet"), "Check for updates"),
            Status::Failed => (ui::icon("icons/alert-circle.svg", 20.0, theme::muted()).into_any_element(), "Couldn't check for updates".to_string(),
                if settings.check_for_updates { "You may be offline. It tries again later." } else { "You may be offline." }.to_string(), "Retry"),
        };
        let checking = self.updates.status == Status::Checking;
        let button = if checking { ui_v3::button("check-updates", button, Kind::Disabled) }
            else { action("check-updates", button, Kind::Secondary, cx, |this, window, cx| this.check_for_updates(window, cx)) };
        div().flex().items_center().gap(px(14.0)).px(px(12.0)).py(px(8.0)).border_1().border_color(gpui::transparent_black())
            .child(ui_v3::tile(40.0, false).child(icon))
            .child(label(title, Some(status(detail))))
            .child(button)
    }
}

/// The overlay over a stand-in screen, at half size: its width, text size and opacity follow the
/// settings as they change.
fn preview(percent: u8, text: TextSize, width: CardWidth) -> Div {
    let line = |w: f32, color: gpui::Rgba| div().w(px(w)).h(px(6.0)).flex_none().rounded(px(3.0)).bg(color);
    let app = div().absolute().left(px(157.0)).top(px(44.0)).w(px(300.0)).h(px(150.0)).flex().flex_col().gap(px(8.0)).px(px(18.0)).py(px(30.0))
        .rounded(px(RADII.small)).bg(theme::raised()).border_1().border_color(theme::hairline())
        .child(line(220.0, gpui::rgba(0x4c8dff66))).child(line(250.0, theme::keycap_border())).child(line(190.0, theme::keycap_border())).child(line(240.0, theme::keycap_border()));
    let title = text.answer() - 4.0;
    let card = div().w(px(width.card() / 2.0)).flex().flex_col().rounded(px(RADII.small)).overflow_hidden()
        .bg(appearance::fill(0x0f1012, percent)).border_1().border_color(theme::keycap_border()).shadow(ui_v3::drop_shadow(8.0, 20.0, 0.4))
        .child(div().px(px(8.0)).py(px(5.0)).bg(appearance::fill(0x17191c, percent))
            .child(div().text_size(px(8.0)).line_height(px(10.0)).text_color(theme::placeholder()).child("Ask about your screen or conversation")))
        .child(div().flex().flex_col().gap(px(3.0)).px(px(8.0)).py(px(6.0)).border_t_1().border_color(theme::divider())
            .child(div().text_size(px(title)).line_height(px(title + 5.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text())
                .child("“Use a token bucket per API key.”"))
            .child(div().text_size(px(title - 1.0)).line_height(px(title + 4.0)).text_color(theme::body())
                .child("Keep buckets in Redis with an atomic Lua script so every gateway node shares one count.")))
        .child(div().h(px(16.0)).flex().flex_none().items_center().gap(px(4.0)).px(px(6.0)).border_t_1().border_color(theme::divider())
            .child(div().size(px(9.0)).rounded(px(5.0)).bg(theme::accent()))
            .child(div().w(px(34.0)).h(px(7.0)).rounded(px(3.0)).border_1().border_dashed().border_color(rgb(0x3a3d42)))
            .child(div().flex_1())
            .child(div().size(px(6.0)).rounded(px(2.0)).bg(theme::muted()))
            .child(div().size(px(6.0)).rounded(px(2.0)).bg(theme::accent_soft()))
            .child(div().size(px(6.0)).rounded(px(2.0)).bg(theme::muted()))
            .child(div().flex_1())
            .child(div().w(px(20.0)).h(px(6.0)).rounded(px(3.0)).bg(theme::keycap_border())));
    div().relative().h(px(176.0)).rounded(px(RADII.card)).overflow_hidden().border_1().border_color(theme::hairline())
        .bg(linear_gradient(160.0, linear_color_stop(theme::chosen(), 0.0), linear_color_stop(theme::window_bg(), 0.7)))
        .child(div().absolute().top_0().left_0().right_0().h(px(12.0)).bg(theme::title_bar()).border_b_1().border_color(theme::divider()))
        .child(app)
        .child(div().absolute().top(px(18.0)).left_0().right_0().flex().justify_center().child(card))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_and_scroll_share_their_modifiers_on_one_row() {
        let labels = |list: &[&str]| list.iter().map(|label| label.to_string()).collect::<Vec<_>>();
        assert_eq!(join_keys(&labels(&["⌃ ⌥ ←", "⌃ ⌥ ↑", "⌃ ⌥ ↓", "⌃ ⌥ →"])), "⌃ ⌥ ← ↑ ↓ →");
        assert_eq!(join_keys(&labels(&["⌘ ↵"])), "⌘ ↵");
    }
}
