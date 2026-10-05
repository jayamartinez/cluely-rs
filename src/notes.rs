//! Model-written session notes: title, overview (per length), topics and follow-ups,
//! plus questions about a saved session. Runs on a background thread via `answer::complete`.

use std::sync::Arc;

use serde::Deserialize;

use crate::answer::{self, complete};
use crate::archive::{Notes, Session, Speaker, SummaryLength, Topic, clock};
use crate::codex::CodexClient;
use crate::providers::{Message, Part, Role};
use crate::settings::Settings;

/// Keep prompts well inside every provider's context window.
const MAX_TRANSCRIPT_CHARS: usize = 60_000;

const SYSTEM: &str = "You write accurate notes about a recorded conversation for the person who was in it (\"You\"). \
Use only what the transcript and answers contain. Never invent names, numbers, decisions or quotes.";

/// The session as plain text the model can read: timestamped lines and the answers shown.
pub fn transcript(session: &Session) -> String {
    let mut entries: Vec<(u64, String)> = session.transcript.iter().map(|line| {
        let who = if line.speaker == Speaker::You { "You" } else { "Them" };
        (line.at_ms, format!("[{}] {who}: {}", clock(line.at_ms), line.text))
    }).chain(session.turns.iter().map(|turn| {
        let asked = if turn.question.is_empty() { String::new() } else { format!(" (asked: {})", turn.question) };
        (turn.at_ms, format!("[{}] Assistant answer for \"{}\"{asked}: {}", clock(turn.at_ms), turn.action, turn.answer))
    })).collect();
    entries.sort_by_key(|(at, _)| *at);
    let mut text = entries.into_iter().map(|(_, line)| line).collect::<Vec<_>>().join("\n");
    if text.len() > MAX_TRANSCRIPT_CHARS {
        // Keep the end: the latest part of a long session matters most for notes.
        let cut = text.len() - MAX_TRANSCRIPT_CHARS;
        let boundary = (cut..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
        text = format!("[earlier part omitted]\n{}", &text[boundary..]);
    }
    text
}

fn length_rule(length: SummaryLength) -> &'static str {
    match length {
        SummaryLength::Brief => "one or two sentences",
        SummaryLength::Standard => "one paragraph of three to five sentences",
        SummaryLength::Detailed => "two to four paragraphs with concrete detail and short quotes where useful",
    }
}

fn ask_model(settings: &Settings, codex: &Arc<CodexClient>, prompt: String) -> Result<String, String> {
    let messages = vec![Message { role: Role::User, parts: vec![Part::Text(prompt)] }];
    complete(answer::target(settings, codex, SYSTEM.to_string(), messages, 2000)?)
}

