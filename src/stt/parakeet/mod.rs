//! NVIDIA Parakeet Realtime EOU 120M, on-device, through parakeet.cpp (MIT, pinned submodule;
//! see PATCHES.md). English only; output is lowercase without punctuation. The model file is
//! downloaded at runtime under the NVIDIA Open Model License.

pub mod ffi;
pub mod session;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use crate::audio::AudioChunk;
use crate::models::ModelFile;
use crate::stt::{AsrError, Availability, Capabilities, EventSink, Locality, StreamingAsr, StreamingAsrSession};

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

#[derive(Clone, Debug)]
pub struct ParakeetConfig {
    pub model_path: Option<PathBuf>,
    pub threads: usize,
}

impl Default for ParakeetConfig {
    fn default() -> Self { Self { model_path: MODEL.path(), threads: DEFAULT_THREADS } }
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
        let model = Arc::new(ffi::Model::load(path).map_err(AsrError::Failed)?);
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
            requires_api_key: false, emits_end_of_utterance: true, languages: &["en"] }
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
