//! Local meeting history. Each Live session becomes a folder:
//!
//! ```text
//! sessions/2026-10-05_14-03-22/
//!   session.json        transcript lines, questions and answers
//!   screenshots/001.jpg screenshots attached to turns
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
}

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

/// Model-written notes, generated when a session ends. Every length that has been
/// generated is kept, so switching back to one is instant.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Notes {
    pub overviews: BTreeMap<SummaryLength, String>,
    pub topics: Vec<Topic>,
    pub follow_ups: Vec<String>,
}

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
    pub transcript: Vec<Line>,
    pub turns: Vec<Turn>,
    #[serde(default)]
    pub notes: Option<Notes>,
}

/// What the session list needs without keeping every transcript in memory.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub id: String,
    pub title: Option<String>,
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
        let recorder = Recorder { dir, session: Session { id, started_at, ended_at: None, title: None, model: None,
            transcript: Vec::new(), turns: Vec::new(), notes: None } };
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
                title: session.title, id: session.id, started_at: session.started_at, ended_at: session.ended_at,
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

    pub fn screenshot_path(&self, id: &str, file: &str) -> Option<PathBuf> {
        let safe = !file.is_empty() && file.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_') && !file.contains("..");
        safe.then(|| self.dir(id).map(|dir| dir.join("screenshots").join(file))).flatten()
    }

    /// Load, change and atomically rewrite a saved session (title and notes from the model).
    pub fn update(&self, id: &str, change: impl FnOnce(&mut Session)) -> std::io::Result<Session> {
        let mut session = self.load(id).ok_or_else(|| std::io::Error::other("session not found"))?;
        change(&mut session);
        let dir = self.dir(id).ok_or_else(|| std::io::Error::other("invalid session id"))?;
        write_atomic(&dir.join("session.json"), &serde_json::to_vec_pretty(&session).map_err(std::io::Error::other)?)?;
        Ok(session)
    }

    /// Write a readable Markdown copy next to the session and return its path.
    pub fn export_markdown(&self, id: &str) -> std::io::Result<PathBuf> {
        let session = self.load(id).ok_or_else(|| std::io::Error::other("session not found"))?;
        let mut out = format!("# {}\n\n", session.title.as_deref().unwrap_or("Session"));
        if let Some(overview) = session.notes.as_ref().and_then(|notes| notes.overviews.values().last()) {
            out.push_str(&format!("{overview}\n\n"));
        }
        let mut entries: Vec<(u64, String)> = session.transcript.iter().map(|line| {
            let who = if line.speaker == Speaker::You { "You" } else { "Them" };
            (line.at_ms, format!("**{who}** ({}) {}", clock(line.at_ms), line.text))
        }).chain(session.turns.iter().map(|turn| {
            let shot = turn.screenshot.as_ref().map(|file| format!("\n\n![screenshot](screenshots/{file})")).unwrap_or_default();
            (turn.at_ms, format!("> **{}** ({}){shot}\n>\n> {}", turn.action, clock(turn.at_ms), turn.answer.replace('\n', "\n> ")))
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
                let last_ms = session.transcript.iter().map(|l| l.at_ms).chain(session.turns.iter().map(|t| t.at_ms)).max().unwrap_or(0);
                session.ended_at = Some(session.started_at + last_ms / 1000);
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

pub struct Recorder {
    dir: PathBuf,
    session: Session,
}

// Transcription and the provider pipeline append through these; until they land only
// Live start/stop drive the recorder.
#[allow(dead_code)]
impl Recorder {
    pub fn session(&self) -> &Session { &self.session }

    pub fn add_line(&mut self, line: Line) -> std::io::Result<()> {
        self.session.transcript.push(line);
        self.flush()
    }

    /// Store a turn; `jpeg` is written to `screenshots/` only when provided.
    pub fn add_turn(&mut self, mut turn: Turn, jpeg: Option<&[u8]>) -> std::io::Result<()> {
        turn.screenshot = None;
        if let Some(bytes) = jpeg {
            let shots = self.dir.join("screenshots");
            fs::create_dir_all(&shots)?;
            let name = format!("{:03}.jpg", self.session.turns.len() + 1);
            write_atomic(&shots.join(&name), bytes)?;
            turn.screenshot = Some(name);
        }
        self.session.turns.push(turn);
        self.flush()
    }

    /// Stamp the end time. A session where nothing was heard or asked is discarded
    /// instead of cluttering History; returns `None` in that case.
    pub fn id(&self) -> &str { &self.session.id }

    pub fn finish(mut self, ended_at: u64) -> std::io::Result<Option<Session>> {
        if self.session.transcript.is_empty() && self.session.turns.is_empty() {
            fs::remove_dir_all(&self.dir)?;
            return Ok(None);
        }
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
        newer.add_turn(Turn { at_ms: 5, action: "Assist".into(), question: "".into(), answer: "Use a token bucket.".into(), screenshot: None, stopped: false }, Some(b"jpeg")).unwrap();
        newer.finish(2_060).unwrap();

        let list = archive.list();
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["newer", "older"]);
        assert_eq!(list[0].screenshots, 1);
        assert_eq!(list[1].preview, "Tell me about rate limiting.");
        let shot = archive.screenshot_path("newer", "001.jpg").unwrap();
        assert_eq!(fs::read(shot).unwrap(), b"jpeg");
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
