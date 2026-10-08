//! Replays recorded audio through a recognizer and [`LiveTranscript`] on the audio clock, for
//! evaluating endpointing on real recordings.
//!
//! Real time is never consulted. [`recognize`] feeds the recording in 10 ms chunks (the
//! capture's own size) to a recognizer session and notes which chunk produced each event;
//! [`endpoint`] then replays the chunks into the endpointer and delivers each event once the
//! audio clock has moved `lag` past the chunk it came from. So running faster than real time
//! adds no recognizer lag of its own, and a lag schedule simulates a recognizer that falls
//! behind (CPU starvation) deterministically. [`score`] rates the commits against where the
//! sound in the recording actually stops.

use std::collections::VecDeque;
use std::sync::mpsc::channel;

use super::endpoint::{EndpointConfig, Reason};
use super::intent::assess;
use super::live::{LiveTranscript, Update};
use super::state::Committed;
use crate::audio::{AudioChunk, SAMPLE_RATE, Source};
use crate::stt::{AsrError, EventSink, Generation, StreamingAsr, TranscriptEvent};

/// Capture delivers 10 ms chunks; the replay uses the same so energy tracking matches.
const CHUNK: usize = SAMPLE_RATE as usize / 100;
/// Quiet fed after the recording so its last utterance can endpoint, as `transcript_replay` does.
const TAIL_MS: f64 = 2000.0;

/// How far behind the audio the recognizer's output arrives, as a function of audio time.
pub type LagSchedule<'a> = &'a dyn Fn(f64) -> f64;

/// A recognizer that keeps up: its events arrive with the chunk that produced them.
pub fn no_lag(_: f64) -> f64 { 0.0 }

