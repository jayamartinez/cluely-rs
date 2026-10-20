//! Acoustic echo cancellation for the microphone. On speakers the mic also hears the other side,
//! and without this "Them" speech is transcribed a second time as "Me". Desktop audio is exactly
//! what the speakers play, so it is the far-end reference: WebRTC's AEC3 (`sonora`, a pure-Rust
//! port) learns the echo path from it, including the delay, and removes the echo from the mic.
//!
//! Chunks go through in arrival order, as the capture threads deliver them in real time: desktop
//! audio feeds the canceller and passes on unchanged; mic audio is cancelled one 10 ms frame at a
//! time and passes on as soon as its frame is complete. Mic audio never waits for desktop audio,
//! so it gains at most one frame of latency. AEC3 finds the echo delay itself, follows it as it
//! changes, and tolerates the two streams' jitter and clock drift, so timestamps are not used to
//! align them.
//!
//! Until desktop audio arrives (it is off, or failed to open) the mic passes through untouched.
//! With headphones the reference has no echo in the mic, and AEC3's transparent mode stops
//! suppressing once it notices.
//!
//! Cancelled mic audio trails its timestamps by 8 ms (AEC3 regroups frames into 4 ms blocks and
//! holds two back), far below what transcript timing resolves, so chunks keep their timestamps.

use sonora::config::{EchoCanceller as Aec3, TransparentModeType};
use sonora::{AudioProcessing, Config, StreamConfig};

use super::frame::{AudioChunk, SAMPLE_RATE, Source};

/// AEC3 works on 10 ms frames.
const FRAME: usize = SAMPLE_RATE as usize / 100;

pub struct EchoCanceller {
    /// Created on the first desktop audio; until then the mic passes through.
    apm: Option<AudioProcessing>,
    /// Desktop audio waiting for a full frame.
    reference: Vec<f32>,
    /// Mic audio waiting for a full frame, and the timestamp of its first sample.
    near: Vec<f32>,
    near_start_ms: f64,
    scratch: Vec<f32>,
}

impl Default for EchoCanceller {
    fn default() -> Self { Self::new() }
}

impl EchoCanceller {
    pub fn new() -> Self {
        Self { apm: None, reference: Vec::with_capacity(FRAME * 4), near: Vec::with_capacity(FRAME * 4), near_start_ms: 0.0,
            scratch: vec![0.0; FRAME] }
    }

    /// Whether desktop audio has started the canceller.
    pub fn active(&self) -> bool { self.apm.is_some() }

    /// Feed a chunk from either source and get back what should go on: desktop audio unchanged,
    /// mic audio with the echo removed (whole frames only; the rest waits for the next chunk).
    pub fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        match chunk.source {
            Source::Them => { self.render(&chunk.samples); Some(chunk) }
            Source::Me => self.capture(chunk),
        }
    }

    fn render(&mut self, samples: &[f32]) {
        let apm = self.apm.get_or_insert_with(new_processor);
        self.reference.extend_from_slice(samples);
        let whole = self.reference.len() / FRAME * FRAME;
        for frame in self.reference[..whole].chunks_exact(FRAME) {
            // Fails only on a frame of the wrong size, which chunks_exact rules out.
            let _ = apm.process_render_f32(&[frame], &mut [&mut self.scratch]);
        }
        self.reference.drain(..whole);
    }

    fn capture(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        let Some(apm) = self.apm.as_mut() else { return Some(chunk) };
        if self.near.is_empty() { self.near_start_ms = chunk.start_ms; }
        self.near.extend_from_slice(&chunk.samples);
        let whole = self.near.len() / FRAME * FRAME;
        if whole == 0 { return None; }
        let mut samples: Vec<f32> = self.near.drain(..whole).collect();
        for frame in samples.chunks_exact_mut(FRAME) {
            self.scratch.copy_from_slice(frame);
            if apm.process_capture_f32(&[&self.scratch], &mut [frame]).is_err() { frame.copy_from_slice(&self.scratch); }
        }
        let start_ms = self.near_start_ms;
        self.near_start_ms += whole as f64 * 1000.0 / SAMPLE_RATE as f64;
        Some(AudioChunk { source: Source::Me, start_ms, samples })
    }

    /// The canceller's view of the echo, for the log: echo delay, what its linear filter removes
    /// (the suppressor after it removes more), and how much quieter than desktop audio the echo is (ERL).
    pub fn summary(&self) -> Option<String> {
        let stats = self.apm.as_ref()?.statistics();
        let db = |value: Option<f64>| value.map_or("?".into(), |db| format!("{db:.1}"));
        Some(format!("delay {} ms, linear filter ERLE {} dB, ERL {} dB", stats.delay_ms.map_or("?".into(), |ms| ms.to_string()),
            db(stats.echo_return_loss_enhancement), db(stats.echo_return_loss)))
    }
}

