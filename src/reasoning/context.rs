//! The conversation as the model sees it: committed lines from both sides in spoken order,
//! bounded to what is recent, plus what each side is saying right now.

use crate::archive::clock;
use crate::audio::Source;

/// Lines older than this (relative to the newest line) are left out.
pub const MAX_AGE_MS: u64 = 10 * 60_000;
/// At most this many committed lines are sent.
pub const MAX_LINES: usize = 40;

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub source: Source,
    /// Milliseconds into the Live session.
    pub at_ms: u64,
    pub text: String,
}

/// An utterance still in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Now {
    pub source: Source,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conversation {
    /// Committed lines in the order they were spoken.
    pub lines: Vec<Line>,
    pub now: Vec<Now>,
}

impl Conversation {
    pub fn is_empty(&self) -> bool { self.lines.is_empty() && self.now.iter().all(|now| now.text.trim().is_empty()) }

    /// The recent part of the conversation as prompt text. `screenshot` says whether one is
    /// attached to the same request, so an empty conversation only points the model at one then.
    pub fn render(&self, screenshot: bool) -> String {
        let mut out = String::new();
        let newest = self.lines.last().map(|line| line.at_ms).unwrap_or(0);
        let recent: Vec<&Line> = self.lines.iter().filter(|line| newest.saturating_sub(line.at_ms) <= MAX_AGE_MS).collect();
        let recent = &recent[recent.len().saturating_sub(MAX_LINES)..];
        if !recent.is_empty() {
            out.push_str("Conversation so far (Them is the other side, Me is me; times are minutes:seconds into the session):\n");
            for line in recent {
                out.push_str(&format!("[{}] {}: {}\n", clock(line.at_ms), line.source.label(), line.text.trim()));
            }
        }
        let now: Vec<String> = self.now.iter().filter(|now| !now.text.trim().is_empty())
            .map(|now| format!("{}: \"{}\" (unfinished)", now.source.label(), now.text.trim())).collect();
        if !now.is_empty() {
            if !out.is_empty() { out.push('\n'); }
            out.push_str("Being said right now: ");
            out.push_str(&now.join("; "));
            out.push('\n');
        }
        if out.is_empty() {
            out.push_str("Nothing has been heard in the conversation yet");
            out.push_str(if screenshot { "; rely on the screenshot." } else { "." });
        }
        out.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(source: Source, at_ms: u64, text: &str) -> Line { Line { source, at_ms, text: text.into() } }

    #[test]
    fn renders_lines_in_order_with_speakers_times_and_what_is_being_said() {
        let conversation = Conversation {
            lines: vec![line(Source::Them, 12_000, "so how would you design a distributed cache"), line(Source::Me, 20_500, "i'd start with a write-through cache ")],
            now: vec![Now { source: Source::Them, text: "and what about".into() }, Now { source: Source::Me, text: "  ".into() }],
        };
        assert_eq!(conversation.render(true), "Conversation so far (Them is the other side, Me is me; times are minutes:seconds into the session):\n\
            [00:12] Them: so how would you design a distributed cache\n\
            [00:20] Me: i'd start with a write-through cache\n\n\
            Being said right now: Them: \"and what about\" (unfinished)");
    }

    #[test]
    fn empty_conversations_say_so_and_old_or_excess_lines_are_dropped() {
        assert!(Conversation::default().is_empty());
        assert_eq!(Conversation::default().render(true), "Nothing has been heard in the conversation yet; rely on the screenshot.");
        let mut lines: Vec<Line> = (0..60).map(|i| line(Source::Them, 1_000_000 + i * 1000, &format!("line {i}"))).collect();
        lines.insert(0, line(Source::Them, 1_000, "ancient"));
        let rendered = Conversation { lines, now: Vec::new() }.render(false);
        assert!(!rendered.contains("ancient"));
        assert!(!rendered.contains("line 19\n"));
        assert!(rendered.contains("line 20\n") && rendered.contains("line 59"));
        assert_eq!(rendered.lines().count(), 1 + MAX_LINES);
    }

    #[test]
    fn an_empty_conversation_points_at_the_screenshot_only_when_one_is_attached() {
        let empty = Conversation::default();
        assert_eq!(empty.render(true), "Nothing has been heard in the conversation yet; rely on the screenshot.");
        assert_eq!(empty.render(false), "Nothing has been heard in the conversation yet.");
        let heard = Conversation { lines: vec![line(Source::Them, 1_000, "hello")], now: Vec::new() };
        assert_eq!(heard.render(true), heard.render(false), "a heard conversation renders the same either way");
    }
}
