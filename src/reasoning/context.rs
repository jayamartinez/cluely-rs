//! The conversation as the model sees it: committed lines from both sides in spoken order,
//! bounded to what is recent, plus what each side is saying right now. With a rolling summary
//! (`super::summary`), what came before the recent lines goes too: condensed, in its own
//! block ahead of them.

use crate::archive::clock;
use crate::audio::Source;

/// Lines older than this (relative to the newest line) are left out.
pub const MAX_AGE_MS: u64 = 10 * 60_000;
/// At most this many committed lines are sent.
pub const MAX_LINES: usize = 40;
/// The newest this many committed lines are what reference files are searched for.
const QUERY_LINES: usize = 6;
/// With a rolling summary, lines that have left the window but aren't in the summary yet (the
/// next batch isn't due, is being written, or failed) are sent as well, up to this many of the
/// newest of them, so nothing drops out of the context while it catches up. A little more than
/// a batch (`summary::BATCH_LINES`), for the lines that leave the window while one is written.
pub const MAX_UNCONDENSED: usize = 24;

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub source: Source,
    /// Milliseconds into the Live session.
    pub at_ms: u64,
    pub text: String,
}

impl Line {
    /// The line as the model reads it: `[mm:ss] Them: text`.
    pub fn render(&self) -> String { format!("[{}] {}: {}", clock(self.at_ms), self.source.label(), self.text.trim()) }
}

/// An utterance still in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Now {
    pub source: Source,
    pub text: String,
}

/// What came before the recent lines, condensed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Earlier {
    /// The rolling summary; empty until the first one is written.
    pub summary: String,
    /// The summary covers every line up to this time (milliseconds into the session).
    pub through_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conversation {
    /// Committed lines in the order they were spoken.
    pub lines: Vec<Line>,
    pub now: Vec<Now>,
    /// The rolling summary, when it is on. `None` sends only the recent lines.
    pub earlier: Option<Earlier>,
}

/// Where the recent lines start in `lines` (in spoken order): the newest [`MAX_LINES`] of those
/// at most [`MAX_AGE_MS`] older than the newest line.
pub fn window_start(lines: &[Line]) -> usize {
    let newest = lines.last().map(|line| line.at_ms).unwrap_or(0);
    let recent = lines.iter().position(|line| newest.saturating_sub(line.at_ms) <= MAX_AGE_MS).unwrap_or(lines.len());
    recent.max(lines.len().saturating_sub(MAX_LINES))
}

/// The lines that have left the recent window and that `earlier` doesn't cover yet, oldest first.
pub fn uncondensed<'a>(lines: &'a [Line], earlier: &Earlier) -> &'a [Line] {
    let aged = &lines[..window_start(lines)];
    let covered = earlier.through_ms.map_or(0, |through| aged.partition_point(|line| line.at_ms <= through));
    &aged[covered..]
}

impl Conversation {
    pub fn is_empty(&self) -> bool { self.lines.is_empty() && self.now.iter().all(|now| now.text.trim().is_empty()) }

