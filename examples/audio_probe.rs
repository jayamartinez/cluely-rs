//! Capture both sources for a few seconds and report what arrived.
//! `cargo run --example audio_probe -- 5`
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use cluely_rs::audio::{AudioCapture, Source};

fn main() -> anyhow::Result<()> {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    let (sink, chunks) = channel();
    let (capture, failures) = AudioCapture::start(&Source::ALL, sink)?;
    for info in &capture.opened { println!("{}: {} @ {} Hz x{}", info.source.label(), info.device, info.sample_rate, info.channels); }
    for (source, error) in &failures { println!("{}: failed: {error}", source.label()); }
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut stats = [(0usize, 0usize, 0f32, 0f64, None::<f64>, 0f64); 2];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(chunk) = chunks.recv_timeout(left) else { break };
        let s = &mut stats[chunk.source as usize];
        s.0 += 1; s.1 += chunk.samples.len(); s.2 = s.2.max(chunk.rms());
        if let Some(end) = s.4 { s.5 = s.5.max((chunk.start_ms - end).abs()); }
        s.3 = chunk.start_ms; s.4 = Some(chunk.end_ms());
    }
    for (source, s) in Source::ALL.iter().zip(stats) {
        println!("{:<4} chunks={:<5} audio={:.2}s peak_rms={:.4} last_start={:.0}ms max_gap={:.3}ms dropped={}",
            source.label(), s.0, s.1 as f64 / 16_000.0, s.2, s.3, s.5, capture.dropped_samples(*source));
    }
    capture.stop();
    Ok(())
}
