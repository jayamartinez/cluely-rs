//! Hand-written bindings for the parts of parakeet.cpp's C API (ABI v10, `parakeet_capi.h`)
//! that CluelyRS uses, wrapped in owning types. No C++ exception crosses this boundary.
//!
//! parakeet.cpp keeps one process-wide compute backend besides the models. On Metal it must be
//! freed after the last model and before the process exits, or ggml aborts during exit; see
//! [`shutdown_backend`].

use std::ffi::{CStr, CString, c_char, c_float, c_int};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
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
    fn cluelyrs_parakeet_device_name() -> *const c_char;
}

/// The C API revision these bindings were written against.
pub const ABI_VERSION: i32 = 10;

/// Sets the inference thread count for every stream in the process. Takes effect on the next
/// inference call.
pub fn set_threads(threads: usize) {
    unsafe { cluelyrs_parakeet_set_threads(threads.min(c_int::MAX as usize) as c_int) }
}

/// Where parakeet.cpp computes. It picks the device once, when it creates its process-wide backend
/// during the first model load, and keeps the model's weights there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device { Gpu, Cpu }

/// The variable parakeet.cpp reads when it creates its backend: unset picks the first GPU, "cpu"
/// the CPU.
const DEVICE_VAR: &str = "PARAKEET_DEVICE";

/// How long a load that needs the other device waits for the previous models to be dropped.
const DEVICE_SWITCH_WAIT: Duration = Duration::from_secs(3);

/// Whether `PARAKEET_DEVICE` was set before CluelyRS first touched it. It then decides the device,
/// and CluelyRS never changes it.
pub fn device_overridden() -> bool {
    static OVERRIDDEN: OnceLock<bool> = OnceLock::new();
    *OVERRIDDEN.get_or_init(|| std::env::var_os(DEVICE_VAR).is_some_and(|value| !value.is_empty()))
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
    LIFECYCLE.shut_down(timeout, || Native.free())
}

static LIFECYCLE: Lifecycle = Lifecycle::new();

/// What a load does to parakeet.cpp's backend; faked in tests.
trait BackendControl {
    /// Make the next backend parakeet.cpp creates use `device`.
    fn select(&self, device: Device);
    fn free(&self);
}

struct Native;

impl BackendControl for Native {
    fn select(&self, device: Device) {
        if device_overridden() { return; }
        // SAFETY: setting the environment while other threads may read it is what makes `set_var`
        // unsafe. Here: (1) parakeet.cpp reads PARAKEET_DEVICE only while creating its backend,
        // which happens inside a model load, and loads run under the lifecycle lock this is
        // called with, so nothing reads this variable while it changes; it copies the value at
        // once. (2) Other threads may still look up other variables. Rust's own lookups take the
        // std environment lock that `set_var` holds, and on macOS (the only platform that asks
        // for a device; elsewhere `ParakeetConfig::device` is None) libc's getenv, setenv and
        // unsetenv serialize on its environment lock, and getenv keeps returning pointers to
        // strings that stay allocated.
        unsafe {
            match device {
                Device::Gpu => std::env::remove_var(DEVICE_VAR),
                Device::Cpu => std::env::set_var(DEVICE_VAR, "cpu"),
            }
        }
    }

    fn free(&self) { unsafe { cluelyrs_parakeet_shutdown_backend() } }
}

/// Which models are alive and which device the backend was set up for, so the backend is freed
/// (to switch devices, or at exit) only after the last model.
struct Lifecycle { state: Mutex<LifecycleState>, model_freed: Condvar }
struct LifecycleState {
    models: usize,
    shut_down: bool,
    /// The device asked for when the current backend was set up; None before the first load
    /// that asked for one, and after the backend is freed.
    device: Option<Device>,
}

impl Lifecycle {
    const fn new() -> Self {
        Self { state: Mutex::new(LifecycleState { models: 0, shut_down: false, device: None }), model_freed: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, LifecycleState> { self.state.lock().unwrap_or_else(PoisonError::into_inner) }

    /// Runs `load` with the backend set up for `device` (None keeps whatever parakeet.cpp picks),
    /// and counts the model it returns as alive. Switching devices frees the backend first, after
    /// waiting up to `wait` for the models on the old one to be dropped. Holds the lock throughout,
    /// so loads, device switches and shutdown never overlap.
    fn load<T>(&self, device: Option<Device>, wait: Duration, backend: &impl BackendControl, load: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let mut state = self.lock();
        if state.shut_down { return Err("Transcription is shutting down.".into()); }
        if let Some(device) = device && state.device != Some(device) {
            if state.device.is_some() {
                (state, _) = self.model_freed.wait_timeout_while(state, wait, |state| state.models > 0).unwrap_or_else(PoisonError::into_inner);
                if state.models > 0 { return Err("The previous transcription is still finishing. Try again in a moment.".into()); }
                if state.shut_down { return Err("Transcription is shutting down.".into()); }
                backend.free();
            }
            backend.select(device);
            state.device = Some(device);
        }
        let model = load()?;
        state.models += 1;
        Ok(model)
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
        state.device = None;
        Ok(())
    }
}

/// One model counted as alive by `Lifecycle::load`; released after the model is freed.
struct Registration;

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
    /// The device the backend computes on: "cpu", or a GPU such as "MTL0".
    device: String,
    /// Dropped after `Drop::drop` has freed `ctx`.
    _registration: Registration,
}

unsafe impl Send for Model {}
unsafe impl Sync for Model {}

impl Model {
    /// Loads a model, with the backend on `device` (None leaves the choice to parakeet.cpp and
    /// `PARAKEET_DEVICE`). If parakeet.cpp can't start the GPU, it computes on the CPU; `device()`
    /// tells.
    pub fn load(path: &Path, device: Option<Device>) -> Result<Self, String> {
        let abi = unsafe { parakeet_capi_abi_version() };
        if abi != ABI_VERSION { return Err(format!("parakeet.cpp ABI {abi} doesn't match the bindings (expected {ABI_VERSION})")); }
        let path = path.to_str().ok_or("The model path isn't valid Unicode.")?;
        let path = CString::new(path).map_err(|_| "The model path contains a NUL byte.")?;
        let (ctx, device) = LIFECYCLE.load(device, DEVICE_SWITCH_WAIT, &Native, || {
            let ctx = NonNull::new(unsafe { parakeet_capi_load(path.as_ptr()) }).ok_or_else(|| {
                let reason = owned_message(unsafe { parakeet_capi_load_error() });
                if reason.is_empty() { "The model couldn't be loaded.".to_string() } else { format!("The model couldn't be loaded: {reason}") }
            })?;
            // The backend exists now (the load created it); read its device before anything can free it.
            Ok((ctx, owned_message(unsafe { cluelyrs_parakeet_device_name() })))
        })?;
        Ok(Self { ctx, device, _registration: Registration })
    }