    /// The recent part of the conversation as prompt text. `screenshot` says whether one is
    /// attached to the same request, so an empty conversation only points the model at one then.
    pub fn render(&self, screenshot: bool) -> String {
        let mut out = String::new();
        let mut start = window_start(&self.lines);
        if let Some(earlier) = &self.earlier {
            // The lines the summary doesn't cover yet lead straight into the recent ones.
            start -= uncondensed(&self.lines, earlier).len().min(MAX_UNCONDENSED);
            if !earlier.summary.trim().is_empty() {
                out.push_str("Earlier in this meeting (a condensed summary of what was said before the lines below; older and less exact than them):\n");
                out.push_str(earlier.summary.trim());
                out.push_str("\n\n");
            }
        }
        let recent = &self.lines[start..];
        if !recent.is_empty() {
            out.push_str("Conversation so far (Them is the other side, Me is me; times are minutes:seconds into the session):\n");
            for line in recent {
                out.push_str(&line.render());
                out.push('\n');
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

    /// What to search reference material for (`search`), as weighted text: the newest few
    /// committed lines, the newest counting double, and what is being said right now, which
    /// is most likely the question.
    pub fn query(&self) -> Vec<(String, f32)> {
        let newest = self.lines.last().map(|line| line.at_ms).unwrap_or(0);
        let recent: Vec<&Line> = self.lines.iter().rev().take_while(|line| newest.saturating_sub(line.at_ms) <= MAX_AGE_MS).take(QUERY_LINES).collect();
        let mut query: Vec<(String, f32)> = recent.iter().enumerate().map(|(n, line)| (line.text.trim().to_string(), if n == 0 { 2.0 } else { 1.0 })).collect();
        query.extend(self.now.iter().filter(|now| !now.text.trim().is_empty()).map(|now| (now.text.trim().to_string(), 2.0)));
        query
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
            earlier: None,
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
        let rendered = Conversation { lines, ..Conversation::default() }.render(false);
        assert!(!rendered.contains("ancient"));
        assert!(!rendered.contains("line 19\n"));
        assert!(rendered.contains("line 20\n") && rendered.contains("line 59"));
        assert_eq!(rendered.lines().count(), 1 + MAX_LINES);
    }

    #[test]
    fn the_search_query_is_the_newest_lines_and_what_is_being_said_now() {
        let mut lines: Vec<Line> = (0..10).map(|i| line(if i % 2 == 0 { Source::Them } else { Source::Me }, 1_000 * i, &format!("line {i} "))).collect();
        lines.insert(0, line(Source::Them, 0, "ancient"));
        let conversation = Conversation { lines, now: vec![Now { source: Source::Them, text: " and what about failover".into() }, Now { source: Source::Me, text: " ".into() }], earlier: None };
        assert_eq!(conversation.query(), [("line 9".to_string(), 2.0), ("line 8".into(), 1.0), ("line 7".into(), 1.0), ("line 6".into(), 1.0),
            ("line 5".into(), 1.0), ("line 4".into(), 1.0), ("and what about failover".into(), 2.0)]);
        assert!(Conversation::default().query().is_empty());
    }

    #[test]
    fn an_empty_conversation_points_at_the_screenshot_only_when_one_is_attached() {
        let empty = Conversation::default();
        assert_eq!(empty.render(true), "Nothing has been heard in the conversation yet; rely on the screenshot.");
        assert_eq!(empty.render(false), "Nothing has been heard in the conversation yet.");
        let heard = Conversation { lines: vec![line(Source::Them, 1_000, "hello")], ..Conversation::default() };
        assert_eq!(heard.render(true), heard.render(false), "a heard conversation renders the same either way");
    }

    /// A long meeting: a line every 20 s for 55 minutes.
    fn meeting() -> Vec<Line> { (0..165u64).map(|i| line(if i.is_multiple_of(2) { Source::Them } else { Source::Me }, i * 20_000, &format!("line {i}"))).collect() }

    #[test]
    fn the_window_is_the_recent_lines_and_what_left_it_is_uncondensed_until_summarized() {
        let lines = meeting();
        // 10 minutes of a line every 20 s is 31 lines (both ends included): under the line cap.
        assert_eq!(window_start(&lines), 165 - 31);
        assert_eq!(uncondensed(&lines, &Earlier::default()).len(), 134);
        let earlier = Earlier { summary: "notes".into(), through_ms: Some(lines[99].at_ms) };
        let pending = uncondensed(&lines, &earlier);
        assert_eq!((pending.len(), pending[0].text.as_str()), (34, "line 100"));
        // Covered through the newest line: nothing is pending, the recent lines still are recent.
        assert!(uncondensed(&lines, &Earlier { through_ms: lines.last().map(|l| l.at_ms), ..earlier }).is_empty());
        assert_eq!(window_start(&[]), 0);
    }

    #[test]
    fn the_summary_goes_first_marked_as_older_then_uncovered_and_recent_lines_verbatim() {
        let lines = meeting();
        let earlier = Earlier { summary: "[00:00-40:00] Them said the budget is 42k.".into(), through_ms: Some(lines[119].at_ms) };
        let rendered = Conversation { lines: lines.clone(), now: Vec::new(), earlier: Some(earlier) }.render(false);
        let (summary, verbatim) = rendered.split_once("\n\nConversation so far").unwrap();
        assert_eq!(summary, "Earlier in this meeting (a condensed summary of what was said before the lines below; older and less exact than them):\n\
            [00:00-40:00] Them said the budget is 42k.");
        // The 14 lines not summarized yet and the 31 recent ones, in order, with nothing left out.
        let shown: Vec<&str> = verbatim.lines().skip(1).collect();
        assert_eq!(shown.len(), 14 + 31);
        assert!(shown[0].ends_with("line 120") && shown.last().unwrap().ends_with("line 164"));
        // Before the first summary: no summary block, and at most MAX_UNCONDENSED lines ahead of the window.
        let pending = Conversation { lines: lines.clone(), now: Vec::new(), earlier: Some(Earlier::default()) }.render(false);
        assert!(pending.starts_with("Conversation so far"));
        assert_eq!(pending.lines().count(), 1 + MAX_UNCONDENSED + 31);
        // Off: exactly the recent window, as before.
        let off = Conversation { lines: lines.clone(), ..Conversation::default() }.render(false);
        assert_eq!(off.lines().count(), 1 + 31);
        // A session that still fits the window is sent exactly what it was before the summary existed.
        let short = Conversation { lines: lines[140..].to_vec(), ..Conversation::default() };
        assert_eq!(Conversation { earlier: Some(Earlier::default()), ..short.clone() }.render(true), short.render(true));
    }
}
