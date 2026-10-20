//! When an upcoming-meeting row shows (Paper F8b). Each reminder (a lead time in minutes) fires
//! once per event occurrence: at its lead time, or late if CluelyRS wasn't running then, as long
//! as no shorter reminder is also due. A reminder that comes due while the row can't be shown (the
//! overlay is hidden, or Live is running) is skipped, never saved for later. A row goes away when
//! it's dismissed, five minutes after the start, when the event is gone, or when Live starts.
//!
//! The clock is passed in, so this is tested without waiting.

use std::collections::HashSet;

use super::Event;

/// How long after its start an event's row stays up.
pub const GRACE_SECONDS: i64 = 5 * 60;

/// A row on the overlay: the event and the reminder that showed it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub lead: u32,
    pub event: Event,
}

#[derive(Default)]
pub struct Reminders {
    /// Reminders that came due, shown or skipped: (event key, lead minutes).
    fired: HashSet<(String, u32)>,
    /// Rows up now, at most one per event.
    rows: Vec<Row>,
}

impl Reminders {
    /// Bring the rows up to date at `now` (Unix seconds) with the latest `events` and reminder
    /// `leads`. `can_show` is whether a new row may appear: the overlay is on screen and Live isn't
    /// running. Returns whether the rows changed.
    pub fn tick(&mut self, events: &[Event], leads: &[u32], now: i64, can_show: bool) -> bool {
        let before = self.rows.clone();
        // Rows follow their event (a renamed title), and go once it's over, gone or no longer remindable.
        self.rows.retain_mut(|row| match events.iter().find(|event| event.key() == row.key && event.remindable()) {
            Some(event) if event.start.at().is_some_and(|start| now < start + GRACE_SECONDS) => { row.event = event.clone(); true }
            _ => false,
        });
        for event in events.iter().filter(|event| event.remindable()) {
            let Some(start) = event.start.at() else { continue };
            if now >= start + GRACE_SECONDS { continue; }
            let key = event.key();
            let due: Vec<u32> = leads.iter().copied().filter(|lead| now >= start - i64::from(*lead) * 60).collect();
            let Some(shortest) = due.iter().copied().min() else { continue };
            let fires = !self.fired.contains(&(key.clone(), shortest));
            for lead in due { self.fired.insert((key.clone(), lead)); }
            if !fires || !can_show { continue; }
            let row = Row { key: key.clone(), lead: shortest, event: event.clone() };
            match self.rows.iter_mut().find(|row| row.key == key) {
                Some(existing) => *existing = row,
                None => self.rows.push(row),
            }
        }
        // Forget occurrences that are no longer listed (past, moved or deleted): they can't fire again.
        let listed: HashSet<String> = events.iter().map(Event::key).collect();
        self.fired.retain(|(key, _)| listed.contains(key));
        self.rows != before
    }

    /// The row the overlay shows: the soonest event's.
    pub fn current(&self) -> Option<&Row> {
        self.rows.iter().min_by_key(|row| row.event.start.at().unwrap_or(i64::MAX))
    }

    /// ✕ on a row: that reminder only. A shorter one for the same event still fires.
    pub fn dismiss(&mut self, key: &str) { self.rows.retain(|row| row.key != key); }

