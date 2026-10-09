//! Finds the Codex and Claude Code CLIs wherever the user installed them.
//!
//! An app opened from Finder or the Dock gets launchd's minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`),
//! not the one the user's shell builds, so CLIs installed with Homebrew, npm, nvm, Volta, bun or pnpm
//! aren't on it. On Windows, a CLI installed after CluelyRS started isn't on its PATH either.
//!
//! Each CLI looks, in order, at the location chosen in Settings ([`chosen`]), its own environment
//! variable (`CODEX_PATH`, `CLAUDE_PATH`), then the directories of [`search_path`]: this process's PATH,
//! followed by the login shell's PATH (macOS, Linux) or the user and system PATH saved in the registry
//! (Windows). Which files a CLI accepts stays with the CLI. The same PATH is given to the CLIs it
//! starts ([`with_search_path`]), as Node-based launchers need `node` on it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The CLIs whose location can be chosen in Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cli { Codex, Claude }

static CHOSEN: Mutex<[Option<PathBuf>; 2]> = Mutex::new([None, None]);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Record the location chosen in Settings; empty means automatic.
pub fn set_chosen(cli: Cli, path: &str) {
    let path = Some(path.trim()).filter(|path| !path.is_empty()).map(PathBuf::from);
    lock(&CHOSEN)[cli as usize] = path;
}

/// The location chosen in Settings, if any. The CLI checks it is usable before relying on it.
pub fn chosen(cli: Cli) -> Option<PathBuf> {
    lock(&CHOSEN)[cli as usize].clone()
}

// ---------------------------------------------------------------------------------------------
// Search PATH

/// Directories found beyond this process's PATH, and when they were read.
struct Extra {
    dirs: Vec<PathBuf>,
    read_at: Instant,
}

static EXTRA: Mutex<Option<Extra>> = Mutex::new(None);

/// A CLI that wasn't found re-reads the extra directories (it may have been installed since), but
/// no more often than this, so a missing CLI doesn't start a login shell on every request.
const REREAD_AFTER: Duration = Duration::from_secs(10);

/// Read the extra directories in the background, so the first request doesn't wait for them.
pub fn warm() {
    let _ = std::thread::Builder::new().name("cluelyrs-cli-path".into()).spawn(|| { extra_dirs(); });
}

/// Read once and cached. The lock is held while reading, so concurrent callers wait for one read.
fn extra_dirs() -> Vec<PathBuf> {
    let mut cache = lock(&EXTRA);
    if let Some(extra) = cache.as_ref() {
        return extra.dirs.clone();
    }
    let dirs = read_extra_dirs();
    *cache = Some(Extra { dirs: dirs.clone(), read_at: Instant::now() });
    dirs
}

/// Re-read the extra directories unless they were read moments ago. Whether they were re-read.
fn reread_after_miss() -> bool {
    let mut cache = lock(&EXTRA);
    if cache.as_ref().is_some_and(|extra| extra.read_at.elapsed() < REREAD_AFTER) {
        return false;
    }
    *cache = Some(Extra { dirs: read_extra_dirs(), read_at: Instant::now() });
    true
}

/// `resolve`, tried again with freshly read directories when `missing` says it found nothing.
pub fn resolve_with_retry<T, E>(resolve: impl Fn() -> Result<T, E>, missing: impl Fn(&E) -> bool) -> Result<T, E> {
    match resolve() {
        Err(error) if missing(&error) && reread_after_miss() => resolve(),
        result => result,
    }
}

/// This process's PATH, unchanged, followed by the extra directories it lacks.
pub fn search_path() -> OsString {
    append_dirs(std::env::var_os("PATH").as_deref(), &extra_dirs())
}

fn append_dirs(path: Option<&OsStr>, extra: &[PathBuf]) -> OsString {
    let mut result = path.map(OsStr::to_os_string).unwrap_or_default();
    let mut present: Vec<PathBuf> = path.map(|path| std::env::split_paths(path).collect()).unwrap_or_default();
    for dir in extra {
        let joinable = std::env::join_paths([dir]).is_ok();
        if !dir.is_absolute() || !joinable || present.iter().any(|seen| same_dir(seen, dir)) {
            continue;
        }
        if !result.is_empty() {
            result.push(if cfg!(windows) { ";" } else { ":" });
        }
        result.push(dir);
        present.push(dir.clone());
    }
    result
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let separators: &[char] = if cfg!(windows) { &['\\', '/'] } else { &['/'] };
    let normal = |path: &Path| {
        let text = path.to_string_lossy();
        let trimmed = text.trim_end_matches(separators);
        let trimmed = if trimmed.is_empty() { &text[..1] } else { trimmed };
        if cfg!(windows) { trimmed.to_lowercase() } else { trimmed.to_string() }
    };
    normal(a) == normal(b)
}