#[derive(Clone, Debug, PartialEq)]
pub struct TimedCommit {
    pub utterance: Committed,
    pub reason: Reason,
    /// Audio time at which endpointing committed it.
    pub at_ms: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Timeline {
    pub commits: Vec<TimedCommit>,
    /// (audio time delivered, text) for each provisional update, for reading word timing.
    pub partials: Vec<(f64, String)>,
    /// Audio length of the recording (without the quiet tail).
    pub duration_ms: f64,
}

/// What a recognizer said about one recording, with the audio time of the chunk that produced
/// each event. Recognition doesn't depend on endpointing, so one recording can be endpointed
/// under many configurations without running the recognizer again.
#[derive(Clone, Debug)]
pub struct Recognized {
    pub source: Source,
    pub provider: &'static str,
    /// The provider's `Capabilities::text_lag_ms`.
    pub text_lag_ms: f64,
    /// (audio time the producing chunk ended, event). Events from flushing at the end have
    /// `f64::INFINITY`.
    pub events: Vec<(f64, TranscriptEvent)>,
}

/// Run `pcm` (16 kHz mono) through a session of `provider` in 10 ms chunks, followed by the
/// quiet tail, and keep its events. Synchronous providers (Parakeet) need no real time; a
/// provider that answers asynchronously would have its events attributed to later chunks.
pub fn recognize(provider: &dyn StreamingAsr, source: Source, pcm: &[f32]) -> Result<Recognized, AsrError> {
    let (sink, inbox) = channel();
    let mut session = provider.start_session(EventSink::new(source, GENERATION, sink))?;
    let mut events = Vec::new();
    for chunk in chunks(source, pcm) {
        session.push(&chunk)?;
        events.extend(inbox.try_iter().map(|event| (chunk.end_ms(), event)));
    }
    session.finish()?;
    events.extend(inbox.try_iter().map(|event| (f64::INFINITY, event)));
    let capabilities = provider.capabilities();
    Ok(Recognized { source, provider: capabilities.id, text_lag_ms: capabilities.text_lag_ms, events })
}

/// Endpoint a recognized recording as the live pipeline would: every chunk updates the
/// endpointer, and each event reaches it once the audio clock is `lag` past the chunk that
/// produced it (never ahead of an earlier event). `text_lag_ms` overrides the provider's.
pub fn endpoint(recognized: &Recognized, pcm: &[f32], config: EndpointConfig, text_lag_ms: Option<f64>, lag: LagSchedule) -> Timeline {
    let mut live = LiveTranscript::new(config, None);
    live.set_generation(GENERATION, recognized.provider);
    live.set_text_lag(text_lag_ms.unwrap_or(recognized.text_lag_ms));
    let duration_ms = pcm.len() as f64 * 1000.0 / SAMPLE_RATE as f64;
    let mut run = Run { live, pending: VecDeque::new(), timeline: Timeline { duration_ms, ..Default::default() } };
    let mut produced = recognized.events.iter().peekable();
    // (when the recognizer is done with a chunk, the chunk's end): its position on the audio clock.
    let mut processing: VecDeque<(f64, f64)> = VecDeque::new();
    // Like `Transcriber::processed_until`, nothing is processed until the first chunk is.
    run.live.set_recognized_until(recognized.source, 0.0);
    let mut released_ms: f64 = 0.0;
    let mut now = 0.0;
    for chunk in chunks(recognized.source, pcm) {
        now = chunk.end_ms();
        let ready = (now + lag(now).max(0.0)).max(released_ms);
        released_ms = ready;
        let updates = run.live.on_audio(&chunk);
        run.record(updates, now);
        while let Some((_, event)) = produced.next_if(|(at, _)| *at <= now) { run.pending.push_back((ready, event.clone())); }
        run.deliver(now);
        // As the pipeline does: the position is reported once the events for it are applied.
        processing.push_back((ready, now));
        while let Some((_, until)) = processing.pop_front_if(|(done, _)| *done <= now) {
            run.live.set_recognized_until(recognized.source, until);
        }
    }
    // Listening stopped: the recognizer's last events, then whatever is still in progress.
    run.pending.extend(produced.map(|(_, event)| (now, event.clone())));
    run.deliver(f64::INFINITY);
    let updates = run.live.finish();
    run.record(updates, now);
    run.timeline
}

/// Recognize and endpoint in one go.
pub fn replay(provider: &dyn StreamingAsr, source: Source, pcm: &[f32], config: EndpointConfig, text_lag_ms: Option<f64>,
    lag: LagSchedule) -> Result<Timeline, AsrError> {
    Ok(endpoint(&recognize(provider, source, pcm)?, pcm, config, text_lag_ms, lag))
}

const GENERATION: Generation = Generation(1);

/// The recording in capture-sized chunks, then the quiet tail.
fn chunks(source: Source, pcm: &[f32]) -> impl Iterator<Item = AudioChunk> + '_ {
    let tail = (TAIL_MS * SAMPLE_RATE as f64 / 1000.0) as usize;
    let total = pcm.len() + tail;
    (0..total.div_ceil(CHUNK)).map(move |i| {
        let range = i * CHUNK..((i + 1) * CHUNK).min(total);
        let samples = range.clone().map(|n| pcm.get(n).copied().unwrap_or(0.0)).collect();
        AudioChunk { source, start_ms: range.start as f64 * 1000.0 / SAMPLE_RATE as f64, samples }
    })
}

struct Run {
    live: LiveTranscript,
    pending: VecDeque<(f64, TranscriptEvent)>,
    timeline: Timeline,
}

impl Run {
    fn deliver(&mut self, now: f64) {
        while let Some((ready, event)) = self.pending.pop_front_if(|(ready, _)| *ready <= now) {
            let updates = self.live.on_event(&event);
            self.record(updates, if now.is_finite() { now } else { ready });
        }
    }

    fn record(&mut self, updates: Vec<Update>, at_ms: f64) {
        for update in updates {
            match update {
                Update::Provisional { stable, unstable, .. } => self.timeline.partials.push((at_ms, format!("{stable}{unstable}"))),
                Update::Committed { utterance, reason } => self.timeline.commits.push(TimedCommit { utterance, reason, at_ms }),
                Update::QuestionLikely { .. } | Update::Error { .. } => {}
            }
        }
    }
}

