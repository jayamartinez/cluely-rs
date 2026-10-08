//! When to get an answer ready before it's asked for. Three levels:
//!
//! 1. **Preparation** (no model requests): as soon as the other side's utterance looks like a
//!    question, capture and encode a screenshot in the background and make sure the provider
//!    is warm. The screenshot is kept only briefly ([`PreparedShot`]).
//! 2. **Readiness** (no model requests): intent, completeness and how much of the text the
//!    recognizer has settled ([`Readiness`]) decide whether an answer is likely wanted and the
//!    question is stable enough to answer.
//! 3. **Speculative answers** (opt-in, off by default): a real request to the selected model,
//!    using the user's subscription or API key, starts when the question is
//!    committed, or earlier on a settled, finished-looking question (not on the ChatGPT
//!    subscription, see [`Budget::early_start_for`]), under a budget
//!    ([`Planner`]). Pressing Assist shows it at once if the conversation still matches
//!    ([`context_key`]); otherwise it is cancelled and a fresh answer starts.
//!
//! Everything here is pure policy driven by explicit times, so it runs the same on any
//! platform and in tests; `ReasoningSession` runs the requests and the overlay feeds events.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use super::context::Line;
use crate::audio::Source;
use crate::settings::{Provider, Settings};
use crate::transcript::intent::assess;
use crate::transcript::live::QUESTION_THRESHOLD;
use crate::transcript::state::UtteranceId;

/// How likely an answer is wanted, and how settled the question is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Readiness {
    /// 0..=1, from `transcript::intent`.
    pub question: f32,
    /// 0..=1: how finished the sentence reads.
    pub complete: f32,
    /// 0..=1: share of the text the recognizer won't revise.
    pub stable: f32,
    pub words: usize,
}

impl Readiness {
    pub fn of(stable: &str, unstable: &str) -> Self {
        let text = format!("{stable}{unstable}");
        let assessment = assess(&text);
        let total = text.trim().chars().count();
        let settled = if total == 0 { 0.0 } else { stable.trim().chars().count().min(total) as f32 / total as f32 };
        Self { question: assessment.question, complete: assessment.complete, stable: settled, words: text.split_whitespace().count() }
    }

    /// Worth preparing for: it looks like something the user will want answered.
    pub fn worth_preparing(&self) -> bool { self.question >= QUESTION_THRESHOLD }

    /// Confident enough to start a speculative answer before endpointing commits: clearly a question,
    /// reads finished, and the recognizer has settled all of it.
    pub fn ready_early(&self) -> bool { self.question >= 0.75 && self.complete >= 0.7 && self.stable >= 1.0 && self.words >= 4 }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    /// Speculative answers started per rolling minute, at most.
    pub per_minute: usize,
    /// Start before the commit on a settled, finished-looking question.
    pub early_start: bool,
    /// A speculation older than this is never shown (the screen has likely moved on).
    pub max_age: Duration,
}

impl Default for Budget {
    fn default() -> Self { Self { per_minute: 4, early_start: true, max_age: Duration::from_secs(90) } }
}

impl Budget {
    /// Whether `provider` starts early. Not on the ChatGPT subscription: an early start is the one
    /// most often overtaken by more speech and interrupted, and on Codex the turn after an
    /// interrupted one has sometimes taken 14–16 s to its first words instead of 2–3 s
    /// (server-side; measured in #16). Waiting for the commit costs little head start.
    pub fn early_start_for(provider: Provider) -> bool { provider != Provider::Codex }
}

/// What the planner wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Preparation: capture a screenshot and warm the provider for this utterance.
    Prepare { utterance: UtteranceId },
    /// Start a speculative answer for this utterance (cancelling any other).
    Speculate { utterance: UtteranceId },
    /// Stop the speculative answer in flight; the conversation moved on.
    Cancel,
    /// The line an early start answers was committed as it was: the speculation stays, but its
    /// context now includes that line.
    Rekey,
}

