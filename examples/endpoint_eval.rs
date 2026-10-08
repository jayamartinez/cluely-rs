//! Evaluate endpointing on recorded sessions. Replays every capture in the metrics folder
//! (`live-*-them.wav` as Them, `live-*-me.wav` as Me) and the bench clip through Parakeet and
//! the real endpointer on the audio clock (`transcript::replay`), and prints per file: commits,
//! fragments (≤ 3 words, or cut mid-sentence: ending on a dangling word or continued by the
//! next line), merged questions, commit latency after the speech
//! stopped (median / p95) and the reason mix.
//!
//!   cargo run --example endpoint_eval -- [options] [wav...]
//!
//!   --bench <wav>         the clip from dev/make-bench-audio.ps1 (default %TEMP%\cluelyrs-bench.wav)
//!   --config "name k=v…"  endpoint with these settings (repeatable; a bare name = defaults). Keys:
//!                         after_eou, eou_unfinished, finished, max_silence, discount, text_lag
//!   --lag <ms>            simulate a recognizer this far behind the audio
//!   --stall <a>-<b>       simulate CPU starvation: the recognizer stops at a seconds, resumes at b,
//!                         then catches up at twice real time
//!   --slow <a>-<b>[@x]    the same, but from a to b it still runs at x times real time (default
//!                         0.1), so it keeps delivering old words while it falls behind
//!   --show <text>         list the commits of files whose name contains <text>
//!   --threads <n>         recognize this many files at once (default 2)
//!
//! Recognition doesn't depend on endpointing, so each file's Parakeet events are cached under
//! %TEMP%\cluelyrs-endpoint-eval (keyed by name, size and modification time). Makes no paid calls.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, bail};
use cluely_rs::audio::Source;
use cluely_rs::stt::parakeet::ParakeetRealtime;
use cluely_rs::stt::{EventKind, Generation, StreamingAsr, TranscriptEvent};
use cluely_rs::transcript::endpoint::{EndpointConfig, Reason};
use cluely_rs::transcript::replay::{self, Recognized, Scores, Timeline};
use serde_json::{Value, json};

struct Config { name: String, endpoint: EndpointConfig, text_lag: Option<f64> }

