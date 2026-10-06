//! Committed and provisional transcript, kept separately for each source.
//!
//! Provider events only ever update the *provisional* utterance. Text becomes *committed*
//! through an explicit [`TranscriptState::commit`] (the endpointer's decision), and committed
//! text changes only through an explicit [`TranscriptState::correct`].

use super::stability::{Split, StabilityTracker, common_word_prefix};
use crate::audio::Source;
use crate::stt::{EventKind, TranscriptEvent};

pub type UtteranceId = u64;

#[derive(Clone, Debug, PartialEq)]
pub struct Committed {
    pub id: UtteranceId,
    pub source: Source,
    pub text: String,
    pub start_ms: f64,
    pub end_ms: f64,
    /// Set when text was replaced after commit.
    pub corrected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Provisional {
    pub id: UtteranceId,
    pub source: Source,
    pub split: Split,
    pub start_ms: f64,
    pub end_ms: f64,
    /// Audio time of the provider's end-of-utterance signal, if it has sent one.
    pub end_of_utterance_ms: Option<f64>,
    /// The provider declared this text final (it won't revise it).
    pub provider_final: bool,
}

impl Provisional {
    pub fn text(&self) -> &str { &self.split.text }
}

/// What an event changed, for listeners (UI, endpointing, metrics).
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Provisional { source: Source, id: UtteranceId, first: bool },
    EndOfUtterance { source: Source, id: UtteranceId },
    ProviderFinal { source: Source, id: UtteranceId },
    Error { source: Source, message: String, fatal: bool },
}

enum Signal { Partial, EndOfUtterance, Final }

#[derive(Default)]
struct Track {
    committed: Vec<Committed>,
    provisional: Option<Provisional>,
    tracker: StabilityTracker,
    /// Text the provider may still be repeating at the start of its next hypothesis.
    carryover: Option<Carryover>,
    /// Earlier hypotheses of the current utterance that the provider closed (an EOU or a
    /// final) before endpointing committed, kept so its next hypothesis adds to them.
    closed: String,
}

#[derive(Default)]
pub struct TranscriptState {
    tracks: [Track; 2],
    next_id: UtteranceId,
}