#[derive(Clone, Debug)]
struct InFlight {
    utterance: UtteranceId,
    /// The text it was started for, to tell whether the committed line still matches.
    text: String,
}

/// Decides preparation and speculative answers from transcript events. One speculation at a time.
#[derive(Clone, Debug)]
pub struct Planner {
    pub budget: Budget,
    /// Speculative answers are on (Settings, off by default). Preparation always runs.
    pub enabled: bool,
    prepared: Option<UtteranceId>,
    in_flight: Option<InFlight>,
    /// The utterance whose speculative answer was shown; it is never speculated again.
    shown: Option<UtteranceId>,
    started: VecDeque<Instant>,
}

impl Planner {
    pub fn new(enabled: bool, budget: Budget) -> Self {
        Self { budget, enabled, prepared: None, in_flight: None, shown: None, started: VecDeque::new() }
    }

    /// The other side's in-progress utterance changed.
    pub fn on_provisional(&mut self, source: Source, id: UtteranceId, stable: &str, unstable: &str, now: Instant) -> Vec<Step> {
        if source != Source::Them { return Vec::new(); }
        let readiness = Readiness::of(stable, unstable);
        let mut steps = Vec::new();
        // A newer question: an answer to the previous one would no longer be shown.
        if readiness.worth_preparing() && self.in_flight.as_ref().is_some_and(|flight| flight.utterance != id) {
            self.in_flight = None;
            steps.push(Step::Cancel);
        }
        if readiness.worth_preparing() && self.prepared != Some(id) {
            self.prepared = Some(id);
            steps.push(Step::Prepare { utterance: id });
        }
        let text = normalize(&format!("{stable}{unstable}"));
        match &self.in_flight {
            // Started early, and the speaker went on: that answer is for a different question.
            Some(flight) if flight.utterance == id && flight.text != text => {
                self.in_flight = None;
                steps.push(Step::Cancel);
            }
            Some(_) => {}
            None if self.budget.early_start && readiness.ready_early() => steps.extend(self.start(id, text, now)),
            None => {}
        }
        steps
    }

    /// A line was committed.
    pub fn on_committed(&mut self, source: Source, id: UtteranceId, text: &str, now: Instant) -> Vec<Step> {
        let text = normalize(text);
        if let Some(flight) = &self.in_flight {
            // The early start answered exactly this line: keep it.
            if source == Source::Them && flight.utterance == id && flight.text == text { return vec![Step::Rekey]; }
        }
        let mut steps = Vec::new();
        // Any other new line changes what an answer should address.
        if self.in_flight.take().is_some() { steps.push(Step::Cancel); }
        if source != Source::Them { return steps; }
        let assessment = assess(&text);
        if assessment.question < QUESTION_THRESHOLD { return steps; }
        if self.prepared != Some(id) {
            self.prepared = Some(id);
            steps.push(Step::Prepare { utterance: id });
        }
        steps.extend(self.start(id, text, now));
        steps
    }

    /// Whether a speculative answer for `id` is still wanted (not cancelled or replaced).
    pub fn wants(&self, id: UtteranceId) -> bool { self.in_flight.as_ref().is_some_and(|flight| flight.utterance == id) }

    /// The speculation ended without being shown (cancelled, a miss, or failed).
    pub fn finished(&mut self) { self.in_flight = None; }

    /// The speculation was shown (asked for, or shown automatically). Its question is answered:
    /// committing it, or more partials for it, start nothing new.
    pub fn shown(&mut self) { self.shown = self.in_flight.take().map(|flight| flight.utterance); }

    fn start(&mut self, id: UtteranceId, text: String, now: Instant) -> Option<Step> {
        if !self.enabled || self.shown == Some(id) { return None; }
        while self.started.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60)) { self.started.pop_front(); }
        if self.started.len() >= self.budget.per_minute { return None; }
        self.started.push_back(now);
        self.in_flight = Some(InFlight { utterance: id, text });
        Some(Step::Speculate { utterance: id })
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_lowercase())
        .filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// What an answer depends on, as one number: a speculative answer is shown only if this is
