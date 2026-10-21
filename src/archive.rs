//! Local meeting history. Each Live session becomes a folder:
//!
//! ```text
//! sessions/2026-10-05_14-03-22/
//!   session.json        transcript lines, questions and answers
//!   screenshots/001.jpg screenshots attached to turns
//!   attachments/002-1.txt what the model was given from a file attached to a question
//! ```
//!
//! Everything stays on this machine. The live pipeline appends as it goes and `finish`
//! stamps the end time; `session.json` is rewritten atomically after every change so a
//! crash loses at most the last line.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Speaker {
    /// Microphone: the user.
    You,
    /// Desktop audio: everyone else on the call.
    Them,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// Milliseconds since the session started.
    pub at_ms: u64,
    pub speaker: Speaker,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub at_ms: u64,
    /// Quick action label ("Assist", "What do I say?") or "Ask" for typed questions.
    pub action: String,
    pub question: String,
    pub answer: String,
    /// File name inside `screenshots/`, when a screenshot was attached and saving them is on.
    pub screenshot: Option<String>,
    /// Live stopped while the answer was still being written: `answer` is what it had so far.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
    /// Files attached to the question, in order (`attachments`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attached>,
}

/// A file attached to a question, as the session keeps it: its name and type, and what the model
/// was given from it when that was saved (the text read from a document, an image as sent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attached {
    pub name: String,
    /// "PDF", "PNG"…
    pub kind: String,
    pub bytes: u64,
    /// File name inside `attachments/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

/// What `Recorder::add_turn` keeps of an attached file.
pub enum Kept<'a> { Text(&'a str), Jpeg(&'a [u8]) }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryLength { Brief, #[default] Standard, Detailed }

impl SummaryLength {
    pub const ALL: [SummaryLength; 3] = [Self::Brief, Self::Standard, Self::Detailed];
    pub fn label(self) -> &'static str { match self { Self::Brief => "Brief", Self::Standard => "Standard", Self::Detailed => "Detailed" } }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Topic {
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
    pub detail: String,
}

/// One heading of the notes, from the session's mode (`modes::NoteSection`), and what was written
/// under it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NoteBlock {
    pub name: String,
    pub text: String,
    /// When the user last changed the text (Unix seconds); `None` while it is the model's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<u64>,
}

/// Model-written notes, generated when a session ends. Notes in a mode with note sections are
/// written under them (`sections`); otherwise, and in sessions saved before sections existed,
/// they are an overview, topics and follow-ups. Every overview length that has been generated is
/// kept, so switching back to one is instant.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Notes {
    pub overviews: BTreeMap<SummaryLength, String>,
    pub topics: Vec<Topic>,
    pub follow_ups: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<NoteBlock>,
}

impl Notes {
    /// The last time the user edited a section.
    pub fn edited_at(&self) -> Option<u64> { self.sections.iter().filter_map(|block| block.edited_at).max() }
}

/// A follow-up email drafted from the session. Never sent by CluelyRS; it is copied.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EmailDraft {
    /// Who it is for, as named in the conversation (not an address); may be empty.
    pub to: String,
    pub subject: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<u64>,
}

impl EmailDraft {
    /// What Copy email puts on the clipboard.
    pub fn clipboard_text(&self) -> String { format!("Subject: {}\n\n{}", self.subject.trim(), self.body.trim()) }
}

/// A part of the email draft the user can edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailField { To, Subject, Body }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    /// Unix seconds.
    pub started_at: u64,
    pub ended_at: Option<u64>,
    /// Model-written title such as "System design interview"; absent until generated.
    #[serde(default)]
    pub title: Option<String>,
    /// Which provider and model answered, for the header ("Claude · Sonnet").
    #[serde(default)]
    pub model: Option<String>,
    /// The mode it was recorded in (its id), for its icon in the overlay's Sessions panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub transcript: Vec<Line>,
    pub turns: Vec<Turn>,
    #[serde(default)]
    pub notes: Option<Notes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<EmailDraft>,
}

