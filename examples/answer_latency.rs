//! Time one Assist-shaped answer through the same path the overlay uses: a ReasoningSession
//! with the selected provider (your saved settings), a realistic conversation and a 1600 px
//! synthetic screenshot (no real screen is captured or sent). Prints the time to the first
//! streamed words and to the full answer.
//!
//!   cargo run --example answer_latency            # as the app would answer now
//!   cargo run --example answer_latency -- --warm  # after Live start's preparation (prewarm)
//!   cargo run --example answer_latency -- --smart # Smart mode
//!   cargo run --example answer_latency -- --provider claude   # override the saved provider (claude|codex)
//!   cargo run --example answer_latency -- --provider claude --model opus   # opus|sonnet|haiku, or a Codex model id / "default"
//!   cargo run --example answer_latency -- --list   # the Codex model list and default (no prompt sent)
//!   cargo run --example answer_latency -- --speculate 1500   # a speculative answer starts when the
//!       question is committed and Assist is pressed 1500 ms later: times press → first visible words
//!       and the speculative answer's own start → first words
//!   cargo run --example answer_latency -- --speculate 1500 --after-answer   # as in the app: one answer
//!       on the session's thread first, then the speculative one on its own thread (TWO requests)
//!   cargo run --example answer_latency -- --long   # a 55-minute meeting whose budget was said at
//!       03:20: asks about it while the rolling summary is being written, then again once it is in
//!       (THREE requests: two answers and the summary)
//!   cargo run --example answer_latency -- --long --no-summary   # the same question without the summary
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
    if args.iter().any(|a| a == "--list") {
        // The account's model list and default, as the pickers see them. Sends no prompt.
        let status = CodexClient::new().status();
        println!("default: {:?}", status.default_model);
        for (id, name) in &status.models { println!("{id}\t{name}"); }
        return;
    }
    if args.iter().any(|a| a == "--smart") { settings.smart_mode = true; }
    match args.iter().position(|a| a == "--provider").and_then(|i| args.get(i + 1)).map(String::as_str) {
        Some("claude") => settings.provider = cluely_rs::settings::Provider::Claude,
        Some("codex") => settings.provider = cluely_rs::settings::Provider::Codex,
        Some(other) => panic!("unknown provider {other} (claude or codex)"),
        None => {}
    }
    if let Some(model) = args.iter().position(|a| a == "--model").and_then(|i| args.get(i + 1)) {
        use cluely_rs::settings::{ClaudeModel, Provider};
        match settings.provider {
            Provider::Claude => settings.claude_model = ClaudeModel::new(model)
                .unwrap_or_else(|| panic!("unknown Claude model {model} (an alias such as opus, or a model id)")),
            Provider::Codex => settings.codex_model = if model == "default" { String::new() } else { model.clone() },
            Provider::ApiKey => { settings.api_models.insert(settings.api_provider.clone(), model.clone()); }
        }
    }
    let recorder = cluely_rs::metrics::LatencyRecorder::new(Instant::now());
    let mut session = ReasoningSession::new(CodexClient::new(), Some(recorder.clone()));
    if args.iter().any(|a| a == "--warm") {
        prewarm(&session, &settings);
    }
    if args.iter().any(|a| a == "--long") {
        long_meeting(&mut session, &settings, !args.iter().any(|a| a == "--no-summary"));
        return;
    }
    let conversation = Conversation {
        lines: vec![
            Line { source: Source::Them, at_ms: 61_000, text: "thanks for joining, so let's get into the system design part".into() },
            Line { source: Source::Me, at_ms: 66_500, text: "sounds good".into() },
            Line { source: Source::Them, at_ms: 70_200, text: "so how would you design a rate limiter for a public api that gets bursty traffic".into() },
        ],
        now: vec![Now { source: Source::Them, text: "and where would you keep the counters".into() }],
        earlier: None,
    };
    let request = Request {
        action: "Assist".into(), question: String::new(), history: Vec::new(),
        query: conversation.query(), conversation: conversation.render(true), screenshot: Some(screenshot()), attachments: Default::default(), utterance: None,
    };
    let speculate = args.iter().position(|a| a == "--speculate").and_then(|i| args.get(i + 1)).map(|ms| ms.parse::<u64>().expect("--speculate <ms>"));
    let (started, mut replies, mut first, mut text) = match speculate {
        None => {
            let started = Instant::now();
            let (_, replies) = session.ask(&settings, request).unwrap_or_else(|error| panic!("couldn't start: {error}"));
            (started, replies, None, String::new())
        }
        Some(press_after) => {
            // The question was just committed: speculation starts on its own prepared thread or
            // process. The user presses Assist `press_after` ms later and the answer is claimed.
            prewarm_speculation(&session, &settings);
            if args.iter().any(|a| a == "--after-answer") { answer_first(&mut session, &settings, &request); }
            let committed = Instant::now();
            session.speculate(&settings, request, 1).unwrap_or_else(|error| panic!("couldn't start: {error}"));
            std::thread::sleep(Duration::from_millis(press_after));
            let pressed = Instant::now();
            let claimed = session.claim(1).expect("same context");
            println!("pressed {:.2} s after the commit; {} words were ready", (pressed - committed).as_secs_f64(), claimed.text.split_whitespace().count());
            let first = (!claimed.text.is_empty()).then(|| pressed.elapsed());
            if let Some(result) = claimed.done {
                // Finished before the press: all of it shows at once.
                match result {
                    Ok(answer) => println!("first visible words: {:.3} s after pressing (finished before the press), {} words
---
{}",
                        first.unwrap_or_default().as_secs_f64(), answer.split_whitespace().count(), answer.trim()),
                    Err(error) => println!("speculative answer failed: {error}"),
                }
                print_speculation_first_words(&recorder);
                return;
            }
            (pressed, claimed.replies, first, claimed.text)
        }
    };
    while let Some(reply) = futures::executor::block_on(replies.next()) {
        match reply.event {
            Event::Delta(delta) => { first.get_or_insert(started.elapsed()); text.push_str(&delta); }
            Event::Done(result) => {
                let total = started.elapsed();
                match result {
                    Ok(answer) => println!("first words: {:.2} s, full answer: {:.2} s, {} words\n---\n{}",
                        first.unwrap_or(total).as_secs_f64(), total.as_secs_f64(), answer.split_whitespace().count(), answer.trim()),
                    Err(error) => println!("failed after {:.2} s: {error}", total.as_secs_f64()),
                }
                print_speculation_first_words(&recorder);
                for (alias, id) in cluely_rs::claude_cli::resolved_models() {
                    println!("claude model: {alias} -> {id} ({})", cluely_rs::claude_cli::model_label(&id));
                }
                if settings.provider == cluely_rs::settings::Provider::Codex {
                    println!("codex model: {}", if settings.codex_model.is_empty() { "default" } else { &settings.codex_model });
                }
                break;
            }
        }
    }
}

