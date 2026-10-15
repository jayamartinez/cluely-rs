//! The two macOS privacy permissions Live needs, for setup's Permissions step: the microphone
//! (AVFoundation) and Screen Recording (CoreGraphics), which desktop audio needs. macOS grants both
//! to CluelyRS.app (`scripts/bundle-macos.sh`); a bare binary, such as a development build, gets
//! them through the app that started it (the terminal). Screen Recording only takes effect in a new
//! process, hence `relaunch_after_exit`.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use block2::RcBlock;
use objc2::msg_send;
use objc2::runtime::{AnyClass, Bool};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};
use objc2_foundation::NSString;

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeAudio: &'static NSString;
}

/// Where a permission stands, as far as this process can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access { NotAsked, Denied, Allowed }

/// The Privacy & Security pane for a permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyPane { Microphone, ScreenRecording }

fn capture_device() -> Option<&'static AnyClass> { AnyClass::get(c"AVCaptureDevice") }

/// The microphone permission. Unlike Screen Recording it changes while CluelyRS runs. Restricted
/// (by an administrator or profile) reads as denied: either way, only System Settings can change it.
pub fn microphone_access() -> Access {
    let Some(class) = capture_device() else { return Access::Allowed };
    // SAFETY: a class method taking a media type constant and returning AVAuthorizationStatus.
    let status: isize = unsafe { msg_send![class, authorizationStatusForMediaType: AVMediaTypeAudio] };
    match status { 0 => Access::NotAsked, 3 => Access::Allowed, _ => Access::Denied }
}

/// Show macOS's microphone prompt (only while the permission was never asked) and report the
/// answer to `done`, on whatever thread AVFoundation calls back on.
pub fn request_microphone(done: impl FnOnce(bool) + Send + 'static) {
    let Some(class) = capture_device() else { return done(true) };
    let done = Mutex::new(Some(done));
    let block = RcBlock::new(move |granted: Bool| {
        if let Some(done) = done.lock().ok().and_then(|mut done| done.take()) { done(granted.as_bool()); }
    });
    // SAFETY: the block lives as long as AVFoundation needs it (it copies it) and is called once.
    let () = unsafe { msg_send![class, requestAccessForMediaType: AVMediaTypeAudio, completionHandler: &*block] };
}

/// Whether this process may record the screen (and so capture desktop audio). macOS answers for
/// the whole process lifetime: allowing it in System Settings shows here only after a relaunch.
pub fn screen_recording_allowed() -> bool { CGPreflightScreenCaptureAccess() }

/// Show macOS's Screen Recording prompt; it appears only the first time it's ever asked.
pub fn request_screen_recording() { CGRequestScreenCaptureAccess(); }

pub fn open_privacy_pane(pane: PrivacyPane) -> std::io::Result<()> {
    let anchor = match pane { PrivacyPane::Microphone => "Privacy_Microphone", PrivacyPane::ScreenRecording => "Privacy_ScreenCapture" };
    Command::new("open").arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}")).spawn().map(drop)
}

/// Start CluelyRS again once this process has exited, so the new one gets the permissions and the
/// global shortcuts this one holds. A shell waits for this process (up to 30 s, then gives up) and
/// reopens the .app bundle, or runs the binary again with the same arguments. The caller quits.
pub fn relaunch_after_exit() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut command = Command::new("/bin/sh");
    command.args(relaunch_args(std::process::id(), &exe, &args)).stdin(Stdio::null());
    command.spawn().map(drop)
}

/// `sh` arguments for `relaunch_after_exit`; the paths are passed as arguments, never parsed as script.
fn relaunch_args(pid: u32, exe: &Path, args: &[OsString]) -> Vec<OsString> {
    const WAIT: &str = "i=0; while kill -0 \"$1\" 2>/dev/null; do [ $i -ge 150 ] && exit 1; sleep 0.2; i=$((i+1)); done; shift; exec \"$@\"";
    let mut all: Vec<OsString> = vec!["-c".into(), WAIT.into(), "sh".into(), pid.to_string().into()];
    match exe.ancestors().find(|dir| dir.extension().is_some_and(|ext| ext == "app")) {
        Some(bundle) => all.extend(["/usr/bin/open".into(), "-n".into(), bundle.as_os_str().to_owned()]),
        None => { all.push(exe.as_os_str().to_owned()); all.extend(args.iter().cloned()); }
    }
    all
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::relaunch_args;

    #[test]
    fn relaunch_waits_for_this_process_then_reopens_the_bundle_or_the_binary() {
        let strings = |args: Vec<std::ffi::OsString>| args.into_iter().map(|arg| arg.into_string().unwrap()).collect::<Vec<_>>();
        let bare = strings(relaunch_args(42, Path::new("/Users/jay/My Code/target/debug/cluely-rs"), &["--flag".into()]));
        assert_eq!((bare[0].as_str(), &bare[2..4]), ("-c", &["sh".to_string(), "42".to_string()][..]));
        assert!(bare[1].contains("kill -0 \"$1\"") && bare[1].ends_with("exec \"$@\""), "{}", bare[1]);
        assert_eq!(bare[4..], ["/Users/jay/My Code/target/debug/cluely-rs", "--flag"], "the path is one argument, not script");
        let bundled = strings(relaunch_args(42, Path::new("/Applications/CluelyRS.app/Contents/MacOS/cluely-rs"), &["--flag".into()]));
        assert_eq!(bundled[4..], ["/usr/bin/open", "-n", "/Applications/CluelyRS.app"]);
    }
}