/// What the session list needs without keeping every transcript in memory.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub id: String,
    pub title: Option<String>,
    pub mode: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub lines: usize,
    pub turns: usize,
    pub screenshots: usize,
    pub preview: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Retention {
    Days7,
    #[default]
    Days30,
    Forever,
}

impl Retention {
    pub fn max_age(self) -> Option<Duration> {
        match self {
            Self::Days7 => Some(Duration::from_secs(7 * 86_400)),
            Self::Days30 => Some(Duration::from_secs(30 * 86_400)),
            Self::Forever => None,
        }
    }
}

pub fn clock(at_ms: u64) -> String { format!("{:02}:{:02}", at_ms / 60_000, at_ms / 1000 % 60) }

pub fn unix_now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

pub struct Archive {
    root: PathBuf,
}

impl Archive {
    /// `CLUELYRS_DATA_DIR` overrides the location (development and tests).
    pub fn default_location() -> Option<Self> {
        let base = std::env::var_os("CLUELYRS_DATA_DIR").map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|dir| dir.join("CluelyRS")))?;
        Some(Self::at(base.join("sessions")))
    }

    pub fn at(root: PathBuf) -> Self { Self { root } }

    pub fn root(&self) -> &Path { &self.root }

    /// Session ids are generated here and are the only path component taken from data,
    /// so they are validated before any filesystem use.
    fn dir(&self, id: &str) -> Option<PathBuf> {
        let valid = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        valid.then(|| self.root.join(id))
    }

    /// Start recording under a readable, unique id such as `2026-10-05_14-03-22`.
    pub fn start_now(&self) -> std::io::Result<Recorder> {
        let started_at = unix_now();
        let base = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let id = (0..100).map(|n| if n == 0 { base.clone() } else { format!("{base}-{n}") })
            .find(|id| !self.root.join(id).exists()).ok_or_else(|| std::io::Error::other("no free session id"))?;
        self.start(started_at, id)
    }

    pub fn start(&self, started_at: u64, id: String) -> std::io::Result<Recorder> {
        let dir = self.dir(&id).ok_or_else(|| std::io::Error::other("invalid session id"))?;
        fs::create_dir_all(&dir)?;
        let recorder = Recorder { dir, session: Session { id, started_at, ended_at: None, title: None, model: None, mode: None,
            transcript: Vec::new(), turns: Vec::new(), notes: None, email: None }, resumed: None };
        recorder.flush()?;
        Ok(recorder)
    }

    /// Reopen a finished session to record more of it: what is heard and asked from now on is
    /// appended to it, timed from where it left off. The session is marked unfinished on disk
    /// straight away, so after a crash `recover` closes it at its last recorded moment as for
    /// any other session; its notes and edits are kept.
    pub fn resume(&self, id: &str, now: u64) -> std::io::Result<Recorder> {
        let dir = self.dir(id).ok_or_else(|| std::io::Error::other("invalid session id"))?;
        let mut session = self.load(id).ok_or_else(|| std::io::Error::other("session not found"))?;
        let ended_at = session.ended_at.take().ok_or_else(|| std::io::Error::other("session is still recording"))?;
        let offset_ms = (ended_at.saturating_sub(session.started_at) * 1000).max(last_ms(&session));
        let recorder = Recorder { dir, session, resumed: Some(Resumed { offset_ms, at: now }) };
        recorder.flush()?;
        Ok(recorder)
    }

    /// Newest first. Unreadable folders are skipped rather than failing the whole list.
    pub fn list(&self) -> Vec<Summary> {
        let Ok(entries) = fs::read_dir(&self.root) else { return Vec::new() };
        let mut summaries: Vec<Summary> = entries.flatten()
            .filter_map(|entry| self.load(&entry.file_name().to_string_lossy()))
            .map(|session| Summary {
                preview: session.transcript.iter().map(|line| line.text.as_str())
                    .chain(session.turns.iter().map(|turn| turn.question.as_str()))
                    .find(|text| !text.trim().is_empty()).unwrap_or("").chars().take(120).collect(),
                screenshots: session.turns.iter().filter(|turn| turn.screenshot.is_some()).count(),
                lines: session.transcript.len(), turns: session.turns.len(),
                title: session.title, mode: session.mode, id: session.id, started_at: session.started_at, ended_at: session.ended_at,
            })
            .collect();
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.started_at));
        summaries
    }

    pub fn load(&self, id: &str) -> Option<Session> {
        let path = self.dir(id)?.join("session.json");
        let bytes = fs::read(path).ok().filter(|bytes| bytes.len() <= 32 * 1024 * 1024)?;
        serde_json::from_slice::<Session>(&bytes).ok().filter(|session| session.id == id)
    }

    pub fn screenshot_path(&self, id: &str, file: &str) -> Option<PathBuf> { self.file_path(id, "screenshots", file) }

    /// What was kept of a file attached to a question (`Attached::file`).
    pub fn attachment_path(&self, id: &str, file: &str) -> Option<PathBuf> { self.file_path(id, "attachments", file) }

    /// File names come from session.json, so they are checked before any filesystem use.
    fn file_path(&self, id: &str, folder: &str, file: &str) -> Option<PathBuf> {
        let safe = !file.is_empty() && file.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_') && !file.contains("..");
        safe.then(|| self.dir(id).map(|dir| dir.join(folder).join(file))).flatten()
    }

    /// Load, change and atomically rewrite a saved session (title and notes from the model).
    pub fn update(&self, id: &str, change: impl FnOnce(&mut Session)) -> std::io::Result<Session> {
        let mut session = self.load(id).ok_or_else(|| std::io::Error::other("session not found"))?;
        change(&mut session);
        let dir = self.dir(id).ok_or_else(|| std::io::Error::other("invalid session id"))?;
        write_atomic(&dir.join("session.json"), &serde_json::to_vec_pretty(&session).map_err(std::io::Error::other)?)?;
        Ok(session)
    }

    /// Replace the text of note section `index` with the user's.
    pub fn edit_section(&self, id: &str, index: usize, text: &str, now: u64) -> std::io::Result<Session> {
        let mut found = false;
        let session = self.update(id, |session| {
            if let Some(block) = session.notes.as_mut().and_then(|notes| notes.sections.get_mut(index)) {
                block.text = text.trim_end().to_string();
                block.edited_at = Some(now);
                found = true;
            }
        })?;
        if found { Ok(session) } else { Err(std::io::Error::other("no such note section")) }
    }

    /// Replace one part of the email draft with the user's.
    pub fn edit_email(&self, id: &str, field: EmailField, text: &str, now: u64) -> std::io::Result<Session> {
        self.update(id, |session| {
            let email = session.email.get_or_insert_with(EmailDraft::default);
            // To and Subject are one line.
            let line = || text.split_whitespace().collect::<Vec<_>>().join(" ");
            match field {
                EmailField::To => email.to = line(),
                EmailField::Subject => email.subject = line(),
                EmailField::Body => email.body = text.trim_end().to_string(),
            }
            email.edited_at = Some(now);
        })
    }

    /// Write a readable Markdown copy next to the session and return its path.
    pub fn export_markdown(&self, id: &str) -> std::io::Result<PathBuf> {
        let session = self.load(id).ok_or_else(|| std::io::Error::other("session not found"))?;
        let mut out = format!("# {}\n\n", session.title.as_deref().unwrap_or("Session"));
        if let Some(notes) = &session.notes {
            out.push_str(&notes_markdown(notes));
        }
        let mut entries: Vec<(u64, String)> = session.transcript.iter().map(|line| {
            let who = if line.speaker == Speaker::You { "You" } else { "Them" };
            (line.at_ms, format!("**{who}** ({}) {}", clock(line.at_ms), line.text))
        }).chain(session.turns.iter().map(|turn| {
            let shot = turn.screenshot.as_ref().map(|file| format!("\n\n![screenshot](screenshots/{file})")).unwrap_or_default();
            let attached = if turn.attachments.is_empty() { String::new() }
                else { format!("\n>\n> Attached: {}", turn.attachments.iter().map(|file| file.name.as_str()).collect::<Vec<_>>().join(", ")) };
            (turn.at_ms, format!("> **{}** ({}){shot}{attached}\n>\n> {}", turn.action, clock(turn.at_ms), turn.answer.replace('\n', "\n> ")))
        })).collect();
        entries.sort_by_key(|(at, _)| *at);
        out.push_str("## Timeline\n\n");
        for (_, entry) in entries { out.push_str(&entry); out.push_str("\n\n"); }
        let path = self.dir(id).expect("validated by load").join("session.md");
        write_atomic(&path, out.as_bytes())?;
        Ok(path)
    }

    pub fn delete(&self, id: &str) -> std::io::Result<()> {
        let dir = self.dir(id).ok_or_else(|| std::io::Error::other("invalid session id"))?;
        // Only remove folders that really are sessions.
        if !dir.join("session.json").is_file() { return Err(std::io::Error::other("not a session folder")); }
        fs::remove_dir_all(dir)
    }

    /// Close out sessions left unfinished by a crash or forced exit: empty ones are removed,
    /// the rest end at their last recorded moment. Call before any recording starts.
    pub fn recover(&self) -> usize {
        let Ok(entries) = fs::read_dir(&self.root) else { return 0 };
        let mut recovered = 0;
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            let Some(mut session) = self.load(&id).filter(|session| session.ended_at.is_none()) else { continue };
            let Some(dir) = self.dir(&id) else { continue };
            let result = if session.transcript.is_empty() && session.turns.is_empty() {
                fs::remove_dir_all(&dir)
            } else {
                session.ended_at = Some(session.started_at + last_ms(&session) / 1000);
                serde_json::to_vec_pretty(&session).map_err(std::io::Error::other)
                    .and_then(|bytes| write_atomic(&dir.join("session.json"), &bytes))
            };
            if result.is_ok() { recovered += 1; }
        }
        recovered
    }

    /// Remove finished sessions older than the retention window. Returns how many were removed.
    pub fn prune(&self, retention: Retention, now: u64) -> usize {
        let Some(max_age) = retention.max_age() else { return 0 };
        self.list().into_iter()
            .filter(|summary| summary.ended_at.is_some() && now.saturating_sub(summary.started_at) > max_age.as_secs())
            .filter(|summary| self.delete(&summary.id).is_ok())
            .count()
    }
}