/// `vars` (a child's environment) with PATH set to [`search_path`].
pub fn with_search_path(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    set_path(vars, search_path())
}

fn set_path(vars: impl IntoIterator<Item = (OsString, OsString)>, path: OsString) -> Vec<(OsString, OsString)> {
    let is_path = |name: &OsStr| if cfg!(windows) { name.eq_ignore_ascii_case("PATH") } else { name == "PATH" };
    let mut vars: Vec<(OsString, OsString)> = vars.into_iter().collect();
    match vars.iter_mut().find(|(name, _)| is_path(name)) {
        Some((_, value)) => *value = path,
        None if !path.is_empty() => vars.push(("PATH".into(), path)),
        None => {}
    }
    vars
}

#[cfg(unix)]
fn read_extra_dirs() -> Vec<PathBuf> {
    let Some(shell) = shell::user_shell() else { return Vec::new() };
    shell::login_path(&shell, shell::TIMEOUT).map_or_else(Vec::new, |path| std::env::split_paths(&path).collect())
}

#[cfg(windows)]
fn read_extra_dirs() -> Vec<PathBuf> {
    registry::saved_path()
}

#[cfg(not(any(unix, windows)))]
fn read_extra_dirs() -> Vec<PathBuf> {
    Vec::new()
}

// ---------------------------------------------------------------------------------------------
// Finding a file

/// File names to look for: `stem` with each of `extensions` ("" is the bare name). Extensions
/// listed in `pathext` (Windows' PATHEXT) come first, in its order; the rest keep theirs.
pub fn file_names(stem: &str, extensions: &[&str], pathext: Option<&OsStr>) -> Vec<String> {
    let listed: Vec<String> = pathext
        .map(|pathext| pathext.to_string_lossy().split(';').map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase()).collect())
        .unwrap_or_default();
    let rank = |ext: &str| listed.iter().position(|known| !ext.is_empty() && known.eq_ignore_ascii_case(ext)).unwrap_or(usize::MAX);
    let mut ordered = extensions.to_vec();
    ordered.sort_by_key(|ext| rank(ext));
    ordered.into_iter().map(|ext| if ext.is_empty() { stem.to_string() } else { format!("{stem}.{ext}") }).collect()
}

/// The first candidate `accept` takes, trying each name in each absolute directory of `path` in order.
pub fn find_in(path: &OsStr, names: &[String], accept: impl Fn(&Path) -> Option<PathBuf>) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| PathBuf::from(dir.to_string_lossy().trim_matches('"')))
        .filter(|dir| dir.is_absolute())
        .find_map(|dir| names.iter().find_map(|name| accept(&dir.join(name))))
}

// ---------------------------------------------------------------------------------------------
// Login shell (macOS, Linux)

#[cfg(unix)]
mod shell {
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    pub const TIMEOUT: Duration = Duration::from_secs(3);
    const BEGIN: &str = "__CLUELYRS_PATH_BEGIN__";
    const END: &str = "__CLUELYRS_PATH_END__";
    /// The only command given to the shell: a constant, so no outside text is ever parsed by it.
    /// `sh` prints PATH so every login shell (fish and nushell included) reports it the same way.
    const COMMAND: &str = "/bin/sh -c 'printf \"%s%s%s\" __CLUELYRS_PATH_BEGIN__ \"$PATH\" __CLUELYRS_PATH_END__'";
    const MAX_OUTPUT: u64 = 256 * 1024;

    /// $SHELL, or the account's shell when an app wasn't given one.
    pub fn user_shell() -> Option<PathBuf> {
        let usable = |path: &PathBuf| path.is_absolute() && path.is_file();
        std::env::var_os("SHELL").map(PathBuf::from).filter(usable).or_else(|| account_shell().filter(usable))
    }

