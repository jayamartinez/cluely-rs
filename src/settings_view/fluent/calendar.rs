//! Settings › Calendar (Paper F8a, F8d): Google first, signed in with the browser, then the account's
//! calendars to read; Advanced for the user's own client ID; and the reminders, each showing its
//! own upcoming-meeting row on the overlay. Outlook, iCloud and other calendars (secret iCal
//! addresses) come in a later slice, between the Google card and Advanced.

use gpui::{
    AnyElement, App, Context, Div, Entity, Focusable, ImageSource, IntoElement, MouseButton, ParentElement, Resource, Rgba, Stateful, Styled, Window, deferred, div,
    img, prelude::*, px, rgb,
};

use super::{button, card, card_label, chevron, combo_face, error_text, expander, expander_header, expander_row, external, flyout, flyout_item, status, text_box, title};
use crate::calendar::google::SyncError;
use crate::calendar::{self, CalendarInfo, MAX_REMINDERS, oauth};
use crate::input::TextInput;
use crate::overlay::Overlay;
use crate::settings_view::{MENU_MAX_HEIGHT, listen};
use crate::{archive, theme, ui};

/// "Sign-in needs a client ID", "No client ID set".
const WARNING: u32 = 0xe8c46a;
const GOOGLE_CLOUD: &str = "https://console.cloud.google.com/apis/credentials";
/// The lead times offered, in minutes; "Custom…" types any other.
const LEADS: [u32; 10] = [1, 2, 3, 5, 10, 15, 30, 45, 60, 120];
/// What "Add reminder" picks: the first of these not already set.
const NEXT_LEADS: [u32; 9] = [30, 10, 5, 15, 60, 1, 3, 45, 120];
const CUSTOM: u32 = 0;
/// Reminder lead-time lists' faces in `Fluent::faces`, past the `Picker` ids.
const FACE_BASE: usize = 1000;
const COMBO_WIDTH: f32 = 200.0;
const MINUTES_WIDTH: f32 = 72.0;

/// `then` (a `listen` handler, which runs in the overlay's window), after giving `input` the
/// keyboard here in the Settings window when `focus` says so.
fn focusing(input: Entity<TextInput>, focus: bool, then: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static)
    -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        if focus { window.focus(&input.focus_handle(cx)); }
        then(event, window, cx);
    }
}

fn logo(path: &'static str, size: f32) -> impl IntoElement {
    img(ImageSource::Resource(Resource::Embedded(path.into()))).size(px(size)).flex_none()
}

fn description(text: &'static str) -> Div {
    div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(text)
}

fn link(id: &'static str, label: impl Into<gpui::SharedString>) -> Stateful<Div> {
    div().id(id).flex().flex_none().items_center().gap(px(4.0)).cursor_pointer().text_size(px(13.0)).line_height(px(16.0)).text_color(theme::accent_soft()).child(label.into())
}

fn colored_status(text: impl Into<gpui::SharedString>, color: Rgba) -> Div {
    div().text_size(px(12.0)).line_height(px(16.0)).text_color(color).child(text.into())
}

/// A status with a dot before it, in the dot's colour ("● Synced 2 min ago").
fn dot_line(text: impl Into<gpui::SharedString>, color: Rgba) -> Div {
    div().flex().flex_none().items_center().gap(px(6.0)).child(div().size(px(6.0)).flex_none().rounded(px(3.0)).bg(color))
        .child(div().text_size(px(13.0)).line_height(px(16.0)).text_color(color).child(text.into()))
}

fn checkbox(checked: bool) -> Div {
    let tile = div().size(px(18.0)).flex_none().flex().items_center().justify_center().rounded(px(4.0));
    if checked { tile.bg(theme::accent()).child(ui::icon("icons/check.svg", 12.0, theme::accent_ink())) }
    else { tile.border_1().border_color(theme::muted()) }
}

/// Google's dark "Sign in with Google" button, as its branding guidelines draw it.
fn google_button() -> Stateful<Div> {
    div().id("google-sign-in").flex().flex_none().items_center().gap(px(10.0)).h(px(40.0)).px(px(12.0)).rounded(px(4.0)).cursor_pointer()
        .bg(rgb(0x131314)).border_1().border_color(rgb(0x8e918f))
        .child(logo("logos/google-g.svg", 20.0))
        .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(gpui::FontWeight::MEDIUM).text_color(rgb(0xe3e3e3)).child("Sign in with Google"))
}

