//! Calendars, shared by Windows and macOS: the events CluelyRS knows about, what Settings ›
//! Calendar saves, and the pieces that need no UI. Google is signed in to with the installed-app
//! OAuth flow (`oauth`) and read with the Calendar API (`google`); `reminders` decides when an
//! event's upcoming-meeting row shows on the overlay.
//!
//! Only non-secret choices are saved in settings.json (`CalendarSettings`). The refresh token and a
//! user's own client secret go to the OS credential store (`crate::secrets`); access tokens live
//! only in memory.

pub mod google;
pub mod oauth;
pub mod reminders;

use serde::{Deserialize, Serialize};

/// The most reminders Settings › Calendar offers.
pub const MAX_REMINDERS: usize = 5;
/// The longest lead time a reminder can have, in minutes (a day).
pub const MAX_LEAD_MINUTES: u32 = 24 * 60;

/// What Settings › Calendar saves in settings.json. Never tokens or secrets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CalendarSettings {
    /// The signed-in Google account's address (its primary calendar's id), for the account row.
    /// `Some` means a refresh token was saved for it.
    pub google_account: Option<String>,
    /// The Google calendars to read, by id. `None` until the user picks: then the calendars that are
    /// shown in Google Calendar itself are read.
    pub google_calendars: Option<Vec<String>>,
    /// The user's own Desktop OAuth client ID (Advanced); empty uses the built-in one. Its secret is
    /// in the credential store (`oauth::OWN_SECRET_ID`).
    pub google_client_id: String,
    /// Each reminder's lead time in minutes, longest first; each shows its own upcoming-meeting row.
    pub reminders: Vec<u32>,
}

impl Default for CalendarSettings {
    fn default() -> Self {
        Self { google_account: None, google_calendars: None, google_client_id: String::new(), reminders: vec![2] }
    }
}

impl CalendarSettings {
    /// The reminders as they're saved: within range, without repeats, at most `MAX_REMINDERS`, longest first.
    pub fn normalize_reminders(leads: impl IntoIterator<Item = u32>) -> Vec<u32> {
        let mut leads: Vec<u32> = leads.into_iter().filter(|lead| (1..=MAX_LEAD_MINUTES).contains(lead)).collect();
        leads.sort_unstable_by(|a, b| b.cmp(a));
        leads.dedup();
        leads.truncate(MAX_REMINDERS);
        leads
    }

    /// Whether `calendar` is read: the user's choice, or until there is one, whether Google Calendar shows it.
    pub fn uses(&self, calendar: &CalendarInfo) -> bool {
        match &self.google_calendars {
            Some(chosen) => chosen.contains(&calendar.id),
            None => calendar.shown_in_google || calendar.primary,
        }
    }
}

/// One calendar of the signed-in account, for the checkbox list.
#[derive(Clone, Debug, PartialEq)]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
    pub primary: bool,
    /// Google's colour for it, as `0xRRGGBB`.
    pub color: Option<u32>,
    /// Shown in Google Calendar's own list (its `selected` flag).
    pub shown_in_google: bool,
}

/// When an event starts or ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum When {
    /// A moment, in Unix seconds.
    At(i64),
    /// An all-day date (no time of day).
    AllDay(chrono::NaiveDate),
}

impl When {
    pub fn at(self) -> Option<i64> { match self { Self::At(at) => Some(at), Self::AllDay(_) => None } }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attendee {
    pub email: String,
    pub name: Option<String>,
    /// The signed-in user themselves.
    pub is_self: bool,
    pub declined: bool,
}

/// One meeting on a calendar, as CluelyRS uses it.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: String,
    /// The calendar it came from (`CalendarInfo::id`).
    pub calendar: String,
    pub title: String,
    pub start: When,
    pub end: When,
    /// The event's own IANA time zone ("America/New_York"), when it has one.
    pub time_zone: Option<String>,
    pub attendees: Vec<Attendee>,
    /// The video call to join (https only).
    pub meeting_link: Option<String>,
    /// Marked "free" rather than "busy".
    pub free: bool,
}

impl Event {
    /// The user declined it.
    pub fn declined(&self) -> bool { self.attendees.iter().any(|attendee| attendee.is_self && attendee.declined) }

    /// Worth a reminder: a timed meeting the user is going to (not all-day, free or declined).
    pub fn remindable(&self) -> bool { self.start.at().is_some() && !self.free && !self.declined() }

    /// Identifies one occurrence at its current time: a moved event counts as new, so it's reminded again.
    pub fn key(&self) -> String { format!("{}\u{1f}{}\u{1f}{:?}", self.calendar, self.id, self.start) }
}

/// "in 2 min", "in 1 h 5 min", "now", "started 3 min ago": the row's countdown.
pub fn countdown(start: i64, now: i64) -> String {
    let seconds = start - now;
    if seconds <= 0 {
        let minutes = (-seconds) / 60;
        return if minutes == 0 { "now".into() } else { format!("started {minutes} min ago") };
    }
    // Rounded up, so "in 2 min" shows until the last minute starts.
    let minutes = (seconds + 59) / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("in {m} min"),
        (h, 0) => format!("in {h} h"),
        (h, m) => format!("in {h} h {m} min"),
    }
}