    /// Live started: every row goes.
    pub fn clear_rows(&mut self) { self.rows.clear(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::{Attendee, When};

    const START: i64 = 1_800_000_000;
    const MIN: i64 = 60;

    fn event(id: &str, start: i64) -> Event {
        Event { id: id.into(), calendar: "primary".into(), title: format!("Meeting {id}"), start: When::At(start), end: When::At(start + 30 * MIN),
            time_zone: Some("Europe/London".into()), attendees: vec![], meeting_link: None, free: false }
    }

    /// The lead of the row up for `id`, if any.
    fn row_for(reminders: &Reminders, id: &str) -> Option<u32> { reminders.rows.iter().find(|row| row.event.id == id).map(|row| row.lead) }

    #[test]
    fn each_lead_time_fires_once_at_its_time() {
        let events = [event("a", START)];
        let leads = [30, 2];
        let mut reminders = Reminders::default();
        assert!(!reminders.tick(&events, &leads, START - 31 * MIN, true));
        assert_eq!(reminders.current(), None);
        assert!(reminders.tick(&events, &leads, START - 30 * MIN, true));
        assert_eq!(row_for(&reminders, "a"), Some(30));
        // Dismissed, it stays away until the next reminder…
        reminders.dismiss(&events[0].key());
        assert!(!reminders.tick(&events, &leads, START - 10 * MIN, true));
        assert_eq!(reminders.current(), None);
        // …which fires on its own.
        assert!(reminders.tick(&events, &leads, START - 2 * MIN, true));
        assert_eq!(row_for(&reminders, "a"), Some(2));
        // Each fires once: dismissing the last one leaves nothing more to fire.
        reminders.dismiss(&events[0].key());
        for at in [START - MIN, START, START + 4 * MIN] { assert!(!reminders.tick(&events, &leads, at, true)); }
        assert_eq!(reminders.current(), None);
    }

    #[test]
    fn an_undismissed_row_moves_to_the_next_reminder_and_leaves_after_the_start() {
        let events = [event("a", START)];
        let mut reminders = Reminders::default();
        reminders.tick(&events, &[30, 2], START - 30 * MIN, true);
        reminders.tick(&events, &[30, 2], START - 2 * MIN, true);
        assert_eq!(reminders.rows.len(), 1, "one row per event");
        assert_eq!(row_for(&reminders, "a"), Some(2));
        assert!(!reminders.tick(&events, &[30, 2], START + GRACE_SECONDS - 1, true));
        assert!(reminders.tick(&events, &[30, 2], START + GRACE_SECONDS, true));
        assert_eq!(reminders.current(), None);
    }

    #[test]
    fn a_late_start_fires_only_the_shortest_reminder_due() {
        let events = [event("a", START)];
        let mut reminders = Reminders::default();
        // Launched a minute before: the 30 and 10 minute reminders are past, only the 2 minute one shows.
        assert!(reminders.tick(&events, &[30, 10, 2], START - MIN, true));
        assert_eq!(row_for(&reminders, "a"), Some(2));
        reminders.dismiss(&events[0].key());
        assert!(!reminders.tick(&events, &[30, 10, 2], START - MIN + 10, true), "the longer ones never fire afterwards");
        // Launched 12 minutes before: the 30 minute one is shown late, the 2 minute one still comes.
        let mut reminders = Reminders::default();
        assert!(reminders.tick(&events, &[30, 2], START - 12 * MIN, true));
        assert_eq!(row_for(&reminders, "a"), Some(30));
    }

    #[test]
    fn a_reminder_due_while_hidden_is_skipped_not_kept() {
        let events = [event("a", START)];
        let mut reminders = Reminders::default();
        assert!(!reminders.tick(&events, &[30, 2], START - 30 * MIN, false));
        assert_eq!(reminders.current(), None);
        // Shown again later: the skipped reminder stays skipped; the next one fires.
        assert!(!reminders.tick(&events, &[30, 2], START - 20 * MIN, true));
        assert!(reminders.tick(&events, &[30, 2], START - 2 * MIN, true));
        assert_eq!(row_for(&reminders, "a"), Some(2));
    }

    #[test]
    fn edited_reminders_neither_repeat_nor_bring_back_past_ones() {
        let events = [event("a", START)];
        let mut reminders = Reminders::default();
        reminders.tick(&events, &[2], START - 2 * MIN, true);
        reminders.dismiss(&events[0].key());
        // Adding a longer reminder after a shorter one fired doesn't show it now.
        assert!(!reminders.tick(&events, &[10, 2], START - 90, true));
        // Adding one that comes due later does fire.
        let mut reminders = Reminders::default();
        reminders.tick(&events, &[2], START - 20 * MIN, true);
        assert!(reminders.tick(&events, &[15, 2], START - 15 * MIN, true));
        assert_eq!(row_for(&reminders, "a"), Some(15));
        // Changing 2 to 5 fires the 5 at its time.
        reminders.dismiss(&events[0].key());
        assert!(reminders.tick(&events, &[15, 5], START - 5 * MIN, true));
        assert_eq!(row_for(&reminders, "a"), Some(5));
    }

    #[test]
    fn moved_cancelled_and_unremindable_events() {
        let mut reminders = Reminders::default();
        reminders.tick(&[event("a", START)], &[2], START - 2 * MIN, true);
        assert!(reminders.current().is_some());
        // Moved later: the old row goes, and the new time is reminded again.
        let moved = [event("a", START + 60 * MIN)];
        assert!(reminders.tick(&moved, &[2], START - MIN, true));
        assert_eq!(reminders.current(), None);
        assert!(reminders.tick(&moved, &[2], START + 58 * MIN, true));
        // Gone from the calendar (cancelled): its row goes.
        assert!(reminders.tick(&[], &[2], START + 58 * MIN + 10, true));
        assert_eq!(reminders.current(), None);
        // Declined, free and all-day events never show.
        let declined = Event { attendees: vec![Attendee { email: "me@x".into(), name: None, is_self: true, declined: true }], ..event("d", START) };
        let free = Event { free: true, ..event("f", START) };
        let all_day = Event { start: When::AllDay(chrono::NaiveDate::from_ymd_opt(2027, 1, 15).unwrap()), ..event("all", START) };
        let mut reminders = Reminders::default();
        assert!(!reminders.tick(&[declined, free, all_day], &[2], START - MIN, true));
    }

    #[test]
    fn the_soonest_meeting_shows_first_and_live_clears_rows() {
        let events = [event("later", START + 10 * MIN), event("soon", START)];
        let mut reminders = Reminders::default();
        reminders.tick(&events, &[30], START - 10 * MIN, true);
        assert_eq!(reminders.current().map(|row| row.event.id.as_str()), Some("soon"));
        reminders.dismiss(&events[1].key());
        assert_eq!(reminders.current().map(|row| row.event.id.as_str()), Some("later"));
        reminders.clear_rows();
        assert_eq!(reminders.current(), None);
        assert!(!reminders.tick(&events, &[30], START - 9 * MIN, true), "cleared rows don't come back");
    }
}