/// The latest moment recorded in a session, in milliseconds from its start.
fn last_ms(session: &Session) -> u64 {
    session.transcript.iter().map(|line| line.at_ms).chain(session.turns.iter().map(|turn| turn.at_ms)).max().unwrap_or(0)
}

/// The notes as Markdown: the sections under their headings, or the overview and follow-ups.
/// Also what Copy notes puts on the clipboard.
pub fn notes_markdown(notes: &Notes) -> String {
    let mut out = String::new();
    if notes.sections.is_empty() {
        if let Some(overview) = notes.overviews.get(&SummaryLength::Standard).or_else(|| notes.overviews.values().last()) {
            out.push_str(&format!("{}\n\n", overview.trim()));
        }
        if !notes.follow_ups.is_empty() {
            out.push_str("## Follow-ups\n\n");
            for item in &notes.follow_ups { out.push_str(&format!("- {item}\n")); }
            out.push('\n');
        }
    }
    for block in notes.sections.iter().filter(|block| !block.text.trim().is_empty()) {
        out.push_str(&format!("## {}\n\n{}\n\n", block.name, block.text.trim()));
    }
    out
}

/// Where a resumed session's new moments are timed from.
struct Resumed {
    /// Added to every new line and turn: the session's length when it was reopened.
    offset_ms: u64,
    /// When it was reopened (Unix seconds).
    at: u64,
}

