//! A Live session's listening pipeline: device capture → streaming ASR → transcript state and
//! endpointing, all on their own threads. The UI starts it, sends one command (stop), and reads
//! [`Message`]s from a channel; it never waits on audio, inference or device setup.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use crate::audio::{AudioCapture, AudioChunk, Devices, Source};
use crate::metrics::{self, LatencyRecorder};
use crate::settings::SttProvider;
use crate::stt::parakeet::{ParakeetConfig, ParakeetRealtime};
use crate::stt::{Availability, StreamingAsr, Transcriber};
use crate::transcript::endpoint::EndpointConfig;
use crate::transcript::live::{LiveTranscript, Update};

/// How often the pipeline checks for provider events and commands while no audio arrives.
const POLL: Duration = Duration::from_millis(20);

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Devices are opening and the model is loading.
    Starting,
    /// Capture and transcription are running for `sources`. `failures` lists sources that
    /// couldn't be opened (the others still run).
    Listening { sources: Vec<Source>, failures: Vec<(Source, String)> },
    /// Nothing is being transcribed. Assist and typed questions still work.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Status(Status),
    Transcript(Update),
    /// How loud each source is right now, 0..=1 (see [`LevelMeter`]), about 25 times a second.
    Level { me: f32, them: f32 },
}

/// Audio levels for the Live waveform: the loudest chunk of each source over every
/// [`LevelMeter::INTERVAL_MS`] of audio, on a decibel scale shaped for speech. Measured on the audio
/// clock, so the UI gets about 25 updates a second however the chunks arrive.
#[derive(Default)]
pub struct LevelMeter {
    peak: [f32; 2],
    next_ms: Option<f64>,
}

impl LevelMeter {
    pub const INTERVAL_MS: f64 = 40.0;

    /// The levels (Me, Them) once a full interval of audio has been observed.
    pub fn observe(&mut self, chunk: &AudioChunk) -> Option<(f32, f32)> {
        let slot = match chunk.source { Source::Me => 0, Source::Them => 1 };
        self.peak[slot] = self.peak[slot].max(chunk.rms());
        let end = chunk.end_ms();
        let due = *self.next_ms.get_or_insert(end + Self::INTERVAL_MS);
        if end < due { return None; }
        self.next_ms = Some(end + Self::INTERVAL_MS);
        let [me, them] = std::mem::take(&mut self.peak).map(level);
        Some((me, them))
    }
}

/// An RMS level as 0..=1: -54 dBFS (a quiet room) and below is 0, -12 dBFS (loud speech) and above is 1.
pub fn level(rms: f32) -> f32 {
    if rms <= 0.0 { return 0.0; }
    ((20.0 * rms.log10() + 54.0) / 42.0).clamp(0.0, 1.0)
}

pub enum Command { Stop }

/// The transcription provider Live uses, from Settings → Listening.
pub fn provider(settings: &crate::settings::Settings) -> Arc<dyn StreamingAsr> {
    match settings.stt_provider {
        SttProvider::Parakeet => Arc::new(ParakeetRealtime::new(ParakeetConfig { use_gpu: settings.use_gpu, ..ParakeetConfig::default() })),
        SttProvider::Deepgram => Arc::new(crate::stt::deepgram::Deepgram::from_store()),
    }
}

/// Why the provider can't start, in the words the Live panel shows.
pub fn not_ready(availability: &Availability) -> Option<String> {
    match availability {
        Availability::Ready => None,
        Availability::NeedsModel { .. } => Some("The transcription model isn't installed. Download it in Settings → Listening.".into()),
        Availability::NeedsApiKey => Some("Add an API key for the transcription provider in Settings → Listening.".into()),
        Availability::Unavailable { reason } => Some(reason.clone()),
    }
}

/// A Live session's metrics export: the shared recorder and the file stem (`live-<time>`)
/// its marks and captured audio are written under.
#[derive(Clone)]
pub struct Metrics {
    pub recorder: LatencyRecorder,
    pub stem: std::path::PathBuf,
}

impl Metrics {
    /// Set up an export for a session starting now, when `CLUELYRS_METRICS=1`.
    pub fn for_session(started: Instant) -> Option<Self> {
        if !metrics::export_enabled() { return None; }
        let stem = metrics::metrics_dir()?.join(format!("live-{}", crate::archive::unix_now()));
        Some(Self { recorder: LatencyRecorder::new(started), stem })
    }