/// A line every 20 s for 55 minutes, filler but for the budget at 03:20.
fn meeting() -> Vec<Line> {
    (0..165u64).map(|i| {
        let source = if i.is_multiple_of(2) { Source::Them } else { Source::Me };
        let text = match i {
            10 => "before we start, the budget for the pilot is 42,000 dollars, and it can't go over that".to_string(),
            _ if source == Source::Them => format!("okay, next point on the agenda, item {i}, let's keep moving"),
            _ => format!("sure, item {i} sounds fine"),
        };
        Line { source, at_ms: i * 20_000, text }
    }).collect()
}

/// The budget question at minute 55, first while the rolling summary is being written (the
/// answer doesn't wait for it), then once it is in. Without the summary: the question once, with
/// the recent lines only (as before the rolling summary existed).
fn long_meeting(session: &mut ReasoningSession, settings: &cluely_rs::settings::Settings, summarize: bool) {
    let settings = &cluely_rs::settings::Settings { rolling_summary: summarize, ..settings.clone() };
    let lines = meeting();
    session.roll(settings, &lines);
    let summary_started = Instant::now();
    ask_timed(session, settings, &lines, if summarize { "while the summary is written" } else { "without the summary" });
    if !summarize { return; }
    while session.earlier(settings).is_some_and(|earlier| earlier.through_ms.is_none()) {
        assert!(summary_started.elapsed() < Duration::from_secs(180), "no summary after 3 minutes");
        std::thread::sleep(Duration::from_millis(50));
    }
    let earlier = session.earlier(settings).unwrap_or_default();
    println!("summary in after {:.2} s ({} bytes, through {} ms)\n---\n{}\n---", summary_started.elapsed().as_secs_f64(), earlier.summary.len(),
        earlier.through_ms.unwrap_or(0), earlier.summary);
    ask_timed(session, settings, &lines, "with the summary");
}