    fn account_shell() -> Option<PathBuf> {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buffer = vec![0 as libc::c_char; 16 * 1024];
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer refers to live, correctly sized storage owned by this frame.
        let status = unsafe { libc::getpwuid_r(libc::getuid(), &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut found) };
        if status != 0 || found.is_null() || entry.pw_shell.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r succeeded, so pw_shell points at a NUL-terminated string in `buffer`.
        let shell = unsafe { CStr::from_ptr(entry.pw_shell) };
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(shell.to_bytes())))
    }

    /// The PATH an interactive login `shell` sets up, or `None` if it can't be read within `timeout`.
    pub fn login_path(shell: &Path, timeout: Duration) -> Option<String> {
        use std::os::unix::process::CommandExt;
        let deadline = Instant::now() + timeout;
        let mut command = Command::new(shell);
        command.args(["-l", "-i", "-c", COMMAND]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        // Its own process group, so a timeout also stops whatever its startup files started.
        command.process_group(0);
        let mut child = command.spawn().ok()?;
        let stdout = child.stdout.take()?;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = stdout.take(MAX_OUTPUT).read_to_end(&mut output);
            let _ = sender.send(output);
        });
        let output = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok();
        finish(&mut child, deadline);
        parse(&String::from_utf8_lossy(&output?)).map(str::to_string)
    }

    /// Lets the shell exit until `deadline`, then stops its process group.
    fn finish(child: &mut Child, deadline: Instant) {
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if Instant::now() >= deadline => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        // The child isn't reaped yet, so its pid (the group id) can't have been reused.
        if let Ok(group) = libc::pid_t::try_from(child.id()) {
            // SAFETY: plain syscall on a process group this function created.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let _ = child.wait();
    }

    /// The text between the last pair of markers, ignoring anything startup files printed around it.
    pub fn parse(output: &str) -> Option<&str> {
        let end = output.rfind(END)?;
        let start = output[..end].rfind(BEGIN)? + BEGIN.len();
        Some(&output[start..end]).filter(|path| !path.trim().is_empty())
    }

    #[cfg(test)]
    pub fn markers() -> (&'static str, &'static str) {
        (BEGIN, END)
    }
}

// ---------------------------------------------------------------------------------------------
// Registry (Windows)

