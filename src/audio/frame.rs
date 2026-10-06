//! The canonical audio unit every downstream stage (VAD, ASR, metrics) consumes.

/// Speech inference runs on mono 16 kHz audio.
pub const SAMPLE_RATE: u32 = 16_000;

/// Where audio came from. Sources are captured, transcribed and labelled separately so
/// question detection can react to the other side of the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Microphone: the user.
    Me,
    /// Desktop audio (WASAPI loopback): everyone else on the call.
    Them,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Me, Source::Them];

    pub fn label(self) -> &'static str {
        match self { Source::Me => "Me", Source::Them => "Them" }
    }
}

/// A run of mono 16 kHz samples from one source.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioChunk {
    pub source: Source,
    /// Position of the first sample, in milliseconds since the capture session started.
    /// Derived from sample counts, so consecutive chunks are exactly contiguous.
    pub start_ms: f64,
    pub samples: Vec<f32>,
}

impl AudioChunk {
    pub fn duration_ms(&self) -> f64 { self.samples.len() as f64 * 1000.0 / SAMPLE_RATE as f64 }

    pub fn end_ms(&self) -> f64 { self.start_ms + self.duration_ms() }

    /// Root-mean-square level in 0..=1, for meters and simple energy gating.
    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() { return 0.0; }
        (self.samples.iter().map(|s| s * s).sum::<f32>() / self.samples.len() as f32).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_levels_follow_the_sample_count() {
        let chunk = AudioChunk { source: Source::Them, start_ms: 250.0, samples: vec![0.5; 1600] };
        assert_eq!(chunk.duration_ms(), 100.0);
        assert_eq!(chunk.end_ms(), 350.0);
        assert!((chunk.rms() - 0.5).abs() < 1e-6);
        assert_eq!(AudioChunk { samples: vec![], ..chunk }.rms(), 0.0);
    }
}