pub struct Recorder {
    dir: PathBuf,
    session: Session,
    resumed: Option<Resumed>,
}

// Transcription and the provider pipeline append through these; until they land only
// Live start/stop drive the recorder.
#[allow(dead_code)]
impl Recorder {
    pub fn session(&self) -> &Session { &self.session }

    /// `line.at_ms` counts from when this recording started; a resumed session adds its length.
    pub fn add_line(&mut self, mut line: Line) -> std::io::Result<()> {
        line.at_ms += self.offset_ms();
        self.session.transcript.push(line);
        self.flush()
    }

    /// Store a turn; `jpeg` is written to `screenshots/` only when provided. `files` go with
    /// `turn.attachments` in order: what to keep of each, as an image (`Kept::Jpeg`) or text.
    pub fn add_turn(&mut self, mut turn: Turn, jpeg: Option<&[u8]>, files: &[Option<Kept>]) -> std::io::Result<()> {
        turn.at_ms += self.offset_ms();
        let number = self.session.turns.len() + 1;
        turn.screenshot = None;
        if let Some(bytes) = jpeg {
            let shots = self.dir.join("screenshots");
            fs::create_dir_all(&shots)?;
            let name = format!("{number:03}.jpg");
            write_atomic(&shots.join(&name), bytes)?;
            turn.screenshot = Some(name);
        }
        for (index, attached) in turn.attachments.iter_mut().enumerate() {
            attached.file = None;
            let Some(Some(kept)) = files.get(index) else { continue };
            let folder = self.dir.join("attachments");
            fs::create_dir_all(&folder)?;
            // Named by position only: nothing of the attached file's own name reaches the path.
            let (extension, bytes) = match kept { Kept::Jpeg(bytes) => ("jpg", *bytes), Kept::Text(text) => ("txt", text.as_bytes()) };
            let name = format!("{number:03}-{}.{extension}", index + 1);
            write_atomic(&folder.join(&name), bytes)?;
            attached.file = Some(name);
        }
        self.session.turns.push(turn);
        self.flush()
    }

