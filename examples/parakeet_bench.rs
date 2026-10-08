//! Parakeet Realtime benchmarks. Make input audio with `dev/make-bench-audio.ps1` (or use any
//! 16 kHz mono 16-bit WAV of speech with pauses), then:
//!
//!   cargo run --example parakeet_bench -- download
//!       Install the model into the CluelyRS models folder (pinned revision, verified).
//!   cargo run --example parakeet_bench -- <wav> sweep [--threads 2,4,6,8]
//!       Decode speed per thread count, for one stream and for two concurrent streams.
//!   cargo run --example parakeet_bench -- <wav> long [--reps 40] [--threads 4]
//!       Long-running reliability: the audio repeated, through one raw parakeet.cpp stream
//!       and through the provider (which replaces streams after each utterance).
//!   cargo run --example parakeet_bench -- <wav> realtime [--seconds 120] [--threads 4]
//!       Me and Them sessions fed at real-time pace: end-of-utterance latency, CPU, memory.
//!   cargo run --example parakeet_bench -- <wav> switch
//!       macOS: decode on the GPU, switch to the CPU and back (as Settings › Listening › Use GPU
//!       does between Live sessions), reporting the speed each time.
//!
//! Uses the installed model (`--model <gguf>` to override). Prints results; writes nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use cluely_rs::audio::{AudioChunk, Source};
use cluely_rs::stt::parakeet::{self, ParakeetConfig, ParakeetRealtime, ffi};
use cluely_rs::stt::{EventKind, EventSink, Generation, StreamingAsr};

const BLOCK: usize = 320; // 20 ms, the size capture delivers

fn main() -> anyhow::Result<()> {
    let result = run();
    // Every model is gone once run returns; free parakeet.cpp's backend before exit (PATCHES.md).
    parakeet::shutdown();
    result
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("download") { return download(); }
    let (Some(wav), Some(mode)) = (args.first(), args.get(1)) else {
        bail!("usage: parakeet_bench <wav> sweep|long|realtime|switch [--threads N,..] [--reps N] [--seconds N] [--model PATH]");
    };
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let model = flag("--model").map(PathBuf::from).or_else(|| parakeet::MODEL.path()).context("no model path")?;
    if !model.exists() { bail!("model not found at {} (download it first, or pass --model)", model.display()); }
    let threads: Vec<usize> = flag("--threads").unwrap_or_else(|| parakeet::DEFAULT_THREADS.to_string())
        .split(',').map(|t| t.trim().parse()).collect::<Result<_, _>>().context("--threads")?;
    let pcm = read_wav(wav)?;
    println!("audio: {:.1}s  model: {}  cpus: {}", pcm.len() as f64 / 16000.0, model.display(),
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0));
    match mode.as_str() {
        "sweep" => sweep(&model, &pcm, if flag("--threads").is_some() { &threads } else { &[2, 4, 6, 8] }),
        "long" => long(&model, &pcm, threads[0], flag("--reps").map(|r| r.parse()).transpose()?.unwrap_or(40)),
        "realtime" => realtime(&model, &pcm, threads[0], flag("--seconds").map(|s| s.parse()).transpose()?.unwrap_or(120.0)),
        "switch" => switch(&model, &pcm, threads[0]),
        other => bail!("unknown mode {other}"),
    }
}

/// Real-time factor (processing time / audio time) and per-block cost for 1 and 2 streams.
fn sweep(model_path: &Path, pcm: &[f32], threads: &[usize]) -> anyhow::Result<()> {
    println!("\nthreads | 1 stream: RTF  p95 block | 2 streams: RTF (each)  p95 block");
    for &n in threads {
        ffi::set_threads(n);
        let model = Arc::new(ffi::Model::load(model_path, None).map_err(anyhow::Error::msg)?);
        decode_all(&model, pcm)?; // warm up
        let (rtf1, p95_1) = decode_all(&model, pcm)?;
        let runs: Vec<_> = (0..2).map(|_| { let model = Arc::clone(&model); let pcm = pcm.to_vec();
            std::thread::spawn(move || decode_all(&model, &pcm)) }).collect();
        // Join both before looking at errors, so no stream outlives this function.
        let results: Vec<(f64, f64)> = runs.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>().into_iter().collect::<anyhow::Result<_>>()?;
        let rtf2 = results.iter().map(|r| r.0).fold(0.0, f64::max);
        let p95_2 = results.iter().map(|r| r.1).fold(0.0, f64::max);
        println!("{n:7} | {rtf1:13.3}  {p95_1:7.1} ms | {rtf2:21.3}  {p95_2:7.1} ms");
    }
    Ok(())
}