    /// The device the backend computes on: "cpu", or a GPU such as "MTL0".
    pub fn device(&self) -> &str { &self.device }

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
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;

    /// Records what loads did to the backend.
    #[derive(Default)]
    struct FakeBackend { calls: RefCell<Vec<String>> }

    impl BackendControl for FakeBackend {
        fn select(&self, device: Device) { self.calls.borrow_mut().push(format!("select {device:?}")); }
        fn free(&self) { self.calls.borrow_mut().push("free".into()); }
    }

    impl FakeBackend {
        fn take(&self) -> Vec<String> { std::mem::take(&mut self.calls.borrow_mut()) }
    }

    fn load(lifecycle: &Lifecycle, device: Option<Device>, wait: Duration, backend: &FakeBackend) -> Result<(), String> {
        lifecycle.load(device, wait, backend, || Ok(()))
    }

    #[test]
    fn the_device_is_selected_once_and_switching_frees_the_backend_after_the_last_model() {
        let lifecycle = Arc::new(Lifecycle::new());
        let backend = FakeBackend::default();
        load(&lifecycle, Some(Device::Gpu), Duration::ZERO, &backend).unwrap();
        assert_eq!(backend.take(), ["select Gpu"]);
        // Same device: the backend stays as it is.
        load(&lifecycle, Some(Device::Gpu), Duration::ZERO, &backend).unwrap();
        assert!(backend.take().is_empty());
        // Another device: waits until both models are released by another thread, then switches.
        let releaser = { let lifecycle = Arc::clone(&lifecycle); std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            lifecycle.release();
            lifecycle.release();
        }) };
        load(&lifecycle, Some(Device::Cpu), Duration::from_secs(5), &backend).unwrap();
        releaser.join().unwrap();
        assert_eq!(backend.take(), ["free", "select Cpu"]);
    }

    #[test]
    fn a_switch_blocked_by_a_live_model_fails_without_touching_the_backend() {
        let lifecycle = Lifecycle::new();
        let backend = FakeBackend::default();
        load(&lifecycle, Some(Device::Gpu), Duration::ZERO, &backend).unwrap();
        backend.take();
        assert!(load(&lifecycle, Some(Device::Cpu), Duration::from_millis(20), &backend).is_err());
        assert!(backend.take().is_empty());
    }

    #[test]
    fn without_a_device_choice_the_backend_is_left_to_parakeet() {
        let lifecycle = Lifecycle::new();
        let backend = FakeBackend::default();
        load(&lifecycle, None, Duration::ZERO, &backend).unwrap();
        load(&lifecycle, None, Duration::ZERO, &backend).unwrap();
        assert!(backend.take().is_empty());
    }

    #[test]
    fn a_failed_load_is_not_counted_as_a_live_model() {
        let lifecycle = Lifecycle::new();
        let backend = FakeBackend::default();
        assert!(lifecycle.load(Some(Device::Gpu), Duration::ZERO, &backend, || Err::<(), _>("no model".to_string())).is_err());
        let mut freed = false;
        assert_eq!(lifecycle.shut_down(Duration::ZERO, || freed = true), Ok(()));
        assert!(freed);
    }

    #[test]
    fn shutdown_frees_the_backend_only_after_the_last_model_and_refuses_new_ones() {
        let lifecycle = Arc::new(Lifecycle::new());
        let backend = FakeBackend::default();
        load(&lifecycle, Some(Device::Gpu), Duration::ZERO, &backend).unwrap();
        load(&lifecycle, Some(Device::Gpu), Duration::ZERO, &backend).unwrap();
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
        assert!(load(&lifecycle, Some(Device::Gpu), Duration::ZERO, &backend).is_err());
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
        load(&busy, None, Duration::ZERO, &FakeBackend::default()).unwrap();
        let mut freed = false;
        assert_eq!(busy.shut_down(Duration::from_millis(20), || freed = true), Err(1));
        assert!(!freed);
    }
}