    /// Stamp the end time. A session where nothing was heard or asked is discarded
    /// instead of cluttering History; returns `None` in that case.
    pub fn id(&self) -> &str { &self.session.id }

    /// Record the mode the session runs in; it is written with the next change.
    pub fn set_mode(&mut self, mode: &str) { self.session.mode = Some(mode.to_string()); }

    /// Name the session (after its calendar event), written at once; the notes written later keep this title.
    pub fn set_title(&mut self, title: &str) -> std::io::Result<()> {
        self.session.title = Some(title.to_string());
        self.flush()
    }

    /// Whether this continues a session reopened with [`Archive::resume`].
    pub fn is_resumed(&self) -> bool { self.resumed.is_some() }

    fn offset_ms(&self) -> u64 { self.resumed.as_ref().map_or(0, |resumed| resumed.offset_ms) }

    /// A resumed session ends after its earlier length plus the time since it was reopened, so
    /// the break in between doesn't count.
    pub fn finish(mut self, ended_at: u64) -> std::io::Result<Option<Session>> {
        if self.resumed.is_none() && self.session.transcript.is_empty() && self.session.turns.is_empty() {
            fs::remove_dir_all(&self.dir)?;
            return Ok(None);
        }
        let ended_at = match &self.resumed {
            Some(resumed) => self.session.started_at + resumed.offset_ms / 1000 + ended_at.saturating_sub(resumed.at),
            None => ended_at,
        };
        self.session.ended_at = Some(ended_at);
        self.flush()?;
        Ok(Some(self.session))
    }