/// the same when the user asks. Committed lines from both sides (any new line changes it),
/// the utterance being answered, the action, the finished exchanges, and the settings that
/// shape the answer. What either side is still saying is left out: a half-said "um" from me
/// shouldn't throw an answer away; a new question from them is caught by the caller.
pub fn context_key(lines: &[Line], utterance: Option<UtteranceId>, action: &str, exchanges: usize, settings: &Settings) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in lines { (line.source == Source::Them, line.at_ms, line.text.trim()).hash(&mut hasher); }
    (utterance, action, exchanges).hash(&mut hasher);
    (format!("{:?}", settings.provider), &settings.codex_model, settings.claude_model.id(), &settings.api_provider, settings.api_model(),
        settings.smart_mode, format!("{:?}", settings.answer_style), settings.screen_on_send).hash(&mut hasher);
    hasher.finish()
}

/// A screenshot taken ahead of an answer: one at a time, used only while fresh.
#[derive(Clone, Debug, Default)]
pub struct PreparedShot {
    shot: Option<(Instant, Vec<u8>)>,
}

/// Older than this, a prepared screenshot is discarded rather than sent.
pub const SHOT_MAX_AGE: Duration = Duration::from_secs(4);

impl PreparedShot {
    pub fn store(&mut self, taken: Instant, jpeg: Vec<u8>) {
        if self.shot.as_ref().is_none_or(|(at, _)| *at <= taken) { self.shot = Some((taken, jpeg)); }
    }

    /// The screenshot if it is still fresh. Stale ones are dropped.
    pub fn fresh(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.shot.as_ref().is_some_and(|(at, _)| now.saturating_duration_since(*at) > SHOT_MAX_AGE) { self.shot = None; }
        self.shot.as_ref().map(|(_, jpeg)| jpeg.clone())
    }

    pub fn clear(&mut self) { self.shot = None; }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planner() -> (Planner, Instant) { (Planner::new(true, Budget::default()), Instant::now()) }

    #[test]
    fn readiness_reads_intent_completeness_and_how_settled_the_text_is() {
        let settling = Readiness::of("so how would you design a ", "distributed");
        assert!(settling.worth_preparing() && !settling.ready_early());
        let settled = Readiness::of("so how would you design a distributed cache", "");
        assert!(settled.ready_early(), "{settled:?}");
        assert!(!Readiness::of("so how would you design a", "").ready_early(), "unfinished");
        assert!(!Readiness::of("i worked on the platform team", "").worth_preparing());
    }

    #[test]
    fn a_question_from_them_is_prepared_once_and_speculated_at_commit() {
        let (mut planner, now) = (Planner::new(true, Budget { early_start: false, ..Budget::default() }), Instant::now());
        assert_eq!(planner.on_provisional(Source::Them, 4, "what is a ", "mutex", now), [Step::Prepare { utterance: 4 }]);
        assert!(planner.on_provisional(Source::Them, 4, "what is a mutex ", "and", now).is_empty());
        assert_eq!(planner.on_committed(Source::Them, 4, "what is a mutex and a semaphore", now), [Step::Speculate { utterance: 4 }]);
        // My side is ignored until it commits; a committed line of mine changes the context.
        assert!(planner.on_provisional(Source::Me, 5, "a mutex is", "", now).is_empty());
        assert_eq!(planner.on_committed(Source::Me, 5, "a mutex is a lock", now), [Step::Cancel]);
        assert!(planner.on_committed(Source::Them, 6, "okay", now).is_empty());
    }

    #[test]
    fn speculative_answers_are_off_unless_enabled_but_preparation_still_runs() {
        let (mut planner, now) = (Planner::new(false, Budget::default()), Instant::now());
        assert_eq!(planner.on_committed(Source::Them, 1, "how would you design a distributed cache", now), [Step::Prepare { utterance: 1 }]);
        assert!(planner.on_provisional(Source::Them, 2, "would you use redis here", "", now).iter().all(|s| matches!(s, Step::Prepare { .. })));
    }

