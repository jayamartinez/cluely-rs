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
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self { after_eou_ms: 200.0, eou_unfinished_ms: 1200.0, finished_ms: 700.0, max_silence_ms: 1500.0, question_discount: 0.8 }
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

/// Tracks audio-time silence for one source from chunk energy, with an adaptive noise floor so
/// a noisy mic or steady background music doesn't count as speech forever.
#[derive(Clone, Debug)]
pub struct SilenceTracker {
    noise_floor: f32,
    /// Absolute floor below which audio is always silence.
    min_speech_rms: f32,
    last_voice_end_ms: Option<f64>,
    latest_end_ms: f64,
}

impl Default for SilenceTracker {
    fn default() -> Self { Self { noise_floor: 0.0, min_speech_rms: 0.01, last_voice_end_ms: None, latest_end_ms: 0.0 } }
}

impl SilenceTracker {
    /// Returns true if the chunk contains voice.
    pub fn observe(&mut self, chunk: &AudioChunk) -> bool {
        let rms = chunk.rms();
        let voiced = rms >= self.min_speech_rms.max(self.noise_floor * 3.0);
        // Learn the floor from quiet audio. Speech may nudge it up only very slowly, so a long
        // sentence never raises the floor until speech itself stops counting as voice.
        let rate = if voiced { 0.0001 } else { 0.2 };
        self.noise_floor += (rms - self.noise_floor) * rate;
        if voiced { self.last_voice_end_ms = Some(chunk.end_ms()); }
        self.latest_end_ms = self.latest_end_ms.max(chunk.end_ms());
        voiced
    }

    /// Silence since the last voiced audio (0 while speaking or before any voice).
    pub fn silence_ms(&self) -> f64 {
        self.last_voice_end_ms.map(|end| (self.latest_end_ms - end).max(0.0)).unwrap_or(0.0)
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
        // Ten seconds of continuous speech must not raise the floor past the speech itself.
        assert!((0..1000).all(|i| tracker.observe(&chunk(200.0 + i as f64 * 10.0, 0.1))));
        assert_eq!(tracker.silence_ms(), 0.0);
    }

    #[test]
    fn silence_is_measured_from_the_last_voiced_audio_against_a_noise_floor() {
        let mut tracker = SilenceTracker::default();
        for i in 0..20 { assert!(!tracker.observe(&chunk(i as f64 * 10.0, 0.002))); }
        assert_eq!(tracker.silence_ms(), 0.0);
        assert!(tracker.observe(&chunk(200.0, 0.2)));
        for i in 21..51 { tracker.observe(&chunk(i as f64 * 10.0, 0.002)); }
        assert!((tracker.silence_ms() - 300.0).abs() < 1e-6, "{}", tracker.silence_ms());
    }
}
