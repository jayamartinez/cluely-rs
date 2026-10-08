//! Timestamped capture buffers → a continuous run of canonical chunks. ScreenCaptureKit delivers
//! system audio with capture timestamps but may deliver nothing while nothing plays; this places
//! each buffer on the session timeline and fills every gap with real silence, so timestamps stay
//! sample-accurate and endpointing sees the pauses. Pure processing, so it is fully testable.

use super::frame::{AudioChunk, Source};
use super::normalize::Normalizer;

/// Timestamp differences below this are jitter, not missing audio.
const JITTER_MS: f64 = 5.0;

pub struct GapFiller {
    normalizer: Normalizer,
    rate: f64,
    channels: usize,
    /// When the capture started, relative to the session start; frame 0 of the timeline.
    origin_ms: f64,
    /// Device frames placed on the timeline so far, silence included.
    frames: u64,
    /// How far the timeline may trail the wall clock before silence is filled in.
    slack_ms: f64,
}

impl GapFiller {
    pub fn new(source: Source, rate: u32, channels: u16, origin_ms: f64, slack_ms: f64) -> anyhow::Result<Self> {
        let normalizer = Normalizer::new(source, rate, channels, origin_ms)?;
        Ok(Self { normalizer, rate: rate as f64, channels: channels as usize, origin_ms, frames: 0, slack_ms })
    }

    /// Interleaved device samples whose first frame was captured at `at_ms` (session clock).
    /// Missing audio before it becomes silence; audio that overlaps what was already placed (it
    /// arrived later than the slack allowed) is appended rather than lost.
    pub fn push(&mut self, at_ms: f64, interleaved: &[f32]) -> anyhow::Result<Option<AudioChunk>> {
        let position = (at_ms - self.origin_ms) * self.rate / 1000.0;
        let gap = position.round() - self.frames as f64;
        let mut block = Vec::new();
        if gap > JITTER_MS * self.rate / 1000.0 { block.resize(gap as usize * self.channels, 0.0); }
        block.extend_from_slice(interleaved);
        self.feed(&block)
    }

    /// Nothing arrived: bring the timeline up to `now_ms` less the slack with silence.
    pub fn idle(&mut self, now_ms: f64) -> anyhow::Result<Option<AudioChunk>> {
        let expected = ((now_ms - self.origin_ms - self.slack_ms) * self.rate / 1000.0).max(0.0) as u64;
        if expected <= self.frames { return Ok(None); }
        self.feed(&vec![0.0; (expected - self.frames) as usize * self.channels])
    }

    fn feed(&mut self, block: &[f32]) -> anyhow::Result<Option<AudioChunk>> {
        self.frames += (block.len() / self.channels) as u64;
        self.normalizer.push(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLACK: f64 = 150.0;

    fn samples(chunks: &[AudioChunk]) -> Vec<f32> { chunks.iter().flat_map(|c| c.samples.iter().copied()).collect() }

    fn assert_contiguous(chunks: &[AudioChunk]) {
        for pair in chunks.windows(2) { assert!((pair[1].start_ms - pair[0].end_ms()).abs() < 1e-6, "{} vs {}", pair[1].start_ms, pair[0].end_ms()); }
    }

    #[test]
    fn back_to_back_buffers_pass_through_with_jitter_ignored() {
        let mut filler = GapFiller::new(Source::Them, 16_000, 1, 100.0, SLACK).unwrap();
        // 10 ms buffers whose timestamps wobble by up to 2 ms.
        let chunks: Vec<AudioChunk> = (0..50).filter_map(|i| {
            let wobble = if i % 2 == 0 { 2.0 } else { -2.0 };
            filler.push(100.0 + i as f64 * 10.0 + wobble, &[0.25; 160]).unwrap()
        }).collect();
        assert_eq!(chunks[0].start_ms, 100.0);
        assert_contiguous(&chunks);
        let audio = samples(&chunks);
        assert_eq!(audio.len(), 8_000);
        assert!(audio.iter().all(|s| *s == 0.25));
    }

    #[test]
    fn a_gap_between_buffers_becomes_exactly_that_much_silence() {
        let mut filler = GapFiller::new(Source::Them, 16_000, 1, 0.0, SLACK).unwrap();
        let mut chunks = Vec::new();
        chunks.extend(filler.push(0.0, &[0.5; 1_600]).unwrap());
        // 100 ms of audio, then nothing until 350 ms: 250 ms of silence.
        chunks.extend(filler.push(350.0, &[0.5; 1_600]).unwrap());
        assert_contiguous(&chunks);
        let audio = samples(&chunks);
        assert_eq!(audio.len(), 1_600 + 4_000 + 1_600);
        assert!(audio[..1_600].iter().all(|s| *s == 0.5));
        assert!(audio[1_600..5_600].iter().all(|s| *s == 0.0));
        assert!(audio[5_600..].iter().all(|s| *s == 0.5));
    }

    #[test]
    fn silence_follows_the_wall_clock_while_nothing_arrives_and_audio_resumes_after_it() {
        let mut filler = GapFiller::new(Source::Them, 16_000, 1, 40.0, SLACK).unwrap();
        let mut chunks = Vec::new();
        // Nothing within the slack yet.
        assert!(filler.idle(40.0 + SLACK).unwrap().is_none());
        // One second of polling every 10 ms with nothing delivered.
        for tick in 1..=100 { chunks.extend(filler.idle(40.0 + SLACK + tick as f64 * 10.0).unwrap()); }
        assert_eq!(chunks[0].start_ms, 40.0);
        assert_eq!(samples(&chunks).len(), 16_000);
        assert!(samples(&chunks).iter().all(|s| *s == 0.0));
        // Audio captured inside the stretch already filled is appended, not dropped.
        chunks.extend(filler.push(40.0 + 990.0, &[0.5; 320]).unwrap());
        // Audio captured after the filled stretch gets the missing silence first.
        chunks.extend(filler.push(40.0 + 1_100.0, &[0.5; 320]).unwrap());
        assert_contiguous(&chunks);
        let audio = samples(&chunks);
        assert_eq!(audio.len(), 16_000 + 320 + 1_280 + 320);
        assert!(audio[16_000..16_320].iter().all(|s| *s == 0.5));
        assert!(audio[16_320..17_600].iter().all(|s| *s == 0.0));
        // Sample 17 600 is 1 100 ms after the origin: where the resumed audio was captured.
        assert!(audio[17_600..].iter().all(|s| *s == 0.5));
    }

    #[test]
    fn other_device_formats_are_normalized_on_the_same_timeline() {
        let mut filler = GapFiller::new(Source::Them, 48_000, 2, 0.0, SLACK).unwrap();
        let mut chunks = Vec::new();
        for i in 0..20 { chunks.extend(filler.push(i as f64 * 20.0, &[0.5; 960 * 2]).unwrap()); }
        for tick in 0..60 { chunks.extend(filler.idle(400.0 + SLACK + tick as f64 * 10.0).unwrap()); }
        assert_contiguous(&chunks);
        assert!(chunks.iter().all(|c| c.source == Source::Them));
        let total: f64 = chunks.iter().map(AudioChunk::duration_ms).sum();
        assert!((total - 990.0).abs() < 25.0, "{total} ms");
    }
}
