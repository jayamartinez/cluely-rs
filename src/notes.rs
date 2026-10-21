//! Model-written session notes and the follow-up email draft, plus questions about a saved
//! session. Runs on a background thread via `answer::complete`.
//!
//! Notes are written under the note sections of the session's mode (`modes::NoteSection`); a mode
//! without sections gets a title, an overview, timestamped topics and follow-ups. The email draft
//! comes back in the same request as the notes, so it costs no second pass over the transcript;
//! writing it again later (`email`) works from the notes alone.
//!
//! Each model call has a `*_with` form that takes the request as a function, so tests can stand in
//! for the provider.

use std::sync::Arc;

use serde::Deserialize;

use crate::answer::{self, complete};
use crate::archive::{EmailDraft, NoteBlock, Notes, Session, Speaker, SummaryLength, Topic, clock, notes_markdown};
use crate::codex::CodexClient;
use crate::modes::NoteSection;
use crate::providers::{Message, Part, Role};
use crate::settings::Settings;

/// Keep prompts well inside every provider's context window.
const MAX_TRANSCRIPT_CHARS: usize = 60_000;

const SYSTEM: &str = "You write accurate notes about a recorded conversation for the person who was in it (\"You\"). \
Use only what the transcript and answers contain. Never invent names, numbers, decisions or quotes.";

const EMAIL_RULES: &str = "The email is a short follow-up from You to the other person in the conversation: thank them, \
recall one or two specifics that were discussed and include anything You promised to send or do. Plain text with blank lines \
between paragraphs, no placeholders. \"to\" is the other person's name if it was said, otherwise empty (then greet with just \"Hi,\"). \
End with \"Best,\" and You's first name only if it was said.";

const MALFORMED: &str = "The model didn't return notes in the expected format.";

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

/// What Copy transcript puts on the clipboard: what was said, with times and speakers.
pub fn spoken_transcript(session: &Session) -> String {
    session.transcript.iter().map(|line| {
        let who = if line.speaker == Speaker::You { "You" } else { "Them" };
        format!("[{}] {who}: {}", clock(line.at_ms), line.text)
    }).collect::<Vec<_>>().join("\n")
}

fn length_rule(length: SummaryLength) -> &'static str {
    match length {
        SummaryLength::Brief => "one or two sentences",
        SummaryLength::Standard => "one paragraph of three to five sentences",
        SummaryLength::Detailed => "two to four paragraphs with concrete detail and short quotes where useful",
    }
}

/// One request to the provider chosen in Settings.
fn ask_model(settings: &Settings, codex: &Arc<CodexClient>, prompt: String, max_tokens: u32) -> Result<String, String> {
    let messages = vec![Message { role: Role::User, parts: vec![Part::Text(prompt)] }];
    complete(answer::target(settings, codex, SYSTEM.to_string(), messages, max_tokens)?)
}

/// Title, notes and email draft for a session, as they come back from the model.
#[derive(Debug, PartialEq)]
pub struct Generated {
    pub title: String,
    pub notes: Notes,
    pub email: Option<EmailDraft>,
}

