//! When is an utterance over? Combines independent signals instead of trusting any single one:
//! the provider's end-of-utterance flag, measured silence, how finished the text reads, and
//! whether it's a question. An EOU on an unfinished sentence waits; long silence commits even
//! without an EOU (providers miss them, and some have none).

use super::intent::Assessment;
use crate::audio::AudioChunk;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EndpointConfig {
    /// Silence needed after an EOU on text that reads finished.
    pub after_eou_ms: f64,
    /// After an EOU on unfinished text ("…and the"), how long to wait for more speech.
    pub eou_unfinished_ms: f64,
    /// Without an EOU, silence that commits finished-looking text.
    pub finished_ms: f64,
    /// Silence that commits whatever is pending.
    pub max_silence_ms: f64,
    /// Questions commit this much sooner, so answers can start earlier.
    pub question_discount: f64,
    /// While the recognizer is this far behind the audio, silence in the live audio says nothing
    /// about its text from seconds earlier, so silence is measured on the recognizer's own clock
    /// (audio it has processed without a word). A recognizer that keeps up is behind by one
    /// chunk's processing (tens of ms); a second means it is starved (a busy CPU).
    pub backlog_hold_ms: f64,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self { after_eou_ms: 200.0, eou_unfinished_ms: 1200.0, finished_ms: 700.0, max_silence_ms: 1500.0, question_discount: 0.8,
            backlog_hold_ms: 1000.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Signals {
    pub has_text: bool,
    /// The provider sent an end-of-utterance for the current text.
    pub end_of_utterance: bool,
    /// Audio-time silence since the speaker last made sound.
    pub silence_ms: f64,
    pub assessment: Assessment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Provider EOU confirmed by silence and text.
    EndOfUtterance,
    /// Finished-looking text followed by silence, without an EOU.
    Silence,
    /// Long silence; committed regardless of how the text reads.
    MaxSilence,
    /// Listening ended with text still in progress.
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision { Wait, Commit(Reason) }

pub fn decide(signals: &Signals, config: &EndpointConfig) -> Decision {
    if !signals.has_text { return Decision::Wait; }
    let finished = signals.assessment.complete >= 0.5;
    let discount = if signals.assessment.question >= 0.5 { config.question_discount } else { 1.0 };
    if signals.end_of_utterance {
        let needed = if finished { config.after_eou_ms } else { config.eou_unfinished_ms };
        if signals.silence_ms >= needed * discount { return Decision::Commit(Reason::EndOfUtterance); }
        return Decision::Wait;
    }
    if signals.silence_ms >= config.max_silence_ms { return Decision::Commit(Reason::MaxSilence); }
    if signals.assessment.complete >= 0.7 && signals.silence_ms >= config.finished_ms * discount {
        return Decision::Commit(Reason::Silence);
    }
    Decision::Wait
}

/// Tracks audio-time silence for one source from chunk energy. The noise floor is the quietest
/// level heard in the last few seconds, so a noisy microphone or a steady hum is learned as
/// the floor instead of counting as speech forever, while speech (which dips between words
/// and syllables) stays well above it.
#[derive(Clone, Debug)]
pub struct SilenceTracker {
    /// (end of chunk, rms) for the recent window; the floor is its minimum.
    recent: std::collections::VecDeque<(f64, f32)>,
    /// Absolute floor below which audio is always silence.
    min_speech_rms: f32,
    last_voice_end_ms: Option<f64>,
    latest_end_ms: f64,
    /// Voiced stretches (start, end) of the last `VOICE_HISTORY_MS`, so silence can also be read
    /// as of an earlier moment: where a lagging recognizer has got to.
    voiced: std::collections::VecDeque<(f64, f64)>,
}

/// How far back the floor looks. Long enough to span a word, short enough that a hum that
/// starts (a fan, a call's noise bed) stops counting as voice within a few seconds.
const FLOOR_WINDOW_MS: f64 = 2500.0;
/// How far back voiced stretches are kept: well beyond any backlog worth endpointing through.
const VOICE_HISTORY_MS: f64 = 60_000.0;

impl Default for SilenceTracker {
    fn default() -> Self { Self { recent: Default::default(), min_speech_rms: 0.01, last_voice_end_ms: None, latest_end_ms: 0.0, voiced: Default::default() } }
}

impl SilenceTracker {
    /// Returns true if the chunk contains voice.
    pub fn observe(&mut self, chunk: &AudioChunk) -> bool {
        let rms = chunk.rms();
        let end_ms = chunk.end_ms();
        self.recent.push_back((end_ms, rms));
        while self.recent.front().is_some_and(|(at, _)| end_ms - at > FLOOR_WINDOW_MS) { self.recent.pop_front(); }
        let voiced = rms >= self.min_speech_rms.max(self.noise_floor() * 3.0);
        if voiced {
            self.last_voice_end_ms = Some(end_ms);
            match self.voiced.back_mut() {
                Some(last) if chunk.start_ms <= last.1 + 1e-6 => last.1 = end_ms,
                _ => self.voiced.push_back((chunk.start_ms, end_ms)),
            }
        }
        while self.voiced.front().is_some_and(|(_, end)| end_ms - end > VOICE_HISTORY_MS) { self.voiced.pop_front(); }
        self.latest_end_ms = self.latest_end_ms.max(end_ms);
        voiced
    }

    /// Audio time up to which this source has been observed.
    pub fn latest_end_ms(&self) -> f64 { self.latest_end_ms }

    /// The quietest level in the recent window.
    pub fn noise_floor(&self) -> f32 { self.recent.iter().map(|(_, rms)| *rms).fold(f32::INFINITY, f32::min).min(1.0) }

    /// Silence since the last voiced audio (0 while speaking or before any voice).
    pub fn silence_ms(&self) -> f64 { self.silence_since(0.0) }

    /// Silence since the later of the last voiced audio and `text_end_ms`, the audio time the
    /// recognizer last produced text for. Recognizers emit words some way behind the audio,
    /// so an utterance isn't over while its text is still arriving.
    pub fn silence_since(&self, text_end_ms: f64) -> f64 {
        self.last_voice_end_ms.map(|end| (self.latest_end_ms - end.max(text_end_ms)).max(0.0)).unwrap_or(0.0)
    }

    /// [`SilenceTracker::silence_since`] as it read at audio time `at_ms` (within the last
    /// minute): silence up to that moment, ignoring anything heard after it.
    pub fn silence_at(&self, at_ms: f64, text_end_ms: f64) -> f64 {
        let voice_end = self.voiced.iter().rev().find(|(start, _)| *start < at_ms).map(|(_, end)| end.min(at_ms));
        voice_end.map(|end| (at_ms - end.max(text_end_ms)).max(0.0)).unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Source;

    fn signals(eou: bool, silence_ms: f64, question: f32, complete: f32) -> Signals {
        Signals { has_text: true, end_of_utterance: eou, silence_ms, assessment: Assessment { question, complete } }
    }

    #[test]
    fn end_of_utterance_alone_never_commits() {
        let config = EndpointConfig::default();
        assert_eq!(decide(&signals(true, 0.0, 0.9, 1.0), &config), Decision::Wait);
        assert_eq!(decide(&signals(true, 250.0, 0.0, 0.7), &config), Decision::Commit(Reason::EndOfUtterance));
    }

    #[test]
    fn an_unfinished_sentence_waits_after_end_of_utterance() {
        let config = EndpointConfig::default();
        assert_eq!(decide(&signals(true, 600.0, 0.6, 0.1), &config), Decision::Wait);
        assert_eq!(decide(&signals(true, 1200.0, 0.0, 0.1), &config), Decision::Commit(Reason::EndOfUtterance));
    }

    #[test]
    fn silence_commits_without_end_of_utterance_and_questions_commit_sooner() {
        let config = EndpointConfig::default();
        assert_eq!(decide(&signals(false, 600.0, 0.0, 0.7), &config), Decision::Wait);
        assert_eq!(decide(&signals(false, 600.0, 0.6, 0.7), &config), Decision::Commit(Reason::Silence));
        assert_eq!(decide(&signals(false, 1000.0, 0.0, 0.1), &config), Decision::Wait);
        assert_eq!(decide(&signals(false, 1500.0, 0.0, 0.1), &config), Decision::Commit(Reason::MaxSilence));
        assert_eq!(decide(&Signals { has_text: false, ..signals(true, 5000.0, 1.0, 1.0) }, &config), Decision::Wait);
    }

    fn chunk(start_ms: f64, level: f32) -> AudioChunk {
        AudioChunk { source: Source::Them, start_ms, samples: vec![level; 160] }
    }

    #[test]
    fn sustained_speech_keeps_counting_as_voice() {
        let mut tracker = SilenceTracker::default();
        for i in 0..20 { tracker.observe(&chunk(i as f64 * 10.0, 0.002)); }
        // Ten seconds of speech: loud syllables with the short dips between words real speech
        // has. The dips set the floor; the speech must stay voice throughout.
        let mut silence_seen: f64 = 0.0;
        for i in 0..1000 {
            let level = if i % 10 >= 8 { 0.01 } else { 0.1 };
            tracker.observe(&chunk(200.0 + i as f64 * 10.0, level));
            silence_seen = silence_seen.max(tracker.silence_ms());
        }
        assert!(silence_seen <= 20.0, "{silence_seen}");
        assert!(tracker.observe(&chunk(10_200.0, 0.1)));
    }

    /// Quiet captured speech (e.g. a call at low volume) has soft consonants below the voice
    /// threshold; those must not pull the floor up until the speech counts as silence.
    #[test]
    fn soft_consonants_in_quiet_speech_do_not_raise_the_floor_past_the_speech() {
        let mut tracker = SilenceTracker::default();
        for i in 0..50 { tracker.observe(&chunk(i as f64 * 10.0, 0.001)); }
        let mut voiced_chunks = 0;
        for i in 0..1000 {
            let level = if i % 3 == 0 { 0.006 } else { 0.02 };
            if tracker.observe(&chunk(500.0 + i as f64 * 10.0, level)) { voiced_chunks += 1; }
        }
        assert!(voiced_chunks >= 660, "{voiced_chunks} of 1000 chunks counted as voice");
        assert!(tracker.silence_ms() <= 10.0, "{}", tracker.silence_ms());
    }

    /// A microphone with a constant noise bed above the absolute floor (fans, a USB mic's hiss)
    /// must still register silence between sentences; this is what kept Me from endpointing.
    #[test]
    fn a_noisy_microphone_learns_its_noise_bed_as_silence() {
        let mut tracker = SilenceTracker::default();
        for i in 0..300 { tracker.observe(&chunk(i as f64 * 10.0, 0.03)); }
        assert!(!tracker.observe(&chunk(3000.0, 0.03)));
        assert!((tracker.noise_floor() - 0.03).abs() < 1e-6);
        for i in 0..100 { assert!(tracker.observe(&chunk(3010.0 + i as f64 * 10.0, 0.15))); }
        for i in 0..80 { tracker.observe(&chunk(4010.0 + i as f64 * 10.0, 0.03)); }
        assert!((tracker.silence_ms() - 800.0).abs() < 1e-6, "{}", tracker.silence_ms());
    }

    #[test]
    fn silence_can_be_read_as_of_an_earlier_moment() {
        let mut tracker = SilenceTracker::default();
        for i in 0..20 { tracker.observe(&chunk(i as f64 * 10.0, 0.002)); }
        for i in 20..50 { tracker.observe(&chunk(i as f64 * 10.0, 0.2)); }
        for i in 50..100 { tracker.observe(&chunk(i as f64 * 10.0, 0.002)); }
        for i in 100..150 { tracker.observe(&chunk(i as f64 * 10.0, 0.2)); }
        // Speaking now, but 300 ms into the pause that ended at 1 s.
        assert_eq!(tracker.silence_ms(), 0.0);
        assert!((tracker.silence_at(800.0, 0.0) - 300.0).abs() < 1e-6, "{}", tracker.silence_at(800.0, 0.0));
        assert!((tracker.silence_at(800.0, 700.0) - 100.0).abs() < 1e-6);
        assert_eq!(tracker.silence_at(400.0, 0.0), 0.0);
        assert_eq!(tracker.silence_at(100.0, 0.0), 0.0);
        assert_eq!(tracker.silence_at(1500.0, 0.0), 0.0);
    }

    #[test]
    fn silence_is_measured_from_the_last_voiced_audio_against_a_noise_floor() {
        let mut tracker = SilenceTracker::default();
        for i in 0..20 { assert!(!tracker.observe(&chunk(i as f64 * 10.0, 0.002))); }
        assert_eq!(tracker.silence_ms(), 0.0);
        assert!(tracker.observe(&chunk(200.0, 0.2)));
        for i in 21..51 { tracker.observe(&chunk(i as f64 * 10.0, 0.002)); }
        assert!((tracker.silence_ms() - 300.0).abs() < 1e-6, "{}", tracker.silence_ms());
        // Text that arrived 100 ms after the voice stopped shortens the silence to 200 ms.
        assert!((tracker.silence_since(310.0) - 200.0).abs() < 1e-6, "{}", tracker.silence_since(310.0));
        assert_eq!(tracker.silence_since(0.0), tracker.silence_ms());
        assert_eq!(tracker.silence_since(10_000.0), 0.0);
    }
}