fn decode_all(model: &Arc<ffi::Model>, pcm: &[f32]) -> anyhow::Result<(f64, f64)> {
    let mut stream = model.begin_stream().map_err(anyhow::Error::msg)?;
    let mut blocks = Vec::new();
    let start = Instant::now();
    for block in pcm.chunks(BLOCK) {
        let t = Instant::now();
        stream.feed(block).map_err(anyhow::Error::msg)?;
        blocks.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let rtf = start.elapsed().as_secs_f64() / (pcm.len() as f64 / 16000.0);
    blocks.sort_by(f64::total_cmp);
    Ok((rtf, blocks[(blocks.len() * 95 / 100).min(blocks.len() - 1)]))
}

/// Loads through the provider with the GPU on, off and on again, each time after the previous
/// model is gone, and decodes the clip once per device.
fn switch(model_path: &Path, pcm: &[f32], threads: usize) -> anyhow::Result<()> {
    // parakeet.cpp logs "pk::Backend using device: ..." each time it creates a GPU backend.
    println!("\nUse GPU | RTF");
    for use_gpu in [true, false, true] {
        let provider = ParakeetRealtime::new(ParakeetConfig { model_path: Some(model_path.to_path_buf()), threads, use_gpu });
        let (tx, _rx) = mpsc::channel();
        let mut session = provider.start_session(EventSink::new(Source::Them, Generation(1), tx)).map_err(|e| anyhow::anyhow!("{e}"))?;
        let start = Instant::now();
        for (i, block) in pcm.chunks(BLOCK).enumerate() {
            session.push(&AudioChunk { source: Source::Them, start_ms: (i * BLOCK) as f64 / 16.0, samples: block.to_vec() }).map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        session.finish().map_err(|e| anyhow::anyhow!("{e}"))?;
        let rtf = start.elapsed().as_secs_f64() / (pcm.len() as f64 / 16000.0);
        let note = if use_gpu && parakeet::gpu_unavailable() { "  (GPU unavailable: ran on the CPU)" } else { "" };
        println!("{use_gpu:7} | {rtf:.3}{note}");
    }
    Ok(())
}

/// The same audio many times over: how many utterances does each approach still end?
fn long(model_path: &Path, pcm: &[f32], threads: usize, reps: usize) -> anyhow::Result<()> {
    ffi::set_threads(threads);
    let model = Arc::new(ffi::Model::load(model_path, None).map_err(anyhow::Error::msg)?);
    let gap = vec![0.0f32; 16000];
    let pass_ms = (pcm.len() + gap.len()) as f64 / 16.0;
    let long: Vec<f32> = (0..reps).flat_map(|_| pcm.iter().chain(&gap).copied()).collect();
    println!("{} passes, {:.1} min of audio", reps, long.len() as f64 / 16000.0 / 60.0);

    // Raw: one parakeet.cpp stream for the whole recording.
    let mut stream = model.begin_stream().map_err(anyhow::Error::msg)?;
    let mut raw_eous = vec![0usize; reps];
    let mut raw_words = vec![0usize; reps];
    for (i, block) in long.chunks(BLOCK).enumerate() {
        let out = stream.feed(block).map_err(anyhow::Error::msg)?;
        let pass = ((i * BLOCK) as f64 / 16.0 / pass_ms) as usize;
        raw_words[pass.min(reps - 1)] += out.text.split_whitespace().count();
        for event in out.events.iter().filter(|e| e.kind == "eou") { raw_eous[((event.time_sec * 1000.0 / pass_ms) as usize).min(reps - 1)] += 1; }
    }
    drop(stream);

    // Provider: what CluelyRS uses.
    let provider = ParakeetRealtime::new(ParakeetConfig { model_path: Some(model_path.to_path_buf()), threads, ..ParakeetConfig::default() });
    let (tx, rx) = mpsc::channel();
    let mut session = provider.start_session(EventSink::new(Source::Them, Generation(1), tx)).map_err(|e| anyhow::anyhow!("{e}"))?;
    for (i, block) in long.chunks(BLOCK).enumerate() {
        session.push(&AudioChunk { source: Source::Them, start_ms: (i * BLOCK) as f64 / 16.0, samples: block.to_vec() }).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    session.finish().map_err(|e| anyhow::anyhow!("{e}"))?;
    drop(session);
    let mut eous = vec![0usize; reps];
    let mut words = vec![0usize; reps];
    for event in rx.try_iter() {
        if let EventKind::EndOfUtterance { text } | EventKind::Final { text } = &event.kind {
            let pass = ((event.end_ms / pass_ms) as usize).min(reps - 1);
            if matches!(event.kind, EventKind::EndOfUtterance { .. }) { eous[pass] += 1; }
            words[pass] += text.split_whitespace().count();
        }
    }

    let expected = eous[0];
    let quarter = |v: &[usize], q: usize| -> usize { v[q * reps / 4..(q + 1) * reps / 4].iter().sum() };
    println!("\n            | EOUs (expected {})   | words per quarter of the recording", expected * reps);
    for (name, e, w) in [("raw stream", &raw_eous, &raw_words), ("provider", &eous, &words)] {
        println!("{name:11} | {:5} ({:5.1}%)       | {} {} {} {}", e.iter().sum::<usize>(),
            100.0 * e.iter().sum::<usize>() as f64 / (expected * reps).max(1) as f64, quarter(w, 0), quarter(w, 1), quarter(w, 2), quarter(w, 3));
    }
    println!("EOUs per pass: raw {raw_eous:?}
               provider {eous:?}");
    Ok(())
}

/// Me and Them at real-time pace; latency from the EOU point in the audio to its event.
fn realtime(model_path: &Path, pcm: &[f32], threads: usize, seconds: f64) -> anyhow::Result<()> {
    let provider = Arc::new(ParakeetRealtime::new(ParakeetConfig { model_path: Some(model_path.to_path_buf()), threads, ..ParakeetConfig::default() }));
    let (tx, rx) = mpsc::channel();
    let cpu_start = process_cpu_seconds();
    let start = Instant::now();
    let blocks = (seconds * 16000.0 / BLOCK as f64) as usize;
    let feeders: Vec<_> = [Source::Me, Source::Them].into_iter().enumerate().map(|(i, source)| {
        let (provider, tx) = (Arc::clone(&provider), tx.clone());
        // Offset Them by half the clip so the two sources don't speak in lockstep.
        let offset = i * pcm.len() / 2;
        let audio: Vec<f32> = pcm.iter().cycle().skip(offset).take(blocks * BLOCK).copied().collect();
        std::thread::spawn(move || -> anyhow::Result<f64> {
            let mut session = provider.start_session(EventSink::new(source, Generation(1), tx)).map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut late_ms: f64 = 0.0;
            for (b, block) in audio.chunks(BLOCK).enumerate() {
                let due = start + Duration::from_secs_f64((b + 1) as f64 * BLOCK as f64 / 16000.0);
                if let Some(wait) = due.checked_duration_since(Instant::now()) { std::thread::sleep(wait); }
                late_ms = late_ms.max(Instant::now().duration_since(due).as_secs_f64() * 1000.0);
                session.push(&AudioChunk { source, start_ms: (b * BLOCK) as f64 / 16.0, samples: block.to_vec() }).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            session.finish().map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(late_ms)
        })
    }).collect();
    drop(tx);
    let mut latencies: HashMap<Source, Vec<f64>> = HashMap::new();
    let mut peak_mb: f64 = 0.0;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => if let EventKind::EndOfUtterance { .. } = event.kind {
                latencies.entry(event.source).or_default().push(start.elapsed().as_secs_f64() * 1000.0 - event.end_ms);
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        peak_mb = peak_mb.max(working_set_mb());
    }
    let late: Vec<f64> = feeders.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>().into_iter().collect::<anyhow::Result<_>>()?;
    let wall = start.elapsed().as_secs_f64();
    println!("\n{seconds:.0}s of Me + Them at real-time pace, {threads} threads");
    for source in [Source::Me, Source::Them] {
        let mut l = latencies.remove(&source).unwrap_or_default();
        l.sort_by(f64::total_cmp);
        let pct = |p: usize| l.get((l.len() * p / 100).min(l.len().saturating_sub(1))).copied().unwrap_or(f64::NAN);
        println!("{:4}: {} EOUs, latency from EOU point p50 {:.0} ms, p95 {:.0} ms, max {:.0} ms", source.label(), l.len(), pct(50), pct(95), l.last().copied().unwrap_or(f64::NAN));
    }
    println!("CPU: {:.0}% of one core on average ({:.1}% of the machine)", 100.0 * (process_cpu_seconds() - cpu_start) / wall,
        100.0 * (process_cpu_seconds() - cpu_start) / wall / std::thread::available_parallelism().map(|n| n.get() as f64).unwrap_or(1.0));
    println!("peak working set while running: {peak_mb:.0} MB; worst feed lag behind real time: {:.1} ms", late.iter().copied().fold(0.0, f64::max));
    Ok(())
}

fn download() -> anyhow::Result<()> {
    let dest = parakeet::MODEL.path().context("no local data folder")?;
    if parakeet::MODEL.is_installed_at(&dest) { println!("already installed: {}", dest.display()); return Ok(()); }
    println!("downloading {} ({:.0} MB)
license: {}", parakeet::MODEL.name, parakeet::MODEL.bytes as f64 / 1e6, parakeet::MODEL.license_url);
    let mut shown = 0;
    parakeet::MODEL.download_to(&dest, &std::sync::atomic::AtomicBool::new(false), |got, total| {
        let pct = got * 100 / total;
        if pct >= shown + 10 { shown = pct; println!("  {pct}%"); }
    })?;
    println!("installed and verified: {}", dest.display());
    Ok(())
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

#[cfg(windows)]
fn process_cpu_seconds() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 1e7;
    match unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) } {
        Ok(()) => ticks(kernel) + ticks(user),
        Err(_) => f64::NAN,
    }
}

#[cfg(windows)]
fn working_set_mb() -> f64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut counters = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    match unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } {
        Ok(()) => counters.WorkingSetSize as f64 / 1_048_576.0,
        Err(_) => f64::NAN,
    }
}

#[cfg(target_os = "macos")]
fn process_cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 { return f64::NAN; }
    let usage = unsafe { usage.assume_init() };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

/// Resident memory, the macOS counterpart of the Windows working set.
#[cfg(target_os = "macos")]
fn working_set_mb() -> f64 {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = size_of::<libc::proc_taskinfo>() as libc::c_int;
    let written = unsafe { libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), size) };
    if written != size { return f64::NAN; }
    unsafe { info.assume_init() }.pti_resident_size as f64 / 1_048_576.0
}