fn slot(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

impl TranscriptState {
    pub fn new() -> Self { Self::default() }

    /// Fold a provider event into the provisional utterance. Never commits.
    pub fn apply(&mut self, event: &TranscriptEvent) -> Option<Change> {
        let source = event.source;
        let (text, stable_hint, kind) = match &event.kind {
            EventKind::Error { message, fatal } => return Some(Change::Error { source, message: message.clone(), fatal: *fatal }),
            EventKind::Partial { text, stable_hint } => (text.as_str(), *stable_hint, Signal::Partial),
            // EOU and final text won't be revised, so all of it counts as stable.
            EventKind::EndOfUtterance { text } => (text.as_str(), Some(usize::MAX), Signal::EndOfUtterance),
            EventKind::Final { text } => (text.as_str(), Some(usize::MAX), Signal::Final),
        };
        let track = &mut self.tracks[slot(source)];
        let text = strip_carryover(&mut track.carryover, text);
        if text.trim().is_empty() && track.provisional.is_none() {
            // An EOU with nothing new still matters if the utterance was already committed.
            return None;
        }
        let first = track.provisional.is_none();
        if first {
            self.next_id += 1;
            track.tracker.reset();
            track.closed.clear();
        }
        let id = track.provisional.as_ref().map(|p| p.id).unwrap_or(self.next_id);
        // The provider closed its hypothesis (EOU/final) and this text doesn't extend it: the
        // speaker went on before endpointing committed, so keep what was closed and add to it.
        if let Some(previous) = &track.provisional
            && (previous.end_of_utterance_ms.is_some() || previous.provider_final)
            && !text.trim().is_empty()
            && !extends(segment(previous, &track.closed), &text) {
            track.closed = previous.text().trim().to_string();
            track.tracker.reset();
        }
        let split = if text.trim().is_empty() { track.provisional.as_ref().map(|p| p.split.clone()).unwrap_or_else(|| track.tracker.update("", None)) }
            else { joined(&track.closed, track.tracker.update(&text, stable_hint)) };
        let previous = track.provisional.take();
        let mut provisional = Provisional {
            id, source, split,
            start_ms: previous.as_ref().map(|p| p.start_ms).unwrap_or(event.start_ms),
            end_ms: event.end_ms.max(previous.as_ref().map(|p| p.end_ms).unwrap_or(0.0)),
            end_of_utterance_ms: previous.as_ref().and_then(|p| p.end_of_utterance_ms),
            provider_final: previous.as_ref().is_some_and(|p| p.provider_final),
        };
        let change = match kind {
            Signal::EndOfUtterance => { provisional.end_of_utterance_ms = Some(event.end_ms); Change::EndOfUtterance { source, id } }
            Signal::Final => { provisional.provider_final = true; Change::ProviderFinal { source, id } }
            // New speech after an EOU means the speaker kept going.
            Signal::Partial => { provisional.end_of_utterance_ms = None; provisional.provider_final = false; Change::Provisional { source, id, first } }
        };
        track.provisional = Some(provisional);
        Some(change)
    }

    /// Commit the provisional utterance for `source` (endpointing decided it's over).
    pub fn commit(&mut self, source: Source) -> Option<Committed> {
        let track = &mut self.tracks[slot(source)];
        let provisional = track.provisional.take()?;
        track.tracker.reset();
        let segment = segment(&provisional, &track.closed).trim().to_string();
        track.closed.clear();
        let text = provisional.text().trim().to_string();
        if text.is_empty() { return None; }
        // The provider may still be extending the hypothesis this came from: either the whole
        // of it (everything committed from it so far) or just its latest segment.
        track.carryover = Some(Carryover {
            whole: match track.carryover.take() { Some(previous) => format!("{} {text}", previous.whole), None => text.clone() },
            segment: if segment.is_empty() { text.clone() } else { segment },
        });
        let committed = Committed { id: provisional.id, source, text, start_ms: provisional.start_ms, end_ms: provisional.end_ms, corrected: false };
        track.committed.push(committed.clone());
        Some(committed)
    }

    /// Replace committed text (explicit correction). Returns whether the utterance existed.
    pub fn correct(&mut self, id: UtteranceId, text: &str) -> bool {
        let Some(utterance) = self.tracks.iter_mut().flat_map(|t| t.committed.iter_mut()).find(|u| u.id == id) else { return false };
        if utterance.text != text { utterance.text = text.to_string(); utterance.corrected = true; }
        true
    }

    /// Drop in-progress text (e.g. the provider was switched or restarted).
    pub fn discard_provisional(&mut self, source: Source) {
        let track = &mut self.tracks[slot(source)];
        track.provisional = None;
        track.carryover = None;
        track.closed.clear();
        track.tracker.reset();
    }

    pub fn provisional(&self, source: Source) -> Option<&Provisional> { self.tracks[slot(source)].provisional.as_ref() }

    pub fn committed(&self, source: Source) -> &[Committed] { &self.tracks[slot(source)].committed }

    /// Both sources' committed utterances in the order they started.
    pub fn conversation(&self) -> Vec<&Committed> {
        let mut all: Vec<&Committed> = self.tracks.iter().flat_map(|t| t.committed.iter()).collect();
        all.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
        all
    }
}

/// Whether `text` is `previous` with more words after it (the provider extended its hypothesis).
fn extends(previous: &str, text: &str) -> bool {
    let shared = common_word_prefix(previous, text);
    previous.trim().is_empty() || (shared > 0 && shared >= previous.trim_end().len())
}

/// The provider's current hypothesis: the provisional text without the closed segments.
fn segment<'a>(provisional: &'a Provisional, closed: &str) -> &'a str {
    provisional.text().get(closed.len()..).unwrap_or("").trim_start()
}

/// Closed segments followed by the current hypothesis; closed text is all stable.
fn joined(closed: &str, split: Split) -> Split {
    if closed.is_empty() { return split; }
    if split.text.trim().is_empty() { return Split { text: closed.to_string(), stable_len: closed.len() }; }
    Split { text: format!("{closed} {}", split.text), stable_len: closed.len() + 1 + split.stable_len }
}

/// Providers that keep one hypothesis across our commit repeat the committed words; drop them.
fn strip_carryover(carryover: &mut Option<Carryover>, text: &str) -> String {
    let Some(previous) = carryover.as_ref() else { return text.to_string() };
    for known in [&previous.whole, &previous.segment] {
        let shared = common_word_prefix(known, text);
        if shared > 0 && shared >= known.trim_end().len() {
            return text[shared..].trim_start().to_string();
        }
    }
    // The provider has moved on to a new hypothesis; stop checking.
    *carryover = None;
    text.to_string()
}