    #[test]
    fn the_chatgpt_subscription_waits_for_the_commit() {
        assert!(!Budget::early_start_for(Provider::Codex));
        assert!(Budget::early_start_for(Provider::Claude) && Budget::early_start_for(Provider::ApiKey));
        let budget = Budget { early_start: Budget::early_start_for(Provider::Codex), ..Budget::default() };
        let (mut planner, now) = (Planner::new(true, budget), Instant::now());
        let steps = planner.on_provisional(Source::Them, 3, "so how would you design a distributed cache", "", now);
        assert_eq!(steps, [Step::Prepare { utterance: 3 }]);
        assert_eq!(planner.on_committed(Source::Them, 3, "so how would you design a distributed cache", now), [Step::Speculate { utterance: 3 }]);
    }

    #[test]
    fn a_settled_question_starts_early_and_is_kept_when_the_commit_matches() {
        let (mut planner, now) = planner();
        let steps = planner.on_provisional(Source::Them, 3, "So how would you design a distributed cache?", "", now);
        assert_eq!(steps, [Step::Prepare { utterance: 3 }, Step::Speculate { utterance: 3 }]);
        assert_eq!(planner.on_committed(Source::Them, 3, "so how would you design a distributed cache", now), [Step::Rekey]);
    }

    #[test]
    fn a_question_whose_answer_was_shown_is_not_answered_again_when_it_commits() {
        let (mut planner, now) = planner();
        planner.on_provisional(Source::Them, 3, "So how would you design a distributed cache?", "", now);
        planner.shown();
        assert!(planner.on_committed(Source::Them, 3, "so how would you design a distributed cache, then", now).is_empty());
        assert_eq!(planner.on_committed(Source::Them, 4, "what is a mutex", now), [Step::Prepare { utterance: 4 }, Step::Speculate { utterance: 4 }]);
    }

    #[test]
    fn an_early_start_is_cancelled_when_the_speaker_goes_on() {
        let (mut planner, now) = planner();
        planner.on_provisional(Source::Them, 3, "would you use redis here", "", now);
        assert_eq!(planner.on_provisional(Source::Them, 3, "would you use redis here or ", "avoid", now), [Step::Cancel]);
        assert_eq!(planner.on_committed(Source::Them, 3, "would you use redis here or avoid caching entirely", now), [Step::Speculate { utterance: 3 }]);
    }

    #[test]
    fn a_newer_question_replaces_the_one_in_flight_within_the_budget() {
        let (mut planner, now) = (Planner::new(true, Budget { per_minute: 2, early_start: false, ..Budget::default() }), Instant::now());
        assert_eq!(planner.on_committed(Source::Them, 1, "what is a mutex", now), [Step::Prepare { utterance: 1 }, Step::Speculate { utterance: 1 }]);
        assert_eq!(planner.on_committed(Source::Them, 2, "what is a semaphore", now), [Step::Cancel, Step::Prepare { utterance: 2 }, Step::Speculate { utterance: 2 }]);
        // The third question in the same minute is over budget: prepared, not speculated.
        assert_eq!(planner.on_committed(Source::Them, 3, "how do they differ", now), [Step::Cancel, Step::Prepare { utterance: 3 }]);
        let later = now + Duration::from_secs(61);
        assert_eq!(planner.on_committed(Source::Them, 4, "which would you use", later), [Step::Prepare { utterance: 4 }, Step::Speculate { utterance: 4 }]);
    }

    #[test]
    fn the_context_key_changes_with_new_lines_actions_exchanges_and_settings_but_not_ids_alone() {
        let lines = vec![Line { source: Source::Them, at_ms: 1000, text: "what is a mutex".into() }];
        let settings = Settings::default();
        let key = context_key(&lines, Some(1), "Assist", 0, &settings);
        assert_eq!(key, context_key(&lines, Some(1), "Assist", 0, &settings));
        let mut more = lines.clone();
        more.push(Line { source: Source::Me, at_ms: 2000, text: "a lock".into() });
        assert_ne!(key, context_key(&more, Some(1), "Assist", 0, &settings));
        assert_ne!(key, context_key(&lines, Some(1), "What do I say?", 0, &settings));
        assert_ne!(key, context_key(&lines, Some(1), "Assist", 1, &settings));
        assert_ne!(key, context_key(&lines, Some(1), "Assist", 0, &Settings { smart_mode: true, ..Settings::default() }));
    }