    /// Write the marks. Call once, when the session ends.
    pub fn export(&self) {
        if let Err(error) = self.recorder.write_jsonl(&self.stem.with_extension("jsonl")) { eprintln!("metrics export failed: {error}"); }
    }
}

/// A running pipeline. Dropping it stops listening; the threads wind down on their own.
pub struct Listening {
    commands: Sender<Command>,
    /// When the pipeline's clock started; audio and transcript times count from here.
    pub started: Instant,
    thread: Option<JoinHandle<()>>,
}

impl Listening {
    /// Start capturing `sources` and transcribing with `provider`. Returns immediately: device
    /// setup and model loading happen on the pipeline thread, which reports through `Message`s.
    /// `metrics` is the session's recorder and export stem when the Live session exports
    /// (`CLUELYRS_METRICS=1`): the pipeline records into it and saves the captured audio next
    /// to it; the owner of the recorder writes the marks when the session ends.
    pub fn start(provider: Arc<dyn StreamingAsr>, sources: Vec<Source>, devices: Devices, metrics: Option<Metrics>) -> (Self, UnboundedReceiver<Message>) {
        let (out, messages) = unbounded();
        let (commands, inbox) = channel();
        let started = Instant::now();
        let thread = std::thread::Builder::new().name("cluelyrs-listening".into()).spawn(move || {
            let _ = out.unbounded_send(Message::Status(Status::Starting));
            let (chunks, audio) = channel();
            let (capture, failures) = match AudioCapture::start_with(&sources, &devices, chunks) {
                Ok(started) => started,
                Err(error) => { let _ = out.unbounded_send(Message::Status(Status::Failed(error.to_string()))); return; }
            };
            let opened: Vec<Source> = capture.opened.iter().map(|info| info.source).collect();
            let live = LiveTranscript::new(EndpointConfig::default(), metrics.as_ref().map(|metrics| metrics.recorder.clone()));
            let mut dump = metrics.as_ref().and_then(|metrics| AudioDump::create(&metrics.stem, &opened).ok());
            run_watching(provider, audio, &opened, failures, live, &inbox, &out, dump.as_mut(), &|| capture.take_ended());
            capture.stop();
            if let Some(dump) = dump && let Err(error) = dump.finish() { eprintln!("audio export failed: {error}"); }
        }).ok();
        (Self { commands, started, thread }, messages)
    }

    pub fn stop(&self) { let _ = self.commands.send(Command::Stop); }
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.stop();
        // Not joined: finishing streams can take a moment and the UI thread must not wait.
        self.thread.take();
    }
}

/// Feed audio to the recognizer and the endpointer, turn provider events into transcript
/// updates, and stop on command or when the audio ends. Device-free, so tests can drive it.
#[allow(clippy::too_many_arguments)]
pub fn run(provider: Arc<dyn StreamingAsr>, audio: Receiver<AudioChunk>, sources: &[Source], failures: Vec<(Source, String)>,
    live: LiveTranscript, commands: &Receiver<Command>, out: &UnboundedSender<Message>, dump: Option<&mut AudioDump>) {
    run_watching(provider, audio, sources, failures, live, commands, out, dump, &Vec::new);
}