#[derive(Deserialize)]
struct RawTopic { start: String, end: String, title: String, #[serde(default)] detail: String }

#[derive(Deserialize)]
struct RawSection { #[serde(default)] name: String, #[serde(default)] text: String }

#[derive(Deserialize)]
struct RawEmail { #[serde(default)] to: String, #[serde(default)] subject: String, #[serde(default)] body: String }

impl RawEmail {
    /// The draft, or `None` when the model left the body empty.
    fn draft(self) -> Option<EmailDraft> {
        let draft = EmailDraft { to: self.to.trim().into(), subject: self.subject.trim().into(), body: self.body.trim().into(), edited_at: None };
        (!draft.body.is_empty()).then_some(draft)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNotes {
    title: String,
    #[serde(default)] overview: String,
    #[serde(default)] topics: Vec<RawTopic>,
    #[serde(default)] follow_ups: Vec<String>,
    #[serde(default)] sections: Vec<RawSection>,
    #[serde(default)] email: Option<RawEmail>,
}

/// The request for a session's notes: under `sections` in order, or in the general format when
/// there are none. Either way the email draft is asked for too.
pub fn notes_prompt(session: &Session, sections: &[NoteSection]) -> String {
    let email = "\"email\": {\"to\": \"their name or empty\", \"subject\": \"short subject\", \"body\": \"the email\"}";
    let shape = if sections.is_empty() {
        format!("{{\"title\": \"3-6 word title\", \"overview\": \"{}\", \"topics\": [{{\"start\": \"mm:ss\", \"end\": \"mm:ss\", \"title\": \"short\", \"detail\": \"one line\"}}], \
            \"followUps\": [\"concrete next step for You\"], {email}}}\n\
            Topics cover the session in order (2-6 of them). Follow-ups only if the conversation implies them (0-4).",
            length_rule(SummaryLength::Standard))
    } else {
        let headings = sections.iter()
            .map(|section| if section.description.is_empty() { format!("- {}", section.name) } else { format!("- {}: {}", section.name, section.description) })
            .collect::<Vec<_>>().join("\n");
        format!("{{\"title\": \"3-6 word title\", \"sections\": [{{\"name\": \"the heading exactly as given\", \"text\": \"what goes under it\"}}], {email}}}\n\
            Write one entry per heading below, in this order. Each text is short Markdown: a sentence or two, or \"- \" bullet points, \
            terse and factual. Leave a text empty when the session has nothing for that heading.\nHeadings:\n{headings}")
    };
    format!("Write notes for this session. Reply with only a JSON object, no code fence:\n{shape}\n{EMAIL_RULES}\n\nSession:\n{}", transcript(session))
}

/// Title, notes and email draft for a session, from the model chosen in Settings.
pub fn generate(settings: &Settings, codex: &Arc<CodexClient>, session: &Session, sections: &[NoteSection]) -> Result<Generated, String> {
    generate_with(|prompt| ask_model(settings, codex, prompt, 3000), session, sections)
}

/// [`generate`], with `ask` sending the request.
pub fn generate_with(ask: impl FnOnce(String) -> Result<String, String>, session: &Session, sections: &[NoteSection]) -> Result<Generated, String> {
    parse_notes(&ask(notes_prompt(session, sections))?, sections)
}

fn parse_notes(reply: &str, sections: &[NoteSection]) -> Result<Generated, String> {
    let json = extract_json(reply).ok_or(MALFORMED)?;
    let raw: RawNotes = serde_json::from_str(json).map_err(|_| MALFORMED.to_string())?;
    let mut notes = Notes::default();
    if sections.is_empty() {
        notes.topics = raw.topics.into_iter().filter_map(|t| Some(Topic { start_ms: parse_clock(&t.start)?, end_ms: parse_clock(&t.end)?, title: t.title, detail: t.detail })).collect();
        notes.follow_ups = raw.follow_ups;
        notes.overviews.insert(SummaryLength::Standard, raw.overview.trim().to_string());
    } else {
        // The headings are the mode's, whatever the model called them: matched by name, else by place.
        notes.sections = sections.iter().enumerate().map(|(index, section)| {
            let text = raw.sections.iter().find(|raw| same_heading(&raw.name, &section.name)).or_else(|| raw.sections.get(index))
                .map(|raw| raw.text.trim().to_string()).unwrap_or_default();
            NoteBlock { name: section.name.clone(), text, edited_at: None }
        }).collect();
    }
    let email = raw.email.and_then(RawEmail::draft);
    Ok(Generated { title: raw.title.trim().chars().take(80).collect(), notes, email })
}

fn same_heading(a: &str, b: &str) -> bool { a.trim().eq_ignore_ascii_case(b.trim()) }

/// Put freshly generated notes on `session`. With `keep_edits`, sections the user edited (still
/// among the headings) and an edited email draft stay as the user left them.
pub fn apply(session: &mut Session, generated: Generated, keep_edits: bool) {
    if !generated.title.is_empty() { session.title = Some(generated.title); }
    let mut notes = generated.notes;
    if keep_edits && let Some(old) = &session.notes {
        for block in &mut notes.sections {
            if let Some(kept) = old.sections.iter().find(|old| old.edited_at.is_some() && same_heading(&old.name, &block.name)) { *block = kept.clone(); }
        }
    }
    session.notes = Some(notes);
    let email_edited = session.email.as_ref().is_some_and(|email| email.edited_at.is_some());
    if let Some(email) = generated.email && !(keep_edits && email_edited) { session.email = Some(email); }
}

/// Whether regenerating would replace anything the user wrote.
pub fn has_edits(session: &Session) -> bool {
    session.notes.as_ref().and_then(Notes::edited_at).is_some() || session.email.as_ref().is_some_and(|email| email.edited_at.is_some())
}

/// The request for an email draft on its own: from the notes when there are some, which keeps it
/// small, else from the transcript.
pub fn email_prompt(session: &Session) -> String {
    let notes = session.notes.as_ref().map(notes_markdown).filter(|text| !text.trim().is_empty());
    let source = match notes {
        Some(notes) => format!("Notes from the session \"{}\":\n{notes}", session.title.as_deref().unwrap_or("Session")),
        None => format!("Session:\n{}", transcript(session)),
    };
    format!("Write a follow-up email for this session. Reply with only a JSON object, no code fence:\n\
        {{\"to\": \"their name or empty\", \"subject\": \"short subject\", \"body\": \"the email\"}}\n{EMAIL_RULES}\n\n{source}")
}

/// An email draft for a session, from the model chosen in Settings.
pub fn email(settings: &Settings, codex: &Arc<CodexClient>, session: &Session) -> Result<EmailDraft, String> {
    email_with(|prompt| ask_model(settings, codex, prompt, 1200), session)
}

/// [`email`], with `ask` sending the request.
pub fn email_with(ask: impl FnOnce(String) -> Result<String, String>, session: &Session) -> Result<EmailDraft, String> {
    let reply = ask(email_prompt(session))?;
    let malformed = "The model didn't return an email in the expected format.";
    let raw: RawEmail = serde_json::from_str(extract_json(&reply).ok_or(malformed)?).map_err(|_| malformed.to_string())?;
    raw.draft().ok_or_else(|| malformed.to_string())
}

/// Rewrite the overview at a given length. `expand` asks for more depth than the current text.
pub fn overview(settings: &Settings, codex: &Arc<CodexClient>, session: &Session, length: SummaryLength, expand: bool) -> Result<String, String> {
    let current = session.notes.as_ref().and_then(|n| n.overviews.get(&length).or_else(|| n.overviews.values().next())).cloned().unwrap_or_default();
    let instruction = if expand && !current.is_empty() {
        format!("Expand this overview with more concrete detail from the session (what was asked, how You answered, outcomes). Current overview:\n{current}")
    } else {
        format!("Write an overview of this session in {}.", length_rule(length))
    };
    let reply = ask_model(settings, codex, format!("{instruction}\nReply with only the overview text, in second person (\"You …\").\n\nSession:\n{}", transcript(session)), 2000)?;
    Ok(reply.trim().to_string())
}

/// Answer a question about a saved session.
pub fn ask(settings: &Settings, codex: &Arc<CodexClient>, session: &Session, question: &str) -> Result<String, String> {
    ask_model(settings, codex, format!("Answer this question about the session, citing timestamps like [12:34] where helpful. \
        If the session doesn't say, answer that it doesn't.\n\nQuestion: {question}\n\nSession:\n{}", transcript(session)), 2000)
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
    use std::cell::RefCell;

    use super::*;
    use crate::archive::Line;

    fn session(lines: Vec<Line>) -> Session {
        Session { id: "s".into(), started_at: 0, ended_at: Some(1), title: None, model: None, mode: None, notes: None, email: None, turns: vec![], transcript: lines }
    }

    fn line(at_ms: u64, speaker: Speaker, text: &str) -> Line { Line { at_ms, speaker, text: text.into() } }

    /// Stands in for the provider: records each request and answers with `reply`.
    fn fake(reply: &str, sent: &RefCell<Vec<String>>) -> impl FnOnce(String) -> Result<String, String> {
        let reply = reply.to_string();
        move |prompt| { sent.borrow_mut().push(prompt); Ok(reply) }
    }

    fn coding_sections() -> Vec<NoteSection> {
        crate::modes::ModeStore::at(None).get("coding").unwrap().note_template()
    }

    #[test]
    fn notes_parse_from_fenced_json_and_drop_unparseable_topics() {
        let reply = "Here you go:\n```json\n{\"title\": \"System design interview\", \"overview\": \"You designed a rate limiter.\",\
            \"topics\": [{\"start\": \"02:58\", \"end\": \"16:40\", \"title\": \"Rate limiting\"}, {\"start\": \"soon\", \"end\": \"later\", \"title\": \"Bad\"}],\
            \"followUps\": [\"Send a thank-you note\"]}\n```";
        let generated = parse_notes(reply, &[]).unwrap();
        assert_eq!(generated.title, "System design interview");
        let notes = generated.notes;
        assert_eq!(notes.overviews[&SummaryLength::Standard], "You designed a rate limiter.");
        assert_eq!(notes.topics.len(), 1);
        assert_eq!((notes.topics[0].start_ms, notes.topics[0].end_ms), (178_000, 1_000_000));
        assert_eq!(notes.follow_ups, ["Send a thank-you note"]);
        assert!(notes.sections.is_empty());
        assert_eq!(generated.email, None, "no email in the reply");
        assert!(parse_notes("no json here", &[]).is_err());
    }

    #[test]
    fn notes_follow_the_modes_sections_in_order() {
        let sent = RefCell::new(Vec::new());
        let reply = r#"{"title": "System design round", "sections": [
            {"name": "my approach", "text": "- Token bucket per key"},
            {"name": "Problem", "text": "Rate limiting for a bursty API."},
            {"name": "Something else", "text": "Ignored"}],
            "email": {"to": " Priya ", "subject": "Thanks for today", "body": "Hi Priya,\n\nThanks.\n\nBest,"}}"#;
        let sections = coding_sections();
        let generated = generate_with(fake(reply, &sent), &session(vec![line(0, Speaker::Them, "Design a rate limiter?")]), &sections).unwrap();
        let prompt = &sent.borrow()[0];
        assert!(prompt.contains("Headings:\n- Problem: The question as they asked it, in one line\n- My approach: What I proposed, with time and space\n- Interviewer signals:"), "{prompt}");
        assert!(prompt.contains("\"email\"") && prompt.contains("[00:00] Them: Design a rate limiter?"));
        let blocks: Vec<(&str, &str)> = generated.notes.sections.iter().map(|b| (b.name.as_str(), b.text.as_str())).collect();
        // Matched by name, else by place; the mode decides the headings and their order.
        assert_eq!(blocks, [("Problem", "Rate limiting for a bursty API."), ("My approach", "- Token bucket per key"),
            ("Interviewer signals", "Ignored"), ("Follow-ups", "")]);
        assert!(generated.notes.overviews.is_empty() && generated.notes.topics.is_empty());
        let email = generated.email.unwrap();
        assert_eq!((email.to.as_str(), email.subject.as_str()), ("Priya", "Thanks for today"));
        assert_eq!(email.clipboard_text(), "Subject: Thanks for today\n\nHi Priya,\n\nThanks.\n\nBest,");
    }