impl Overlay {
    pub(super) fn fluent_calendar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let leads = &self.store.value.calendar.reminders;
        let calendars = div().flex().flex_col().gap(px(8.0))
            .child(div().flex().flex_col().gap(px(4.0))
                .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::text()).child("Calendars"))
                .child(description("CluelyRS only reads your calendars, on this PC, to name sessions and remind you before meetings.")))
            .child(self.google_card(cx))
            .children(self.calendar.error.clone().map(|error| error_text(error).px(px(2.0))))
            .child(self.advanced_card(window, cx));
        let mut reminders = div().flex().flex_col().gap(px(4.0));
        for (index, lead) in leads.iter().enumerate() { reminders = reminders.child(self.reminder_card(index, *lead, window, cx)); }
        if leads.len() < MAX_REMINDERS { reminders = reminders.child(self.add_reminder(cx)); }
        let reminders = div().flex().flex_col().gap(px(8.0))
            .child(div().flex().flex_col().gap(px(4.0))
                .child(div().flex().items_center().justify_between()
                    .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::text()).child("Reminders"))
                    .child(status(format!("{} of {MAX_REMINDERS}", leads.len()))))
                .child(description("Each one shows its own upcoming-meeting row on the overlay card. It never takes focus.")))
            .child(reminders);
        div().flex().flex_col().gap(px(24.0)).child(title("Calendar")).child(calendars).child(reminders)
    }

    /// Not signed in, signing in in the browser, or the signed-in account with its calendars.
    fn google_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.store.value.calendar;
        if self.calendar.signing_in.is_some() {
            return card("google").min_h(px(76.0)).child(logo("logos/google-calendar.svg", 20.0))
                .child(card_label("Finish signing in in your browser…", Some(status("Google opened in your default browser").into_any_element())))
                .child(crate::settings_view::about::spinner())
                .child(button("google-cancel", "Cancel", false).on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.cancel_calendar_sign_in(cx))))
                .into_any_element();
        }
        let Some(account) = settings.google_account.clone() else {
            let line = if oauth::has_client(&settings.google_client_id) { status("Sign in, then choose which calendars to use") }
                else { colored_status("Sign-in needs a client ID · set one below", rgb(WARNING)) };
            return card("google").min_h(px(76.0)).child(logo("logos/google-calendar.svg", 20.0))
                .child(card_label("Google Calendar", Some(line.into_any_element())))
                .child(google_button().on_mouse_down(MouseButton::Left, focusing(self.calendar.client_id_input.clone(), !oauth::has_client(&settings.google_client_id),
                    listen(cx, |this, _, window, cx| this.calendar_sign_in(window, cx)))))
                .into_any_element();
        };
        let signed_out_by_google = self.calendar.sync_error == Some(SyncError::SignInAgain);
        let sync = match (&self.calendar.sync_error, self.calendar.synced_at) {
            (Some(error), _) => dot_line(error.message(), theme::danger()),
            (None, Some(at)) => dot_line(calendar::synced_label(at, archive::unix_now() as i64), theme::ok()),
            (None, None) => dot_line("Syncing…", theme::muted()),
        };
        let header = expander_header().id("google-header").min_h(px(76.0))
            .child(logo("logos/google-calendar.svg", 20.0))
            .child(card_label(if account.is_empty() { "Google account".to_string() } else { account }, Some(status("Google Calendar · read-only").into_any_element())))
            .child(sync)
            .when(signed_out_by_google, |header| header.child(link("google-sign-in-again", "Sign in")
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.calendar_sign_in(window, cx)))))
            .child(link("google-sign-out", "Sign out").on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.calendar_sign_out(window, cx))));
        let rows: Vec<AnyElement> = if self.calendar.calendars.is_empty() {
            vec![expander_row("google-loading").child(status(if self.calendar.syncing { "Loading your calendars…" } else { "No calendars yet" })).into_any_element()]
        } else {
            self.calendar.calendars.iter().enumerate().map(|(index, info)| self.calendar_row(index, info, cx)).collect()
        };
        expander("google", header, Some(rows)).into_any_element()
    }

    fn calendar_row(&self, index: usize, info: &CalendarInfo, cx: &mut Context<Self>) -> AnyElement {
        let id = info.id.clone();
        expander_row(("google-calendar", index)).min_h(px(44.0)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.toggle_calendar(&id, window, cx)))
            .child(checkbox(self.store.value.calendar.uses(info)))
            .child(div().size(px(8.0)).flex_none().rounded(px(4.0)).bg(info.color.map_or(theme::muted(), rgb)))
            .child(div().min_w_0().truncate().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::text()).child(info.name.clone()))
            .when(info.primary, |row| row.child(status("Primary")))
            .into_any_element()
    }

    /// "Advanced: use your own Google client ID": opens by itself when there is no ID at all.
    fn advanced_card(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.store.value.calendar;
        let open = self.calendar.advanced_open;
        let missing = !oauth::has_client(&settings.google_client_id);
        let which = if missing { colored_status("No client ID set", rgb(WARNING)) }
            else if settings.google_client_id.trim().is_empty() { status("Using built-in ID") } else { status("Using your own ID") };
        let header = div().id("google-advanced").flex().items_center().gap(px(16.0)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| { this.calendar.advanced_open = !this.calendar.advanced_open; cx.notify(); }))
            .child(div().w(px(20.0)).flex_none().flex().justify_center().child(if open { chevron(false).into_any_element() } else { super::chevron_right(12.0, theme::body()).into_any_element() }))
            .child(card_label("Advanced: use your own Google client ID", None))
            .child(which);
        let body = open.then(|| {
            let saved_secret = self.store.value.saved_keys.get(oauth::OWN_SECRET_ID).map(|hint| format!("Saved {hint}"));
            let field = |label: &'static str, control: Stateful<Div>| div().flex().items_center().gap(px(16.0))
                .child(div().w(px(100.0)).flex_none().text_size(px(13.0)).text_color(theme::body()).child(label))
                .child(control.flex_1());
            let steps = div().flex().flex_col().gap(px(4.0)).text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted())
                .child("1. In Google Cloud Console, create a project and enable the Calendar API")
                .child("2. Create an OAuth client ID of type Desktop app")
                .child("3. On the OAuth consent screen, set publishing status to In production");
            let footer = div().flex().items_center().gap(px(16.0))
                .child(div().flex_1().min_w_0().text_size(px(12.0)).text_color(theme::muted()).child("Your own ID overrides the built-in one."))
                .child(link("google-cloud", "Open Google Cloud").child(external(10.0, theme::accent_soft()))
                    .on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url(GOOGLE_CLOUD))))
                .child(button("google-client-save", "Save", true).on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.save_own_client(window, cx))));
            vec![div().flex().flex_col().gap(px(12.0)).pl(px(52.0)).pr(px(16.0)).pt(px(4.0)).pb(px(16.0))
                .child(steps)
                .child(field("Client ID", text_box("google-client-id", &self.calendar.client_id_input, window, cx)))
                .child(field("Client secret", text_box("google-client-secret", &self.calendar.client_secret_input, window, cx)))
                .children(saved_secret.map(|saved| div().pl(px(116.0)).child(status(saved))))
                .child(footer)
                .into_any_element()]
        });
        expander("google-advanced-card", header, body).when(open && missing, |card| card.border_color(theme::accent())).into_any_element()
    }

    fn reminder_card(&self, index: usize, lead: u32, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let typed = self.calendar.typed_reminder == Some(lead);
        let control = if typed {
            div().flex().items_center().gap(px(8.0))
                .child(text_box(("reminder-minutes", index), &self.calendar.reminder_input, window, cx).w(px(MINUTES_WIDTH)))
                .child(self.lead_combo(index, "min before".into(), COMBO_WIDTH - MINUTES_WIDTH - 8.0, cx))
        } else {
            div().child(self.lead_combo(index, calendar::lead_label(lead), COMBO_WIDTH, cx))
        };
        let close = div().id(("reminder-remove", index)).size(px(28.0)).flex().flex_none().items_center().justify_center().rounded(px(4.0)).cursor_pointer()
            .hover(|button| button.bg(theme::fluent_selected()))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.remove_reminder(index, window, cx)))
            .child(ui::icon("icons/close.svg", 10.0, theme::body()));
        card(("reminder", index)).child(ui::icon("icons/bell.svg", 20.0, theme::body())).child(card_label("Remind me", None)).child(control).child(close).into_any_element()
    }

    /// A reminder's lead-time ComboBox: the presets and "Custom…", which types the minutes.
    fn lead_combo(&self, index: usize, value: String, width: f32, cx: &mut Context<Self>) -> Div {
        let open = self.calendar.open_reminder == Some(index);
        let faces = self.fluent.faces.clone();
        let face = combo_face(("reminder-lead", index), None, value.into(), width)
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| {
                cx.stop_propagation();
                this.calendar.open_reminder = if this.calendar.open_reminder == Some(index) { None } else { Some(index) };
                cx.notify();
            }))
            .child(gpui::canvas(move |bounds, _, _| { faces.borrow_mut().insert(FACE_BASE + index, bounds); }, |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        let mut wrapper = div().relative().flex_none().child(face);
        if !open { return wrapper; }
        let current = self.store.value.calendar.reminders.get(index).copied();
        let face_bounds = self.fluent.faces.borrow().get(&(FACE_BASE + index)).copied();
        let mut list = flyout().id("reminder-leads").w(px(COMBO_WIDTH)).max_h(px(MENU_MAX_HEIGHT)).overflow_y_scroll()
            .on_mouse_down_out(listen(cx, move |this, event: &gpui::MouseDownEvent, _, cx| {
                if face_bounds.is_some_and(|face| face.contains(&event.position)) { return; }
                this.calendar.open_reminder = None;
                cx.notify();
            }));
        for (row, minutes) in LEADS.into_iter().chain([CUSTOM]).enumerate() {
            let label = if minutes == CUSTOM { "Custom…".to_string() } else { calendar::lead_label(minutes) };
            list = list.child(flyout_item(("reminder-lead-option", row), current == Some(minutes), true)
                .child(div().flex_1().min_w_0().truncate().child(label))
                .on_mouse_down(MouseButton::Left, focusing(self.calendar.reminder_input.clone(), minutes == CUSTOM, listen(cx, move |this, _, window, cx| {
                    cx.stop_propagation();
                    if minutes == CUSTOM { this.type_reminder(index, cx); } else { this.set_reminder(index, minutes, window, cx); }
                }))));
        }
        // Reminders sit at the bottom of the page: the list opens upward when it wouldn't fit below.
        let list_height = ((LEADS.len() + 1) as f32 * 34.0 + 8.0).min(MENU_MAX_HEIGHT + 8.0);
        let upward = face_bounds.is_some_and(|face| f32::from(face.bottom()) + 4.0 + list_height > super::window::HEIGHT - 8.0);
        let anchor = if upward { div().absolute().bottom(px(36.0)) } else { div().absolute().top(px(36.0)) };
        wrapper = wrapper.child(deferred(anchor.right_0().child(list)).with_priority(1));
        wrapper
    }

    /// "Custom…": type the reminder's minutes in place of the list.
    fn type_reminder(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(minutes) = self.store.value.calendar.reminders.get(index).copied() else { return };
        self.calendar.open_reminder = None;
        self.calendar.typed_reminder = Some(minutes);
        self.calendar.reminder_input.update(cx, |input, cx| { input.set_text(minutes.to_string(), cx); input.select_all_text(cx); });
        cx.notify();
    }

    fn add_reminder(&self, cx: &mut Context<Self>) -> AnyElement {
        div().id("reminder-add").flex().items_center().justify_center().gap(px(8.0)).h(px(52.0)).rounded(px(6.0)).cursor_pointer()
            .border_1().border_dashed().border_color(theme::keycap_border())
            .hover(|add| add.bg(theme::field()))
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| {
                let leads = this.store.value.calendar.reminders.clone();
                let Some(next) = NEXT_LEADS.into_iter().find(|lead| !leads.contains(lead)) else { return };
                this.set_reminder(leads.len(), next, window, cx);
            }))
            .child(ui::icon("icons/plus.svg", 12.0, theme::accent_soft()))
            .child(div().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::accent_soft()).child("Add reminder"))
            .into_any_element()
    }
}
