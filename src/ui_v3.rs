//! Building blocks of the Paper "Redesign v3" macOS pages: the Settings window and first-run setup.
//! Corners follow the macOS radius scale (`RADII`); colours are the theme's.

use gpui::{
    BoxShadow, Div, ElementId, FontWeight, ImageSource, IntoElement, ParentElement, Resource, Rgba, SharedString, Stateful, Styled,
    div, hsla, img, linear_color_stop, linear_gradient, point, prelude::*, px, relative,
};

use crate::{theme, ui};

/// Corner radii, from the macOS scale: cards 12, 40–48 px tiles 10, controls 9, small tiles and icon
/// buttons 8, keycaps 6. The windows' own corners are the system's.
pub(crate) struct Radii { pub card: f32, pub tile: f32, pub control: f32, pub small: f32, pub keycap: f32 }

pub(crate) const RADII: Radii = Radii { card: 12.0, tile: 10.0, control: 9.0, small: 8.0, keycap: 6.0 };

/// Segments in a level meter.
pub(crate) const METER_SEGMENTS: usize = 14;

/// What a button does now.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind { Primary, Secondary, Disabled }

/// A text button sized by its padding (12 px text): Grant, Use, Open folder.
pub(crate) fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Kind) -> Stateful<Div> {
    let base = div().id(id).flex_none().flex().items_center().justify_center().gap(px(6.0)).px(px(13.0)).py(px(6.0)).rounded(px(RADII.control))
        .border_1().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).child(label.into());
    match kind {
        Kind::Primary => base.cursor_pointer().bg(theme::accent()).border_color(theme::accent()).text_color(theme::accent_ink()),
        Kind::Secondary => base.cursor_pointer().bg(theme::raised()).border_color(theme::keycap_border()).text_color(theme::text()),
        Kind::Disabled => base.bg(theme::raised()).border_color(theme::hairline()).text_color(theme::placeholder()),
    }
}

/// A page's one main button: 280 × 44.
pub(crate) fn big_button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Kind) -> Stateful<Div> {
    button(id, label, kind).w(px(280.0)).h(px(44.0)).px_0().py_0().text_size(px(14.0)).line_height(px(18.0))
}

/// An icon on a square tile: 40 px in rows, 44 in setup's lists, 28–36 in smaller places. Chosen, it
/// takes the accent's dark blue.
pub(crate) fn tile(size: f32, chosen: bool) -> Div {
    let radius = if size >= 40.0 { RADII.tile } else { RADII.small };
    div().size(px(size)).flex_none().flex().items_center().justify_center().rounded(px(radius)).border_1()
        .bg(if chosen { theme::bubble() } else { theme::raised() }).border_color(if chosen { theme::bubble_border() } else { theme::hairline() })
}

/// The tile's icon colour: the soft accent when chosen.
pub(crate) fn tile_ink(chosen: bool) -> Rgba { if chosen { theme::accent_soft() } else { theme::body() } }

/// The round check on a chosen card or the active mode.
pub(crate) fn check_badge(check: f32) -> Div {
    div().size(px(18.0)).flex_none().flex().items_center().justify_center().rounded_full().bg(theme::accent())
        .child(ui::icon("icons/check.svg", check, theme::accent_ink()))
}

/// An on/off switch (macOS Settings): 36 × 20, the knob light on the accent when on.
pub(crate) fn switch(on: bool) -> Div {
    let track = div().w(px(36.0)).h(px(20.0)).flex_none().flex().items_center().p(px(2.0)).rounded_full()
        .bg(if on { theme::accent() } else { theme::hairline() });
    let knob = div().size(px(16.0)).rounded_full().bg(if on { theme::text() } else { theme::body() });
    if on { track.justify_end().child(knob) } else { track.child(knob) }
}

/// A segmented control's frame (macOS Settings); `segment` fills it.
pub(crate) fn segmented() -> Div {
    div().flex().flex_none().gap(px(2.0)).p(px(3.0)).rounded(px(RADII.tile)).bg(theme::window_bg()).border_1().border_color(theme::divider())
}