/// (start, end) in audio ms of each stretch of sound: 20 ms blocks above 0.01 RMS, with gaps
/// under 300 ms bridged. The reference for where speech stops.
pub fn speech_segments(pcm: &[f32]) -> Vec<(f64, f64)> {
    const BLOCK: usize = SAMPLE_RATE as usize / 50;
    let mut segments: Vec<(f64, f64)> = Vec::new();
    for (i, block) in pcm.chunks(BLOCK).enumerate() {
        let rms = (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt();
        if rms < 0.01 { continue; }
        let (start, end) = ((i * BLOCK) as f64 / 16.0, ((i + 1) * BLOCK) as f64 / 16.0);
        match segments.last_mut() {
            Some(last) if start - last.1 < 300.0 => last.1 = end,
            _ => segments.push((start, end)),
        }
    }
    segments
}

/// How one commit fared.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Verdict {
    /// Three words or fewer.
    pub short: bool,
    /// Cut while the sentence was still going: the text ends on a word a sentence rarely ends
    /// on, or the next line from the same side carries on from it ("…here" / "or avoid…").
    pub mid_sentence: bool,
    /// A question shares its line with an earlier sentence (see [`merges_a_question`]).
    pub merged: bool,
    /// Commit time after the sound it belongs to stopped. None when there was no measurable
    /// stop (sound carried on across the commit, as with music under desktop audio).
    pub latency_ms: Option<f64>,
}

impl Verdict {
    pub fn fragment(&self) -> bool { self.short || self.mid_sentence }
}

/// Endpointing quality for one replay.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scores {
    pub commits: usize,
    pub short: usize,
    pub mid_sentence: usize,
    /// Commits that are short or mid-sentence (each counted once).
    pub fragments: usize,
    pub merged_questions: usize,
    pub latencies_ms: Vec<f64>,
    /// Commits per reason: end-of-utterance, silence, max silence, stopped.
    pub reasons: [usize; 4],
}

impl Scores {
    pub fn fragment_rate(&self) -> f64 { ratio(self.fragments, self.commits) }
    pub fn latency_percentile(&self, p: f64) -> Option<f64> {
        if self.latencies_ms.is_empty() { return None; }
        let mut sorted = self.latencies_ms.clone();
        sorted.sort_by(f64::total_cmp);
        Some(sorted[((sorted.len() - 1) as f64 * p).round() as usize])
    }

    pub fn add(&mut self, other: &Scores) {
        self.commits += other.commits;
        self.short += other.short;
        self.mid_sentence += other.mid_sentence;
        self.fragments += other.fragments;
        self.merged_questions += other.merged_questions;
        self.latencies_ms.extend(&other.latencies_ms);
        for (total, n) in self.reasons.iter_mut().zip(other.reasons) { *total += n; }
    }
}

fn ratio(n: usize, of: usize) -> f64 { if of == 0 { 0.0 } else { n as f64 / of as f64 } }

/// Index into `Scores::reasons`.
fn reason_index(reason: Reason) -> usize {
    match reason { Reason::EndOfUtterance => 0, Reason::Silence => 1, Reason::MaxSilence => 2, Reason::Stopped => 3 }
}

/// Words a line rarely starts with unless it continues the previous one.
const CONTINUATIONS: &[&str] = &["or", "and", "but", "because", "than", "that", "which", "to", "of", "for", "with", "like"];
/// The next line continues this one only if it starts this soon after it ends.
const CONTINUES_WITHIN_MS: f64 = 1500.0;

pub fn judge(timeline: &Timeline, segments: &[(f64, f64)]) -> Vec<Verdict> {
    timeline.commits.iter().enumerate().map(|(i, commit)| {
        let text = &commit.utterance.text;
        let short = text.split_whitespace().count() <= 3;
        let next = timeline.commits[i + 1..].iter().find(|c| c.utterance.source == commit.utterance.source);
        let continued = next.is_some_and(|next| next.utterance.start_ms - commit.utterance.end_ms < CONTINUES_WITHIN_MS
            && next.utterance.text.split_whitespace().next().is_some_and(|w| CONTINUATIONS.contains(&w.to_lowercase().as_str())));
        let dangling = !short && assess(text).complete < 0.5;
        let mid_sentence = commit.reason != Reason::Stopped && (dangling || continued);
        // Sound that carries on across the commit leaves no stop to measure from.
        let sounding = segments.iter().any(|&(start, end)| start < commit.at_ms - 300.0 && end > commit.at_ms);
        let latency_ms = if commit.reason == Reason::Stopped || sounding { None } else {
            segments.iter().rev().find(|&&(start, end)| end <= commit.at_ms && start <= commit.utterance.end_ms + 300.0).map(|&(_, end)| commit.at_ms - end)
        };
        Verdict { short, mid_sentence, merged: merges_a_question(text), latency_ms }
    }).collect()
}

