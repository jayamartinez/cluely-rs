//! Hand-written bindings for the parts of parakeet.cpp's C API (ABI v10, `parakeet_capi.h`)
//! that CluelyRS uses, wrapped in owning types. No C++ exception crosses this boundary.
//!
//! parakeet.cpp keeps one process-wide compute backend besides the models. On Metal it must be
//! freed after the last model and before the process exits, or ggml aborts during exit; see
//! [`shutdown_backend`].

use std::ffi::{CStr, CString, c_char, c_float, c_int};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Deserialize;

#[repr(C)]
struct RawContext { _private: [u8; 0] }
#[repr(C)]
struct RawStream { _private: [u8; 0] }

unsafe extern "C" {
    fn parakeet_capi_abi_version() -> c_int;
    fn parakeet_capi_load(gguf_path: *const c_char) -> *mut RawContext;
    fn parakeet_capi_load_error() -> *const c_char;
    fn parakeet_capi_free(ctx: *mut RawContext);
    fn parakeet_capi_last_error(ctx: *mut RawContext) -> *const c_char;
    fn parakeet_capi_stream_begin(ctx: *mut RawContext) -> *mut RawStream;
    fn parakeet_capi_stream_feed_json(stream: *mut RawStream, pcm: *const c_float, n_samples: c_int) -> *mut c_char;
    fn parakeet_capi_stream_finalize_json(stream: *mut RawStream) -> *mut c_char;
    fn parakeet_capi_stream_free(stream: *mut RawStream);
    fn parakeet_capi_free_string(s: *mut c_char);
    // CluelyRS shim (native/parakeet_shim.cpp); see PATCHES.md.
    fn cluelyrs_parakeet_set_threads(n_threads: c_int);
    fn cluelyrs_parakeet_shutdown_backend();
}

/// The C API revision these bindings were written against.
pub const ABI_VERSION: i32 = 10;

/// Sets the inference thread count for every stream in the process. Takes effect on the next
/// inference call.
pub fn set_threads(threads: usize) {
    unsafe { cluelyrs_parakeet_set_threads(threads.min(c_int::MAX as usize) as c_int) }
}

/// Frees parakeet.cpp's process-wide compute backend. Call once everything that transcribes has
/// been told to stop, before the process exits. Waits up to `timeout` for every model to be
/// dropped (a stream keeps its model alive, so every stream too), then frees the backend. From
/// the first call on, loading a model fails, so nothing can recreate the backend afterwards.
/// Safe when no model was ever loaded, and when called more than once.
///
/// Returns the number of models still alive if they weren't all dropped in time. The backend is
/// then left alone, because freeing it under a live model would be a use-after-free.
pub fn shutdown_backend(timeout: Duration) -> Result<(), usize> {
    LIFECYCLE.shut_down(timeout, || unsafe { cluelyrs_parakeet_shutdown_backend() })
}

static LIFECYCLE: Lifecycle = Lifecycle::new();

/// Which models are alive (or loading), so the backend is freed only after the last one.
struct Lifecycle { state: Mutex<LifecycleState>, model_freed: Condvar }
struct LifecycleState { models: usize, shut_down: bool }

