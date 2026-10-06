//! Device capture: the only platform-specific part of the audio pipeline.
//!
//! Each source gets its own thread that owns the cpal stream (streams are not `Send` on every
//! platform). The device callback only converts samples to f32 and pushes them into a lock-free
//! ring buffer; the same thread drains it every few milliseconds, normalizes, and sends
//! canonical chunks on. Nothing here ever runs on the UI thread.
//!
//! - `Source::Me` = default input device (microphone)
//! - `Source::Them` = default output device opened for input, which WASAPI turns into loopback

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};

use super::frame::{AudioChunk, Source};
use super::normalize::Normalizer;

const DRAIN_EVERY: Duration = Duration::from_millis(10);
/// Device audio buffered between the callback and the worker before samples are dropped.
const RING_SECONDS: usize = 2;
/// How far the device may lag the wall clock before a gap is treated as silence.
const GAP_SLACK_MS: f64 = 60.0;

/// What was opened for a source.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub source: Source,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
}

struct Shared {
    stop: AtomicBool,
    /// Latest chunk RMS per source (f32 bits), for level meters.
    levels: [AtomicU32; 2],
    /// Samples dropped because the worker fell behind, per source.
    dropped: [AtomicU64; 2],
}

pub struct AudioCapture {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    pub opened: Vec<SourceInfo>,
}

fn index(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

impl AudioCapture {
    /// Open the requested sources. Sources that fail are reported but don't stop the others;
    /// it's an error only when nothing could be opened.
    pub fn start(sources: &[Source], sink: Sender<AudioChunk>) -> anyhow::Result<(Self, Vec<(Source, String)>)> {
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), levels: Default::default(), dropped: Default::default() });
        let session_start = Instant::now();
        let mut capture = Self { shared, threads: Vec::new(), opened: Vec::new() };
        let mut failures = Vec::new();
        for &source in sources {
            let (ready_tx, ready_rx) = channel();
            let (shared, sink) = (capture.shared.clone(), sink.clone());
            let thread = std::thread::Builder::new().name(format!("cluelyrs-audio-{}", source.label()))
                .spawn(move || run_source(source, session_start, shared, sink, ready_tx))?;
            match ready_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(info)) => { capture.opened.push(info); capture.threads.push(thread); }
                Ok(Err(error)) => { failures.push((source, error)); let _ = thread.join(); }
                Err(_) => failures.push((source, "The audio device didn't respond.".into())),
            }
        }
        anyhow::ensure!(!capture.opened.is_empty(), "No audio source could be opened: {}",
            failures.iter().map(|(s, e)| format!("{}: {e}", s.label())).collect::<Vec<_>>().join("; "));
        Ok((capture, failures))
    }

    pub fn level(&self, source: Source) -> f32 { f32::from_bits(self.shared.levels[index(source)].load(Ordering::Relaxed)) }

    pub fn dropped_samples(&self, source: Source) -> u64 { self.shared.dropped[index(source)].load(Ordering::Relaxed) }

    pub fn stop(mut self) { self.shutdown(); }

    fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) { let _ = thread.join(); }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) { self.shutdown(); }
}