/// `run`, also reporting sources whose capture ends mid-session: `ended` returns each once, with
/// why, and the status then lists it as unavailable (or fails when nothing is left to hear).
#[allow(clippy::too_many_arguments)]
fn run_watching(provider: Arc<dyn StreamingAsr>, audio: Receiver<AudioChunk>, sources: &[Source], mut failures: Vec<(Source, String)>,
    mut live: LiveTranscript, commands: &Receiver<Command>, out: &UnboundedSender<Message>, mut dump: Option<&mut AudioDump>,
    ended: &dyn Fn() -> Vec<(Source, String)>) {
    let (events, inbox) = channel();
    let mut transcriber = Transcriber::new(events);
    match transcriber.start(provider.clone(), sources) {
        Ok(generation) => { let caps = provider.capabilities(); live.set_generation(generation, caps.id); live.set_text_lag(caps.text_lag_ms); }
        Err(error) => { let _ = out.unbounded_send(Message::Status(Status::Failed(error.to_string()))); return; }
    }
    let mut heard = sources.to_vec();
    let _ = out.unbounded_send(Message::Status(Status::Listening { sources: heard.clone(), failures: failures.clone() }));
    let send = |updates: Vec<Update>| updates.into_iter().all(|update| out.unbounded_send(Message::Transcript(update)).is_ok());
    let mut meter = LevelMeter::default();
    loop {
        if commands.try_recv().is_ok() { break; }
        match audio.recv_timeout(POLL) {
            Ok(chunk) => {
                if let Some(dump) = dump.as_deref_mut() { dump.write(&chunk); }
                let updates = live.on_audio(&chunk);
                let levels = meter.observe(&chunk);
                transcriber.feed(chunk);
                if !send(updates) { break; }
                if let Some((me, them)) = levels && out.unbounded_send(Message::Level { me, them }).is_err() { break; }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // How far each recognizer has got, read before draining its events, so everything it
        // said about that audio has been applied when the position is reported.
        let positions: Vec<(Source, f64)> = sources.iter().filter_map(|&source| transcriber.processed_until(source).map(|ms| (source, ms))).collect();
        let updates: Vec<Update> = inbox.try_iter().flat_map(|event| live.on_event(&event)).collect();
        for (source, ms) in positions { live.set_recognized_until(source, ms); }
        if !send(updates) { break; }
        let lost = ended();
        if !lost.is_empty() {
            heard.retain(|source| !lost.iter().any(|(gone, _)| gone == source));
            failures.extend(lost);
            let status = if heard.is_empty() {
                Status::Failed(failures.iter().map(|(source, error)| format!("{} unavailable: {error}", source.label())).collect::<Vec<_>>().join(" · "))
            } else {
                Status::Listening { sources: heard.clone(), failures: failures.clone() }
            };
            if out.unbounded_send(Message::Status(status)).is_err() { break; }
        }
    }
    // Flush the recognizers, apply their last events, then commit anything still in progress.
    transcriber.stop();
    let updates: Vec<Update> = inbox.try_iter().flat_map(|event| live.on_event(&event)).collect();
    if send(updates) { send(live.finish()); }
}

/// With `CLUELYRS_METRICS=1`, what each source captured is also saved beside the metrics file as
/// 16 kHz mono 16-bit WAV (`live-<time>-me.wav`, `-them.wav`), so a session can be replayed
/// through `examples/transcript_replay.rs` exactly as the pipeline heard it.
pub struct AudioDump {
    files: Vec<(Source, std::io::BufWriter<std::fs::File>, u32)>,
}

impl AudioDump {
    pub fn create(stem: &std::path::Path, sources: &[Source]) -> std::io::Result<Self> {
        if let Some(dir) = stem.parent() { std::fs::create_dir_all(dir)?; }
        let mut files = Vec::new();
        for &source in sources {
            let path = stem.with_file_name(format!("{}-{}.wav", stem.file_name().unwrap_or_default().to_string_lossy(), source.label().to_lowercase()));
            let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
            std::io::Write::write_all(&mut file, &wav_header(0))?;
            files.push((source, file, 0));
        }
        Ok(Self { files })
    }

    fn write(&mut self, chunk: &AudioChunk) {
        let Some((_, file, written)) = self.files.iter_mut().find(|(source, ..)| *source == chunk.source) else { return };
        let bytes: Vec<u8> = chunk.samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
        if std::io::Write::write_all(file, &bytes).is_ok() { *written += chunk.samples.len() as u32; }
    }

    /// Patch the sizes into each header.
    pub fn finish(self) -> std::io::Result<()> {
        use std::io::{Seek, Write};
        for (_, mut file, samples) in self.files {
            file.flush()?;
            file.seek(std::io::SeekFrom::Start(0))?;
            file.write_all(&wav_header(samples))?;
            file.flush()?;
        }
        Ok(())
    }
}

fn wav_header(samples: u32) -> [u8; 44] {
    let data = samples * 2;
    let mut header = [0u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&1u16.to_le_bytes());
    header[24..28].copy_from_slice(&crate::audio::SAMPLE_RATE.to_le_bytes());
    header[28..32].copy_from_slice(&(crate::audio::SAMPLE_RATE * 2).to_le_bytes());
    header[32..34].copy_from_slice(&2u16.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::stt::scripted::{ScriptedAsr, Step};
    use crate::transcript::endpoint::Reason;

    fn drain(mut messages: UnboundedReceiver<Message>) -> Vec<Message> {
        let mut out = Vec::new();
        while let Ok(message) = messages.try_recv() { out.push(message); }
        out
    }

    #[test]
    fn audio_flows_through_recognition_and_endpointing_to_committed_updates_per_source() {
        let provider = Arc::new(ScriptedAsr::new(vec![
            Step::partial(Source::Them, 200.0, "what is a"),
            Step::partial(Source::Them, 400.0, "what is a mutex"),
            Step::eou(Source::Them, 600.0, "what is a mutex"),
            Step::partial(Source::Me, 300.0, "let me think"),
        ]));
        let (chunks, audio) = channel();
        let (commands, inbox) = channel();
        let (out, messages) = unbounded();
        let worker = std::thread::spawn(move || {
            run(provider, audio, &Source::ALL, Vec::new(), LiveTranscript::new(EndpointConfig::default(), None), &inbox, &out, None);
        });
        // 0.8 s of speech on both sources, then 2 s of silence, then stop. The pauses let the
        // recognizer threads deliver their events before the silence is fed, as in real time.
        for i in 0..40 {
            for source in Source::ALL { chunks.send(AudioChunk { source, start_ms: i as f64 * 20.0, samples: vec![0.2; 320] }).unwrap(); }
        }
        std::thread::sleep(Duration::from_millis(200));
        for i in 40..140 {
            for source in Source::ALL { chunks.send(AudioChunk { source, start_ms: i as f64 * 20.0, samples: vec![0.0; 320] }).unwrap(); }
        }
        std::thread::sleep(Duration::from_millis(200));
        commands.send(Command::Stop).unwrap();
        worker.join().unwrap();
        let messages = drain(messages);
        assert!(matches!(&messages[0], Message::Status(Status::Listening { sources, failures }) if sources == &Source::ALL && failures.is_empty()));
        let provisional: Vec<(Source, String)> = messages.iter().filter_map(|m| match m {
            Message::Transcript(Update::Provisional { source, stable, unstable, .. }) => Some((*source, format!("{stable}|{unstable}"))), _ => None }).collect();
        assert!(provisional.contains(&(Source::Them, "what is a|mutex".into())) || provisional.contains(&(Source::Them, "|what is a mutex".into())), "{provisional:?}");
        assert!(provisional.iter().any(|(source, _)| *source == Source::Me));
        let committed: Vec<(Source, &str, Reason)> = messages.iter().filter_map(|m| match m {
            Message::Transcript(Update::Committed { utterance, reason }) => Some((utterance.source, utterance.text.as_str(), *reason)), _ => None }).collect();
        assert!(committed.contains(&(Source::Them, "what is a mutex", Reason::EndOfUtterance)), "{committed:?}");
        assert!(committed.contains(&(Source::Me, "let me think", Reason::Silence)) || committed.contains(&(Source::Me, "let me think", Reason::MaxSilence)), "{committed:?}");
        let them = messages.iter().find_map(|m| match m { Message::Transcript(Update::Committed { utterance, .. }) if utterance.source == Source::Them => Some(utterance.clone()), _ => None }).unwrap();
        assert!(them.start_ms <= 200.0 && them.end_ms >= 600.0, "{them:?}");
    }

    #[test]
    fn levels_come_once_per_interval_of_audio_with_each_source_at_its_loudest() {
        let mut meter = LevelMeter::default();
        let chunk = |source, start_ms: f64, rms: f32| AudioChunk { source, start_ms, samples: vec![rms; 160] };
        // 10 ms chunks from both sources: nothing until 40 ms of audio has gone by.
        let mut levels = Vec::new();
        for i in 0..8 {
            let t = i as f64 * 10.0;
            levels.extend(meter.observe(&chunk(Source::Me, t, if i == 1 { 0.25 } else { 0.001 })));
            levels.extend(meter.observe(&chunk(Source::Them, t, 0.02)));
        }
        assert_eq!(levels.len(), 1, "{levels:?}");
        let (me, them) = levels[0];
        assert!((me - 1.0).abs() < 0.01 && them > 0.3 && them < 0.6, "{levels:?}");
        assert_eq!(level(0.0), 0.0);
        assert_eq!(level(0.001), 0.0);
        assert_eq!(level(1.0), 1.0);
        assert!(level(0.05) > level(0.02));
    }

    #[test]
    fn stopping_mid_utterance_commits_the_text_in_progress() {
        let provider = Arc::new(ScriptedAsr::new(vec![Step::partial(Source::Me, 100.0, "i was saying")]));
        let (chunks, audio) = channel();
        let (commands, inbox) = channel();
        let (out, messages) = unbounded();
        for i in 0..10 { chunks.send(AudioChunk { source: Source::Me, start_ms: i as f64 * 20.0, samples: vec![0.2; 320] }).unwrap(); }
        let worker = std::thread::spawn(move || {
            run(provider, audio, &[Source::Me], Vec::new(), LiveTranscript::new(EndpointConfig::default(), None), &inbox, &out, None);
        });
        std::thread::sleep(Duration::from_millis(200));
        commands.send(Command::Stop).unwrap();
        worker.join().unwrap();
        let last = drain(messages).into_iter().last().unwrap();
        assert!(matches!(last, Message::Transcript(Update::Committed { ref utterance, reason: Reason::Stopped }) if utterance.text == "i was saying"), "{last:?}");
    }

    #[test]
    fn a_provider_that_is_not_ready_reports_a_failure_instead_of_running() {
        let provider = Arc::new(ScriptedAsr::new(vec![]).with_availability(Availability::NeedsModel { download_bytes: 1 }));
        let (_chunks, audio) = channel::<AudioChunk>();
        let (_commands, inbox) = channel();
        let (out, messages) = unbounded();
        run(provider, audio, &Source::ALL, Vec::new(), LiveTranscript::new(EndpointConfig::default(), None), &inbox, &out, None);
        let messages = drain(messages);
        assert!(matches!(&messages[..], [Message::Status(Status::Failed(reason))] if reason.contains("isn't installed")), "{messages:?}");
        assert!(not_ready(&Availability::NeedsModel { download_bytes: 1 }).unwrap().contains("Settings"));
        assert!(not_ready(&Availability::Ready).is_none());
    }

    #[test]
    fn a_source_whose_capture_ends_is_reported_unavailable_and_failure_follows_when_none_is_left() {
        let run_until_lost = |sources: Vec<Source>, lost: Vec<(Source, String)>| {
            let (_chunks, audio) = channel::<AudioChunk>();
            let (commands, inbox) = channel();
            let (out, messages) = unbounded();
            let pending = std::sync::Mutex::new(Some(lost));
            let worker = std::thread::spawn(move || {
                let ended = || pending.lock().unwrap().take().unwrap_or_default();
                run_watching(Arc::new(ScriptedAsr::new(vec![])), audio, &sources, Vec::new(), LiveTranscript::new(EndpointConfig::default(), None),
                    &inbox, &out, None, &ended);
            });
            std::thread::sleep(Duration::from_millis(100));
            commands.send(Command::Stop).unwrap();
            worker.join().unwrap();
            drain(messages).into_iter().filter_map(|m| match m { Message::Status(status) => Some(status), _ => None }).collect::<Vec<_>>()
        };
        let statuses = run_until_lost(Source::ALL.to_vec(), vec![(Source::Them, "Screen Recording is off.".into())]);
        assert_eq!(statuses.last(), Some(&Status::Listening { sources: vec![Source::Me], failures: vec![(Source::Them, "Screen Recording is off.".into())] }));
        assert_eq!(statuses.len(), 2, "{statuses:?}");
        let statuses = run_until_lost(vec![Source::Them], vec![(Source::Them, "System audio stopped.".into())]);
        assert_eq!(statuses.last(), Some(&Status::Failed("Them unavailable: System audio stopped.".into())));
    }

    #[test]
    fn dropping_the_handle_stops_the_pipeline_without_blocking() {
        // No devices in CI-like runs is fine: the pipeline reports Failed or Listening on its own thread.
        let (listening, mut messages) = Listening::start(Arc::new(ScriptedAsr::new(vec![])), vec![Source::Me], Devices::default(), None);
        let started = Instant::now();
        drop(listening);
        assert!(started.elapsed() < Duration::from_millis(50));
        let first = futures::executor::block_on(messages.next());
        assert_eq!(first, Some(Message::Status(Status::Starting)));
    }
}