/// Committed text a provider may repeat: all of the hypothesis it came from, and the last
/// segment of it (a provider that starts over after each EOU repeats only that).
#[derive(Clone, Debug)]
struct Carryover { whole: String, segment: String }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::Generation;

    fn event(source: Source, end_ms: f64, kind: EventKind) -> TranscriptEvent {
        TranscriptEvent { source, generation: Generation(1), start_ms: 0.0, end_ms, kind }
    }
    fn partial(source: Source, end_ms: f64, text: &str) -> TranscriptEvent {
        event(source, end_ms, EventKind::Partial { text: text.into(), stable_hint: None })
    }

    #[test]
    fn partials_update_provisional_text_and_end_of_utterance_does_not_commit() {
        let mut state = TranscriptState::new();
        assert_eq!(state.apply(&partial(Source::Them, 300.0, "how would you")), Some(Change::Provisional { source: Source::Them, id: 1, first: true }));
        state.apply(&partial(Source::Them, 600.0, "how would you design a cache"));
        assert!(matches!(state.apply(&event(Source::Them, 900.0, EventKind::EndOfUtterance { text: "how would you design a cache".into() })),
            Some(Change::EndOfUtterance { id: 1, .. })));
        let provisional = state.provisional(Source::Them).unwrap();
        assert_eq!((provisional.text(), provisional.end_of_utterance_ms, provisional.end_ms), ("how would you design a cache", Some(900.0), 900.0));
        assert!(state.committed(Source::Them).is_empty());
    }

    #[test]
    fn commit_is_explicit_and_starts_a_new_utterance() {
        let mut state = TranscriptState::new();
        state.apply(&partial(Source::Them, 400.0, "so earlier you mentioned postgres"));
        let committed = state.commit(Source::Them).unwrap();
        assert_eq!((committed.id, committed.text.as_str()), (1, "so earlier you mentioned postgres"));
        assert!(state.provisional(Source::Them).is_none());
        assert!(matches!(state.apply(&partial(Source::Them, 900.0, "how would you")), Some(Change::Provisional { id: 2, first: true, .. })));
        assert!(state.commit(Source::Me).is_none());
    }

    #[test]
    fn words_a_provider_repeats_after_a_commit_are_not_duplicated() {
        let mut state = TranscriptState::new();
        state.apply(&partial(Source::Them, 400.0, "would you use redis"));
        state.commit(Source::Them);
        state.apply(&partial(Source::Them, 700.0, "would you use redis here or avoid caching"));
        assert_eq!(state.provisional(Source::Them).unwrap().text(), "here or avoid caching");
        state.commit(Source::Them);
        state.apply(&partial(Source::Them, 900.0, "next question"));
        assert_eq!(state.provisional(Source::Them).unwrap().text(), "next question");
    }

    /// Seen with Parakeet: it signals an end-of-utterance at a comma, the speaker goes straight
    /// on, and the next hypothesis starts from scratch. The first clause must survive.
    #[test]
    fn a_new_hypothesis_after_an_end_of_utterance_adds_to_the_closed_text() {
        let mut state = TranscriptState::new();
        state.apply(&partial(Source::Them, 400.0, "would you use reedy's here"));
        state.apply(&event(Source::Them, 600.0, EventKind::EndOfUtterance { text: "would you use reedy's here".into() }));
        assert!(matches!(state.apply(&partial(Source::Them, 800.0, "or")), Some(Change::Provisional { first: false, .. })));
        let provisional = state.provisional(Source::Them).unwrap();
        assert_eq!((provisional.split.stable(), provisional.split.unstable()), ("would you use reedy's here", "or"));
        assert_eq!(provisional.end_of_utterance_ms, None);
        state.apply(&partial(Source::Them, 1000.0, "or avoid"));
        state.apply(&event(Source::Them, 1400.0, EventKind::EndOfUtterance { text: "or avoid cashing entirely".into() }));
        let provisional = state.provisional(Source::Them).unwrap();
        assert_eq!(provisional.text(), "would you use reedy's here or avoid cashing entirely");
        assert_eq!(provisional.split.stable(), provisional.text());
        assert_eq!(provisional.end_of_utterance_ms, Some(1400.0));
        let committed = state.commit(Source::Them).unwrap();
        assert_eq!((committed.text.as_str(), committed.start_ms, committed.end_ms), ("would you use reedy's here or avoid cashing entirely", 0.0, 1400.0));
        // A late repeat of just the last segment (or of the whole) adds nothing.
        assert_eq!(state.apply(&event(Source::Them, 1600.0, EventKind::EndOfUtterance { text: "or avoid cashing entirely".into() })), None);
        assert_eq!(state.apply(&event(Source::Them, 1700.0, EventKind::Final { text: "would you use reedy's here or avoid cashing entirely".into() })), None);
        assert!(state.provisional(Source::Them).is_none());
        // The next utterance starts clean, and a provider that extends across its EOU is unchanged.
        state.apply(&partial(Source::Them, 2000.0, "what is a mutex"));
        state.apply(&event(Source::Them, 2200.0, EventKind::EndOfUtterance { text: "what is a mutex".into() }));
        state.apply(&partial(Source::Them, 2400.0, "what is a mutex and a semaphore"));
        assert_eq!(state.provisional(Source::Them).unwrap().text(), "what is a mutex and a semaphore");
    }

    #[test]
    fn a_hypothesis_committed_in_pieces_is_not_repeated_by_its_final_event() {
        let mut state = TranscriptState::new();
        state.apply(&partial(Source::Them, 400.0, "walk me through what happens when two writers"));
        state.commit(Source::Them);
        state.apply(&partial(Source::Them, 900.0, "walk me through what happens when two writers update the same key"));
        assert_eq!(state.provisional(Source::Them).unwrap().text(), "update the same key");
        state.commit(Source::Them);
        // The provider's end-of-utterance repeats the whole sentence: nothing new to show.
        assert_eq!(state.apply(&event(Source::Them, 1500.0, EventKind::EndOfUtterance { text: "walk me through what happens when two writers update the same key".into() })), None);
        assert!(state.provisional(Source::Them).is_none());
        assert_eq!(state.committed(Source::Them).len(), 2);
        state.apply(&partial(Source::Them, 2000.0, "what is a mutex"));
        assert_eq!(state.provisional(Source::Them).unwrap().text(), "what is a mutex");
    }

    #[test]
    fn sources_are_independent_with_unique_ids_and_a_merged_conversation() {
        let mut state = TranscriptState::new();
        state.apply(&TranscriptEvent { start_ms: 1000.0, ..partial(Source::Me, 1500.0, "let me think") });
        state.apply(&partial(Source::Them, 800.0, "what would you do"));
        let them = state.commit(Source::Them).unwrap();
        let me = state.commit(Source::Me).unwrap();
        assert_ne!(them.id, me.id);
        assert_eq!(state.conversation().iter().map(|u| u.source).collect::<Vec<_>>(), [Source::Them, Source::Me]);
    }

    #[test]
    fn corrections_are_explicit_and_marked() {
        let mut state = TranscriptState::new();
        state.apply(&partial(Source::Them, 400.0, "would you use reedys here"));
        let id = state.commit(Source::Them).unwrap().id;
        assert!(state.correct(id, "would you use Redis here"));
        let fixed = &state.committed(Source::Them)[0];
        assert_eq!((fixed.text.as_str(), fixed.corrected), ("would you use Redis here", true));
        assert!(!state.correct(99, "nothing"));
    }

    #[test]
    fn provider_finals_and_errors_are_reported_without_committing() {
        let mut state = TranscriptState::new();
        state.apply(&partial(Source::Me, 200.0, "i think"));
        assert!(matches!(state.apply(&event(Source::Me, 300.0, EventKind::Final { text: "i think so".into() })), Some(Change::ProviderFinal { .. })));
        let provisional = state.provisional(Source::Me).unwrap();
        assert!(provisional.provider_final);
        assert_eq!(provisional.split.stable(), "i think so");
        assert!(matches!(state.apply(&event(Source::Me, 0.0, EventKind::Error { message: "x".into(), fatal: true })), Some(Change::Error { fatal: true, .. })));
        state.discard_provisional(Source::Me);
        assert!(state.provisional(Source::Me).is_none() && state.committed(Source::Me).is_empty());
    }
}