/// "2 min before", "1 h before", "1 h 30 min before": a reminder's lead time.
pub fn lead_label(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min before"),
        (h, 0) => format!("{h} h before"),
        (h, m) => format!("{h} h {m} min before"),
    }
}

/// "Synced just now", "Synced 2 min ago", "Synced 1 h ago".
pub fn synced_label(at: i64, now: i64) -> String {
    let minutes = (now - at).max(0) / 60;
    match minutes {
        0 => "Synced just now".into(),
        m if m < 60 => format!("Synced {m} min ago"),
        m if m < 48 * 60 => format!("Synced {} h ago", m / 60),
        m => format!("Synced {} d ago", m / (24 * 60)),
    }
}

/// The event's start in local time, as the row shows it ("2:00 PM").
pub fn local_time(at: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(at, 0).single() {
        Some(time) => time.format("%-I:%M %p").to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_are_kept_in_range_once_each_longest_first() {
        assert_eq!(CalendarSettings::default().reminders, vec![2]);
        assert_eq!(CalendarSettings::normalize_reminders([2, 30, 2, 0, 5000, 10]), vec![30, 10, 2]);
        assert_eq!(CalendarSettings::normalize_reminders([1, 2, 3, 4, 5, 6]), vec![6, 5, 4, 3, 2]);
    }

    /// settings.json keeps the account, the choices and the reminders, never a token or secret; older
    /// settings without them read as the defaults.
    #[test]
    fn settings_json_holds_only_non_secret_calendar_choices() {
        let calendar = CalendarSettings { google_account: Some("maya@example.com".into()), google_calendars: Some(vec!["team".into()]),
            google_client_id: "1-a.apps.googleusercontent.com".into(), reminders: vec![30, 2] };
        let settings = crate::settings::Settings { calendar, ..Default::default() };
        let json = serde_json::to_value(&settings).unwrap();
        let calendar = json["calendar"].as_object().unwrap();
        let mut keys: Vec<&str> = calendar.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["googleAccount", "googleCalendars", "googleClientId", "reminders"]);
        let text = json.to_string().to_ascii_lowercase();
        assert!(!text.contains("token") && !text.contains("secret"), "{text}");
        let older: crate::settings::Settings = serde_json::from_str(r#"{"provider":"claude"}"#).unwrap();
        assert_eq!(older.calendar, CalendarSettings::default());
    }

    #[test]
    fn calendars_are_read_by_choice_or_as_google_shows_them() {
        let calendar = |id: &str, primary, shown| CalendarInfo { id: id.into(), name: id.into(), primary, color: None, shown_in_google: shown };
        let mut settings = CalendarSettings::default();
        assert!(settings.uses(&calendar("me", true, false)));
        assert!(settings.uses(&calendar("team", false, true)));
        assert!(!settings.uses(&calendar("holidays", false, false)));
        settings.google_calendars = Some(vec!["holidays".into()]);
        assert!(!settings.uses(&calendar("me", true, true)));
        assert!(settings.uses(&calendar("holidays", false, false)));
    }

    #[test]
    fn labels_read_as_drawn() {
        assert_eq!(countdown(1000, 1000 - 120), "in 2 min");
        assert_eq!(countdown(1000, 1000 - 61), "in 2 min");
        assert_eq!(countdown(1000, 1000 - 30), "in 1 min");
        assert_eq!(countdown(1000, 1000 - 3900), "in 1 h 5 min");
        assert_eq!(countdown(1000, 1000 - 3600), "in 1 h");
        assert_eq!(countdown(1000, 1010), "now");
        assert_eq!(countdown(1000, 1000 + 180), "started 3 min ago");
        assert_eq!((lead_label(2), lead_label(60), lead_label(90)), ("2 min before".into(), "1 h before".into(), "1 h 30 min before".into()));
        assert_eq!((synced_label(0, 30), synced_label(0, 125), synced_label(0, 3700)), ("Synced just now".into(), "Synced 2 min ago".into(), "Synced 1 h ago".into()));
    }

    #[test]
    fn only_timed_busy_accepted_meetings_are_reminded() {
        let event = Event { id: "e".into(), calendar: "c".into(), title: "t".into(), start: When::At(10), end: When::At(20), time_zone: None,
            attendees: vec![], meeting_link: None, free: false };
        assert!(event.remindable());
        assert!(!Event { free: true, ..event.clone() }.remindable());
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        assert!(!Event { start: When::AllDay(day), ..event.clone() }.remindable());
        let me = |declined| Attendee { email: "me@x".into(), name: None, is_self: true, declined };
        assert!(!Event { attendees: vec![me(true)], ..event.clone() }.remindable());
        assert!(Event { attendees: vec![me(false)], ..event.clone() }.remindable());
        assert_ne!(event.key(), Event { start: When::At(11), ..event.clone() }.key(), "a moved event is a new occurrence");
    }
}
