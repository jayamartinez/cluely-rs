//! Which part of a streaming hypothesis is unlikely to change.
//!
//! Streaming recognizers revise the tail of their hypothesis as more audio arrives. A word is
//! treated as stable once consecutive hypotheses agree on it; the half-word at the boundary
//! ("distrib" vs "distributed") never counts. Providers that never revise (append-only
//! output) can declare their own stable length, which is honoured.

use std::collections::VecDeque;

/// Byte length of the longest prefix of whole words that `a` and `b` share.
/// Word comparison ignores case and surrounding punctuation-free whitespace runs.
pub fn common_word_prefix(a: &str, b: &str) -> usize {
    let (mut ia, mut ib) = (words(a), words(b));
    let mut end = 0;
    loop {
        match (ia.next(), ib.next()) {
            (Some((_, wa_end, wa)), Some((_, _, wb))) if wa.eq_ignore_ascii_case(wb) => end = wa_end,
            _ => return end,
        }
    }
}

/// Words with their byte span in the original string.
fn words(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    text.split_whitespace().map(move |word| {
        let start = word.as_ptr() as usize - text.as_ptr() as usize;
        (start, start + word.len(), word)
    })
}

/// A hypothesis split into the part that has settled and the part that may still change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    pub text: String,
    /// Byte offset where the unstable tail begins (always on a word boundary).
    pub stable_len: usize,
}

impl Split {
    pub fn stable(&self) -> &str { self.text[..self.stable_len].trim_end() }
    pub fn unstable(&self) -> &str { self.text[self.stable_len..].trim_start() }
}

/// Tracks consecutive hypotheses for one utterance.
#[derive(Debug)]
pub struct StabilityTracker {
    /// How many consecutive hypotheses must agree before a word is stable (≥ 2).
    agreement: usize,
    history: VecDeque<String>,
    /// Stable length never shrinks within an utterance unless the provider rewrites it.
    stable_len: usize,
}

impl Default for StabilityTracker {
    fn default() -> Self { Self::new(2) }
}

impl StabilityTracker {
    pub fn new(agreement: usize) -> Self {
        Self { agreement: agreement.max(2), history: VecDeque::new(), stable_len: 0 }
    }

    /// Record the provider's latest full hypothesis for the utterance.
    pub fn update(&mut self, text: &str, provider_stable: Option<usize>) -> Split {
        // Words this hypothesis shares with each of the previous `agreement - 1` hypotheses.
        let agreed = if self.history.len() + 1 < self.agreement { 0 } else {
            self.history.iter().rev().take(self.agreement - 1).fold(text.len(), |len, previous| len.min(common_word_prefix(previous, text)))
        };
        // Earlier stability survives only while the text still starts with those words.
        let kept = match self.history.back() {
            Some(last) if common_word_prefix(last, text) >= self.stable_len => self.stable_len,
            _ => 0,
        };
        let hinted = provider_stable.map(|n| snap_to_word_end(text, n)).unwrap_or(0);
        self.stable_len = kept.max(agreed).max(hinted).min(text.len());
        self.history.push_back(text.to_string());
        while self.history.len() > self.agreement { self.history.pop_front(); }
        Split { text: text.to_string(), stable_len: self.stable_len }
    }

    /// Start tracking a new utterance.
    pub fn reset(&mut self) {
        self.history.clear();
        self.stable_len = 0;
    }
}

/// Largest whole-word boundary at or before `n`.
fn snap_to_word_end(text: &str, n: usize) -> usize {
    if n >= text.len() { return text.trim_end().len(); }
    words(text).map(|(_, end, _)| end).take_while(|end| *end <= n).last().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_settle_once_consecutive_hypotheses_agree() {
        let mut tracker = StabilityTracker::default();
        let first = tracker.update("how would you build a", None);
        assert_eq!(first.stable(), "");
        let second = tracker.update("how would you build a distributed", None);
        assert_eq!((second.stable(), second.unstable()), ("how would you build a", "distributed"));
        let third = tracker.update("how would you build a distributed caching", None);
        assert_eq!((third.stable(), third.unstable()), ("how would you build a distributed", "caching"));
    }

    #[test]
    fn half_words_at_the_boundary_are_never_stable() {
        assert_eq!(common_word_prefix("build a distrib", "build a distributed"), "build a".len());
        assert_eq!(common_word_prefix("Redis here", "redis there"), "Redis".len());
        assert_eq!(common_word_prefix("", "anything"), 0);
    }

    #[test]
    fn a_revised_middle_word_withdraws_stability() {
        let mut tracker = StabilityTracker::default();
        tracker.update("would you use redis", None);
        assert_eq!(tracker.update("would you use redis here", None).stable(), "would you use redis");
        let revised = tracker.update("would you use radix here", None);
        assert_eq!(revised.stable(), "would you use");
    }

    #[test]
    fn provider_stability_claims_are_honoured_on_word_boundaries() {
        let mut tracker = StabilityTracker::default();
        let split = tracker.update("so how would you", Some(usize::MAX));
        assert_eq!((split.stable(), split.unstable()), ("so how would you", ""));
        let mut tracker = StabilityTracker::default();
        assert_eq!(tracker.update("so how wou", Some(9)).stable(), "so how");
    }

    #[test]
    fn reset_starts_a_fresh_utterance() {
        let mut tracker = StabilityTracker::default();
        tracker.update("one two", None);
        tracker.update("one two three", None);
        tracker.reset();
        assert_eq!(tracker.update("four five", None).stable(), "");
    }
}