    fn flush(&self) -> std::io::Result<()> {
        write_atomic(&self.dir.join("session.json"), &serde_json::to_vec_pretty(&self.session).map_err(std::io::Error::other)?)
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per test. Tests run in parallel and Windows clocks tick in 100 ns steps,
    /// so a timestamp alone can collide; the counter can't.
    fn temp() -> (Archive, PathBuf) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("cluelyrs-archive-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        (Archive::at(root.clone()), root)
    }

    #[test]
    fn records_transcript_turns_and_screenshots_then_lists_newest_first() {
        let (archive, root) = temp();
        let mut older = archive.start(1_000, "older".into()).unwrap();
        older.add_line(Line { at_ms: 0, speaker: Speaker::Them, text: "Tell me about rate limiting.".into() }).unwrap();
        older.finish(1_100).unwrap();
        let mut newer = archive.start(2_000, "newer".into()).unwrap();
        newer.add_turn(Turn { at_ms: 5, action: "Assist".into(), question: "".into(), answer: "Use a token bucket.".into(), screenshot: None, stopped: false,
            attachments: Vec::new() }, Some(b"jpeg"), &[]).unwrap();
        newer.finish(2_060).unwrap();

        let list = archive.list();
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["newer", "older"]);
        assert_eq!(list[0].screenshots, 1);
        assert_eq!(list[1].preview, "Tell me about rate limiting.");
        let shot = archive.screenshot_path("newer", "001.jpg").unwrap();
        assert_eq!(fs::read(shot).unwrap(), b"jpeg");
        fs::remove_dir_all(root).unwrap();
    }