pub(crate) fn segment(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    let base = div().id(id).flex().items_center().justify_center().gap(px(7.0)).px(px(11.0)).py(px(4.0)).rounded(px(RADII.small)).cursor_pointer()
        .text_size(px(12.0)).line_height(px(16.0)).child(label.into());
    if selected { base.bg(theme::selected_fill()).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()) }
    else { base.font_weight(FontWeight::MEDIUM).text_color(theme::muted()) }
}

/// A dropdown's closed face (macOS Settings): an optional logo, the value and a chevron.
pub(crate) fn dropdown_face(id: impl Into<ElementId>, logo: Option<&'static str>, value: impl Into<SharedString>, open: bool) -> Stateful<Div> {
    div().id(id).flex().items_center().justify_between().gap(px(10.0)).py(px(7.0)).pl(px(12.0)).pr(px(10.0)).rounded(px(RADII.control)).cursor_pointer()
        .bg(theme::field()).border_1().border_color(if open { theme::accent() } else { theme::hairline() })
        .child(div().flex_1().flex().items_center().gap(px(8.0)).min_w_0().overflow_hidden()
            .when_some(logo, |face, logo| face.child(ui::icon(logo, 14.0, theme::text())))
            .child(div().min_w_0().truncate().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::MEDIUM).text_color(theme::text()).child(value.into())))
        .child(ui::icon("icons/chevron-down.svg", 12.0, theme::muted()))
}

/// One key of a shortcut, 22 px (Settings) or 20 px (setup's hints). Words (Esc, Space) are set a
/// size smaller than the modifiers, and arrows and punctuation a size larger.
pub(crate) fn keycap(key: &str, large: bool) -> Div {
    let (cap, modifier, symbol, word) = if large { (22.0, 12.0, 14.0, 11.0) } else { (20.0, 11.0, 12.0, 10.0) };
    let text = if key.chars().count() > 1 { word } else if key.chars().all(char::is_alphanumeric) || "⌘⇧⌃⌥".contains(key) { modifier } else { symbol };
    div().min_w(px(cap)).h(px(cap)).flex().items_center().justify_center().px(px(if large { 6.0 } else { 5.0 })).rounded(px(RADII.keycap))
        .bg(theme::raised()).border_1().border_color(theme::keycap_border())
        .text_size(px(text)).line_height(px(if large { 14.0 } else { 12.0 })).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child(key.to_string())
}

/// A shortcut's keys ("⌘ ⇧ Space"), each on its own cap.
pub(crate) fn keycaps(label: &str, large: bool) -> Div {
    label.split(' ').filter(|key| !key.is_empty()).fold(div().flex().items_center().gap(px(if large { 4.0 } else { 3.0 })), |caps, key| caps.child(keycap(key, large)))
}

/// A level meter: `METER_SEGMENTS` segments lit up to `level` (0..=1), green.
pub(crate) fn meter(level: f32, width: Option<f32>) -> Div {
    let lit = (level.clamp(0.0, 1.0) * METER_SEGMENTS as f32).round() as usize;
    (0..METER_SEGMENTS).fold(div().flex().flex_none().gap(px(3.0)).when_some(width, |bar, width| bar.w(px(width))), |bar, index| {
        let segment = div().h(px(6.0)).rounded(px(2.0)).bg(if index < lit { theme::ok() } else { theme::hairline() });
        bar.child(if width.is_some() { segment.flex_1() } else { segment.w(px(6.0)) })
    })
}

/// A thin progress bar (a download), `fraction` filled.
pub(crate) fn progress_bar(fraction: f32) -> Div {
    div().flex_1().h(px(4.0)).rounded(px(2.0)).bg(theme::divider())
        .child(div().h_full().rounded(px(2.0)).bg(theme::accent()).w(relative(fraction.clamp(0.0, 1.0))))
}

