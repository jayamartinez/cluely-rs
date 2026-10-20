//! Measure the echo canceller on a synthetic speakerphone call. The far end (Them) plays through
//! a simulated loudspeaker and room into the mic, on top of the user's own speech (Me), and both
//! streams reach the canceller in small, jittered chunks as the capture threads deliver them.
//!
//!   cargo run --release --example echo_eval -- <them.wav> <me.wav> [options]
//!
//!   --me-at <s>        where Me starts on the Them timeline (default 3), so the two overlap
//!   --delay <ms>       echo delay from the reference to the mic (default 120)
//!   --echo-db <dB>     echo level relative to the far-end signal (default -6)
//!   --reverb <x>       room reflections relative to the direct sound (default 0.15; 0.5 is a
//!                      bare, echoey room)
//!   --headphones       no echo at all: the canceller must leave Me alone
//!   --out <dir>        write mic.wav (Me as captured), them.wav and out.wav (Me after the canceller)
//!
//! Both inputs are 16 kHz mono 16-bit WAV, e.g. dev/make-bench-audio.ps1 for Them and another
//! voice for Me. `transcript_replay <dir>/mic.wav --source me --reference <dir>/them.wav` then
//! transcribes Me through the real pipeline with the canceller, and without `--reference`, as it
//! was before. Per 20 ms block of the clean signals, the report splits the audio into far-end only
//! (ERLE: echo removed), near-end only (what happened to Me) and double talk (both at once).

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use cluely_rs::audio::{AudioChunk, EchoCanceller, SAMPLE_RATE, Source};

const BLOCK: usize = 320;
/// A 20 ms block counts as speech above this RMS (-50 dBFS).
const ACTIVE: f32 = 0.003;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let number = |flag: &str, default: f64| value(flag).map_or(Ok(default), |v| v.parse::<f64>().with_context(|| format!("{flag} {v}")));
    let (Some(them_path), Some(me_path)) = (args.first(), args.get(1)) else { bail!("usage: echo_eval <them.wav> <me.wav> [options]") };
    let ms = |ms: f64| (ms * SAMPLE_RATE as f64 / 1000.0) as usize;
    let headphones = args.iter().any(|a| a == "--headphones");
    let (delay, echo_gain) = (ms(number("--delay", 120.0)?), 10f32.powf(number("--echo-db", -6.0)? as f32 / 20.0));
    let reverb = number("--reverb", 0.15)? as f32;

    let far = read_wav(them_path)?;
    let me_at = ms(number("--me-at", 3.0)? * 1000.0);
    let me = read_wav(me_path)?;
    let len = far.len().max(me_at + me.len()) + delay;
    let mut near = vec![0.0; len];
    near[me_at..me_at + me.len()].copy_from_slice(&me);
    let echo = if headphones { vec![0.0; len] } else { loudspeaker_and_room(&far, len, delay, echo_gain, reverb) };
    let mut rng = Lcg(7);
    let mic: Vec<f32> = (0..len).map(|i| near[i] + echo[i] + rng.uniform() * 0.0005).collect();
    let mut them = far.clone();
    them.resize(len, 0.0);

    let (out, busy, summary) = cancel(&mic, &them, 11);
    // What the canceller makes of Me alone (its high-pass filter and block framing), so Me's
    // fate below is measured against that rather than against the raw recording.
    let near_seen = cancel(&near, &vec![0.0; len], 11).0;

    let seconds = len as f64 / SAMPLE_RATE as f64;
    println!("{:.1} s of audio, echo {} ({} ms delay, {:+.0} dB)", seconds, if headphones { "off (headphones)" } else { "on" },
        delay / 16, 20.0 * echo_gain.log10());
    println!("canceller: {summary}");
    println!("CPU: {:.1} µs per 10 ms frame ({:.2} % of one core)", busy.as_secs_f64() * 1e6 / (seconds * 100.0), busy.as_secs_f64() / seconds * 100.0);
    report(&near, &near_seen, &echo, &mic, &out);

    if let Some(dir) = value("--out") {
        std::fs::create_dir_all(&dir)?;
        for (name, samples) in [("mic", &mic), ("them", &them), ("out", &out)] { write_wav(&format!("{dir}/{name}.wav"), samples)?; }
        println!("wrote mic.wav, them.wav and out.wav to {dir}");
    }
    Ok(())
}

/// The far end as the mic hears it from a small loudspeaker: band-limited, slightly saturated,
/// delayed, and smeared by a room's decaying reflections.
fn loudspeaker_and_room(far: &[f32], len: usize, delay: usize, gain: f32, reverb: f32) -> Vec<f32> {
    let (mut low, mut high) = (0.0f32, 0.0f32);
    let speaker: Vec<f32> = far.iter().map(|&x| {
        low += 0.55 * (x - low); // rolls off above ~3 kHz
        high += 0.04 * (low - high); // and below ~100 Hz
        ((low - high) * 0.8).tanh() / 0.8 // mild saturation on peaks
    }).collect();
    let mut rng = Lcg(3);
    let mut room: Vec<f32> = (0..2400).map(|k| if k == 0 { 1.0 } else { rng.uniform() * reverb * (-(k as f32) / 600.0).exp() }).collect();
    let norm = room.iter().map(|h| h * h).sum::<f32>().sqrt();
    room.iter_mut().for_each(|h| *h *= gain / norm);
    let mut echo = vec![0.0; len];
    for (i, &x) in speaker.iter().enumerate() {
        if x == 0.0 { continue; }
        for (k, &h) in room.iter().enumerate() {
            if let Some(slot) = echo.get_mut(i + delay + k) { *slot += x * h; }
        }
    }
    echo
}