impl Lifecycle {
    const fn new() -> Self {
        Self { state: Mutex::new(LifecycleState { models: 0, shut_down: false }), model_freed: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, LifecycleState> { self.state.lock().unwrap_or_else(PoisonError::into_inner) }

    fn register(&self) -> Result<(), String> {
        let mut state = self.lock();
        if state.shut_down { return Err("Transcription is shutting down.".into()); }
        state.models += 1;
        Ok(())
    }

    fn release(&self) {
        self.lock().models -= 1;
        self.model_freed.notify_all();
    }

    fn shut_down(&self, timeout: Duration, free_backend: impl FnOnce()) -> Result<(), usize> {
        let mut state = self.lock();
        state.shut_down = true;
        (state, _) = self.model_freed.wait_timeout_while(state, timeout, |state| state.models > 0).unwrap_or_else(PoisonError::into_inner);
        if state.models > 0 { return Err(state.models); }
        // Still holding the lock, so no model can start loading meanwhile.
        free_backend();
        Ok(())
    }
}

/// Counts one model as alive from before it loads until after it is freed.
struct Registration;

impl Registration {
    fn new() -> Result<Self, String> { LIFECYCLE.register().map(|()| Self) }
}

impl Drop for Registration {
    fn drop(&mut self) { LIFECYCLE.release(); }
}

fn owned_message(ptr: *const c_char) -> String {
    if ptr.is_null() { return String::new(); }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

/// A loaded model. Calls from several threads are safe; parakeet.cpp serializes compute.
pub struct Model {
    ctx: NonNull<RawContext>,
    /// Dropped after `Drop::drop` has freed `ctx`.
    _registration: Registration,
}

unsafe impl Send for Model {}
unsafe impl Sync for Model {}

impl Model {
    pub fn load(path: &Path) -> Result<Self, String> {
        let abi = unsafe { parakeet_capi_abi_version() };
        if abi != ABI_VERSION { return Err(format!("parakeet.cpp ABI {abi} doesn't match the bindings (expected {ABI_VERSION})")); }
        let path = path.to_str().ok_or("The model path isn't valid Unicode.")?;
        let path = CString::new(path).map_err(|_| "The model path contains a NUL byte.")?;
        let registration = Registration::new()?;
        let ctx = unsafe { parakeet_capi_load(path.as_ptr()) };
        NonNull::new(ctx).map(|ctx| Self { ctx, _registration: registration }).ok_or_else(|| {
            let reason = owned_message(unsafe { parakeet_capi_load_error() });
            if reason.is_empty() { "The model couldn't be loaded.".into() } else { format!("The model couldn't be loaded: {reason}") }
        })
    }

    pub fn begin_stream(self: &Arc<Self>) -> Result<Stream, String> {
        let raw = unsafe { parakeet_capi_stream_begin(self.ctx.as_ptr()) };
        NonNull::new(raw).map(|raw| Stream { raw, model: Arc::clone(self) }).ok_or_else(|| self.last_error("start a stream"))
    }

    fn last_error(&self, action: &str) -> String {
        let reason = owned_message(unsafe { parakeet_capi_last_error(self.ctx.as_ptr()) });
        if reason.is_empty() { format!("parakeet.cpp couldn't {action}.") } else { format!("parakeet.cpp couldn't {action}: {reason}") }
    }
}

impl Drop for Model {
    fn drop(&mut self) { unsafe { parakeet_capi_free(self.ctx.as_ptr()) } }
}

/// One streaming decoder. Not shared between threads, but may move to another one. Keeps its
/// model alive.
pub struct Stream { raw: NonNull<RawStream>, model: Arc<Model> }

unsafe impl Send for Stream {}

/// What one feed produced. Times are seconds from the start of this stream.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct FeedOutput {
    /// Text finalized by this call (may end mid-word).
    pub text: String,
    #[serde(default)]
    pub events: Vec<StreamEvent>,
    #[serde(default)]
    pub words: Vec<Word>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct StreamEvent {
    /// "eou" (end of utterance) or "eob" (end of a backchannel such as "uh-huh").
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "t")]
    pub time_sec: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Word {
    #[serde(rename = "w")]
    pub text: String,
    pub start: f64,
    pub end: f64,
}

impl Stream {
    /// Feed 16 kHz mono PCM; decodes whenever a full encoder chunk is buffered.
    pub fn feed(&mut self, pcm: &[f32]) -> Result<FeedOutput, String> {
        let n = c_int::try_from(pcm.len()).map_err(|_| "Audio block too large.".to_string())?;
        let json = unsafe { parakeet_capi_stream_feed_json(self.raw.as_ptr(), pcm.as_ptr(), n) };
        self.parse(json, "decode audio")
    }

    /// Decode whatever audio is still buffered (end of input).
    pub fn finalize(&mut self) -> Result<FeedOutput, String> {
        let json = unsafe { parakeet_capi_stream_finalize_json(self.raw.as_ptr()) };
        self.parse(json, "finish the stream")
    }

    fn parse(&self, json: *mut c_char, action: &str) -> Result<FeedOutput, String> {
        if json.is_null() { return Err(self.model.last_error(action)); }
        let parsed = serde_json::from_slice(unsafe { CStr::from_ptr(json) }.to_bytes());
        unsafe { parakeet_capi_free_string(json) };
        parsed.map_err(|error| format!("Unexpected output from parakeet.cpp: {error}"))
    }
}

impl Drop for Stream {
    fn drop(&mut self) { unsafe { parakeet_capi_stream_free(self.raw.as_ptr()) } }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;

    #[test]
    fn shutdown_frees_the_backend_only_after_the_last_model_and_refuses_new_ones() {
        let lifecycle = Arc::new(Lifecycle::new());
        lifecycle.register().unwrap();
        lifecycle.register().unwrap();
        let freed = AtomicUsize::new(0);
        // Both models are released by another thread while shutdown waits.
        let releaser = { let lifecycle = Arc::clone(&lifecycle); std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            lifecycle.release();
            std::thread::sleep(Duration::from_millis(50));
            lifecycle.release();
        }) };
        let started = Instant::now();
        assert_eq!(lifecycle.shut_down(Duration::from_secs(5), || { freed.fetch_add(1, Ordering::SeqCst); }), Ok(()));
        assert!(started.elapsed() >= Duration::from_millis(100));
        releaser.join().unwrap();
        assert_eq!(freed.load(Ordering::SeqCst), 1);
        assert!(lifecycle.register().is_err());
        // A second call is harmless.
        assert_eq!(lifecycle.shut_down(Duration::ZERO, || { freed.fetch_add(1, Ordering::SeqCst); }), Ok(()));
    }

    #[test]
    fn shutdown_with_no_model_frees_at_once_and_a_live_model_keeps_the_backend() {
        let idle = Lifecycle::new();
        let mut freed = false;
        assert_eq!(idle.shut_down(Duration::ZERO, || freed = true), Ok(()));
        assert!(freed);

        let busy = Lifecycle::new();
        busy.register().unwrap();
        let mut freed = false;
        assert_eq!(busy.shut_down(Duration::from_millis(20), || freed = true), Err(1));
        assert!(!freed);
    }
}