/// The mark: the accent disc with its viewfinder corners, in colour.
pub(crate) fn mark(size: f32) -> impl IntoElement {
    img(ImageSource::Resource(Resource::Embedded("app-icon/mark.svg".into()))).size(px(size)).flex_none()
}

/// The app's tile: the mark on a dark gradient, as the app icon draws it (setup's Welcome, About).
pub(crate) fn app_tile(size: f32, radius: f32, mark_size: f32) -> Div {
    div().size(px(size)).flex_none().flex().items_center().justify_center().rounded(px(radius)).border_1().border_color(theme::keycap_border())
        .bg(linear_gradient(180.0, linear_color_stop(gpui::rgb(0x24272c), 0.0), linear_color_stop(theme::window_bg(), 1.0)))
        .child(mark(mark_size))
}

/// A soft drop shadow (`#00000080 0 y blur`).
pub(crate) fn drop_shadow(y: f32, blur: f32, alpha: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color: hsla(0.0, 0.0, 0.0, alpha), offset: point(px(0.0), px(y)), blur_radius: px(blur), spread_radius: px(0.0) }]
}

/// Setup's step indicator: a long dash for this step, short ones before (done) and after.
pub(crate) fn progress_dashes(count: usize, current: usize) -> Div {
    (0..count).fold(div().flex().items_center().justify_center().gap(px(6.0)), |row, index| {
        let dash = div().h(px(4.0)).rounded(px(2.0)).flex_none();
        row.child(match index.cmp(&current) {
            std::cmp::Ordering::Less => dash.w(px(6.0)).bg(theme::bubble_border()),
            std::cmp::Ordering::Equal => dash.w(px(22.0)).bg(theme::accent()),
            std::cmp::Ordering::Greater => dash.w(px(6.0)).bg(theme::keycap_border()),
        })
    })
}

