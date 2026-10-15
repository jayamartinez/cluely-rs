//! The log file a user can send when something breaks. A release build has no console (Windows
//! builds it as a GUI app; a macOS .app's stderr goes nowhere), so [`init`] points the process's
//! stderr at `cluelyrs.log`: every `eprintln!`, Rust's panic message and whatever the C++ speech
//! engine (parakeet.cpp/ggml) prints to `stderr` land there, behind one header line per launch.
//! A panic also writes its time and a backtrace. Debug builds keep printing to the console unless
//! `CLUELYRS_LOG_FILE=1` asks for the file.
//!
//! Only what the app already prints is logged: errors and status, never keys, prompts,
//! transcripts or answers (keep it that way when adding an `eprintln!`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The current log, and the one before it once the current one passes [`MAX_BYTES`].
pub const LOG_FILE: &str = "cluelyrs.log";
pub const OLD_LOG_FILE: &str = "cluelyrs.old.log";
/// Checked at launch, so a log can pass this during a long session; at most two are kept.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Where the logs go: `%LOCALAPPDATA%\CluelyRS\logs` (machine-local, never synced), or on macOS
/// `~/Library/Logs/CluelyRS`, where Console.app looks.
pub fn logs_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    return dirs::home_dir().map(|home| home.join("Library").join("Logs").join("CluelyRS"));
    #[cfg(not(target_os = "macos"))]
    dirs::data_local_dir().map(|dir| dir.join("CluelyRS").join("logs"))
}

/// Send stderr to the log file and log panics with a backtrace (release builds, or
/// `CLUELYRS_LOG_FILE=1`). Call first thing in `main`; returns the log's path, or `None` when the
/// console is kept or the file can't be opened (there's nowhere to say so).
pub fn init() -> Option<PathBuf> {
    let wanted = cfg!(not(debug_assertions)) || std::env::var_os("CLUELYRS_LOG_FILE").is_some_and(|value| value == "1");
    if !wanted { return None; }
    let dir = logs_dir()?;
    fs::create_dir_all(&dir).ok()?;
    let _ = rotate(&dir, MAX_BYTES);
    let path = dir.join(LOG_FILE);
    // Append mode: every write lands at the end, even with a second launch writing its header.
    let mut file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    let _ = writeln!(file, "\n=== CluelyRS {} ({} {}) started {} (pid {}) ===",
        env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH, now(), std::process::id());
    redirect_stderr(file);
    log_panics();
    Some(path)
}

/// Keep one previous log: past `limit` bytes, `cluelyrs.log` becomes `cluelyrs.old.log`
/// (replacing it) and the next write starts a new one.
pub fn rotate(dir: &Path, limit: u64) -> io::Result<bool> {
    let log = dir.join(LOG_FILE);
    if !fs::metadata(&log).is_ok_and(|meta| meta.len() > limit) { return Ok(false); }
    fs::rename(&log, dir.join(OLD_LOG_FILE)).map(|()| true)
}

fn now() -> String { chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string() }

/// Chained to the default hook, which writes the message, location and thread; the time and a
/// backtrace (captured whatever `RUST_BACKTRACE` says) go around it.
fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("--- panic at {} ---", now());
        default(info);
        eprintln!("backtrace:\n{}", std::backtrace::Backtrace::force_capture());
        let _ = io::stderr().flush();
    }));
}

/// What stderr was before [`init`] replaced it (no handle at all in a release build).
#[cfg(windows)]
static ORIGINAL_STDERR: std::sync::OnceLock<isize> = std::sync::OnceLock::new();

/// Rust's stderr asks Windows for `STD_ERROR_HANDLE` on every write, so replacing that handle
/// redirects every `eprintln!`. The C runtime keeps its own descriptor 2 (unusable in a GUI app),
/// shared with the C++ engine since both link the same CRT, so its `stderr` stream is reopened
/// onto a descriptor of the same file. The file is never closed.
#[cfg(windows)]
fn redirect_stderr(file: File) {
    use std::ffi::c_void;
    use std::os::windows::io::IntoRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
    use windows::core::w;
    unsafe extern "C" {
        fn __acrt_iob_func(index: u32) -> *mut c_void;
        fn _wfreopen(path: *const u16, mode: *const u16, stream: *mut c_void) -> *mut c_void;
        fn _open_osfhandle(handle: isize, flags: i32) -> i32;
        fn _fileno(stream: *mut c_void) -> i32;
        fn _dup2(from: i32, to: i32) -> i32;
        fn setvbuf(stream: *mut c_void, buffer: *mut c_void, mode: i32, size: usize) -> i32;
    }
    const _IONBF: i32 = 4;
    let handle = file.into_raw_handle();
    // SAFETY: `handle` is an open file this process keeps for its lifetime; the CRT calls only
    // take that handle and the CRT's own `stderr` stream.
    unsafe {
        let original = GetStdHandle(STD_ERROR_HANDLE).map_or(0, |original| original.0 as isize);
        if SetStdHandle(STD_ERROR_HANDLE, HANDLE(handle)).is_err() { return; }
        let _ = ORIGINAL_STDERR.set(original);
        let fd = _open_osfhandle(handle as isize, 0);
        let stream = __acrt_iob_func(2);
        if fd < 0 || _wfreopen(w!("NUL").as_ptr(), w!("wb").as_ptr(), stream).is_null() { return; }
        _dup2(fd, _fileno(stream));
        // Unbuffered as before, so nothing waits in memory for a crash to lose.
        setvbuf(stream, std::ptr::null_mut(), _IONBF, 0);
    }
}

/// Run `spawn` with stderr as it was before [`init`], so a process given a console of its own
/// (the CLI sign-in) writes there rather than into the log. With a stderr handle set, Windows
/// would also take the child's missing stdin and stdout as given, leaving it unable to talk.
#[cfg(windows)]
pub fn with_original_stderr<T>(spawn: impl FnOnce() -> T) -> T {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
    let Some(&original) = ORIGINAL_STDERR.get() else { return spawn() };
    // SAFETY: only swaps the process's stderr handle between two handles that stay open; anything
    // printed meanwhile on another thread is lost, as it would be without the log.
    unsafe {
        let Ok(log) = GetStdHandle(STD_ERROR_HANDLE) else { return spawn() };
        let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(original as *mut _));
        let result = spawn();
        let _ = SetStdHandle(STD_ERROR_HANDLE, log);
        result
    }
}

/// Descriptor 2 is stderr for Rust and C alike.
#[cfg(unix)]
fn redirect_stderr(file: File) {
    use std::os::fd::AsRawFd;
    // SAFETY: both descriptors are open; `file` is dropped afterwards, leaving descriptor 2 open.
    unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_only_past_the_limit_and_keeps_one_old_log() {
        let dir = std::env::temp_dir().join(format!("cluelyrs-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(!rotate(&dir, 10).unwrap(), "no log yet");
        fs::write(dir.join(LOG_FILE), "0123456789").unwrap();
        assert!(!rotate(&dir, 10).unwrap(), "at the limit");
        fs::write(dir.join(OLD_LOG_FILE), "older").unwrap();
        fs::write(dir.join(LOG_FILE), "0123456789+").unwrap();
        assert!(rotate(&dir, 10).unwrap());
        assert!(!dir.join(LOG_FILE).exists());
        assert_eq!(fs::read_to_string(dir.join(OLD_LOG_FILE)).unwrap(), "0123456789+", "the previous old log is replaced");
        fs::remove_dir_all(&dir).unwrap();
    }
}