pub fn score(timeline: &Timeline, segments: &[(f64, f64)]) -> Scores {
    let mut scores = Scores { commits: timeline.commits.len(), ..Default::default() };
    for (commit, verdict) in timeline.commits.iter().zip(judge(timeline, segments)) {
        scores.reasons[reason_index(commit.reason)] += 1;
        scores.short += verdict.short as usize;
        scores.mid_sentence += verdict.mid_sentence as usize;
        scores.fragments += verdict.fragment() as usize;
        scores.merged_questions += verdict.merged as usize;
        scores.latencies_ms.extend(verdict.latency_ms);
    }
    scores
}

/// Whether a question shares its line with an earlier finished sentence, so it can't be seen
/// (or answered) on its own: "…design a distributed cache would you use redis here", or
/// background chatter followed by "so how would you design a distributed cache". A leading
/// clause ("so when the cache fills up … what should happen") is one question, not two.
pub fn merges_a_question(text: &str) -> bool {
    let words: Vec<String> = text.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_lowercase()).collect();
    let fillers = |from: usize| words[from..].iter().take_while(|w| FILLERS.contains(&w.as_str())).count();
    let lead = fillers(0);
    if words.get(lead).is_some_and(|w| SUBORDINATORS.contains(&w.as_str())) { return false; }
    (lead + 3..words.len().saturating_sub(3)).any(|split| {
        let opener = split + fillers(split);
        opener + 2 < words.len()
            && opens_question(&words[opener], &words[opener + 1])
            && assess(&words[..split].join(" ")).complete >= 0.7
            && assess(&words[opener..].join(" ")).question >= 0.5
    })
}

const FILLERS: &[&str] = &["so", "ok", "okay", "and", "um", "uh", "well", "now", "alright", "right", "yeah"];
const SUBORDINATORS: &[&str] = &["when", "if", "because", "while", "since", "after", "before", "once", "as", "whenever", "until"];

