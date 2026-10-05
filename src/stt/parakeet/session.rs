//! Turns parakeet.cpp's stream output into transcript events, and works around a quirk of the
//! pinned version: a stream that keeps running across utterances misses many end-of-utterance
//! signals (about 40% in testing, from the first minute on) and its text varies between
//! identical passages. So each stream is kept short. After every
//! end-of-utterance, and after long quiet stretches, the stream is replaced with a fresh one,
//! and the recent audio it hadn't fully decoded is replayed into the new stream from a rolling
//! buffer. Nothing outside this provider knows streams are being replaced.

use std::collections::VecDeque;

use super::ffi::FeedOutput;
use crate::audio::SAMPLE_RATE;
use crate::stt::{EventKind, EventSink};

/// The decoder operations a session needs; implemented by `ffi::Stream`.
pub trait Decoder {
    fn feed(&mut self, pcm: &[f32]) -> Result<FeedOutput, String>;
    fn finalize(&mut self) -> Result<FeedOutput, String>;
}

impl Decoder for super::ffi::Stream {
    fn feed(&mut self, pcm: &[f32]) -> Result<FeedOutput, String> { super::ffi::Stream::feed(self, pcm) }
    fn finalize(&mut self) -> Result<FeedOutput, String> { super::ffi::Stream::finalize(self) }
}

/// Audio kept for replay. Must cover the decoder's lookahead (well under a second) plus the
/// half-second idle cut-back.
const ROLLING_MS: f64 = 3000.0;
/// Replace a stream that has run this long once it has been quiet for `IDLE_MS`.
const MAX_STREAM_MS: f64 = 20_000.0;
const IDLE_MS: f64 = 1000.0;
/// How far back an idle reset cuts, so audio where speech may be starting is replayed.
const IDLE_CUT_BACK_MS: f64 = 500.0;
/// Text appears about this long after the speech it transcribes.
const TEXT_DELAY_MS: f64 = 160.0;

fn samples_to_ms(samples: usize) -> f64 { samples as f64 * 1000.0 / SAMPLE_RATE as f64 }
fn ms_to_samples(ms: f64) -> usize { (ms.max(0.0) * SAMPLE_RATE as f64 / 1000.0).round() as usize }

pub struct Session<D, F> {
    sink: EventSink,
    new_decoder: F,
    decoder: D,
    /// Audio time (ms) of the current stream's first sample.
    origin_ms: f64,
    /// Audio time just past the last sample fed to the current stream.
    fed_until_ms: f64,
    rolling: VecDeque<f32>,
    /// Audio time of `rolling[0]`.
    rolling_start_ms: f64,
    utterance: String,
    utterance_start_ms: f64,
    last_text_ms: f64,
    started: bool,
    /// Number of times the stream was replaced (diagnostics/benchmarks).
    pub resets: u64,
}

impl<D: Decoder, F: FnMut() -> Result<D, String>> Session<D, F> {
    pub fn new(sink: EventSink, mut new_decoder: F) -> Result<Self, String> {
        let decoder = new_decoder()?;
        Ok(Self { sink, new_decoder, decoder, origin_ms: 0.0, fed_until_ms: 0.0, rolling: VecDeque::new(), rolling_start_ms: 0.0,
            utterance: String::new(), utterance_start_ms: 0.0, last_text_ms: 0.0, started: false, resets: 0 })
    }

    /// Feed one chunk of 16 kHz mono audio starting at `start_ms` (audio time).
    pub fn push(&mut self, start_ms: f64, samples: &[f32]) -> Result<(), String> {
        if !self.started {
            self.started = true;
            (self.origin_ms, self.fed_until_ms, self.rolling_start_ms, self.last_text_ms) = (start_ms, start_ms, start_ms, start_ms);
        }
        self.rolling.extend(samples);
        let excess = self.rolling.len().saturating_sub(ms_to_samples(ROLLING_MS));
        if excess > 0 {
            self.rolling.drain(..excess);
            self.rolling_start_ms += samples_to_ms(excess);
        }
        let mut cut = self.feed_decoder(samples)?;
        // A reset replays audio, and the replay itself can end another utterance.
        while let Some(cut_ms) = cut { cut = self.reset(cut_ms)?; }
        Ok(())
    }