    /// A question's attachments are kept with the session: names and types always, and what the
    /// model was given from each when the caller passes it, under names the archive picks.
    #[test]
    fn attachments_are_kept_with_their_turn_and_shown_in_the_export() {
        let (archive, root) = temp();
        let mut recorder = archive.start(3_000, "attached".into()).unwrap();
        let attached = |name: &str, kind: &str| Attached { name: name.into(), kind: kind.into(), bytes: 10, file: Some("../../evil".into()) };
        let turn = Turn { at_ms: 9_000, action: "Ask".into(), question: "Compare the Team tier".into(), answer: "$18 a seat.".into(), screenshot: None,
            stopped: false, attachments: vec![attached("pricing.pdf", "PDF"), attached("Pasted image", "PNG"), attached("../notes.md", "MD")] };
        recorder.add_turn(turn, None, &[Some(Kept::Text("Team $18")), Some(Kept::Jpeg(&[0xFF, 0xD8])), None]).unwrap();
        recorder.finish(3_100).unwrap();
        let saved = archive.load("attached").unwrap();
        let files: Vec<(&str, Option<&str>)> = saved.turns[0].attachments.iter().map(|a| (a.name.as_str(), a.file.as_deref())).collect();
        assert_eq!(files, [("pricing.pdf", Some("001-1.txt")), ("Pasted image", Some("001-2.jpg")), ("../notes.md", None)]);
        assert_eq!(fs::read(archive.attachment_path("attached", "001-1.txt").unwrap()).unwrap(), b"Team $18");
        assert_eq!(fs::read(archive.attachment_path("attached", "001-2.jpg").unwrap()).unwrap(), [0xFF, 0xD8]);
        assert!(archive.attachment_path("attached", "../session.json").is_none());
        let markdown = fs::read_to_string(archive.export_markdown("attached").unwrap()).unwrap();
        assert!(markdown.contains("> Attached: pricing.pdf, Pasted image, ../notes.md"), "{markdown}");
        // A turn without attachments is saved as before, without the field.
        let json = fs::read_to_string(root.join("attached").join("session.json")).unwrap();
        assert_eq!(json.matches("\"attachments\"").count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn older_files_without_notes_load_and_export_to_markdown() {
        let (archive, root) = temp();
        fs::create_dir_all(root.join("legacy")).unwrap();
        fs::write(root.join("legacy").join("session.json"),
            r#"{"id":"legacy","startedAt":5,"endedAt":9,"transcript":[{"atMs":1000,"speaker":"them","text":"Any questions?"}],"turns":[]}"#).unwrap();
        let session = archive.load("legacy").unwrap();
        assert_eq!(session.notes, None);
        let markdown = fs::read_to_string(archive.export_markdown("legacy").unwrap()).unwrap();
        assert!(markdown.contains("**Them** (00:01) Any questions?"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sessions_from_before_note_sections_load_unchanged() {
        let (archive, root) = temp();
        fs::create_dir_all(root.join("v1")).unwrap();
        let json = r#"{"id":"v1","startedAt":5,"endedAt":65,"title":"Weekly sync","model":"Claude · Sonnet",
            "transcript":[{"atMs":1000,"speaker":"them","text":"Ship Friday?"}],"turns":[],
            "notes":{"overviews":{"standard":"You agreed to ship Friday."},"topics":[{"startMs":0,"endMs":1000,"title":"Release","detail":"Friday"}],"followUps":["Tell QA"]}}"#;
        fs::write(root.join("v1").join("session.json"), json).unwrap();
        let session = archive.load("v1").unwrap();
        let notes = session.notes.as_ref().unwrap();
        assert!(notes.sections.is_empty() && session.email.is_none());
        assert_eq!(notes.overviews[&SummaryLength::Standard], "You agreed to ship Friday.");
        assert_eq!(notes_markdown(notes), "You agreed to ship Friday.

## Follow-ups

- Tell QA

");
        // Rewritten without the new fields, so older builds read it as before.
        let rewritten = archive.update("v1", |_| {}).unwrap();
        assert_eq!(rewritten, session);
        let saved = fs::read_to_string(root.join("v1").join("session.json")).unwrap();
        assert!(!saved.contains("sections") && !saved.contains("email") && !saved.contains("editedAt"), "{saved}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn section_and_email_edits_are_saved_with_their_time() {
        let (archive, root) = temp();
        let mut recorder = archive.start(100, "edited".into()).unwrap();
        recorder.add_line(Line { at_ms: 0, speaker: Speaker::Them, text: "Hi".into() }).unwrap();
        recorder.finish(160).unwrap();
        assert!(archive.edit_section("edited", 0, "Mine", 500).is_err(), "no notes yet");
        archive.update("edited", |s| s.notes = Some(Notes { sections: vec![
            NoteBlock { name: "Problem".into(), text: "Model".into(), edited_at: None },
            NoteBlock { name: "My approach".into(), text: "".into(), edited_at: None }], ..Notes::default() })).unwrap();
        archive.edit_section("edited", 1, "- Token bucket
", 500).unwrap();
        archive.edit_email("edited", EmailField::Subject, "  Thanks for today ", 510).unwrap();
        archive.edit_email("edited", EmailField::Body, "Hi Priya,

Thanks.
", 520).unwrap();
        assert!(archive.edit_section("edited", 9, "x", 530).is_err());

        let session = archive.load("edited").unwrap();
        let notes = session.notes.as_ref().unwrap();
        assert_eq!((notes.sections[0].edited_at, notes.sections[1].text.as_str(), notes.sections[1].edited_at), (None, "- Token bucket", Some(500)));
        assert_eq!(notes.edited_at(), Some(500));
        let email = session.email.unwrap();
        assert_eq!((email.subject.as_str(), email.body.as_str(), email.edited_at), ("Thanks for today", "Hi Priya,

Thanks.", Some(520)));
        assert_eq!(notes_markdown(notes), "## Problem

Model

## My approach

- Token bucket

");
        let markdown = fs::read_to_string(archive.export_markdown("edited").unwrap()).unwrap();
        assert!(markdown.contains("## My approach

- Token bucket"), "{markdown}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_resumed_session_continues_where_it_left_off() {
        let (archive, root) = temp();
        let mut first = archive.start(1_000, "resumed".into()).unwrap();
        first.add_line(Line { at_ms: 30_000, speaker: Speaker::Them, text: "Part one".into() }).unwrap();
        first.add_turn(Turn { at_ms: 40_000, action: "Assist".into(), question: "".into(), answer: "A".into(), screenshot: None, stopped: false, attachments: Vec::new() }, Some(b"one"), &[]).unwrap();
        first.finish(1_060).unwrap();
        archive.update("resumed", |s| s.notes = Some(Notes { sections: vec![NoteBlock { name: "Summary".into(), text: "Mine".into(), edited_at: Some(9) }], ..Notes::default() })).unwrap();

        // Reopened an hour later: on disk it's unfinished straight away; new moments follow the first minute.
        let mut again = archive.resume("resumed", 4_600).unwrap();
        assert!(again.is_resumed());
        assert_eq!(archive.load("resumed").unwrap().ended_at, None);
        assert!(archive.resume("resumed", 4_600).is_err(), "it's recording already");
        again.add_line(Line { at_ms: 5_000, speaker: Speaker::You, text: "Part two".into() }).unwrap();
        again.add_turn(Turn { at_ms: 6_000, action: "Ask".into(), question: "q".into(), answer: "B".into(), screenshot: None, stopped: false, attachments: Vec::new() }, Some(b"two"), &[]).unwrap();
        let session = again.finish(4_630).unwrap().unwrap();
        assert_eq!(session.transcript.iter().map(|l| l.at_ms).collect::<Vec<_>>(), [30_000, 65_000]);
        assert_eq!(session.turns[1].at_ms, 66_000);
        assert_eq!(session.turns[1].screenshot.as_deref(), Some("002.jpg"));
        assert_eq!(session.ended_at, Some(1_000 + 60 + 30), "the hour in between doesn't count");
        assert_eq!(session.notes.as_ref().unwrap().sections[0].text, "Mine", "notes and edits are kept");
        assert_eq!(fs::read(archive.screenshot_path("resumed", "001.jpg").unwrap()).unwrap(), b"one");

        // A crash while resumed: recovery closes it at the last moment, keeping everything.
        let mut crashed = archive.resume("resumed", 9_000).unwrap();
        crashed.add_line(Line { at_ms: 10_000, speaker: Speaker::Them, text: "Part three".into() }).unwrap();
        drop(crashed);
        assert_eq!(archive.recover(), 1);
        let recovered = archive.load("resumed").unwrap();
        assert_eq!((recovered.transcript.len(), recovered.turns.len()), (3, 2));
        assert_eq!(recovered.ended_at, Some(1_000 + 90 + 10));
        assert!(recovered.notes.is_some());
        // Nothing new was said: resuming and stopping keeps the session.
        archive.resume("resumed", 9_500).unwrap().finish(9_501).unwrap();
        assert!(archive.load("resumed").is_some());
        assert!(archive.resume("missing", 0).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recover_closes_sessions_left_open_by_a_crash() {
        let (archive, root) = temp();
        drop(archive.start(100, "crashed-empty".into()).unwrap());
        let mut busy = archive.start(100, "crashed-busy".into()).unwrap();
        busy.add_line(Line { at_ms: 90_000, speaker: Speaker::Them, text: "Last thing said".into() }).unwrap();
        drop(busy);
        assert_eq!(archive.recover(), 2);
        assert!(archive.load("crashed-empty").is_none());
        assert_eq!(archive.load("crashed-busy").unwrap().ended_at, Some(190));
        assert_eq!(archive.recover(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_sessions_are_discarded_on_finish() {
        let (archive, root) = temp();
        assert_eq!(archive.start_now().unwrap().finish(unix_now()).unwrap(), None);
        assert!(archive.list().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_path_traversal_and_prunes_only_finished_old_sessions() {
        let (archive, root) = temp();
        assert!(archive.start(0, "../escape".into()).is_err());
        assert!(archive.load("..").is_none());
        assert!(archive.screenshot_path("ok", "../session.json").is_none());
        let said = || Line { at_ms: 0, speaker: Speaker::You, text: "Hello".into() };
        let mut old = archive.start(0, "old".into()).unwrap();
        old.add_line(said()).unwrap();
        old.finish(10).unwrap();
        archive.start(0, "still-recording".into()).unwrap();
        let mut recent = archive.start(40 * 86_400, "recent".into()).unwrap();
        recent.add_line(said()).unwrap();
        recent.finish(40 * 86_400 + 5).unwrap();
        assert_eq!(archive.prune(Retention::Days30, 40 * 86_400 + 10), 1);
        let left: Vec<String> = archive.list().into_iter().map(|s| s.id).collect();
        assert!(left.contains(&"recent".to_string()) && left.contains(&"still-recording".to_string()) && !left.contains(&"old".to_string()));
        assert_eq!(archive.prune(Retention::Forever, u64::MAX), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