    /// The whole path from recognizer events: Parakeet-like partials and end-of-utterance
    /// signals (scripted) through the real listening pipeline and endpointing to the planner.
    #[test]
    fn transcript_events_from_the_pipeline_drive_preparation_and_speculation() {
        use std::sync::Arc;
        use crate::audio::AudioChunk;
        use crate::listening::{self, Message};
        use crate::stt::scripted::{ScriptedAsr, Step as Script};
        use crate::transcript::endpoint::EndpointConfig;
        use crate::transcript::live::{LiveTranscript, Update};

        let provider = Arc::new(ScriptedAsr::new(vec![
            Script::partial(Source::Them, 400.0, "what is a"),
            Script::partial(Source::Them, 800.0, "what is a mutex"),
            Script::eou(Source::Them, 900.0, "what is a mutex"),
            Script::partial(Source::Them, 2400.0, "and how is it different from a semaphore"),
            Script::eou(Source::Them, 2600.0, "and how is it different from a semaphore"),
        ]));
        let (chunks, audio) = std::sync::mpsc::channel();
        let (commands, inbox) = std::sync::mpsc::channel();
        let (out, mut messages) = futures::channel::mpsc::unbounded();
        let worker = std::thread::spawn(move || {
            listening::run(provider, audio, &[Source::Them], Vec::new(), LiveTranscript::new(EndpointConfig::default(), None), &inbox, &out, None);
        });
        // Speech, a pause long enough to commit, more speech, a pause; the waits let the
        // scripted recognizer's events land before the quiet that follows them, as in real time.
        let feed = |from: usize, to: usize, level: f32| for i in from..to {
            let level = if level > 0.0 && i % 10 >= 8 { level * 0.1 } else { level };
            chunks.send(AudioChunk { source: Source::Them, start_ms: i as f64 * 10.0, samples: vec![level; 160] }).unwrap();
        };
        feed(0, 95, 0.2);
        std::thread::sleep(Duration::from_millis(150));
        feed(95, 200, 0.0);
        feed(200, 265, 0.2);
        std::thread::sleep(Duration::from_millis(150));
        feed(265, 400, 0.0);
        std::thread::sleep(Duration::from_millis(150));
        commands.send(listening::Command::Stop).unwrap();
        worker.join().unwrap();

        let (mut planner, now) = (Planner::new(true, Budget { early_start: false, ..Budget::default() }), Instant::now());
        let mut steps = Vec::new();
        while let Ok(message) = messages.try_recv() {
            let Message::Transcript(update) = message else { continue };
            steps.extend(match &update {
                Update::Provisional { source, id, stable, unstable } => planner.on_provisional(*source, *id, stable, unstable, now),
                Update::Committed { utterance, .. } => planner.on_committed(utterance.source, utterance.id, &utterance.text, now),
                _ => Vec::new(),
            });
        }
        assert_eq!(steps, [Step::Prepare { utterance: 1 }, Step::Speculate { utterance: 1 },
            Step::Cancel, Step::Prepare { utterance: 2 }, Step::Speculate { utterance: 2 }]);
    }

    #[test]
    fn prepared_screenshots_are_used_only_while_fresh() {
        let now = Instant::now();
        let mut shot = PreparedShot::default();
        shot.store(now, vec![1]);
        shot.store(now - Duration::from_secs(1), vec![0]);
        assert_eq!(shot.fresh(now + Duration::from_secs(3)), Some(vec![1]));
        assert_eq!(shot.fresh(now + Duration::from_secs(5)), None);
        assert_eq!(shot.fresh(now), None, "a stale screenshot is dropped, not kept");
    }
}