    /// Flush the decoder at the end of input and report any unfinished utterance as final.
    pub fn finish(&mut self) -> Result<(), String> {
        let output = self.decoder.finalize()?;
        self.absorb_text(&output.text, self.fed_until_ms);
        if !self.utterance.is_empty() {
            let text = std::mem::take(&mut self.utterance);
            self.sink.emit(self.utterance_start_ms, self.fed_until_ms, EventKind::Final { text });
        }
        Ok(())
    }

    /// Feeds the current stream; returns the audio time to cut at if the stream should be replaced.
    fn feed_decoder(&mut self, samples: &[f32]) -> Result<Option<f64>, String> {
        let output = self.decoder.feed(samples)?;
        self.fed_until_ms += samples_to_ms(samples.len());
        self.absorb_text(&output.text, self.fed_until_ms);
        // Text and the EOU that follows it arrive together or in that order, so everything
        // accumulated so far belongs to the utterance that just ended.
        if let Some(event) = output.events.first() {
            let end_ms = self.origin_ms + event.time_sec * 1000.0;
            if !self.utterance.is_empty() {
                let text = std::mem::take(&mut self.utterance);
                self.sink.emit(self.utterance_start_ms, end_ms, EventKind::EndOfUtterance { text });
            }
            return Ok(Some(end_ms.min(self.fed_until_ms)));
        }
        let quiet = self.fed_until_ms - self.last_text_ms >= IDLE_MS && self.utterance.is_empty();
        if self.fed_until_ms - self.origin_ms >= MAX_STREAM_MS && quiet {
            return Ok(Some(self.fed_until_ms - IDLE_CUT_BACK_MS));
        }
        Ok(None)
    }

    fn absorb_text(&mut self, piece: &str, at_ms: f64) {
        if piece.is_empty() { return; }
        if self.utterance.is_empty() {
            self.utterance_start_ms = (at_ms - TEXT_DELAY_MS).max(self.origin_ms);
            self.utterance.push_str(piece.trim_start());
        } else {
            self.utterance.push_str(piece);
        }
        self.last_text_ms = at_ms;
        if self.utterance.is_empty() { return; }
        // Pieces can end mid-word ("distrib" + "uted"); only words followed by a space are done.
        let stable = self.utterance.rfind(' ').unwrap_or(0);
        self.sink.emit(self.utterance_start_ms, at_ms, EventKind::Partial { text: self.utterance.clone(), stable_hint: Some(stable) });
    }