    #[test]
    fn modes_without_sections_ask_for_the_general_format() {
        let prompt = notes_prompt(&session(vec![]), &[]);
        assert!(prompt.contains("\"overview\"") && prompt.contains("\"followUps\"") && !prompt.contains("Headings:"));
        assert!(prompt.contains("\"email\""), "the email comes with the notes");
    }

    #[test]
    fn the_email_request_works_from_the_notes_when_there_are_some() {
        let sent = RefCell::new(Vec::new());
        let mut with_notes = session(vec![line(0, Speaker::Them, "SECRET TRANSCRIPT")]);
        with_notes.title = Some("System design round".into());
        with_notes.notes = Some(Notes { sections: vec![NoteBlock { name: "Problem".into(), text: "Rate limiting.".into(), edited_at: Some(5) },
            NoteBlock { name: "Empty".into(), text: "".into(), edited_at: None }], ..Notes::default() });
        let draft = email_with(fake(r#"{"to": "Priya", "subject": "Thanks", "body": "Hi Priya,"}"#, &sent), &with_notes).unwrap();
        assert_eq!(draft, EmailDraft { to: "Priya".into(), subject: "Thanks".into(), body: "Hi Priya,".into(), edited_at: None });
        let prompt = sent.borrow()[0].clone();
        assert!(prompt.contains("Notes from the session \"System design round\":\n## Problem\n\nRate limiting.") && !prompt.contains("## Empty"), "{prompt}");
        assert!(!prompt.contains("SECRET TRANSCRIPT"), "the transcript isn't sent again");
        // No notes yet: the transcript is the source.
        let sent = RefCell::new(Vec::new());
        assert!(email_with(fake(r#"{"to": "", "subject": "", "body": ""}"#, &sent), &session(vec![line(0, Speaker::Them, "Hello?")])).is_err(), "an empty body is no draft");
        assert!(sent.borrow()[0].contains("Session:\n[00:00] Them: Hello?"));
    }

    #[test]
    fn regenerating_keeps_edits_only_when_asked() {
        let mut saved = session(vec![]);
        saved.notes = Some(Notes { sections: vec![
            NoteBlock { name: "Problem".into(), text: "Mine".into(), edited_at: Some(10) },
            NoteBlock { name: "My approach".into(), text: "Old model text".into(), edited_at: None }], ..Notes::default() });
        saved.email = Some(EmailDraft { body: "My email".into(), edited_at: Some(11), ..EmailDraft::default() });
        assert!(has_edits(&saved));
        let fresh = || Generated { title: "New".into(), email: Some(EmailDraft { body: "Model email".into(), ..EmailDraft::default() }),
            notes: Notes { sections: vec![NoteBlock { name: "problem".into(), text: "Model".into(), edited_at: None },
                NoteBlock { name: "My approach".into(), text: "New model text".into(), edited_at: None }], ..Notes::default() } };
        let mut kept = saved.clone();
        apply(&mut kept, fresh(), true);
        let texts: Vec<&str> = kept.notes.as_ref().unwrap().sections.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["Mine", "New model text"]);
        assert_eq!((kept.title.as_deref(), kept.email.as_ref().unwrap().body.as_str()), (Some("New"), "My email"));
        let mut replaced = saved.clone();
        apply(&mut replaced, fresh(), false);
        assert_eq!(replaced.notes.as_ref().unwrap().sections[0].text, "Model");
        assert_eq!(replaced.email.as_ref().unwrap().body, "Model email");
        assert!(!has_edits(&replaced));
    }

    #[test]
    fn clocks_accept_minutes_seconds_and_hours() {
        assert_eq!(parse_clock("1:02:03"), Some(3_723_000));
        assert_eq!(parse_clock(" 12:05 "), Some(725_000));
        assert_eq!(parse_clock("x:1"), None);
    }

    #[test]
    fn long_transcripts_keep_the_most_recent_part() {
        let session = session(vec![line(0, Speaker::Them, &"a".repeat(70_000)), line(1000, Speaker::Them, "final question?")]);
        let text = transcript(&session);
        assert!(text.len() <= MAX_TRANSCRIPT_CHARS + 40);
        assert!(text.starts_with("[earlier part omitted]") && text.ends_with("final question?"));
        assert_eq!(spoken_transcript(&super::tests::session(vec![line(61_000, Speaker::You, "Hi")])), "[01:01] You: Hi");
    }
}