#[cfg(windows)]
mod registry {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_EXPAND_SZ, REG_VALUE_TYPE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
        RegGetValueW,
    };
    use windows::core::{PCWSTR, w};

    /// The system PATH followed by the user's, as saved now (Windows builds a new session's PATH the
    /// same way), with `%VARIABLE%` references expanded.
    pub fn saved_path() -> Vec<PathBuf> {
        let system = read(HKEY_LOCAL_MACHINE, w!("SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment"));
        let user = read(HKEY_CURRENT_USER, w!("Environment"));
        [system, user].into_iter().flatten().flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>()).collect()
    }

    fn read(root: HKEY, key: PCWSTR) -> Option<OsString> {
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut kind = REG_VALUE_TYPE::default();
        let mut size = 0u32;
        // SAFETY: a size query; the out-pointers refer to locals.
        unsafe { RegGetValueW(root, key, w!("Path"), flags, Some(&mut kind), None, Some(&mut size)) }.ok().ok()?;
        let mut buffer = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut bytes = u32::try_from(buffer.len() * 2).ok()?;
        // SAFETY: `buffer` holds `bytes` bytes.
        unsafe { RegGetValueW(root, key, w!("Path"), flags, Some(&mut kind), Some(buffer.as_mut_ptr().cast()), Some(&mut bytes)) }.ok().ok()?;
        buffer.truncate(bytes as usize / 2);
        while buffer.last() == Some(&0) {
            buffer.pop();
        }
        if kind == REG_EXPAND_SZ { expand(&buffer) } else { Some(OsString::from_wide(&buffer)) }
    }

    fn expand(raw: &[u16]) -> Option<OsString> {
        let source: Vec<u16> = raw.iter().copied().chain([0]).collect();
        // SAFETY: `source` is NUL-terminated; the first call only measures.
        let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None) };
        if needed == 0 {
            return None;
        }
        let mut expanded = vec![0u16; needed as usize];
        // SAFETY: `expanded` is as large as the measured result.
        let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(&mut expanded)) };
        if written == 0 || written > needed {
            return None;
        }
        expanded.truncate(written as usize - 1);
        Some(OsString::from_wide(&expanded))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cluelyrs-cli-path-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pathext_orders_the_names_tried_in_each_directory() {
        let today = ["exe", "cmd", "ps1", ""];
        let default = OsString::from(".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC");
        assert_eq!(file_names("codex", &today, Some(&default)), ["codex.exe", "codex.cmd", "codex.ps1", "codex"], "unchanged with the default PATHEXT");
        let cmd_first = OsString::from(".cmd;.Exe");
        assert_eq!(file_names("codex", &today, Some(&cmd_first)), ["codex.cmd", "codex.exe", "codex.ps1", "codex"]);
        assert_eq!(file_names("codex", &today, None), ["codex.exe", "codex.cmd", "codex.ps1", "codex"], "no PATHEXT keeps the given order");
        assert_eq!(file_names("claude", &["exe"], Some(&default)), ["claude.exe"], "PATHEXT never adds names");
    }

    #[test]
    fn find_in_tries_each_name_in_each_absolute_directory_in_order() {
        let first = scratch("find-first");
        let second = scratch("find-second");
        std::fs::write(second.join("tool.cmd"), "").unwrap();
        std::fs::write(second.join("tool.exe"), "").unwrap();
        let path = std::env::join_paths([PathBuf::from("relative"), first.clone(), second.clone()]).unwrap();
        let names = vec!["tool.exe".to_string(), "tool.cmd".to_string()];
        let exists = |candidate: &Path| candidate.is_file().then(|| candidate.to_path_buf());
        assert_eq!(find_in(&path, &names, exists), Some(second.join("tool.exe")));
        assert_eq!(find_in(&path, &names, |candidate: &Path| exists(candidate).filter(|found| found.ends_with("tool.cmd"))), Some(second.join("tool.cmd")));
        assert_eq!(find_in(&path, &["other".to_string()], exists), None);
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
    }

    #[test]
    fn search_path_keeps_the_process_path_and_appends_new_directories() {
        let root = PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
        let (usr, brew, npm) = (root.join("usr").join("bin"), root.join("opt").join("homebrew").join("bin"), root.join("npm"));
        let process = std::env::join_paths([&usr]).unwrap();
        let extra = [usr.clone(), brew.clone(), PathBuf::from("relative"), brew.clone(), npm.clone()];
        assert_eq!(append_dirs(Some(&process), &extra), std::env::join_paths([&usr, &brew, &npm]).unwrap());
        assert_eq!(append_dirs(Some(&process), &[]), process, "nothing new leaves PATH exactly as it was");
        assert_eq!(append_dirs(None, std::slice::from_ref(&brew)), brew.into_os_string());
        let trailing = PathBuf::from(format!("{}{}", usr.display(), std::path::MAIN_SEPARATOR));
        assert_eq!(append_dirs(Some(&process), &[trailing]), process, "a trailing separator is the same directory");
    }

    #[test]
    fn children_get_the_search_path() {
        let vars = vec![("HOME".into(), "/home/me".into()), ("PATH".into(), "/usr/bin".into())];
        let set = set_path(vars, "/usr/bin:/opt/homebrew/bin".into());
        assert_eq!(set, [("HOME".into(), "/home/me".into()), ("PATH".into(), "/usr/bin:/opt/homebrew/bin".into())]);
        assert_eq!(set_path(Vec::new(), "/opt/bin".into()), [("PATH".into(), "/opt/bin".into())]);
        assert!(set_path(Vec::new(), OsString::new()).is_empty());
    }

    #[test]
    fn chosen_locations_are_kept_per_cli() {
        set_chosen(Cli::Claude, "  /Applications/Tools/claude  ");
        assert_eq!(chosen(Cli::Claude), Some(PathBuf::from("/Applications/Tools/claude")));
        assert_eq!(chosen(Cli::Codex), None);
        set_chosen(Cli::Claude, "");
        assert_eq!(chosen(Cli::Claude), None, "empty is automatic");
    }

    #[cfg(unix)]
    mod login_shell {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        #[test]
        fn the_path_between_markers_is_kept_and_banner_noise_dropped() {
            let (begin, end) = shell::markers();
            let output = format!("Last login: Fri Oct 9\n\x1b[1mWelcome\x1b[0m {begin}stale{end}\n{begin}/opt/homebrew/bin:/usr/bin{end}bye\n");
            assert_eq!(shell::parse(&output), Some("/opt/homebrew/bin:/usr/bin"));
            assert_eq!(shell::parse(&format!("{begin}{end}")), None, "an empty PATH is no answer");
            assert_eq!(shell::parse("no markers at all"), None);
            assert_eq!(shell::parse(&format!("{end}{begin}")), None);
        }

        #[test]
        fn a_login_shell_reports_its_path_and_a_slow_one_times_out() {
            let dir = scratch("shell");
            // A stand-in login shell: prints a banner, sets its own PATH and runs the command it's
            // given (`-l -i -c COMMAND` puts COMMAND in $4).
            let login = script(&dir, "fake-login-shell", "echo 'Welcome to the fake shell'\nPATH=/fake/bin:/usr/bin\nexport PATH\n[ \"$1 $2 $3\" = '-l -i -c' ] && /bin/sh -c \"$4\"");
            assert_eq!(shell::login_path(&login, Duration::from_secs(5)).as_deref(), Some("/fake/bin:/usr/bin"));

            let slow = script(&dir, "slow-shell", "sleep 30");
            let started = Instant::now();
            assert_eq!(shell::login_path(&slow, Duration::from_millis(300)), None);
            assert!(started.elapsed() < Duration::from_secs(5), "gave up at the timeout: {:?}", started.elapsed());

            let silent = script(&dir, "silent-shell", "echo 'no PATH here'");
            assert_eq!(shell::login_path(&silent, Duration::from_secs(5)), None);
            assert_eq!(shell::login_path(&dir.join("missing-shell"), Duration::from_secs(1)), None);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