fn ask_timed(session: &mut ReasoningSession, settings: &cluely_rs::settings::Settings, lines: &[Line], label: &str) {
    let conversation = Conversation { lines: lines.to_vec(), now: Vec::new(), earlier: session.earlier(settings) }.render(true);
    let request = Request { action: "Ask".into(), question: "What budget did they mention for the pilot?".into(), history: Vec::new(),
        conversation: conversation.clone(), query: Vec::new(), screenshot: Some(screenshot()), attachments: Default::default(), utterance: None };
    let started = Instant::now();
    let (_, mut replies) = session.ask(settings, request).unwrap_or_else(|error| panic!("couldn't start: {error}"));
    let mut first = None;
    while let Some(reply) = futures::executor::block_on(replies.next()) {
        match reply.event {
            Event::Delta(_) => { first.get_or_insert(started.elapsed()); }
            Event::Done(result) => {
                let total = started.elapsed();
                println!("{label}: conversation {} bytes, first words {:.2} s, full answer {:.2} s\n---\n{}\n---", conversation.len(),
                    first.unwrap_or(total).as_secs_f64(), total.as_secs_f64(), result.unwrap_or_else(|error| format!("failed: {error}")).trim());
                return;
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
    println!("prepared in {:.2} s (Codex: app-server, model list, thread; Claude: process started)", started.elapsed().as_secs_f64());
    // Live has been on for a moment before anyone presses Assist; a prepared Claude process
    // finishes booting in that time.
    std::thread::sleep(Duration::from_secs(2));
}

/// The speculation's own thread or process, prepared as when a question is first heard.
/// One answer on the session's own thread, as a Live session would have had before.
fn answer_first(session: &mut ReasoningSession, settings: &cluely_rs::settings::Settings, request: &Request) {
    let earlier = Request { action: request.action.clone(), question: request.question.clone(), history: Vec::new(),
        conversation: request.conversation.clone(), query: request.query.clone(), screenshot: request.screenshot.clone(), attachments: Default::default(), utterance: None };
    let started = Instant::now();
    let (_, mut replies) = session.ask(settings, earlier).unwrap_or_else(|error| panic!("couldn't start: {error}"));
    let mut first = None;
    while let Some(reply) = futures::executor::block_on(replies.next()) {
        match reply.event {
            Event::Delta(_) => { first.get_or_insert(started.elapsed()); }
            Event::Done(result) => {
                println!("earlier answer on the session's thread: first words {:.2} s ({})", first.unwrap_or_default().as_secs_f64(),
                    if result.is_ok() { "done" } else { "failed" });
                break;
            }
        }
    }
}

/// The speculative answer's own time to first words, from its marks.
fn print_speculation_first_words(recorder: &cluely_rs::metrics::LatencyRecorder) {
    use cluely_rs::metrics::Stage;
    let marks = recorder.snapshot();
    let at = |stage| marks.iter().find(|m| m.stage == stage).map(|m| m.at_ms);
    if let (Some(started), Some(first)) = (at(Stage::SpeculationStarted), at(Stage::SpeculationFirstToken)) {
        println!("speculative answer: first words {:.2} s after it started", (first - started) / 1000.0);
    }
}

fn prewarm_speculation(session: &ReasoningSession, settings: &cluely_rs::settings::Settings) {
    session.prewarm_speculation(settings);
    std::thread::sleep(Duration::from_millis(100));
    session.wait_prepared();
    std::thread::sleep(Duration::from_secs(2));
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
