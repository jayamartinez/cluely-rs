//! Setup › Permissions' audio check: the microphone and desktop audio, each captured as Live would
//! capture it, only to show its level. Nothing is recorded, transcribed or sent. A source runs only
//! while its permission is allowed, so the check never shows a permission prompt of its own.

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use gpui::Context;

use super::OnboardingWindow;
use crate::audio::{AudioCapture, AudioChunk, Devices, Source};
use crate::platform::Access;
use crate::settings::{OnboardingStep as Step, Settings};

const POLL: Duration = Duration::from_millis(50);
/// A level (on `listening::level`'s 0..=1 scale) at or above this counts as hearing something.
const HEARD: f32 = 0.15;
/// How long "Hearing you" stays up after the last sound.
const HEARD_FOR: Duration = Duration::from_secs(2);

/// What setup knows about the two permissions.
#[derive(Clone, Debug)]
pub(crate) struct Permissions {
    pub mic: Access,
    /// Screen Recording, as this process can use it: allowing it takes a relaunch.
    pub screen_allowed: bool,
    /// Allow… was pressed this run, so it may be allowed in System Settings but need a relaunch.
    pub screen_asked: bool,
    /// Why Quit & Reopen couldn't start CluelyRS again.
    pub relaunch_error: Option<String>,
}

impl Permissions {
    pub(crate) fn read() -> Self {
        Self { mic: crate::platform::microphone_access(), screen_allowed: crate::platform::screen_recording_allowed(), screen_asked: false, relaunch_error: None }
    }
}

/// One source's meter.
#[derive(Default)]
pub(crate) struct Meter {
    /// The device it should capture from (`None` inside: the system default); `None`: stopped.
    wanted: Option<Option<String>>,
    generation: u64,
    pub state: MeterState,
    pub level: f32,
    heard: Option<Instant>,
}

#[derive(Default)]
pub(crate) enum MeterState {
    #[default]
    Off,
    Starting,
    Running(AudioCapture, Receiver<AudioChunk>),
    Failed(String),
}

impl Meter {
    /// Whether it heard something in the last `HEARD_FOR`.
    pub fn hearing(&self) -> bool { self.heard.is_some_and(|at| at.elapsed() < HEARD_FOR) }

    fn stop(&mut self) {
        self.generation += 1;
        self.level = 0.0;
        self.heard = None;
        // Stopping joins its thread, and system audio waits up to 2 s for ScreenCaptureKit: not on the UI thread.
        if let MeterState::Running(capture, _) = std::mem::take(&mut self.state) { std::thread::spawn(move || capture.stop()); }
    }
}

impl Drop for Meter {
    fn drop(&mut self) { self.stop(); }
}

/// Microphone and desktop audio, in that order.
pub(crate) type AudioCheck = [Meter; 2];

