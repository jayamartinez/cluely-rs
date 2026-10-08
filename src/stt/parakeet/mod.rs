//! NVIDIA Parakeet Realtime EOU 120M, on-device, through parakeet.cpp (MIT, pinned submodule;
//! see PATCHES.md). English only; output is lowercase without punctuation. The model file is
//! downloaded at runtime under the NVIDIA Open Model License.

pub mod ffi;
pub mod session;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::audio::AudioChunk;
use crate::models::ModelFile;
use crate::stt::{AsrError, Availability, Capabilities, EventSink, Locality, StreamingAsr, StreamingAsrSession};
use ffi::Device;

pub const MODEL: ModelFile = ModelFile {
    family: "parakeet",
    name: "realtime_eou_120m-v1-q8_0.gguf",
    url: "https://huggingface.co/mudler/parakeet-cpp-gguf/resolve/741158ae71e64ef5c89385862c18f777d07a97a1/realtime_eou_120m-v1-q8_0.gguf",
    bytes: 176_001_472,
    sha256: "62616b914d6f5a683a5dea672df055b57de5c49dddf871b8b44b9c814dc3d896",
    license_url: "https://huggingface.co/nvidia/parakeet_realtime_eou_120m-v1",
};

/// Inference threads shared by all Parakeet streams. Chosen by `examples/parakeet_bench.rs`;
/// see the benchmark notes in PATCHES.md.
pub const DEFAULT_THREADS: usize = 4;

/// How long [`shutdown`] waits for running streams to finish.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Frees parakeet.cpp's shared backend before the process exits; required with Metal, where ggml
/// aborts during exit otherwise (PATCHES.md). Call after telling everything that transcribes to
/// stop: it waits up to [`SHUTDOWN_TIMEOUT`] for the last model and stream to be dropped, and
/// later model loads fail. Safe if no model was ever loaded, and if called twice.
pub fn shutdown() {
    if let Err(models) = ffi::shutdown_backend(SHUTDOWN_TIMEOUT) {
        eprintln!("parakeet.cpp: {models} model(s) still in use at exit; its backend was not freed");
    }
}

/// Set while the GPU was asked for but transcription runs on the CPU (Settings › Listening says so).
static GPU_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether the last model loaded for the GPU ended up on the CPU.
pub fn gpu_unavailable() -> bool { GPU_UNAVAILABLE.load(Ordering::Relaxed) }

#[derive(Clone, Debug)]
pub struct ParakeetConfig {
    pub model_path: Option<PathBuf>,
    pub threads: usize,
    /// macOS: compute on the GPU (Metal) rather than the CPU (Settings › Listening). Ignored on
    /// Windows, and when `PARAKEET_DEVICE` is set.
    pub use_gpu: bool,
}

impl Default for ParakeetConfig {
    fn default() -> Self { Self { model_path: MODEL.path(), threads: DEFAULT_THREADS, use_gpu: true } }
}

impl ParakeetConfig {
    fn device(&self) -> Option<Device> { choose_device(self.use_gpu, cfg!(target_os = "macos"), ffi::device_overridden()) }
}

/// The device to ask parakeet.cpp for. None keeps its own choice: on Windows (CPU builds), and
/// when `PARAKEET_DEVICE` decides.
fn choose_device(use_gpu: bool, macos: bool, overridden: bool) -> Option<Device> {
    (macos && !overridden).then_some(if use_gpu { Device::Gpu } else { Device::Cpu })
}

/// Loads on `device`, and on the CPU when the GPU was asked for but failed. `load` returns the
/// model and the device it computes on. Also returns whether the GPU was asked for but the model
/// runs on the CPU, either after that retry or because parakeet.cpp couldn't start the GPU.
fn load_with_fallback<T>(device: Option<Device>, mut load: impl FnMut(Option<Device>) -> Result<(T, String), String>) -> Result<(T, bool), String> {
    let wants_gpu = device == Some(Device::Gpu);
    match load(device) {
        Ok((model, actual)) => {
            let fell_back = wants_gpu && actual == "cpu";
            if fell_back { eprintln!("parakeet.cpp couldn't start the GPU; transcribing on the CPU"); }
            Ok((model, fell_back))
        }
        Err(error) if wants_gpu => {
            eprintln!("parakeet.cpp couldn't load the model on the GPU ({error}); transcribing on the CPU");
            load(Some(Device::Cpu)).map(|(model, _)| (model, true))
        }
        Err(error) => Err(error),
    }
}

pub struct ParakeetRealtime {
    config: ParakeetConfig,
    /// Loaded once and shared by the Me and Them sessions; freed when both end.
    model: Mutex<Weak<ffi::Model>>,
}

