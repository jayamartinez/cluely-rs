//! Time one Assist-shaped answer through the same path the overlay uses: a ReasoningSession
//! with the selected provider (your saved settings), a realistic conversation and a 1600 px
//! synthetic screenshot (no real screen is captured or sent). Prints the time to the first
//! streamed words and to the full answer.
//!
//!   cargo run --example answer_latency            # as the app would answer now
//!   cargo run --example answer_latency -- --warm  # after Live start's preparation (prewarm)
//!   cargo run --example answer_latency -- --smart # Smart mode
//!
//! Every run makes ONE real request through your ChatGPT/Claude subscription or API key and
//! counts against its usage. Never run it automatically.

use std::time::{Duration, Instant};

use futures::StreamExt;
use image::{Rgb, RgbImage};

use cluely_rs::audio::Source;
use cluely_rs::codex::CodexClient;
use cluely_rs::reasoning::{Conversation, Line, Now, ReasoningSession, Request};
use cluely_rs::reasoning::session::Event;
use cluely_rs::settings::Store;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut settings = Store::load().value;
    if args.iter().any(|a| a == "--smart") { settings.smart_mode = true; }
    let mut session = ReasoningSession::new(CodexClient::new(), None);
    if args.iter().any(|a| a == "--warm") {
        prewarm(&session, &settings);
    }
    let conversation = Conversation {
        lines: vec![
            Line { source: Source::Them, at_ms: 61_000, text: "thanks for joining, so let's get into the system design part".into() },
            Line { source: Source::Me, at_ms: 66_500, text: "sounds good".into() },
            Line { source: Source::Them, at_ms: 70_200, text: "so how would you design a rate limiter for a public api that gets bursty traffic".into() },
        ],
        now: vec![Now { source: Source::Them, text: "and where would you keep the counters".into() }],
    };
    let request = Request {
        action: "Assist".into(), question: String::new(), history: Vec::new(),
        conversation: conversation.render(), screenshot: Some(screenshot()), utterance: None,
    };
    let started = Instant::now();
    let (_, mut replies) = session.ask(&settings, request).unwrap_or_else(|error| panic!("couldn't start: {error}"));
    let mut first: Option<Duration> = None;
    let mut text = String::new();
    while let Some(reply) = futures::executor::block_on(replies.next()) {
        match reply.event {
            Event::Delta(delta) => { first.get_or_insert(started.elapsed()); text.push_str(&delta); }
            Event::Done(result) => {
                let total = started.elapsed();
                match result {
                    Ok(answer) => println!("first words: {:.2} s, full answer: {:.2} s, {} chars\n---\n{}",
                        first.unwrap_or(total).as_secs_f64(), total.as_secs_f64(), answer.chars().count(), answer.trim()),
                    Err(error) => println!("failed after {:.2} s: {error}", total.as_secs_f64()),
                }
                break;
            }
        }
    }
}

/// What Live start does before the first Assist; waits for it so the timing is the answer only.
fn prewarm(session: &ReasoningSession, settings: &cluely_rs::settings::Settings) {
    let started = Instant::now();
    session.prewarm(settings);
    // The preparation holds the session's thread lock until the thread is open.
    std::thread::sleep(Duration::from_millis(100));
    session.wait_prepared();
    println!("prepared in {:.2} s (app-server, model list, thread)", started.elapsed().as_secs_f64());
}

/// A 1600×900 screenshot-sized JPEG with enough detail to encode like a real screen.
fn screenshot() -> Vec<u8> {
    let mut image = RgbImage::new(1600, 900);
    let mut seed = 0x2545_f491u32;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
        let band = ((y / 18) % 2) as u8;
        let text = (x / 7 + y / 3) % 5 == 0 && band == 1;
        let base = 18 + (seed % 6) as u8;
        *pixel = if text { Rgb([200, 200, 190]) } else { Rgb([base, base + 2, base + 4]) };
    }
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 80).encode_image(&image).unwrap();
    bytes
}