fn slot(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

/// The device each source should capture from now: only on the Permissions step, while Live is
/// off (Live has the audio then), with its switch on and its permission allowed.
fn wanted(step: Step, settings: &Settings, permissions: &Permissions, live: bool) -> [Option<Option<String>>; 2] {
    if step != Step::Permissions || live { return [None, None]; }
    let Devices { mic, desktop } = settings.devices();
    [
        (settings.listen_mic && permissions.mic == Access::Allowed).then_some(mic),
        (settings.listen_desktop && permissions.screen_allowed).then_some(desktop),
    ]
}

impl OnboardingWindow {
    /// Start, restart or stop each meter to match what it should capture now.
    pub(super) fn sync_audio_check(&mut self, cx: &mut Context<Self>) {
        let overlay = self.overlay.read(cx);
        let wanted = wanted(self.step, &overlay.store.value, &self.permissions, overlay.live_since.is_some());
        for (source, wanted) in [Source::Me, Source::Them].into_iter().zip(wanted) {
            let meter = &mut self.audio[slot(source)];
            if meter.wanted == wanted { continue; }
            meter.stop();
            meter.wanted = wanted.clone();
            let Some(device) = wanted else { continue };
            meter.state = MeterState::Starting;
            let generation = meter.generation;
            let devices = match source { Source::Me => Devices { mic: device, desktop: None }, Source::Them => Devices { mic: None, desktop: device } };
            cx.spawn(async move |this, cx| {
                let (sink, chunks) = channel();
                // Opening a device can take a moment (up to 5 s before it counts as failed).
                let mut started = Some(cx.background_executor().spawn(async move { AudioCapture::start_with(&[source], &devices, sink) }).await);
                let running = this.update(cx, |this, cx| {
                    let meter = &mut this.audio[slot(source)];
                    if meter.generation != generation { return false; }
                    meter.state = match started.take() {
                        Some(Ok((capture, _))) => MeterState::Running(capture, chunks),
                        Some(Err(error)) => MeterState::Failed(reason(source, &error.to_string())),
                        None => MeterState::Off,
                    };
                    cx.notify();
                    matches!(meter.state, MeterState::Running(..))
                }).unwrap_or(false);
                // Replaced (or setup closed) while it opened: stop it off the UI thread too.
                if let Some(Ok((capture, _))) = started { std::thread::spawn(move || capture.stop()); }
                if !running { return; }
                loop {
                    cx.background_executor().timer(POLL).await;
                    if !this.update(cx, |this, cx| this.poll_meter(source, generation, cx)).unwrap_or(false) { return; }
                }
            }).detach();
        }
        cx.notify();
    }

    /// Read a running meter's level; `false` once it has stopped or been replaced.
    fn poll_meter(&mut self, source: Source, generation: u64, cx: &mut Context<Self>) -> bool {
        let meter = &mut self.audio[slot(source)];
        if meter.generation != generation { return false; }
        let MeterState::Running(capture, chunks) = &meter.state else { return false };
        // The chunks themselves aren't needed; keeping the receiver keeps capture running.
        while chunks.try_recv().is_ok() {}
        let ended = capture.take_ended().into_iter().next();
        meter.level = crate::listening::level(capture.level(source));
        if meter.level >= HEARD { meter.heard = Some(Instant::now()); }
        if let Some((_, why)) = ended {
            meter.stop();
            meter.state = MeterState::Failed(why);
        }
        cx.notify();
        matches!(meter.state, MeterState::Running(..))
    }
}

/// `AudioCapture::start_with`'s error for one source, without its "No audio source could be
/// opened: Me: " prefix.
fn reason(source: Source, error: &str) -> String {
    let detail = error.strip_prefix("No audio source could be opened: ").unwrap_or(error);
    detail.strip_prefix(source.label()).and_then(|rest| rest.strip_prefix(": ")).unwrap_or(detail).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_runs_only_on_its_step_with_its_switch_and_permission_and_without_live() {
        let allowed = Permissions { mic: Access::Allowed, screen_allowed: true, screen_asked: false, relaunch_error: None };
        let settings = Settings { mic_device: "USB Mic".into(), ..Settings::default() };
        let runs = |step, settings: &Settings, permissions: &Permissions, live| wanted(step, settings, permissions, live).map(|device| device.is_some());
        assert_eq!(wanted(Step::Permissions, &settings, &allowed, false), [Some(Some("USB Mic".into())), Some(None)]);
        assert_eq!(runs(Step::Privacy, &settings, &allowed, false), [false, false], "only on the Permissions step");
        assert_eq!(runs(Step::Permissions, &settings, &allowed, true), [false, false], "Live has the audio");
        let not_yet = Permissions { mic: Access::NotAsked, screen_allowed: false, ..allowed.clone() };
        assert_eq!(runs(Step::Permissions, &settings, &not_yet, false), [false, false], "never prompts on its own");
        let mic_off = Settings { listen_mic: false, ..settings.clone() };
        assert_eq!(runs(Step::Permissions, &mic_off, &allowed, false), [false, true]);
    }

    #[test]
    fn a_failure_reads_without_the_capture_prefix() {
        assert_eq!(reason(Source::Me, "No audio source could be opened: Me: No microphone is available."), "No microphone is available.");
        assert_eq!(reason(Source::Them, "No audio source could be opened: Them: Stream error: busy"), "Stream error: busy");
        assert_eq!(reason(Source::Me, "The audio device didn't respond."), "The audio device didn't respond.");
    }
}
