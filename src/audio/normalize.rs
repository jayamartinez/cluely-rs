//! Device audio → canonical mono 16 kHz chunks. Pure processing with no device access, so it
//! is fully testable; capture threads feed it whatever the device delivers.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use super::frame::{AudioChunk, SAMPLE_RATE, Source};

/// Input block per resampler call: 10 ms keeps added latency small.
const BLOCK_MS: usize = 10;

pub struct Normalizer {
    source: Source,
    channels: usize,
    /// None when the device already runs at 16 kHz.
    resampler: Option<Fft<f32>>,
    block: usize,
    /// Mono input waiting for a full resampler block.
    pending: Vec<f32>,
    out: Vec<f32>,
    /// Output samples emitted so far; with `origin_ms` this timestamps every chunk exactly.
    emitted: u64,
    origin_ms: f64,
    /// The FFT resampler delays its output; that many leading samples are dropped so
    /// timestamps line up with the input.
    skip: usize,
}

impl Normalizer {
    /// `origin_ms`: when this source's first sample was captured, relative to the session start.
    pub fn new(source: Source, device_rate: u32, channels: u16, origin_ms: f64) -> anyhow::Result<Self> {
        anyhow::ensure!(device_rate > 0 && channels > 0, "invalid device format");
        let block = (device_rate as usize * BLOCK_MS / 1000).max(1);
        let (resampler, skip, out_len) = if device_rate == SAMPLE_RATE {
            (None, 0, block)
        } else {
            let resampler = Fft::<f32>::new(device_rate as usize, SAMPLE_RATE as usize, block, 1, FixedSync::Input)?;
            let skip = resampler.output_delay();
            let out_len = resampler.output_frames_max();
            (Some(resampler), skip, out_len)
        };
        Ok(Self { source, channels: channels as usize, resampler, block, pending: Vec::with_capacity(block * 2),
            out: vec![0.0; out_len], emitted: 0, origin_ms, skip })
    }

    /// Feed interleaved device samples; returns any complete 16 kHz chunks.
    pub fn push(&mut self, interleaved: &[f32]) -> anyhow::Result<Option<AudioChunk>> {
        self.pending.extend(interleaved.chunks_exact(self.channels).map(|frame| frame.iter().sum::<f32>() / self.channels as f32));
        let mut produced = Vec::new();
        while self.pending.len() >= self.block {
            match &mut self.resampler {
                None => produced.extend_from_slice(&self.pending[..self.block]),
                Some(resampler) => {
                    let input = InterleavedSlice::new(&self.pending[..self.block], 1, self.block)?;
                    let capacity = self.out.len();
                    let mut output = InterleavedSlice::new_mut(&mut self.out, 1, capacity)?;
                    let (_, written) = resampler.process_into_buffer(&input, &mut output, None)?;
                    produced.extend_from_slice(&self.out[..written]);
                }
            }
            self.pending.drain(..self.block);
        }
        let dropped = self.skip.min(produced.len());
        self.skip -= dropped;
        produced.drain(..dropped);
        Ok(self.chunk(produced))
    }

    fn chunk(&mut self, samples: Vec<f32>) -> Option<AudioChunk> {
        if samples.is_empty() { return None; }
        let start_ms = self.origin_ms + self.emitted as f64 * 1000.0 / SAMPLE_RATE as f64;
        self.emitted += samples.len() as u64;
        Some(AudioChunk { source: self.source, start_ms, samples })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, seconds: f32, channels: usize) -> Vec<f32> {
        (0..(rate as f32 * seconds) as usize)
            .flat_map(|i| std::iter::repeat_n((i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5, channels))
            .collect()
    }

    fn collect(normalizer: &mut Normalizer, input: &[f32], piece: usize) -> Vec<AudioChunk> {
        input.chunks(piece).filter_map(|part| normalizer.push(part).unwrap()).collect()
    }

    /// Dominant frequency via zero crossings.
    fn frequency(samples: &[f32]) -> f32 {
        let crossings = samples.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        crossings as f32 / 2.0 / (samples.len() as f32 / SAMPLE_RATE as f32)
    }

    #[test]
    fn stereo_48k_becomes_mono_16k_at_the_same_pitch_with_contiguous_timestamps() {
        let mut normalizer = Normalizer::new(Source::Them, 48_000, 2, 120.0).unwrap();
        // Uneven device buffer sizes, as WASAPI delivers them.
        let chunks = collect(&mut normalizer, &sine(48_000, 440.0, 1.0, 2), 1_234 * 2);
        let samples: Vec<f32> = chunks.iter().flat_map(|c| c.samples.iter().copied()).collect();
        assert!((15_500..=16_000).contains(&samples.len()), "{} samples", samples.len());
        assert!((frequency(&samples[1600..]) - 440.0).abs() < 5.0);
        assert_eq!(chunks[0].start_ms, 120.0);
        for pair in chunks.windows(2) { assert!((pair[1].start_ms - pair[0].end_ms()).abs() < 1e-6); }
        assert!(chunks.iter().all(|c| c.source == Source::Them));
    }

    #[test]
    fn odd_rates_resample_and_16k_mono_passes_through_unchanged() {
        let mut odd = Normalizer::new(Source::Me, 44_100, 1, 0.0).unwrap();
        let out: usize = collect(&mut odd, &sine(44_100, 300.0, 0.5, 1), 777).iter().map(|c| c.samples.len()).sum();
        assert!((7_600..=8_000).contains(&out), "{out}");

        let input = sine(16_000, 200.0, 0.1, 1);
        let mut same = Normalizer::new(Source::Me, 16_000, 1, 0.0).unwrap();
        let samples: Vec<f32> = collect(&mut same, &input, 160).into_iter().flat_map(|c| c.samples).collect();
        assert_eq!(samples, input);
    }

    #[test]
    fn channels_are_averaged_and_invalid_formats_rejected() {
        let mut normalizer = Normalizer::new(Source::Me, 16_000, 2, 0.0).unwrap();
        let chunk = normalizer.push(&[1.0, 0.0].repeat(160)).unwrap().unwrap();
        assert!(chunk.samples.iter().all(|s| (*s - 0.5).abs() < 1e-6));
        assert!(Normalizer::new(Source::Me, 0, 1, 0.0).is_err());
    }
}