fn run_source(source: Source, session_start: Instant, shared: Arc<Shared>, sink: Sender<AudioChunk>,
    ready: Sender<Result<SourceInfo, String>>) {
    let (device, config) = match open_device(source) {
        Ok(found) => found,
        Err(error) => { let _ = ready.send(Err(error)); return; }
    };

    let rate = config.sample_rate();
    let channels = config.channels();
    let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(rate as usize * channels as usize * RING_SECONDS);
    let dropped = Arc::clone(&shared);
    let slot = index(source);
    let error_flag = Arc::new(AtomicBool::new(false));
    let errored = error_flag.clone();
    let stream = build_stream(&device, config.sample_format(), config.config(), move |samples: &mut dyn Iterator<Item = f32>| {
        for sample in samples {
            if producer.push(sample).is_err() { dropped.dropped[slot].fetch_add(1, Ordering::Relaxed); }
        }
    }, move || errored.store(true, Ordering::Relaxed));
    let stream = match stream.and_then(|s| s.play().map(|_| s).map_err(|e| e.to_string())) {
        Ok(stream) => stream,
        Err(error) => { let _ = ready.send(Err(error)); return; }
    };
    let _ = ready.send(Ok(SourceInfo { source, device: device.to_string(), sample_rate: rate, channels }));

    let mut normalizer: Option<Normalizer> = None;
    // Device frames fed to the normalizer, and when the first one was captured.
    let (mut fed_frames, mut origin_ms) = (0u64, 0.0f64);
    let mut block = Vec::with_capacity(rate as usize * channels as usize / 10);
    while !shared.stop.load(Ordering::Relaxed) && !error_flag.load(Ordering::Relaxed) {
        std::thread::sleep(DRAIN_EVERY);
        block.clear();
        while let Ok(sample) = consumer.pop() { block.push(sample); }
        let now_ms = session_start.elapsed().as_secs_f64() * 1000.0;
        if normalizer.is_none() {
            if block.is_empty() { continue; }
            // The first sample was captured roughly one buffer before we saw it.
            origin_ms = (now_ms - block.len() as f64 / channels as f64 * 1000.0 / rate as f64).max(0.0);
            match Normalizer::new(source, rate, channels, origin_ms) { Ok(n) => normalizer = Some(n), Err(_) => break }
        }
        let Some(normalizer) = normalizer.as_mut() else { break };
        if block.is_empty() {
            // WASAPI loopback delivers nothing while nothing plays. Feed real silence so the
            // timeline stays on the wall clock and endpointing sees the pause.
            let expected = ((now_ms - origin_ms - GAP_SLACK_MS) * rate as f64 / 1000.0).max(0.0) as u64;
            if expected <= fed_frames { continue; }
            block.resize((expected - fed_frames) as usize * channels as usize, 0.0);
        }
        fed_frames += (block.len() / channels as usize) as u64;
        match normalizer.push(&block) {
            Ok(Some(chunk)) => {
                shared.levels[slot].store(chunk.rms().to_bits(), Ordering::Relaxed);
                if sink.send(chunk).is_err() { break; }
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    drop(stream);
}

fn open_device(source: Source) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    let host = cpal::default_host();
    let (device, config) = match source {
        Source::Me => {
            let device = host.default_input_device().ok_or("No microphone is available.")?;
            let config = device.default_input_config();
            (device, config)
        }
        Source::Them => {
            let device = host.default_output_device().ok_or("No playback device is available for desktop audio.")?;
            let config = device.default_output_config();
            (device, config)
        }
    };
    Ok((device, config.map_err(|error| error.to_string())?))
}

/// Xruns (WASAPI loopback reports them around silence), reroutes and refused real-time
/// priority are recoverable; only losing the device or the stream ends capture.
fn is_fatal(error: &cpal::Error) -> bool {
    use cpal::ErrorKind::*;
    matches!(error.kind(), DeviceNotAvailable | HostUnavailable | StreamInvalidated | PermissionDenied)
}

type Push = dyn FnMut(&mut dyn Iterator<Item = f32>) + Send;

fn build_stream(device: &cpal::Device, format: SampleFormat, config: cpal::StreamConfig,
    push: impl FnMut(&mut dyn Iterator<Item = f32>) + Send + 'static, on_error: impl Fn() + Send + 'static) -> Result<cpal::Stream, String> {
    fn open<T: SizedSample + cpal::Sample>(device: &cpal::Device, config: cpal::StreamConfig, mut push: Box<Push>,
        on_error: impl Fn() + Send + 'static) -> Result<cpal::Stream, String>
    where f32: cpal::FromSample<T> {
        device.build_input_stream::<T, _, _>(config, move |data: &[T], _| push(&mut data.iter().map(|s| s.to_sample::<f32>())),
            move |error: cpal::Error| if is_fatal(&error) { on_error() }, None).map_err(|e| e.to_string())
    }
    let push: Box<Push> = Box::new(push);
    match format {
        SampleFormat::F32 => open::<f32>(device, config, push, on_error),
        SampleFormat::I16 => open::<i16>(device, config, push, on_error),
        SampleFormat::U16 => open::<u16>(device, config, push, on_error),
        SampleFormat::I32 => open::<i32>(device, config, push, on_error),
        other => Err(format!("Unsupported device sample format {other:?}.")),
    }
}
