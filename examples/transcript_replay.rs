//! Replay a 16 kHz mono 16-bit WAV through the real transcript pipeline (Parakeet → transcript
//! state → endpointing) at real-time pace, and print a timeline for tuning endpointing:
//! where speech starts and stops in the audio, when the recognizer's words and end-of-utterance
//! signals arrive relative to that, and when each utterance is committed and why.
//!
//!   cargo run --example transcript_replay -- <wav> [--source me|them] [--provider parakeet|deepgram]
//!
//! Parakeet uses the installed model (`cargo run --example parakeet_bench -- download`). Deepgram
//! (paid) uses the key saved in Settings → Listening, or DEEPGRAM_API_KEY. Writes nothing.
//!
//!   DEEPGRAM_API_KEY=... cargo run --example transcript_replay -- --store-key
//!
//! stores that key in the OS credential store (what Settings → Listening does), so the key
//! never has to be typed on a command line in plain text.

use std::sync::Arc;
use futures::StreamExt;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use cluely_rs::audio::{AudioChunk, Source};
use cluely_rs::listening::{self, Message, Status};
use cluely_rs::stt::parakeet::ParakeetRealtime;
use cluely_rs::stt::{StreamingAsr, deepgram};
use cluely_rs::transcript::endpoint::EndpointConfig;
use cluely_rs::transcript::live::{LiveTranscript, Update};

const BLOCK: usize = 320; // 20 ms

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--store-key") {
        let key = std::env::var("DEEPGRAM_API_KEY").context("set DEEPGRAM_API_KEY in the environment first")?;
        cluely_rs::secrets::set(deepgram::PROVIDER_ID, &key)?;
        println!("Deepgram key stored in {} (service CluelyRS, id {}).", cluely_rs::secrets::STORE_NAME, deepgram::PROVIDER_ID);
        return Ok(());
    }
    let Some(wav) = args.first() else { bail!("usage: transcript_replay <wav> [--source me|them]") };
    let source = match args.iter().position(|a| a == "--source").and_then(|i| args.get(i + 1)).map(String::as_str) {
        Some("me") => Source::Me, _ => Source::Them,
    };
    let provider: Arc<dyn StreamingAsr> = match args.iter().position(|a| a == "--provider").and_then(|i| args.get(i + 1)).map(String::as_str) {
        Some("deepgram") => match std::env::var("DEEPGRAM_API_KEY") {
            Ok(key) => Arc::new(deepgram::Deepgram::with_key(key)),
            Err(_) => Arc::new(deepgram::Deepgram::from_store()),
        },
        _ => Arc::new(ParakeetRealtime::default()),
    };
    if let Some(reason) = listening::not_ready(&provider.availability()) { bail!("{reason}"); }
    let pcm = read_wav(wav)?;
    println!("audio: {:.1}s as {} via {}", pcm.len() as f64 / 16000.0, source.label(), provider.capabilities().label);
    println!("\nspeech in the audio (RMS over 20 ms blocks above 0.01, gaps under 300 ms joined):");
    for (start, end) in speech_segments(&pcm) { println!("  {start:7.0} – {end:7.0} ms"); }

    let (chunks, audio) = channel();
    let (_commands, inbox) = channel::<listening::Command>();
    let (out, mut messages) = futures::channel::mpsc::unbounded();
    let live = LiveTranscript::new(EndpointConfig::default(), None);
    let worker = std::thread::spawn(move || listening::run(provider, audio, &[source], Vec::new(), live, &inbox, &out, None));
    let feeder = std::thread::spawn(move || {
        let start = Instant::now();
        for (b, block) in pcm.chunks(BLOCK).enumerate() {
            let due = start + Duration::from_secs_f64((b + 1) as f64 * BLOCK as f64 / 16000.0);
            if let Some(wait) = due.checked_duration_since(Instant::now()) { std::thread::sleep(wait); }
            if chunks.send(AudioChunk { source, start_ms: (b * BLOCK) as f64 / 16.0, samples: block.to_vec() }).is_err() { break; }
        }
        // Two seconds of silence so the last utterance can endpoint, then the channel closes.
        for b in 0..100 {
            std::thread::sleep(Duration::from_millis(20));
            let start_ms = (pcm.len() + b * BLOCK) as f64 / 16.0;
            if chunks.send(AudioChunk { source, start_ms, samples: vec![0.0; BLOCK] }).is_err() { break; }
        }
    });
    let start = Instant::now();
    println!("\ntimeline (wall ms since the first audio block ≈ audio ms fed so far):");
    while let Some(message) = futures::executor::block_on(messages.next()) {
        let wall = start.elapsed().as_secs_f64() * 1000.0;
        match message {
            Message::Status(Status::Failed(reason)) => { println!("  failed: {reason}"); break; }
            Message::Status(status) => println!("  {wall:7.0}  status {status:?}"),
            Message::Transcript(Update::Provisional { stable, unstable, .. }) => println!("  {wall:7.0}  partial   [{stable}] {unstable}"),
            Message::Transcript(Update::QuestionLikely { score, .. }) => println!("  {wall:7.0}  question? {score:.2}"),
            Message::Transcript(Update::Committed { utterance, reason }) =>
                println!("  {wall:7.0}  COMMIT {reason:?} ({:.0}–{:.0} ms): {}", utterance.start_ms, utterance.end_ms, utterance.text),
            Message::Transcript(Update::Error { message, .. }) => println!("  {wall:7.0}  error {message}"),
        }
    }
    feeder.join().ok();
    worker.join().ok();
    Ok(())
}

/// (start, end) in audio ms of each stretch of sound, with short gaps bridged.
fn speech_segments(pcm: &[f32]) -> Vec<(f64, f64)> {
    let mut segments: Vec<(f64, f64)> = Vec::new();
    for (i, block) in pcm.chunks(BLOCK).enumerate() {
        let rms = (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt();
        if rms < 0.01 { continue; }
        let (start, end) = ((i * BLOCK) as f64 / 16.0, ((i + 1) * BLOCK) as f64 / 16.0);
        match segments.last_mut() {
            Some(last) if start - last.1 < 300.0 => last.1 = end,
            _ => segments.push((start, end)),
        }
    }
    segments
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