    /// Replace the stream and replay audio from `cut_ms` onward into the new one.
    fn reset(&mut self, cut_ms: f64) -> Result<Option<f64>, String> {
        self.decoder = (self.new_decoder)()?;
        self.resets += 1;
        let from = ms_to_samples(cut_ms - self.rolling_start_ms).min(self.rolling.len());
        let replay: Vec<f32> = self.rolling.iter().skip(from).copied().collect();
        self.origin_ms = self.rolling_start_ms + samples_to_ms(from);
        self.fed_until_ms = self.origin_ms;
        if replay.is_empty() { return Ok(None); }
        self.feed_decoder(&replay)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc;

    use super::*;
    use crate::audio::Source;
    use crate::stt::parakeet::ffi::StreamEvent;
    use crate::stt::{Generation, TranscriptEvent};

    /// Emits scripted output once a stream has received audio up to a stream-relative time.
    struct Fake { script: Script, fed_ms: f64, log: FeedLog, id: usize }

    impl Decoder for Fake {
        fn feed(&mut self, pcm: &[f32]) -> Result<FeedOutput, String> {
            let before = self.fed_ms;
            self.fed_ms += samples_to_ms(pcm.len());
            self.log.borrow_mut().push((self.id, samples_to_ms(pcm.len())));
            let mut out = FeedOutput::default();
            for (at, text, eou) in &self.script {
                if *at > before && *at <= self.fed_ms {
                    out.text.push_str(text);
                    if let Some(t) = eou { out.events.push(StreamEvent { kind: "eou".into(), time_sec: *t }); }
                }
            }
            Ok(out)
        }
        fn finalize(&mut self) -> Result<FeedOutput, String> { Ok(FeedOutput { text: " tail".into(), ..Default::default() }) }
    }

    type Script = Vec<(f64, &'static str, Option<f64>)>;
    /// (stream id, milliseconds fed) for every feed call.
    type FeedLog = Rc<RefCell<Vec<(usize, f64)>>>;
    type FakeSession = Session<Fake, Box<dyn FnMut() -> Result<Fake, String>>>;

    fn session(scripts: Vec<Script>) -> (FakeSession, mpsc::Receiver<TranscriptEvent>, FeedLog) {
        let (tx, rx) = mpsc::channel();
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut scripts = scripts.into_iter();
        let mut id = 0;
        let factory_log = Rc::clone(&log);
        let factory: Box<dyn FnMut() -> Result<Fake, String>> = Box::new(move || {
            id += 1;
            Ok(Fake { script: scripts.next().unwrap_or_default(), fed_ms: 0.0, log: Rc::clone(&factory_log), id })
        });
        (Session::new(EventSink::new(Source::Them, Generation(1), tx), factory).unwrap(), rx, log)
    }

    fn feed_ms(session: &mut FakeSession, from_ms: f64, to_ms: f64) {
        let mut t = from_ms;
        while t < to_ms { session.push(t, &[0.0; 320]).unwrap(); t += 20.0; }
    }

    #[test]
    fn pieces_accumulate_and_only_whole_words_are_marked_stable() {
        let (mut s, rx, _) = session(vec![vec![(400.0, "so how", None), (600.0, " distrib", None), (800.0, "uted", None)]]);
        feed_ms(&mut s, 0.0, 1000.0);
        let partials: Vec<(String, Option<usize>)> = rx.try_iter().map(|e| match e.kind {
            EventKind::Partial { text, stable_hint } => (text, stable_hint), other => panic!("{other:?}") }).collect();
        assert_eq!(partials, [("so how".into(), Some(2)), ("so how distrib".into(), Some(6)), ("so how distributed".into(), Some(6))]);
    }

    #[test]
    fn end_of_utterance_resets_the_stream_and_replays_audio_after_the_cut() {
        // Stream 1: an utterance whose EOU (at 1.0 s) is reported once 1.2 s has been fed.
        // Stream 2 hears the replayed 1.0–1.2 s and then the next utterance.
        let (mut s, rx, log) = session(vec![
            vec![(600.0, "what is a mutex", None), (1200.0, "", Some(1.0))],
            vec![(1000.0, "and a semaphore", None), (1600.0, "", Some(1.5))],
        ]);
        feed_ms(&mut s, 0.0, 3000.0);
        let events: Vec<(EventKind, f64, f64)> = rx.try_iter().map(|e| (e.kind, e.start_ms, e.end_ms)).collect();
        let eous: Vec<(String, f64)> = events.iter().filter_map(|(k, _, end)| match k {
            EventKind::EndOfUtterance { text } => Some((text.clone(), *end)), _ => None }).collect();
        // Second stream starts at the 1.0 s cut, so its EOU at 1.5 s is 2.5 s in audio time.
        assert_eq!(eous, [("what is a mutex".to_string(), 1000.0), ("and a semaphore".to_string(), 2500.0)]);
        // The replay into stream 2 is exactly the 200 ms after the cut, fed in one block.
        let log = log.borrow();
        let first_stream2 = log.iter().find(|(id, _)| *id == 2).unwrap();
        assert!((first_stream2.1 - 200.0).abs() < 1e-6, "{first_stream2:?}");
        assert!(s.resets >= 2);
    }

    #[test]
    fn a_long_quiet_stream_is_replaced_and_later_speech_is_still_heard() {
        let (mut s, rx, _) = session(vec![
            vec![(500.0, "hello", None), (800.0, "", Some(0.7))],
            vec![], // stays quiet for 20+ s, then gets replaced
            vec![(800.0, "still here", None)],
        ]);
        feed_ms(&mut s, 0.0, 22_000.0);
        assert_eq!(s.resets, 2);
        feed_ms(&mut s, 22_000.0, 23_000.0);
        let texts: Vec<String> = rx.try_iter().filter_map(|e| match e.kind { EventKind::Partial { text, .. } => Some(text), _ => None }).collect();
        assert_eq!(texts.last().map(String::as_str), Some("still here"));
    }

    #[test]
    fn finish_flushes_and_reports_the_unfinished_utterance_as_final() {
        let (mut s, rx, _) = session(vec![vec![(400.0, "and then", None)]]);
        feed_ms(&mut s, 0.0, 600.0);
        s.finish().unwrap();
        let last = rx.try_iter().last().unwrap();
        assert_eq!(last.kind, EventKind::Final { text: "and then tail".into() });
    }
}