/// A setup page's title and the line under it.
pub(crate) fn heading(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Div {
    div().flex().flex_col().items_center().gap(px(10.0))
        .child(div().text_size(px(34.0)).line_height(px(40.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(title.into()))
        .child(div().text_size(px(15.0)).line_height(px(22.0)).text_color(theme::muted()).child(detail.into()))
}

/// The back arrow at the top left of setup's pages: the arrow, turned to point left.
pub(crate) fn back_arrow() -> impl IntoElement {
    gpui::svg().path("icons/setup-arrow.svg").size(px(18.0)).flex_none().text_color(theme::muted())
        .with_transformation(gpui::Transformation::rotate(gpui::percentage(0.5)))
}

/// An overlay scrollbar's thumb over the right edge of what `scroll` tracks, `right` px in, while
/// it's taller than its box. Place it in a `relative()` parent beside the scrolling element.
pub(crate) fn scroll_thumb(scroll: &gpui::ScrollHandle, right: f32) -> Option<Div> {
    let viewport = f32::from(scroll.bounds().size.height);
    let overflow = f32::from(scroll.max_offset().height);
    if viewport <= 0.0 || overflow <= 0.5 { return None; }
    let track = viewport - 12.0;
    let thumb = (track * viewport / (viewport + overflow)).max(32.0);
    let scrolled = (-f32::from(scroll.offset().y) / overflow).clamp(0.0, 1.0);
    Some(div().absolute().right(px(right)).top(px(6.0 + (track - thumb) * scrolled)).w(px(6.0)).h(px(thumb)).rounded(px(3.0)).bg(theme::scroll_thumb()))
}

// Setup's pages.

/// The width of setup's lists and choices.
pub(crate) const SETUP_COLUMN: f32 = 520.0;

/// One of the three ways to answer: a logo tile, its name and a line, checked once chosen.
pub(crate) fn answer_card(id: impl Into<ElementId>, logo: impl IntoElement, title: &'static str, detail: impl Into<SharedString>, chosen: bool) -> Stateful<Div> {
    div().id(id).relative().w(px(160.0)).flex_none().flex().flex_col().items_center().gap(px(10.0)).pt(px(20.0)).pb(px(16.0)).px(px(12.0))
        .rounded(px(RADII.card)).border_1().cursor_pointer()
        .bg(if chosen { theme::chosen() } else { theme::panel() }).border_color(if chosen { theme::accent() } else { theme::divider() })
        .child(tile(44.0, chosen).child(logo))
        .child(div().flex().flex_col().items_center().gap(px(2.0))
            .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(detail.into())))
        .when(chosen, |card| card.child(div().absolute().top(px(10.0)).right(px(10.0)).child(check_badge(10.0))))
}

/// The line under the answer cards: a status icon, what it says, and an action.
pub(crate) fn account_line(icon: impl IntoElement, text: impl Into<SharedString>, trailing: impl IntoElement) -> Div {
    div().w(px(504.0)).flex().items_center().gap(px(10.0)).px(px(14.0)).py(px(12.0)).rounded(px(RADII.card)).bg(theme::panel()).border_1().border_color(theme::divider())
        .child(icon)
        .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(text.into()))
        .child(trailing)
}

/// A field on macOS setup's forms: 40 px on the API key form, 30 px in a speech card.
pub(crate) fn form_field(height: f32) -> Div {
    div().h(px(height)).flex().items_center().gap(px(9.0)).px(px(12.0)).rounded(px(RADII.control)).bg(theme::field()).border_1().border_color(theme::hairline())
}

/// A box of rows, a hairline between each (setup's permissions and meters).
pub(crate) fn rows_box(rows: Vec<gpui::AnyElement>) -> Div {
    let last = rows.len().saturating_sub(1);
    rows.into_iter().enumerate().fold(div().w(px(SETUP_COLUMN)).flex().flex_col().rounded(px(RADII.card)).overflow_hidden().bg(theme::panel()).border_1().border_color(theme::divider()),
        |list, (index, row)| list.child(div().when(index < last, |row| row.border_b_1().border_color(theme::divider())).child(row)))
}

/// An audio source on setup's Listening page: its tile, who it is and its device (a dropdown), its
/// level and a status (heard, or a problem).
pub(crate) fn meter_row(icon: &'static str, who: &'static str, device: impl IntoElement, level: f32, status: Option<gpui::AnyElement>) -> Div {
    div().flex().items_center().gap(px(14.0)).px(px(16.0)).py(px(14.0))
        .child(tile(40.0, false).child(ui::icon(icon, 20.0, theme::body())))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.0))
            .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(who))
            .child(device))
        .child(meter(level, Some(154.0)))
        .child(div().w(px(18.0)).flex_none().flex().justify_center().children(status))
}

/// A device name with a chevron: the face of a meter row's device dropdown.
pub(crate) fn device_face(id: impl Into<ElementId>, name: impl Into<SharedString>) -> Stateful<Div> {
    div().id(id).flex().items_center().gap(px(4.0)).min_w_0().cursor_pointer()
        .child(div().min_w_0().truncate().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(name.into()))
        .child(ui::icon("icons/chevron-down.svg", 10.0, theme::muted()))
}

/// One of the two speech models: its tile, name and line, and under them its action (Download, a
/// progress bar, Add API key, or the key field). Checked once chosen.
pub(crate) fn speech_card(id: impl Into<ElementId>, icon: &'static str, icon_size: f32, title: &'static str, detail: impl Into<SharedString>, chosen: bool,
    bottom: impl IntoElement) -> Stateful<Div> {
    div().id(id).relative().flex_1().min_w_0().flex().flex_col().gap(px(10.0)).p(px(12.0)).rounded(px(RADII.card)).border_1().cursor_pointer()
        .bg(if chosen { theme::chosen() } else { theme::panel() }).border_color(if chosen { theme::accent() } else { theme::divider() })
        .child(div().flex().items_center().gap(px(12.0))
            .child(tile(36.0, chosen).child(ui::icon(icon, icon_size, tile_ink(chosen))))
            .child(div().flex().flex_col().gap(px(2.0)).min_w_0()
                .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
                .child(div().truncate().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(detail.into()))))
        .child(div().h(px(30.0)).flex().items_center().child(bottom))
        .when(chosen, |card| card.child(div().absolute().top(px(10.0)).right(px(10.0)).child(check_badge(12.0))))
}