/// Echo cancellation only: noise suppression and gain control would change what the recognizer
/// hears for no benefit to it. AEC3 brings its own high-pass filter. Its HMM transparent mode
/// notices headphones (a reference with no echo in the mic) within a second or two, where the
/// default counter takes several seconds, suppressing the user's voice meanwhile.
fn new_processor() -> AudioProcessing {
    let mono = StreamConfig::new(SAMPLE_RATE, 1);
    let mut apm = AudioProcessing::builder()
        .config(Config { echo_canceller: Some(Aec3 { transparent_mode: TransparentModeType::Hmm, ..Aec3::default() }), ..Default::default() })
        .capture_config(mono)
        .render_config(mono)
        .build();
    // AEC3 estimates the delay itself; the hint only has to be set.
    let _ = apm.set_stream_delay_ms(0);
    apm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech-like test signal: band-limited noise in syllable-rate bursts, deterministic per seed.
    fn talker(seed: u64, seconds: f32) -> Vec<f32> {
        let mut state = seed;
        let mut low = 0.0f32;
        (0..(seconds * SAMPLE_RATE as f32) as usize).map(|i| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let white = (state >> 33) as f32 / (1u64 << 31) as f32 - 0.5;
            low += 0.3 * (white - low);
            let t = i as f32 / SAMPLE_RATE as f32;
            let syllables = (t * std::f32::consts::PI * (3.0 + seed as f32 % 2.0)).sin().powi(2);
            low * syllables * 0.6
        }).collect()
    }

    /// The far end through a speaker and room: delayed, attenuated, with one reflection.
    fn echo_of(far: &[f32], delay: usize) -> Vec<f32> {
        (0..far.len()).map(|i| {
            let tap = |d: usize, gain: f32| if i >= d { far[i - d] * gain } else { 0.0 };
            tap(delay, 0.5) + tap(delay + 37, -0.2)
        }).collect()
    }

    /// Feed `far` and `mic` in step, in `piece`-sample chunks, and collect the mic's output.
    fn run(canceller: &mut EchoCanceller, far: Option<&[f32]>, mic: &[f32], piece: usize) -> Vec<AudioChunk> {
        let mut out = Vec::new();
        for (i, block) in mic.chunks(piece).enumerate() {
            let start_ms = (i * piece) as f64 * 1000.0 / SAMPLE_RATE as f64;
            if let Some(far) = far {
                let reference = AudioChunk { source: Source::Them, start_ms, samples: far[i * piece..i * piece + block.len()].to_vec() };
                assert_eq!(canceller.process(reference.clone()), Some(reference), "desktop audio passes on unchanged");
            }
            out.extend(canceller.process(AudioChunk { source: Source::Me, start_ms, samples: block.to_vec() }));
        }
        out
    }

    fn samples(chunks: &[AudioChunk]) -> Vec<f32> { chunks.iter().flat_map(|chunk| chunk.samples.iter().copied()).collect() }

    fn energy(samples: &[f32]) -> f32 { samples.iter().map(|s| s * s).sum() }

    #[test]
    fn without_desktop_audio_the_mic_passes_through_untouched() {
        let mic = talker(1, 1.0);
        let mut canceller = EchoCanceller::new();
        let out = run(&mut canceller, None, &mic, 333);
        assert!(!canceller.active());
        assert_eq!(samples(&out), mic, "not even filtered or framed");
        assert!(canceller.summary().is_none());
    }

    #[test]
    fn cancelled_mic_audio_comes_in_whole_frames_on_its_own_timeline() {
        let mut canceller = EchoCanceller::new();
        let far = talker(2, 1.0);
        let out = run(&mut canceller, Some(&far), &talker(1, 1.0), 250);
        assert!(canceller.active());
        assert!(out.iter().all(|chunk| chunk.source == Source::Me && chunk.samples.len() % FRAME == 0));
        assert_eq!(out[0].start_ms, 0.0);
        for pair in out.windows(2) { assert!((pair[1].start_ms - pair[0].end_ms()).abs() < 1e-9, "{pair:?}"); }
        // What is held back is less than one frame.
        assert!(16_000 - samples(&out).len() < FRAME);
    }

    #[test]
    fn the_far_end_echo_is_removed_from_the_mic() {
        let far = talker(2, 6.0);
        let mic = echo_of(&far, 960); // 60 ms
        let out = samples(&run(&mut EchoCanceller::new(), Some(&far), &mic, 160));
        let tail = 4 * SAMPLE_RATE as usize..out.len();
        let erle = 10.0 * (energy(&mic[tail.clone()]) / energy(&out[tail])).log10();
        assert!(erle > 20.0, "ERLE {erle:.1} dB");
    }

    #[test]
    fn with_headphones_the_users_voice_keeps_its_level() {
        // Desktop audio plays but none of it reaches the mic.
        let far = talker(2, 6.0);
        let me = talker(1, 6.0);
        let out = samples(&run(&mut EchoCanceller::new(), Some(&far), &me, 160));
        let tail = 3 * SAMPLE_RATE as usize..out.len();
        let kept = 10.0 * (energy(&out[tail.clone()]) / energy(&me[tail])).log10();
        assert!(kept > -1.5, "Me changed by {kept:.1} dB");
    }
}