#[derive(Deserialize)]
struct RawTopic { start: String, end: String, title: String, #[serde(default)] detail: String }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNotes { title: String, overview: String, #[serde(default)] topics: Vec<RawTopic>, #[serde(default)] follow_ups: Vec<String> }

/// Title and notes for a finished session.
pub fn generate(settings: &Settings, codex: &Arc<CodexClient>, session: &Session) -> Result<(String, Notes), String> {
    let prompt = format!(
        "Write notes for this session. Reply with only a JSON object, no code fence:\n\
         {{\"title\": \"3-6 word title\", \"overview\": \"{}\", \"topics\": [{{\"start\": \"mm:ss\", \"end\": \"mm:ss\", \"title\": \"short\", \"detail\": \"one line\"}}], \
         \"followUps\": [\"concrete next step for You\"]}}\n\
         Topics cover the session in order (2-6 of them). Follow-ups only if the conversation implies them (0-4).\n\nSession:\n{}",
        length_rule(SummaryLength::Standard), transcript(session));
    parse_notes(&ask_model(settings, codex, prompt)?)
}

fn parse_notes(reply: &str) -> Result<(String, Notes), String> {
    let json = extract_json(reply).ok_or("The model didn't return notes in the expected format.")?;
    let raw: RawNotes = serde_json::from_str(json).map_err(|_| "The model didn't return notes in the expected format.".to_string())?;
    let topics = raw.topics.into_iter().filter_map(|t| Some(Topic { start_ms: parse_clock(&t.start)?, end_ms: parse_clock(&t.end)?, title: t.title, detail: t.detail })).collect();
    let mut notes = Notes { topics, follow_ups: raw.follow_ups, ..Notes::default() };
    notes.overviews.insert(SummaryLength::Standard, raw.overview.trim().to_string());
    Ok((raw.title.trim().chars().take(80).collect(), notes))
}

/// Rewrite the overview at a given length. `expand` asks for more depth than the current text.
pub fn overview(settings: &Settings, codex: &Arc<CodexClient>, session: &Session, length: SummaryLength, expand: bool) -> Result<String, String> {
    let current = session.notes.as_ref().and_then(|n| n.overviews.get(&length).or_else(|| n.overviews.values().next())).cloned().unwrap_or_default();
    let instruction = if expand && !current.is_empty() {
        format!("Expand this overview with more concrete detail from the session (what was asked, how You answered, outcomes). Current overview:\n{current}")
    } else {
        format!("Write an overview of this session in {}.", length_rule(length))
    };
    let reply = ask_model(settings, codex, format!("{instruction}\nReply with only the overview text, in second person (\"You …\").\n\nSession:\n{}", transcript(session)))?;
    Ok(reply.trim().to_string())
}

/// Answer a question about a saved session.
pub fn ask(settings: &Settings, codex: &Arc<CodexClient>, session: &Session, question: &str) -> Result<String, String> {
    ask_model(settings, codex, format!("Answer this question about the session, citing timestamps like [12:34] where helpful. \
        If the session doesn't say, answer that it doesn't.\n\nQuestion: {question}\n\nSession:\n{}", transcript(session)))
}

fn extract_json(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

fn parse_clock(text: &str) -> Option<u64> {
    let mut parts = text.trim().split(':').map(|p| p.trim().parse::<u64>().ok());
    let (a, b, c) = (parts.next()??, parts.next().flatten(), parts.next().flatten());
    Some(match (b, c) { (Some(b), Some(c)) => (a * 3600 + b * 60 + c) * 1000, (Some(b), None) => (a * 60 + b) * 1000, _ => a * 1000 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_parse_from_fenced_json_and_drop_unparseable_topics() {
        let reply = "Here you go:\n```json\n{\"title\": \"System design interview\", \"overview\": \"You designed a rate limiter.\",\
            \"topics\": [{\"start\": \"02:58\", \"end\": \"16:40\", \"title\": \"Rate limiting\"}, {\"start\": \"soon\", \"end\": \"later\", \"title\": \"Bad\"}],\
            \"followUps\": [\"Send a thank-you note\"]}\n```";
        let (title, notes) = parse_notes(reply).unwrap();
        assert_eq!(title, "System design interview");
        assert_eq!(notes.overviews[&SummaryLength::Standard], "You designed a rate limiter.");
        assert_eq!(notes.topics.len(), 1);
        assert_eq!((notes.topics[0].start_ms, notes.topics[0].end_ms), (178_000, 1_000_000));
        assert_eq!(notes.follow_ups, ["Send a thank-you note"]);
        assert!(parse_notes("no json here").is_err());
    }

    #[test]
    fn clocks_accept_minutes_seconds_and_hours() {
        assert_eq!(parse_clock("1:02:03"), Some(3_723_000));
        assert_eq!(parse_clock(" 12:05 "), Some(725_000));
        assert_eq!(parse_clock("x:1"), None);
    }

    #[test]
    fn long_transcripts_keep_the_most_recent_part() {
        let line = |at_ms, text: &str| crate::archive::Line { at_ms, speaker: Speaker::Them, text: text.into() };
        let session = Session { id: "s".into(), started_at: 0, ended_at: Some(1), title: None, model: None, notes: None, turns: vec![],
            transcript: vec![line(0, &"a".repeat(70_000)), line(1000, "final question?")] };
        let text = transcript(&session);
        assert!(text.len() <= MAX_TRANSCRIPT_CHARS + 40);
        assert!(text.starts_with("[earlier part omitted]") && text.ends_with("final question?"));
    }
}
