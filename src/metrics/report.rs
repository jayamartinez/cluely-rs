//! Turn recorded marks into stage-to-stage latencies per utterance, with simple percentiles.

use std::collections::BTreeMap;

use super::recorder::{Mark, Stage};

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

/// For every utterance that reached both stages, the time from the first `from` mark to the
/// first `to` mark after it. With `provider`, only utterances that provider took part in count
/// (all of their marks are used, since shared stages like commits carry no provider).
pub fn durations(marks: &[Mark], from: Stage, to: Stage, provider: Option<&str>) -> Vec<f64> {
    let mut by_utterance: BTreeMap<u64, Vec<&Mark>> = BTreeMap::new();
    for mark in marks {
        if let Some(id) = mark.utterance { by_utterance.entry(id).or_default().push(mark); }
    }
    by_utterance.values()
        .filter(|marks| provider.is_none_or(|p| marks.iter().any(|m| m.provider.as_deref() == Some(p))))
        .filter_map(|marks| {
        let start = marks.iter().filter(|m| m.stage == from).map(|m| m.at_ms).reduce(f64::min)?;
        let end = marks.iter().filter(|m| m.stage == to && m.at_ms >= start).map(|m| m.at_ms).reduce(f64::min)?;
        Some(end - start)
    }).collect()
}