/// A 30 px button filling a speech card: Download, Add API key.
pub(crate) fn card_button(id: impl Into<ElementId>, icon: &'static str, label: &'static str) -> Stateful<Div> {
    div().id(id).flex_1().h(px(30.0)).flex().items_center().justify_center().gap(px(6.0)).rounded(px(RADII.control)).cursor_pointer()
        .bg(theme::raised()).border_1().border_color(theme::keycap_border())
        .child(ui::icon(icon, 14.0, theme::body()))
        .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(label))
}

/// A download in a speech card: the bar and how far it is.
pub(crate) fn download_progress(fraction: f32, words: impl Into<SharedString>) -> Div {
    div().flex_1().flex().items_center().gap(px(10.0))
        .child(progress_bar(fraction))
        .child(div().flex_none().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::body()).child(words.into()))
}

/// The three shortcuts under setup's last button: keys, then what they do.
pub(crate) fn shortcut_hints(hints: &[(String, &'static str)]) -> Div {
    hints.iter().fold(div().flex().items_center().gap(px(16.0)), |row, (keys, what)| row.child(
        div().flex().items_center().gap(px(3.0)).child(keycaps(keys, false))
            .child(div().pl(px(4.0)).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::muted()).child(*what))))
}

/// A setup page: its content centred in the space above its actions (the main button, and what goes
/// under it). Listening's page is `tight`: it sits higher to fit its meters and choices.
pub(crate) fn setup_page(content: Div, actions: Div, tight: bool) -> Div {
    div().flex_1().min_h_0().flex().flex_col().items_center().justify_between().px(px(64.0))
        .pt(px(if tight { 12.0 } else { 28.0 })).pb(px(if tight { 28.0 } else { 36.0 }))
        .child(content.flex_1().flex().flex_col().items_center().justify_center())
        .child(actions.flex().flex_col().items_center().gap(px(14.0)))
}

/// Welcome: the app's tile in its blue ring, its name and promise, and that the overlay hides from
/// screen sharing. `tile_radius` is the platform's (26 on macOS, 16 on Windows).
pub(crate) fn welcome(tile_radius: f32, start: Stateful<Div>) -> Div {
    let ring = BoxShadow { color: hsla(218.0 / 360.0, 1.0, 0.65, 0.15), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(8.0) };
    let mut shadows = drop_shadow(16.0, 40.0, 0.5);
    shadows.insert(0, ring);
    let pill = div().flex().items_center().gap(px(7.0)).px(px(12.0)).py(px(6.0)).rounded_full().bg(theme::chosen()).border_1().border_color(theme::bubble_border())
        .child(ui::icon("icons/eye-off.svg", 14.0, theme::accent_soft()))
        .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child("Hidden from screen sharing by default"));
    setup_page(div().gap(px(28.0)).child(app_tile(104.0, tile_radius, 68.0).shadow(shadows))
            .child(heading("Welcome to CluelyRS", "Live answers for your calls, in an overlay only you can see."))
            .child(pill),
        div().child(start)
            .child(div().text_size(px(11.0)).line_height(px(16.0)).text_color(theme::placeholder())
                .child("An independent open-source project. Not affiliated with or endorsed by Cluely.")),
        false)
}

/// Something done, in green with a check: Downloaded, Key saved.
pub(crate) fn done_note(label: &'static str) -> Div {
    div().flex().items_center().gap(px(6.0))
        .child(ui::icon("icons/setup-circle-check.svg", 14.0, theme::ok()))
        .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::ok()).child(label))
}
