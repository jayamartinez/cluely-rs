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
];

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
    fn percentiles_use_nearest_rank() {
        let summary = summarize((1..=20).map(|v| v as f64 * 10.0).collect()).unwrap();
        assert_eq!(summary, Summary { count: 20, p50_ms: 100.0, p95_ms: 190.0, max_ms: 200.0 });
        assert_eq!(summarize(vec![42.0]).unwrap().p95_ms, 42.0);
        assert!(summarize(vec![]).is_none());
    }
}
