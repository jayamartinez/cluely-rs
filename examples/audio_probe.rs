//! Capture for a few seconds and report what arrived.
//!
//!   cargo run --example audio_probe -- [seconds] [--them] [--wav <path>]
//!
//! Without `--them` both sources are captured (the microphone too); `--them` captures desktop audio
//! only (ScreenCaptureKit on macOS, which needs the Screen Recording permission) and prints its
//! level every 500 ms. `--wav` saves what desktop audio delivered as 16 kHz mono 16-bit WAV, which
//! `transcript_replay` reads.
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use cluely_rs::audio::{AudioCapture, AudioChunk, SAMPLE_RATE, Source};

const WINDOW: usize = SAMPLE_RATE as usize / 2;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(4);
    let them_only = args.iter().any(|a| a == "--them");
    let wav = args.iter().position(|a| a == "--wav").and_then(|i| args.get(i + 1)).cloned();
    let sources: &[Source] = if them_only { &[Source::Them] } else { &Source::ALL };
    let (sink, chunks) = channel();
    let started = Instant::now();
    let (capture, failures) = AudioCapture::start(sources, sink)?;
    for info in &capture.opened { println!("{}: {} @ {} Hz x{}", info.source.label(), info.device, info.sample_rate, info.channels); }
    for (source, error) in &failures { println!("{}: failed: {error}", source.label()); }
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut stats = [Stats::default(), Stats::default()];
    let mut them = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(chunk) = chunks.recv_timeout(left) else { break };
        let received_ms = started.elapsed().as_secs_f64() * 1000.0;
        if chunk.source == Source::Them { them.extend_from_slice(&chunk.samples); }
        stats[chunk.source as usize].add(&chunk, received_ms);
    }
    for (source, s) in Source::ALL.iter().zip(&stats) {
        if s.chunks == 0 && !sources.contains(source) { continue; }
        println!("{:<4} chunks={:<5} audio={:.2}s first_start={:.0}ms last_end={:.0}ms peak_rms={:.4} max_gap={:.3}ms max_lag={:.0}ms dropped={}",
            source.label(), s.chunks, s.samples as f64 / SAMPLE_RATE as f64, s.first_start.unwrap_or(0.0), s.last_end.unwrap_or(0.0),
            s.peak, s.max_gap, s.max_lag, capture.dropped_samples(*source));
    }
    for (source, why) in capture.take_ended() { println!("{}: ended: {why}", source.label()); }
    capture.stop();
    if them_only {
        println!("\nThem level per 500 ms (from its first sample):");
        for (i, window) in them.chunks(WINDOW).enumerate() {
            let rms = (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt();
            println!("  {:5.1}s  rms={rms:.5} {}", i as f64 * 0.5, "#".repeat((rms * 200.0).min(60.0) as usize));
        }
    }
    if let Some(path) = wav {
        write_wav(&path, &them)?;
        println!("\nwrote {:.2}s of Them to {path}", them.len() as f64 / SAMPLE_RATE as f64);
    }
    Ok(())
}

#[derive(Default)]
struct Stats {
    chunks: usize,
    samples: usize,
    peak: f32,
    first_start: Option<f64>,
    last_end: Option<f64>,
    /// Largest timestamp discontinuity between consecutive chunks.
    max_gap: f64,
    /// Largest delay from a chunk's last sample to its arrival here.
    max_lag: f64,
}

impl Stats {
    fn add(&mut self, chunk: &AudioChunk, received_ms: f64) {
        self.chunks += 1;
        self.samples += chunk.samples.len();
        self.peak = self.peak.max(chunk.rms());
        if let Some(end) = self.last_end { self.max_gap = self.max_gap.max((chunk.start_ms - end).abs()); }
        self.first_start.get_or_insert(chunk.start_ms);
        self.last_end = Some(chunk.end_ms());
        self.max_lag = self.max_lag.max(received_ms - chunk.end_ms());
    }
}

fn write_wav(path: &str, samples: &[f32]) -> std::io::Result<()> {
    let data = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    for field in [16u32.to_le_bytes(), [1, 0, 1, 0], SAMPLE_RATE.to_le_bytes(), (SAMPLE_RATE * 2).to_le_bytes(), [2, 0, 16, 0]] { bytes.extend_from_slice(&field); }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    bytes.extend(samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()));
    std::fs::write(path, bytes)
}