/// "would you", "what is", "how do", "tell me": the start of a new question rather than a clause.
fn opens_question(word: &str, next: &str) -> bool {
    const AUX: &[&str] = &["can", "could", "would", "will", "should", "do", "does", "did", "is", "are", "was", "were", "have", "has"];
    const SUBJECTS: &[&str] = &["you", "we", "i", "they", "it", "he", "she", "that", "this", "there", "your"];
    const WH: &[&str] = &["what", "why", "how", "when", "where", "who", "which"];
    (AUX.contains(&word) && SUBJECTS.contains(&next))
        || (WH.contains(&word) && (AUX.contains(&next) || next == "about"))
        || (matches!(word, "walk" | "tell" | "talk") && matches!(next, "me" | "us"))
        || matches!(word, "explain" | "describe")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::scripted::{ScriptedAsr, Step};

    /// Speech-like audio: loud with the short dips between syllables real speech has.
    fn speech(pcm: &mut Vec<f32>, ms: usize) {
        for i in 0..ms / 10 { pcm.extend(std::iter::repeat_n(if i % 10 >= 8 { 0.002 } else { 0.2 }, CHUNK)); }
    }
    fn quiet(pcm: &mut Vec<f32>, ms: usize) { pcm.extend(std::iter::repeat_n(0.001, ms * 16)); }

    /// One 18-word sentence spoken over 9 s, a word every half second, then an EOU.
    fn long_sentence() -> (Vec<f32>, ScriptedAsr, String) {
        let words = "so when the cache fills up and two writers race on the same key what should happen next".split(' ').collect::<Vec<_>>();
        let mut steps: Vec<Step> = (1..=words.len()).map(|n| Step::partial(Source::Them, n as f64 * 500.0, &words[..n].join(" "))).collect();
        steps.push(Step::eou(Source::Them, 9200.0, &words.join(" ")));
        let mut pcm = Vec::new();
        speech(&mut pcm, 9000);
        quiet(&mut pcm, 3000);
        (pcm, ScriptedAsr::new(steps), words.join(" "))
    }

    #[test]
    fn commits_are_timed_on_the_audio_clock() {
        let (pcm, asr, text) = long_sentence();
        let timeline = replay(&asr, Source::Them, &pcm, EndpointConfig::default(), None, &no_lag).unwrap();
        assert_eq!(timeline.commits.len(), 1, "{:?}", timeline.commits);
        let commit = &timeline.commits[0];
        assert_eq!((commit.utterance.text.as_str(), commit.reason), (text.as_str(), Reason::EndOfUtterance));
        // EOU at 9.2 s confirmed by 200 ms of quiet (less the question discount).
        assert!((9200.0..9500.0).contains(&commit.at_ms), "{}", commit.at_ms);
        let segments = speech_segments(&pcm);
        // The last 20 ms of the speech is one of its quiet dips.
        assert_eq!(segments, [(0.0, 8980.0)]);
        let scores = score(&timeline, &segments);
        assert_eq!((scores.commits, scores.fragments, scores.merged_questions), (1, 0, 0));
        assert!((200.0..550.0).contains(&scores.latencies_ms[0]), "{:?}", scores.latencies_ms);
    }

    #[test]
    fn a_lagging_recognizer_delivers_its_events_later_on_the_audio_clock() {
        let (pcm, asr, _) = long_sentence();
        let timeline = replay(&asr, Source::Them, &pcm, EndpointConfig::default(), None, &|_| 1000.0).unwrap();
        let first = timeline.partials.first().unwrap();
        assert_eq!(first, &(1500.0, "so".to_string()));
    }

    /// The "a new" / "age" fragments (#9): with the CPU saturated the recognizer fell far behind
    /// the audio. When the live speaker paused, the energy silence was applied to text from
    /// seconds earlier, and the backlog was committed a few words at a time.
    #[test]
    fn a_recognizer_far_behind_the_audio_does_not_fragment_its_backlog() {
        let (mut pcm, asr, text) = long_sentence();
        // Quiet long enough for the recognizer, 6 s behind, to get past the end of the sentence.
        quiet(&mut pcm, 6000);
        let six_seconds_behind = |_| 6000.0;
        // Without the hold (as before), the sentence is cut into pieces once the live audio goes quiet.
        let unheld = EndpointConfig { backlog_hold_ms: f64::INFINITY, ..EndpointConfig::default() };
        let timeline = replay(&asr, Source::Them, &pcm, unheld, None, &six_seconds_behind).unwrap();
        assert!(timeline.commits.len() >= 3, "{:?}", timeline.commits.iter().map(|c| &c.utterance.text).collect::<Vec<_>>());
        assert!(timeline.commits.iter().all(|c| c.utterance.text != text));

        let timeline = replay(&asr, Source::Them, &pcm, EndpointConfig::default(), None, &six_seconds_behind).unwrap();
        let commits: Vec<(&str, Reason)> = timeline.commits.iter().map(|c| (c.utterance.text.as_str(), c.reason)).collect();
        assert_eq!(commits, [(text.as_str(), Reason::EndOfUtterance)]);
    }

    /// Rapid consecutive questions merged into one line after a stall: catching up, the
    /// recognizer delivers the first question's end-of-utterance and the next question's words
    /// within a few milliseconds, so the end-of-utterance was never confirmed by silence.
    #[test]
    fn questions_a_recognizer_catches_up_on_stay_separate_lines() {
        let first = "so how would you design a distributed cache".split(' ').collect::<Vec<_>>();
        let second = "what is the difference between a mutex and a semaphore".split(' ').collect::<Vec<_>>();
        let mut steps: Vec<Step> = (1..=first.len()).map(|n| Step::partial(Source::Them, n as f64 * 300.0, &first[..n].join(" "))).collect();
        steps.push(Step::eou(Source::Them, 2600.0, &first.join(" ")));
        steps.extend((1..=second.len()).map(|n| Step::partial(Source::Them, 4000.0 + n as f64 * 300.0, &second[..n].join(" "))));
        steps.push(Step::eou(Source::Them, 7300.0, &second.join(" ")));
        let asr = ScriptedAsr::new(steps);
        let mut pcm = Vec::new();
        speech(&mut pcm, 2500);
        quiet(&mut pcm, 1500);
        // The second question, and the speaker carrying on while the recognizer catches up.
        speech(&mut pcm, 8000);
        quiet(&mut pcm, 3000);
        // Stalled from the start, then everything up to 9 s is recognized at once.
        let stalled_then_caught_up = |at: f64| if at < 9000.0 { 9000.0 - at } else { 0.0 };
        let commits = |config| -> Vec<String> {
            replay(&asr, Source::Them, &pcm, config, None, &stalled_then_caught_up).unwrap().commits.into_iter().map(|c| c.utterance.text).collect()
        };
        let unheld = EndpointConfig { backlog_hold_ms: f64::INFINITY, ..EndpointConfig::default() };
        assert_eq!(commits(unheld).len(), 1, "as before, the two questions merge");
        assert_eq!(commits(EndpointConfig::default()), [first.join(" "), second.join(" ")]);
    }

    /// The same without end-of-utterance signals (as on desktop audio, where Parakeet often
    /// sends none): sentences a pause apart are split by the silence as the recognizer heard it.
    /// Without an end-of-utterance the recognizer keeps extending one hypothesis.
    #[test]
    fn sentences_a_recognizer_catches_up_on_are_split_by_the_silence_it_heard() {
        let first = "so the cache sits in front of the database".split(' ').collect::<Vec<_>>();
        let second = "and every read goes there before it goes to disk".split(' ').collect::<Vec<_>>();
        let mut steps: Vec<Step> = (1..=first.len()).map(|n| Step::partial(Source::Them, n as f64 * 300.0, &first[..n].join(" "))).collect();
        steps.extend((1..=second.len()).map(|n| Step::partial(Source::Them, 3900.0 + n as f64 * 300.0, &format!("{} {}", first.join(" "), second[..n].join(" ")))));
        let asr = ScriptedAsr::new(steps);
        let mut pcm = Vec::new();
        speech(&mut pcm, 2400);
        quiet(&mut pcm, 1500);
        speech(&mut pcm, 8600);
        quiet(&mut pcm, 3000);
        let stalled_then_caught_up = |at: f64| if at < 9000.0 { 9000.0 - at } else { 0.0 };
        let commits = |config| -> Vec<String> {
            replay(&asr, Source::Them, &pcm, config, None, &stalled_then_caught_up).unwrap().commits.into_iter().map(|c| c.utterance.text).collect()
        };
        let unheld = EndpointConfig { backlog_hold_ms: f64::INFINITY, ..EndpointConfig::default() };
        assert_eq!(commits(unheld).len(), 1, "as before, the two sentences merge");
        assert_eq!(commits(EndpointConfig::default()), [first.join(" "), second.join(" ")]);
        // Keeping up, the same audio and words give the same two lines.
        let timeline = replay(&asr, Source::Them, &pcm, EndpointConfig::default(), None, &no_lag).unwrap();
        assert_eq!(timeline.commits.iter().map(|c| c.utterance.text.clone()).collect::<Vec<_>>(), [first.join(" "), second.join(" ")]);
    }

    #[test]
    fn a_question_sharing_a_line_with_an_earlier_sentence_is_detected() {
        assert!(merges_a_question("so how would you design a distributed cache would you use redis here"));
        assert!(merges_a_question("what is a mutex. how is it different from a semaphore?"));
        assert!(merges_a_question("the turbo is way too big and it lags a lot so how would you design a distributed cache"));
        for one in ["so how would you design a distributed cache", "what would you do if you could change it", "i think we would use redis here",
            "ok and what about cache invalidation when the database changes", "walk me through what happens when two writers update the same key",
            "so when the cache fills up and two writers race on the same key what should happen next", "i don't know what you mean by that",
            "would you use redis here or avoid caching entirely", "can you tell me about a time you had to debug a production outage"] {
            assert!(!merges_a_question(one), "{one}");
        }
    }
}