struct File { path: PathBuf, source: Source, pcm: Vec<f32>, segments: Vec<(f64, f64)>, recognized: Recognized }

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let values = |flag: &str| args.windows(2).filter(|w| w[0] == flag).map(|w| w[1].clone()).collect::<Vec<_>>();
    let mut configs: Vec<Config> = values("--config").iter().map(|spec| parse_config(spec)).collect::<anyhow::Result<_>>()?;
    if configs.is_empty() { configs.push(parse_config("defaults")?); }
    let lag_ms: f64 = value("--lag").map(|v| v.parse()).transpose()?.unwrap_or(0.0);
    let window = |flag: &str, speed: f64| value(flag).map(|v| -> anyhow::Result<(f64, f64, f64)> {
        let (range, speed) = match v.split_once('@') { Some((range, speed)) => (range, speed.parse()?), None => (v.as_str(), speed) };
        let (a, b) = range.split_once('-').with_context(|| format!("{flag} a-b"))?;
        Ok((a.parse::<f64>()? * 1000.0, b.parse::<f64>()? * 1000.0, speed))
    }).transpose();
    let stall = window("--stall", 0.0)?.or(window("--slow", 0.1)?);
    let lag = move |at: f64| lag_ms + stall.map(|(a, b, speed)| stall_lag(at, a, b, speed)).unwrap_or(0.0);
    let threads: usize = value("--threads").map(|v| v.parse()).transpose()?.unwrap_or(2);

    let mut paths: Vec<(PathBuf, Source)> = Vec::new();
    let flagged: Vec<usize> = args.iter().enumerate().filter(|(_, a)| a.starts_with("--")).map(|(i, _)| i + 1).collect();
    let positional: Vec<&String> = args.iter().enumerate().filter(|(i, a)| !a.starts_with("--") && !flagged.contains(i)).map(|(_, a)| a).collect();
    if positional.is_empty() {
        let dir = cluely_rs::metrics::metrics_dir().context("no local data folder")?;
        let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "wav")).collect();
        found.sort();
        for path in found {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            // Captures under a second hold nothing to endpoint.
            if std::fs::metadata(&path)?.len() < 32_044 { continue; }
            paths.push((path, if name.ends_with("-me.wav") { Source::Me } else { Source::Them }));
        }
        let bench = value("--bench").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("cluelyrs-bench.wav"));
        if bench.exists() { paths.push((bench, Source::Them)); } else { eprintln!("no bench clip at {} (dev/make-bench-audio.ps1)", bench.display()); }
    } else {
        for path in positional { paths.push((PathBuf::from(path), if path.ends_with("-me.wav") { Source::Me } else { Source::Them })); }
    }

    let provider = ParakeetRealtime::default();
    if let Some(reason) = cluely_rs::listening::not_ready(&provider.availability()) { bail!("{reason}"); }
    let queue = Mutex::new(paths.into_iter().enumerate().collect::<Vec<_>>());
    let done = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| loop {
                let Some((index, (path, source))) = queue.lock().unwrap().pop() else { break };
                match load(&provider, &path, source) {
                    Ok(file) => done.lock().unwrap().push((index, file)),
                    Err(error) => eprintln!("{}: {error:#}", path.display()),
                }
            });
        }
    });
    let mut files = done.into_inner().unwrap();
    files.sort_by_key(|(index, _)| *index);
    let files: Vec<File> = files.into_iter().map(|(_, file)| file).collect();

    let show = value("--show");
    for config in &configs {
        println!("\n== {} ({:?}, text lag {})", config.name, config.endpoint,
            config.text_lag.map(|ms| format!("{ms} ms")).unwrap_or("provider default".into()));
        println!("{:<26} {:>4} {:>6} {:>7} {:>7} {:>6} {:>6} {:>5}  {:<13}", "file", "src", "commit", "frag%", "merged", "p50", "p95", "speak", "eou/sil/max/stop");
        let mut total = Scores::default();
        for file in &files {
            let timeline = replay::endpoint(&file.recognized, &file.pcm, config.endpoint, config.text_lag, &lag);
            let scores = replay::score(&timeline, &file.segments);
            let name = file.path.file_name().unwrap().to_string_lossy().replace(".wav", "");
            let speech: f64 = file.segments.iter().map(|(a, b)| b - a).sum::<f64>() / 1000.0;
            println!("{:<26} {:>4} {:>6} {:>6.0}% {:>7} {:>6} {:>6} {:>4.0}s  {}", name, file.source.label(), scores.commits,
                scores.fragment_rate() * 100.0, scores.merged_questions, ms(scores.latency_percentile(0.5)), ms(scores.latency_percentile(0.95)),
                speech, reasons(&scores));
            if show.as_ref().is_some_and(|s| name.contains(s.as_str())) { print_commits(&timeline, &replay::judge(&timeline, &file.segments)); }
            total.add(&scores);
        }
        println!("{:<26} {:>4} {:>6} {:>6.0}% {:>7} {:>6} {:>6} {:>5}  {}   (short {}, mid-sentence {})", "TOTAL", "", total.commits,
            total.fragment_rate() * 100.0, total.merged_questions, ms(total.latency_percentile(0.5)), ms(total.latency_percentile(0.95)), "",
            reasons(&total), total.short, total.mid_sentence);
    }
    Ok(())
}

/// How late the recognizer finishes audio from `at` when from `a` to `b` it runs at `speed`
/// times real time (0 = stopped), then catches up at twice real time.
fn stall_lag(at: f64, a: f64, b: f64, speed: f64) -> f64 {
    // Where the recognizer has got to when it starts catching up, and when it is level again.
    let behind_at_b = a + speed * (b - a);
    let level = 2.0 * b - behind_at_b;
    let done = if at < a || at >= level { at } else if at < behind_at_b { a + (at - a) / speed } else { b + (at - behind_at_b) / 2.0 };
    done - at
}

fn ms(value: Option<f64>) -> String { value.map(|v| format!("{v:.0}")).unwrap_or("-".into()) }

fn reasons(scores: &Scores) -> String { format!("{}/{}/{}/{}", scores.reasons[0], scores.reasons[1], scores.reasons[2], scores.reasons[3]) }

fn print_commits(timeline: &Timeline, verdicts: &[replay::Verdict]) {
    for (commit, verdict) in timeline.commits.iter().zip(verdicts) {
        let flags = format!("{}{}{}", if verdict.short { "S" } else { "" }, if verdict.mid_sentence { "C" } else { "" }, if verdict.merged { "M" } else { "" });
        let reason = match commit.reason { Reason::EndOfUtterance => "eou", Reason::Silence => "sil", Reason::MaxSilence => "max", Reason::Stopped => "stop" };
        let latency = verdict.latency_ms.map(|ms| format!("+{ms:.0}")).unwrap_or_default();
        println!("    {:>8.0} {:>4} {:>3} {:>6} [{:>7.0}–{:>7.0}] {}", commit.at_ms, reason, flags, latency, commit.utterance.start_ms, commit.utterance.end_ms, commit.utterance.text);
    }
}

