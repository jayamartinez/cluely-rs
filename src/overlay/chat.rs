//! The chat on the card across Live sessions, as in the Paper artboard "Session continuity v2 ·
//! Chat stays · New session divider". When Live stops, the chat stays on the card. When the next
//! session starts (the waveform, typing a question, Assist or a quick action all start it through
//! `Overlay::set_live`), the earlier turns stay above a thin "New session · 7:42 PM" divider and
//! the new session's turns follow below it. Only the display keeps them: the model, speculation
//! and Recap see the current session's turns only, and each session is saved to Sessions on its
//! own. Nothing is kept across launches, so the overlay starts empty after a restart.

use chrono::{DateTime, Local};
use gpui::{FontWeight, IntoElement, ParentElement, Styled, div, px};

use super::{Status, Turn};
use crate::answer::Exchange;
use crate::theme;

/// Where a later session starts on the card.
pub(super) struct Divider {
    /// Index in `Overlay::turns` of the session's first turn (the length of `turns` until it has one).
    pub before: usize,
    pub label: String,
}

/// "New session · 7:42 PM", with the time the session started (written as the Sessions window does).
pub(super) fn divider_label(started: DateTime<Local>) -> String {
    format!("New session · {}", started.format("%-I:%M %p"))
}

/// A session starts with `turns` earlier turns on the card. Returns whether a divider was added:
/// none when the card is empty (the first session since launch), and a session that ended without
/// a turn has its divider replaced rather than stacked.
pub(super) fn begin(turns: usize, dividers: &mut Vec<Divider>, label: String) -> bool {
    if turns == 0 { return false; }
    match dividers.last_mut() {
        Some(last) if last.before == turns => last.label = label,
        _ => dividers.push(Divider { before: turns, label }),
    }
    true
}

/// Live stopped: answers still being written stay on the card, marked as stopped.
pub(super) fn stop(turns: &mut [Turn]) {
    for turn in turns.iter_mut().filter(|turn| turn.status == Status::Streaming) { turn.status = Status::Stopped; }
}

/// Finished exchanges of `session`, oldest first: what the model is given as earlier context.
pub(super) fn history(turns: &[Turn], session: u64) -> Vec<Exchange> {
    turns.iter().filter(|turn| turn.session == session && turn.status == Status::Done)
        .map(|turn| Exchange { action: turn.action.to_string(), question: turn.question.clone(), answer: turn.text.clone() }).collect()
}

/// The hairline with its centred label between two sessions.
pub(super) fn divider(label: &str) -> impl IntoElement {
    let rule = || div().flex_1().h(px(1.0)).bg(theme::hairline());
    div().flex().items_center().gap(px(10.0)).pt(px(4.0))
        .child(rule())
        .child(div().flex_none().text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(label.to_string()))
        .child(rule())
}

/// Under an answer Live stopped while it was being written.
pub(super) fn stopped_mark() -> impl IntoElement {
    div().flex().items_center().gap(px(6.0)).pt(px(6.0))
        .child(div().size(px(6.0)).flex_none().rounded_full().bg(theme::muted()))
        .child(div().text_size(px(12.0)).text_color(theme::muted()).child("Stopped when Live ended"))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn turn(id: u64, session: u64, status: Status) -> Turn {
        Turn { id, action: "Ask".into(), question: format!("question {id}"), text: format!("answer {id}"), status, at_ms: 0,
            screenshot: None, auto: None, session }
    }

    fn label(hour: u32, minute: u32) -> String {
        divider_label(Local.with_ymd_and_hms(2026, 10, 9, hour, minute, 0).single().unwrap())
    }

    #[test]
    fn the_divider_says_when_the_new_session_started() {
        assert_eq!(label(19, 42), "New session · 7:42 PM");
        assert_eq!(label(9, 5), "New session · 9:05 AM");
    }

    /// Display and model context per session: every turn stays on the card, the model only gets
    /// the current session's finished exchanges, and an answer cut off by Live stopping is kept
    /// (marked stopped) but never becomes context.
    #[test]
    fn the_card_keeps_every_session_but_the_model_sees_only_the_current_one() {
        let mut turns = vec![turn(1, 1, Status::Done), turn(2, 1, Status::Failed("Network error".into())), turn(3, 1, Status::Streaming)];
        assert_eq!(history(&turns, 1).iter().map(|e| e.question.as_str()).collect::<Vec<_>>(), ["question 1"]);
        stop(&mut turns);
        assert!(turns[2].status == Status::Stopped && turns[2].text == "answer 3", "kept with its text, marked stopped");
        assert!(turns[1].status == Status::Failed("Network error".into()) && turns[0].status == Status::Done, "others unchanged");
        // The next session starts: nothing from session 1 is context, though all of it is shown.
        assert!(history(&turns, 2).is_empty());
        turns.push(turn(4, 2, Status::Done));
        assert_eq!(history(&turns, 2).iter().map(|e| e.question.as_str()).collect::<Vec<_>>(), ["question 4"]);
        assert_eq!(turns.len(), 4);
    }

    /// The request the model gets in session 2 (built as for any answer, history budget included)
    /// carries session 2's exchanges only, even with many earlier ones still on the card.
    #[test]
    fn a_request_in_a_new_session_carries_none_of_the_earlier_sessions() {
        use crate::answer::{Target, build};
        use crate::providers::Part;
        use crate::settings::{Provider, Settings};
        let mut turns: Vec<Turn> = (1..=12).map(|id| turn(id, 1, Status::Done)).collect();
        turns.push(turn(13, 2, Status::Done));
        let settings = Settings { provider: Provider::ApiKey, api_provider: "custom".into(), custom_base_url: "http://127.0.0.1:9/v1".into(),
            api_models: [("custom".to_string(), "m".to_string())].into(), ..Settings::default() };
        let Target::Api(request, _) = build(&settings, &crate::codex::CodexClient::new(), "Assist", "", &history(&turns, 2), "", None).unwrap()
            else { panic!("API") };
        let texts: Vec<String> = request.messages.iter().map(|m| match &m.parts[0] { Part::Text(text) => text.clone(), Part::Jpeg(_) => String::new() }).collect();
        assert_eq!(texts.len(), 3, "one earlier exchange and the new turn: {texts:?}");
        assert_eq!((texts[0].as_str(), texts[1].as_str()), ("Ask: question 13", "answer 13"));
    }

    /// Every start goes through `Overlay::set_live(true)`, which calls `begin` with the turns on
    /// the card. The first session since launch (an empty card, as after a restart) gets no
    /// divider; each later one gets one where its turns begin; a session that asked nothing has
    /// its divider replaced by the next one.
    #[test]
    fn each_later_session_starts_under_one_divider() {
        let mut dividers = Vec::new();
        assert!(!begin(0, &mut dividers, label(19, 30)), "after launch the card is empty");
        assert!(dividers.is_empty());
        assert!(begin(2, &mut dividers, label(19, 35)));
        assert!(begin(2, &mut dividers, label(19, 42)), "the 7:35 session asked nothing");
        assert!(begin(5, &mut dividers, label(19, 50)));
        let shown: Vec<(usize, &str)> = dividers.iter().map(|d| (d.before, d.label.as_str())).collect();
        assert_eq!(shown, [(2, "New session · 7:42 PM"), (5, "New session · 7:50 PM")]);
    }
}