/// Nearest-rank percentiles; `None` when there's nothing to summarize.
pub fn summarize(mut values: Vec<f64>) -> Option<Summary> {
    if values.is_empty() { return None; }
    values.sort_by(f64::total_cmp);
    let rank = |p: f64| values[((p * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1];
    Some(Summary { count: values.len(), p50_ms: rank(0.5), p95_ms: rank(0.95), max_ms: *values.last().unwrap() })
}

/// The comparisons that matter when benchmarking speech providers end to end.
pub const KEY_SPANS: &[(&str, Stage, Stage)] = &[
    ("ASR end-of-utterance → commit", Stage::AsrEndOfUtterance, Stage::UtteranceCommitted),
    ("intent detected → commit (head start)", Stage::IntentDetected, Stage::UtteranceCommitted),
    ("LLM request → first token", Stage::LlmRequestStarted, Stage::LlmFirstToken),
    ("commit → first token", Stage::UtteranceCommitted, Stage::LlmFirstToken),
    ("commit → response committed", Stage::UtteranceCommitted, Stage::ResponseCommitted),
    ("intent → speculation started", Stage::IntentDetected, Stage::SpeculationStarted),
    ("speculation started → first token", Stage::SpeculationStarted, Stage::SpeculationFirstToken),
];

/// Speculative answers in a session, and how long asked-for answers took to appear.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Speculation {
    pub started: usize,
    /// Asked for while their context still matched, so shown at once.
    pub hits: usize,
    /// Running when an answer was asked for, but for a different context.
    pub misses: usize,
    /// Stopped without being shown (misses, newer questions, Live ending).
    pub cancelled: usize,
    /// Asked → first visible words, for answers a speculation was shown for.
    pub with_ms: Vec<f64>,
    /// The same for answers that started when asked.
    pub without_ms: Vec<f64>,
}

impl Speculation {
    pub fn hit_rate(&self) -> Option<f64> { (self.hits + self.misses > 0).then(|| self.hits as f64 / (self.hits + self.misses) as f64) }
}

/// Pairs each `AnswerRequested` with the next `AnswerShown`, split by whether a
/// `SpeculationHit` came in between. A request with nothing shown before the next one is skipped.
pub fn speculation(marks: &[Mark]) -> Speculation {
    let mut sorted: Vec<&Mark> = marks.iter().collect();
    sorted.sort_by(|a, b| a.at_ms.total_cmp(&b.at_ms));
    let mut out = Speculation::default();
    let mut asked: Option<(f64, bool)> = None;
    for mark in sorted {
        match mark.stage {
            Stage::SpeculationStarted => out.started += 1,
            Stage::SpeculationMissed => out.misses += 1,
            // Cancelled asked-for answers are marked too, without a provider; only speculation counts here.
            Stage::SpeculationCancelled if mark.provider.is_some() => out.cancelled += 1,
            Stage::AnswerRequested => asked = Some((mark.at_ms, false)),
            Stage::SpeculationHit => {
                out.hits += 1;
                if let Some((_, hit)) = &mut asked { *hit = true; }
            }
            Stage::AnswerShown => if let Some((at, hit)) = asked.take() {
                if hit { out.with_ms.push(mark.at_ms - at) } else { out.without_ms.push(mark.at_ms - at) }
            },
            _ => {}
        }
    }
    out
}

/// What speculation would have done for a session recorded without it: every answer
/// requested for a committed question from the other side, with nothing committed by either
/// side in between, counts as an estimated hit, its first words arriving the same request
/// time after the commit instead of after the request. Returns (estimated hits, requests
/// considered, actual and estimated request → first words per estimated hit).
pub fn what_if(marks: &[Mark]) -> (usize, usize, Vec<(f64, f64)>) {
    let commits: Vec<&Mark> = marks.iter().filter(|m| m.stage == Stage::UtteranceCommitted).collect();
    let questions: Vec<u64> = marks.iter().filter(|m| m.stage == Stage::IntentDetected).filter_map(|m| m.utterance).collect();
    let mut considered = 0;
    let mut hits = Vec::new();
    for request in marks.iter().filter(|m| m.stage == Stage::LlmRequestStarted) {
        let Some(first) = marks.iter().filter(|m| m.stage == Stage::LlmFirstToken && m.at_ms >= request.at_ms && m.utterance == request.utterance)
            .map(|m| m.at_ms).reduce(f64::min) else { continue };
        considered += 1;
        let Some(id) = request.utterance.filter(|id| questions.contains(id)) else { continue };
        let Some(commit) = commits.iter().find(|m| m.utterance == Some(id) && m.at_ms <= request.at_ms) else { continue };
        if commits.iter().any(|m| m.at_ms > commit.at_ms && m.at_ms < request.at_ms) { continue; }
        let actual = first - request.at_ms;
        hits.push((actual, (commit.at_ms + actual - request.at_ms).max(0.0)));
    }
    (hits.len(), considered, hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(stage: Stage, at_ms: f64, utterance: u64, provider: Option<&str>) -> Mark {
        Mark { stage, at_ms, source: None, utterance: Some(utterance), provider: provider.map(Into::into), audio_ms: None }
    }

    #[test]
    fn durations_pair_the_first_marks_per_utterance_and_ignore_incomplete_ones() {
        let marks = vec![
            mark(Stage::UtteranceCommitted, 1000.0, 1, None),
            mark(Stage::LlmFirstToken, 1400.0, 1, Some("claude")),
            mark(Stage::LlmFirstToken, 1900.0, 1, Some("claude")),
            mark(Stage::UtteranceCommitted, 5000.0, 2, None),
            mark(Stage::LlmFirstToken, 5250.0, 2, Some("codex")),
            mark(Stage::UtteranceCommitted, 9000.0, 3, None),
        ];
        assert_eq!(durations(&marks, Stage::UtteranceCommitted, Stage::LlmFirstToken, None), [400.0, 250.0]);
        assert_eq!(durations(&marks, Stage::UtteranceCommitted, Stage::LlmFirstToken, Some("codex")), [250.0]);
    }

    #[test]
    fn provider_filters_select_whole_utterances_including_their_shared_marks() {
        let marks = vec![
            mark(Stage::AsrEndOfUtterance, 2900.0, 1, Some("parakeet-realtime")),
            mark(Stage::UtteranceCommitted, 3150.0, 1, None),
            mark(Stage::LlmFirstToken, 3620.0, 1, None),
            mark(Stage::AsrEndOfUtterance, 9100.0, 2, Some("deepgram")),
            mark(Stage::UtteranceCommitted, 9240.0, 2, None),
        ];
        assert_eq!(durations(&marks, Stage::UtteranceCommitted, Stage::LlmFirstToken, Some("parakeet-realtime")), [470.0]);
        assert!(durations(&marks, Stage::UtteranceCommitted, Stage::LlmFirstToken, Some("deepgram")).is_empty());
        assert_eq!(durations(&marks, Stage::AsrEndOfUtterance, Stage::UtteranceCommitted, Some("deepgram")), [140.0]);
    }

    #[test]
    fn a_to_mark_before_the_from_mark_does_not_count() {
        let marks = vec![mark(Stage::LlmFirstToken, 100.0, 1, None), mark(Stage::UtteranceCommitted, 200.0, 1, None)];
        assert!(durations(&marks, Stage::UtteranceCommitted, Stage::LlmFirstToken, None).is_empty());
    }

    #[test]
    fn speculation_pairs_each_request_with_its_first_words_and_counts_hits_and_misses() {
        let at = |stage: Stage, at_ms: f64, provider: Option<&str>| mark(stage, at_ms, 1, provider);
        let marks = vec![
            at(Stage::SpeculationStarted, 100.0, Some("codex")),
            at(Stage::AnswerRequested, 900.0, None),
            at(Stage::SpeculationHit, 901.0, Some("codex")),
            at(Stage::AnswerShown, 905.0, Some("codex")),
            at(Stage::SpeculationStarted, 2000.0, Some("codex")),
            at(Stage::AnswerRequested, 3000.0, None),
            at(Stage::SpeculationMissed, 3001.0, Some("codex")),
            at(Stage::SpeculationCancelled, 3001.0, Some("codex")),
            at(Stage::AnswerShown, 5500.0, Some("codex")),
            // A cancelled asked-for answer carries no provider and isn't a speculation.
            at(Stage::SpeculationCancelled, 6000.0, None),
        ];
        let summary = speculation(&marks);
        assert_eq!((summary.started, summary.hits, summary.misses, summary.cancelled), (2, 1, 1, 1));
        assert_eq!((&summary.with_ms, &summary.without_ms), (&vec![5.0], &vec![2500.0]));
        assert_eq!(summary.hit_rate(), Some(0.5));
    }

    #[test]
    fn what_if_estimates_from_sessions_recorded_without_speculation() {
        let marks = vec![
            mark(Stage::IntentDetected, 500.0, 1, None),
            mark(Stage::UtteranceCommitted, 1000.0, 1, None),
            mark(Stage::LlmRequestStarted, 2500.0, 1, Some("codex")),
            mark(Stage::LlmFirstToken, 4500.0, 1, Some("codex")),
            // Asked about a statement: no speculation would have run.
            mark(Stage::UtteranceCommitted, 6000.0, 2, None),
            mark(Stage::LlmRequestStarted, 6500.0, 2, Some("codex")),
            mark(Stage::LlmFirstToken, 8000.0, 2, Some("codex")),
        ];
        let (hits, considered, pairs) = what_if(&marks);
        assert_eq!((hits, considered), (1, 2));
        // Speculation from the commit at 1.0 s has its first words at 3.0 s, before the 2.5 s
        // request would have them at 4.5 s: shown 0.5 s after asking instead of 2 s.
        assert_eq!(pairs, [(2000.0, 500.0)]);
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let summary = summarize((1..=20).map(|v| v as f64 * 10.0).collect()).unwrap();
        assert_eq!(summary, Summary { count: 20, p50_ms: 100.0, p95_ms: 190.0, max_ms: 200.0 });
        assert_eq!(summarize(vec![42.0]).unwrap().p95_ms, 42.0);
        assert!(summarize(vec![]).is_none());
    }
}