fn parse_config(spec: &str) -> anyhow::Result<Config> {
    let mut parts = spec.split_whitespace();
    let name = parts.next().context("empty --config")?.to_string();
    let mut config = Config { name, endpoint: EndpointConfig::default(), text_lag: None };
    for part in parts {
        let (key, v) = part.split_once('=').with_context(|| format!("{part}: expected key=value"))?;
        let v: f64 = v.parse().with_context(|| format!("{part}: not a number"))?;
        let e = &mut config.endpoint;
        match key {
            "after_eou" => e.after_eou_ms = v,
            "eou_unfinished" => e.eou_unfinished_ms = v,
            "finished" => e.finished_ms = v,
            "max_silence" => e.max_silence_ms = v,
            "discount" => e.question_discount = v,
            "text_lag" => config.text_lag = Some(v),
            other => bail!("unknown key {other}"),
        }
    }
    Ok(config)
}

fn load(provider: &ParakeetRealtime, path: &Path, source: Source) -> anyhow::Result<File> {
    let pcm = read_wav(path)?;
    let segments = replay::speech_segments(&pcm);
    let meta = std::fs::metadata(path)?;
    let stamp = meta.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let cache_dir = std::env::temp_dir().join("cluelyrs-endpoint-eval");
    let cache = cache_dir.join(format!("{}-{}-{}.json", path.file_stem().unwrap().to_string_lossy(), meta.len(), stamp));
    let recognized = match std::fs::read_to_string(&cache).ok().and_then(|text| from_json(&text, source)) {
        Some(recognized) => recognized,
        None => {
            let started = std::time::Instant::now();
            let recognized = replay::recognize(provider, source, &pcm).map_err(|e| anyhow::anyhow!("{e}"))?;
            eprintln!("recognized {} ({:.0} s of audio) in {:.0} s", path.display(), pcm.len() as f64 / 16000.0, started.elapsed().as_secs_f64());
            std::fs::create_dir_all(&cache_dir)?;
            std::fs::write(&cache, to_json(&recognized))?;
            recognized
        }
    };
    Ok(File { path: path.to_path_buf(), source, pcm, segments, recognized })
}

fn to_json(recognized: &Recognized) -> String {
    let events: Vec<Value> = recognized.events.iter().map(|(at, e)| {
        let (kind, text, stable) = match &e.kind {
            EventKind::Partial { text, stable_hint } => ("partial", text.clone(), stable_hint.map(|s| s as u64)),
            EventKind::EndOfUtterance { text } => ("eou", text.clone(), None),
            EventKind::Final { text } => ("final", text.clone(), None),
            EventKind::Error { message, .. } => ("error", message.clone(), None),
        };
        json!({ "at": if at.is_finite() { json!(at) } else { Value::Null }, "start": e.start_ms, "end": e.end_ms, "kind": kind, "text": text, "stable": stable })
    }).collect();
    json!({ "provider": recognized.provider, "text_lag_ms": recognized.text_lag_ms, "events": events }).to_string()
}

fn from_json(text: &str, source: Source) -> Option<Recognized> {
    let value: Value = serde_json::from_str(text).ok()?;
    let provider = match value["provider"].as_str()? { "parakeet-realtime" => "parakeet-realtime", _ => return None };
    let events = value["events"].as_array()?.iter().map(|e| {
        let text = e["text"].as_str()?.to_string();
        let kind = match e["kind"].as_str()? {
            "partial" => EventKind::Partial { text, stable_hint: e["stable"].as_u64().map(|s| s as usize) },
            "eou" => EventKind::EndOfUtterance { text },
            "final" => EventKind::Final { text },
            _ => EventKind::Error { message: text, fatal: true },
        };
        let event = TranscriptEvent { source, generation: Generation(1), start_ms: e["start"].as_f64()?, end_ms: e["end"].as_f64()?, kind };
        Some((e["at"].as_f64().unwrap_or(f64::INFINITY), event))
    }).collect::<Option<Vec<_>>>()?;
    Some(Recognized { source, provider, text_lag_ms: value["text_lag_ms"].as_f64()?, events })
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("couldn't read {}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" { bail!("not a WAV file"); }
    let (mut at, mut format_ok) = (12, false);
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        match &bytes[at..at + 4] {
            b"fmt " => {
                let (format, channels, rate, bits) = (u16::from_le_bytes([body[0], body[1]]), u16::from_le_bytes([body[2], body[3]]),
                    u32::from_le_bytes(body[4..8].try_into()?), u16::from_le_bytes([body[14], body[15]]));
                if (format, channels, rate, bits) != (1, 1, 16000, 16) { bail!("must be 16 kHz mono 16-bit PCM"); }
                format_ok = true;
            }
            // A capture cut short (the app closed mid-session) has a zero size; read what's there.
            b"data" if format_ok => {
                let body = if size == 0 { &bytes[at + 8..] } else { body };
                return Ok(body.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0).collect());
            }
            _ => {}
        }
        at += 8 + size + (size & 1);
    }
    bail!("no audio data")
}