/// Deliver both streams to a canceller in uneven chunks, each arriving a few ms after its last
/// sample (in order within a source, as one capture thread sends them), and collect Me's output.
fn cancel(mic: &[f32], them: &[f32], seed: u64) -> (Vec<f32>, Duration, String) {
    let mut rng = Lcg(seed);
    let mut arrivals = Vec::new();
    for (source, samples) in [(Source::Them, them), (Source::Me, mic)] {
        let (mut at, mut last) = (0, 0.0f64);
        while at < samples.len() {
            let size = (80 + (rng.next() % 240) as usize).min(samples.len() - at);
            let arrives = ((at + size) as f64 + rng.next() as f64 % 128.0).max(last);
            last = arrives;
            arrivals.push((arrives, AudioChunk { source, start_ms: at as f64 / 16.0, samples: samples[at..at + size].to_vec() }));
            at += size;
        }
    }
    arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut canceller = EchoCanceller::new();
    let mut out = vec![0.0f32; mic.len()];
    let mut busy = Duration::ZERO;
    for (_, chunk) in arrivals {
        let started = Instant::now();
        let passed = canceller.process(chunk);
        busy += started.elapsed();
        if let Some(chunk) = passed.filter(|chunk| chunk.source == Source::Me) {
            let at = (chunk.start_ms * 16.0).round() as usize;
            out[at..at + chunk.samples.len()].copy_from_slice(&chunk.samples);
        }
    }
    (out, busy, canceller.summary().unwrap_or_else(|| "never started".into()))
}

/// `near` is Me as recorded, `seen` Me through the canceller on its own.
fn report(near: &[f32], seen: &[f32], echo: &[f32], mic: &[f32], out: &[f32]) {
    let energy = |s: &[f32]| s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
    let rms = |s: &[f32]| (energy(s) / s.len() as f64).sqrt() as f32;
    let db = |ratio: f64| 10.0 * ratio.max(1e-12).log10();
    // Blocks per class: far end only, near end only, both.
    let mut classes: [Vec<usize>; 3] = Default::default();
    for b in 0..near.len() / BLOCK {
        let span = b * BLOCK..(b + 1) * BLOCK;
        match (rms(&echo[span.clone()]) > ACTIVE, rms(&near[span]) > ACTIVE) {
            (true, false) => classes[0].push(b),
            (false, true) => classes[1].push(b),
            (true, true) => classes[2].push(b),
            _ => {}
        }
    }
    let gather = |s: &[f32], blocks: &[usize]| blocks.iter().flat_map(|b| s[b * BLOCK..(b + 1) * BLOCK].iter().copied()).collect::<Vec<f32>>();
    for (name, blocks) in ["far end only", "near end only", "double talk"].iter().zip(&classes) {
        if blocks.is_empty() { println!("{name:<14} none"); continue; }
        let (n, e, m, o) = (gather(seen, blocks), gather(echo, blocks), gather(mic, blocks), gather(out, blocks));
        let seconds = blocks.len() as f64 * BLOCK as f64 / SAMPLE_RATE as f64;
        if *name == "far end only" {
            println!("{name:<14} {seconds:5.1} s  ERLE {:5.1} dB (mic {:.1} dBFS → out {:.1} dBFS)", db(energy(&m) / energy(&o)),
                db(energy(&m) / m.len() as f64), db(energy(&o) / o.len() as f64));
            continue;
        }
        // Me's share of the output, and the rest once it is taken out: echo residue plus
        // whatever the canceller did to Me (suppression varies over time and frequency).
        let scale = n.iter().zip(&o).map(|(a, b)| (*a as f64) * (*b as f64)).sum::<f64>() / energy(&n);
        let rest: Vec<f32> = n.iter().zip(&o).map(|(a, b)| b - a * scale as f32).collect();
        let reference = if *name == "double talk" { format!("rest {:5.1} dB under the echo", db(energy(&e) / energy(&rest))) }
            else { format!("rest {:5.1} dB under Me", db(energy(&n) / energy(&rest))) };
        println!("{name:<14} {seconds:5.1} s  Me kept at {:+5.1} dB, {reference}", db(scale * scale));
    }
}

/// A tiny deterministic generator, so runs are repeatable.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); self.0 >> 33 }
    fn uniform(&mut self) -> f32 { self.next() as f32 / (1u64 << 31) as f32 * 2.0 - 1.0 }
}

fn read_wav(path: &str) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("couldn't read {path}"))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" { bail!("{path} is not a WAV file"); }
    let (mut at, mut format_ok) = (12, false);
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        match &bytes[at..at + 4] {
            b"fmt " => {
                let (format, channels, rate, bits) = (u16::from_le_bytes([body[0], body[1]]), u16::from_le_bytes([body[2], body[3]]),
                    u32::from_le_bytes(body[4..8].try_into()?), u16::from_le_bytes([body[14], body[15]]));
                if (format, channels, rate, bits) != (1, 1, 16000, 16) { bail!("{path} must be 16 kHz mono 16-bit PCM"); }
                format_ok = true;
            }
            b"data" if format_ok => return Ok(body.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0).collect()),
            _ => {}
        }
        at += 8 + size + (size & 1);
    }
    bail!("{path} has no audio data")
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