impl ParakeetRealtime {
    pub fn new(config: ParakeetConfig) -> Self { Self { config, model: Mutex::new(Weak::new()) } }

    fn load_model(&self) -> Result<Arc<ffi::Model>, AsrError> {
        let mut slot = self.model.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(model) = slot.upgrade() { return Ok(model); }
        let path = self.config.model_path.as_ref().ok_or_else(|| AsrError::Failed("No local data folder for models.".into()))?;
        ffi::set_threads(self.config.threads);
        let device = self.config.device();
        let (model, fell_back) = load_with_fallback(device, |device| ffi::Model::load(path, device).map(|model| {
            let actual = model.device().to_string();
            (model, actual)
        })).map_err(AsrError::Failed)?;
        if device == Some(Device::Gpu) { GPU_UNAVAILABLE.store(fell_back, Ordering::Relaxed); }
        let model = Arc::new(model);
        *slot = Arc::downgrade(&model);
        Ok(model)
    }
}

impl Default for ParakeetRealtime {
    fn default() -> Self { Self::new(ParakeetConfig::default()) }
}

impl StreamingAsr for ParakeetRealtime {
    fn capabilities(&self) -> Capabilities {
        Capabilities { id: "parakeet-realtime", label: "Parakeet Realtime (on-device)",
            summary: "NVIDIA Parakeet Realtime EOU 120M. Private and free; English only.", locality: Locality::OnDevice,
            requires_api_key: false, emits_end_of_utterance: true, languages: &["en"], text_lag_ms: 400.0 }
    }

    fn availability(&self) -> Availability {
        match &self.config.model_path {
            None => Availability::Unavailable { reason: "No local data folder for models.".into() },
            Some(path) if MODEL.is_installed_at(path) => Availability::Ready,
            Some(_) => Availability::NeedsModel { download_bytes: MODEL.bytes },
        }
    }

    fn start_session(&self, sink: EventSink) -> Result<Box<dyn StreamingAsrSession>, AsrError> {
        let model = self.load_model()?;
        let session = session::Session::new(sink, move || model.begin_stream()).map_err(AsrError::Failed)?;
        Ok(Box::new(ParakeetSession(session)))
    }
}

struct ParakeetSession<F>(session::Session<ffi::Stream, F>);

impl<F: FnMut() -> Result<ffi::Stream, String> + Send> StreamingAsrSession for ParakeetSession<F> {
    fn push(&mut self, chunk: &AudioChunk) -> Result<(), AsrError> { self.0.push(chunk.start_ms, &chunk.samples).map_err(AsrError::Failed) }
    fn finish(&mut self) -> Result<(), AsrError> { self.0.finish().map_err(AsrError::Failed) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gpu_switch_picks_the_device_on_macos_only_and_parakeet_device_overrides_it() {
        assert_eq!(choose_device(true, true, false), Some(Device::Gpu));
        assert_eq!(choose_device(false, true, false), Some(Device::Cpu));
        assert_eq!(choose_device(true, true, true), None, "PARAKEET_DEVICE decides");
        assert_eq!(choose_device(true, false, false), None, "Windows keeps parakeet.cpp's choice");
        assert_eq!(choose_device(false, false, false), None);
    }

    #[test]
    fn a_failed_gpu_load_is_retried_on_the_cpu_and_reported() {
        let mut asked = Vec::new();
        let result = load_with_fallback(Some(Device::Gpu), |device| {
            asked.push(device);
            if device == Some(Device::Gpu) { Err("no Metal".to_string()) } else { Ok(("model", "cpu".to_string())) }
        });
        assert_eq!(result, Ok(("model", true)));
        assert_eq!(asked, [Some(Device::Gpu), Some(Device::Cpu)]);
    }

    #[test]
    fn a_gpu_load_that_lands_on_the_cpu_is_reported_without_a_retry() {
        let mut calls = 0;
        assert_eq!(load_with_fallback(Some(Device::Gpu), |_| { calls += 1; Ok(("model", "cpu".to_string())) }), Ok(("model", true)));
        assert_eq!(calls, 1);
        assert_eq!(load_with_fallback(Some(Device::Gpu), |_| Ok(("model", "MTL0".to_string()))), Ok(("model", false)));
    }

    #[test]
    fn cpu_and_unchosen_loads_are_not_retried_or_reported() {
        assert_eq!(load_with_fallback(Some(Device::Cpu), |_| Ok(("model", "cpu".to_string()))), Ok(("model", false)));
        assert_eq!(load_with_fallback(None, |_| Ok(("model", "cpu".to_string()))), Ok(("model", false)));
        let mut calls = 0;
        assert!(load_with_fallback::<&str>(Some(Device::Cpu), |_| { calls += 1; Err("broken file".to_string()) }).is_err());
        assert_eq!(calls, 1);
    }
}
