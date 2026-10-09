//! Answers through the user's Claude subscription using the official Claude Code CLI.
//!
//! The CLI runs in print mode with stream-json input and output, every built-in tool,
//! MCP server, customization and slash command disabled, no session persistence, an
//! owned empty working directory and an allowlisted environment that strips API-key,
//! token and cloud-provider switches so only the CLI's own claude.ai login is used.
//! The child is spawned directly (never through a shell) and its stderr is discarded.
//! Pure helpers (argv, environment, history folding, stream parsing, status parsing)
//! are kept separate from the process IO so they can be tested without a process.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::chat::{ChatRequest, Effort, SubscriptionStatus};
use crate::cli_path::{self, Cli};
use crate::providers::{Message, Part, Role};

const MODELS: [(&str, &str); 3] = [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")];

const MAX_MESSAGES: usize = 200;
const MAX_TEXT: usize = 1_000_000;
const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;
const MAX_SYSTEM: usize = 250_000;
/// Longest stdout line accepted from the CLI.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Longest answer, in characters.
const MAX_ANSWER_CHARS: usize = 200_000;
/// Windows caps the whole command line at 32,767 UTF-16 units; leave headroom.
const COMMAND_LINE_LIMIT: usize = if cfg!(windows) { 30_000 } else { 120_000 };
const STATUS_OUTPUT_LIMIT: usize = 64 * 1024;

const STREAM_TIMEOUT: Duration = Duration::from_secs(300);
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
/// How long a finished child may linger before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// Wait after a kill before the final hard kill.
const KILL_GRACE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(50);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const NOT_FOUND: &str = "Install the native Claude Code CLI, or set CLAUDE_PATH to its executable. Script launchers (.cmd, .bat, .ps1, .js) are not supported.";
const BAD_CLAUDE_PATH: &str = "CLAUDE_PATH must be the absolute path of the native Claude Code executable, without arguments.";
const COULD_NOT_START: &str = "Claude Code could not start.";
const INPUT_INVALID: &str = "Provide text or an attached image for the assistant.";
const STATUS_FAILED: &str = "Could not read Claude Code sign-in status.";
const UNSUPPORTED_AUTH: &str = "Sign in to Claude Code with your Claude subscription. API-key, Console and cloud-provider sign-in are not used here.";
const USAGE_LIMIT: &str = "Your Claude usage limit has been reached. Try again when it resets.";
const AUTH_REQUIRED: &str = "Sign in to Claude Code with your Claude subscription.";
const TURN_FAILED: &str = "Claude could not complete the response. Please try again.";
const MALFORMED: &str = "Claude Code sent malformed output.";
const LINE_TOO_LARGE: &str = "Claude Code sent a message exceeding the size limit.";
const TOO_LARGE: &str = "The response exceeded the supported size.";
const EMPTY: &str = "Claude returned an empty response.";
const REFUSED: &str = "The model declined this request.";
const EXITED_EARLY: &str = "Claude Code exited before completing the response.";
const CANCELLED: &str = "Cancelled.";

static LOGIN_ACTIVE: AtomicBool = AtomicBool::new(false);
static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct ClaudeCli;

impl ClaudeCli {
    /// Where the Claude Code CLI was found, or a user-safe reason it wasn't.
    pub fn executable() -> Result<PathBuf, String> {
        resolve_executable()
    }

    /// Reads the CLI's sign-in state with `claude auth status --json`.
    pub fn status() -> SubscriptionStatus {
        let mut status = SubscriptionStatus { models: models(), ..SubscriptionStatus::default() };
        let exe = match resolve_executable() {
            Ok(exe) => exe,
            Err(message) => {
                status.error = Some(message);
                return status;
            }
        };
        status.installed = true;
        let args = ["auth", "status", "--json"].map(String::from);
        match collect(&exe, &args, STATUS_TIMEOUT) {
            Ok(output) => parse_status(&output),
            Err(message) => {
                status.error = Some(message);
                status
            }
        }
    }

    /// Runs the CLI's own browser sign-in for a Claude subscription. Its output, which
    /// can contain sign-in URLs, is never read.
    pub fn login() -> Result<(), String> {
        if LOGIN_ACTIVE.swap(true, Ordering::SeqCst) {
            return Err("Claude sign-in is already in progress.".into());
        }
        let result = run_login();
        LOGIN_ACTIVE.store(false, Ordering::SeqCst);
        result
    }

    /// Streams one answer. `on_delta` receives answer text only; reasoning, tool and
    /// system events are never forwarded.
    pub fn stream(req: &ChatRequest, cancel: &AtomicBool, on_delta: &mut dyn FnMut(&str)) -> Result<String, String> {
        Self::stream_warm(&mut None, false, req, cancel, on_delta)
    }

    /// Start a Claude Code process for the next answer, so the CLI's boot (about 0.9 s) is
    /// already done when it's needed. Nothing is sent: the process waits for its first input,
    /// with the same restrictions as any answer. Replaces any spare with other settings.
    pub fn prepare(spare: &mut Option<Spare>, system: &str, model: Option<&str>, effort: Effort) -> Result<(), String> {
        let model = validate_model(model)?;
        if system.chars().count() > MAX_SYSTEM { return Err("The assistant instructions are too large.".into()); }
        let exe = resolve_executable()?;
        let args = fit_command_line(&exe, stream_args(system, model, effort), COMMAND_LINE_LIMIT, prompt_dir().as_deref())?;
        if spare.as_mut().is_some_and(|ready| ready.usable_for(&args)) { return Ok(()); }
        *spare = None;
        *spare = Some(Spare { running: Some(launch(&exe, &args, true, true)?), args, started: Instant::now() });
        Ok(())
    }

    /// Like [`ClaudeCli::stream`], answering with the spare process when it was started with
    /// this request's settings. With `refill`, a new spare is started for the next answer once
    /// this one ends (each answer gets a fresh process: its history is folded in as usual, so
    /// earlier turns' context never piles up inside a long-lived process).
    pub fn stream_warm(spare: &mut Option<Spare>, refill: bool, req: &ChatRequest, cancel: &AtomicBool, on_delta: &mut dyn FnMut(&str)) -> Result<String, String> {
        validate_messages(&req.messages)?;
        let model = validate_model(req.model.as_deref())?;
        if req.system.chars().count() > MAX_SYSTEM {
            return Err("The assistant instructions are too large.".into());
        }
        let exe = resolve_executable()?;
        let args = fit_command_line(&exe, stream_args(&req.system, model, req.effort), COMMAND_LINE_LIMIT, prompt_dir().as_deref())?;
        let mut line = serde_json::to_string(&fold_history(&req.messages)).map_err(|_| INPUT_INVALID.to_string())?;
        line.push('\n');
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let ready = spare.take().and_then(|mut ready| if ready.usable_for(&args) { ready.running.take() } else { None });
        let mut running = match ready { Some(running) => running, None => launch(&exe, &args, true, true)? };
        let result = run_stream(&mut running, line, cancel, on_delta);
        if let Some(id) = running.model.take() { remember_model(model.unwrap_or(DEFAULT_ALIAS), &id); }
        // After a complete turn the CLI may exit on its own within a grace period;
        // on failure or cancellation it is killed immediately.
        reap(running, if result.is_ok() { EXIT_GRACE } else { Duration::ZERO });
        if refill && let Ok(next) = launch(&exe, &args, true, true) {
            *spare = Some(Spare { running: Some(next), args, started: Instant::now() });
        }
        result
    }
}

/// A Claude Code process started ahead of time for one answer (see [`ClaudeCli::prepare`]).
/// It holds no conversation; it is killed when dropped.
pub struct Spare {
    running: Option<Running>,
    args: Vec<String>,
    started: Instant,
}

/// A spare older than this is replaced rather than trusted (sign-in or CLI state may have moved on).
const SPARE_MAX_AGE: Duration = Duration::from_secs(10 * 60);

impl Spare {
    /// Started with exactly these arguments, recently, and still waiting.
    fn usable_for(&mut self, args: &[String]) -> bool {
        self.args == args && self.started.elapsed() < SPARE_MAX_AGE
            && self.running.as_mut().is_some_and(|running| matches!(running.child.try_wait(), Ok(None)))
    }
}

impl Drop for Spare {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() { reap(running, Duration::ZERO); }
    }
}

/// The alias a request without `--model` is recorded under (the CLI picks its default model).
pub const DEFAULT_ALIAS: &str = "default";

/// Model ids the CLI resolved each alias to, as seen in answers this run (see [`resolved_models`]).
static RESOLVED: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

fn remember_model(alias: &str, id: &str) {
    let mut resolved = RESOLVED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    resolved.retain(|(known, _)| known != alias);
    resolved.push((alias.to_string(), id.to_string()));
}

/// (alias, model id) for every alias the CLI has resolved during this run, e.g. ("opus", "claude-opus-5-5").
/// The CLI only reports the model once it has a prompt, so an alias appears after its first answer.
pub fn resolved_models() -> Vec<(String, String)> {
    RESOLVED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// A model id from the CLI's `init` line, accepted only if it looks like one (it is shown in the UI).
fn plausible_model_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'[' | b']'))
}

/// A readable name for a Claude model id: "claude-opus-5-5" → "Opus 5.5", "claude-haiku-4-5-20251001" →
/// "Haiku 4.5", "claude-sonnet-5-5[1m]" → "Sonnet 5.5". Ids in any other shape are returned unchanged.
pub fn model_label(id: &str) -> String {
    let base = id.split('[').next().unwrap_or(id);
    let Some(rest) = base.strip_prefix("claude-") else { return id.to_string() };
    let parts: Vec<&str> = rest.split('-').collect();
    let Some((family, version)) = parts.split_first() else { return id.to_string() };
    let numbers: Vec<&str> = version.iter().copied().take_while(|part| part.len() <= 2 && part.bytes().all(|b| b.is_ascii_digit())).collect();
    if family.is_empty() || !family.bytes().all(|b| b.is_ascii_alphabetic()) || numbers.is_empty() { return id.to_string() }
    let mut name = family[..1].to_ascii_uppercase();
    name.push_str(&family[1..]);
    format!("{name} {}", numbers.join("."))
}

fn models() -> Vec<(String, String)> {
    MODELS.iter().map(|(id, name)| (id.to_string(), name.to_string())).collect()
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Restricted print-mode argv. Every value is its own argv entry; no shell parses it.
fn stream_args(system: &str, model: Option<&str>, effort: Effort) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // `--tools` is variadic: the empty entry disables every built-in tool and the
        // next flag ends the list.
        "--tools",
        "",
        "--strict-mcp-config",
        "--safe-mode",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--system-prompt",
    ]
    .map(String::from)
    .to_vec();
    args.push(system.to_string());
    if let Some(model) = model {
        args.push("--model".into());
        args.push(model.to_string());
    }
    if let Some(level) = effort_level(model, effort) {
        args.push("--effort".into());
        args.push(level.into());
    }
    args
}

/// `--effort` for the request: the least thinking for normal answers, high in Smart mode.
/// Haiku is left at its default (it is the fast model already and may not take the flag).
fn effort_level(model: Option<&str>, effort: Effort) -> Option<&'static str> {
    if model.is_some_and(|model| model.to_ascii_lowercase().contains("haiku")) { return None; }
    Some(match effort { Effort::Fast => "low", Effort::Smart => "high" })
}

/// Upper bound of the quoted Windows command line, in UTF-16 units: each argument may
/// be wrapped in quotes and have every quote and backslash escaped.
fn command_line_len(exe: &Path, args: &[String]) -> usize {
    let quoted = |text: &str| {
        text.encode_utf16().count() + 3 + text.chars().filter(|c| *c == '"' || *c == '\\').count()
    };
    quoted(&exe.to_string_lossy()) + args.iter().map(|arg| quoted(arg)).sum::<usize>()
}

/// Rejects (rather than truncates) instructions that would overflow the command line.
fn check_command_line(exe: &Path, args: &[String], limit: usize) -> Result<(), String> {
    if command_line_len(exe, args) > limit {
        return Err("The assistant instructions are too long for the Claude Code command line.".into());
    }
    Ok(())
}

/// Prompt files older than this are deleted when another is written. Far longer than any
/// process waits before reading its own (the CLI reads it at startup).
const PROMPT_FILE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Where instructions too long for the command line are written for the CLI to read.
fn prompt_dir() -> Option<PathBuf> { dirs::data_local_dir().map(|dir| dir.join("CluelyRS").join("prompts")) }

/// `args` as they are when they fit within `limit`. Otherwise (long instructions, such as a mode
/// with files, against Windows' ~32k-character command line) the instructions are written to a
/// file in `dir`, named by their hash so the same instructions always give the same arguments,
/// and passed with `--system-prompt-file`.
fn fit_command_line(exe: &Path, mut args: Vec<String>, limit: usize, dir: Option<&Path>) -> Result<Vec<String>, String> {
    if command_line_len(exe, &args) <= limit { return Ok(args); }
    let (Some(at), Some(dir)) = (args.iter().position(|arg| arg == "--system-prompt"), dir) else {
        check_command_line(exe, &args, limit)?;
        return Ok(args);
    };
    let path = write_prompt_file(dir, &args[at + 1]).map_err(|_| "The assistant instructions could not be saved for Claude Code.".to_string())?;
    args[at] = "--system-prompt-file".into();
    args[at + 1] = path.to_string_lossy().into_owned();
    check_command_line(exe, &args, limit)?;
    Ok(args)
}

fn write_prompt_file(dir: &Path, system: &str) -> std::io::Result<PathBuf> {
    use sha2::{Digest, Sha256};
    std::fs::create_dir_all(dir)?;
    let name = format!("{}.txt", Sha256::digest(system.as_bytes()).iter().take(16).map(|byte| format!("{byte:02x}")).collect::<String>());
    for entry in std::fs::read_dir(dir)?.flatten() {
        let old = entry.metadata().and_then(|meta| meta.modified()).ok().and_then(|modified| modified.elapsed().ok()).is_some_and(|age| age > PROMPT_FILE_MAX_AGE);
        if old && entry.file_name() != name.as_str() { let _ = std::fs::remove_file(entry.path()); }
    }
    let path = dir.join(&name);
    let temporary = dir.join(format!("{name}.tmp"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    // The instructions can hold the user's files: readable by the user only.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(system.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    Ok(path)
}

const ALLOWED_ENV: &[&str] = &[
    "PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "SYSTEMDRIVE", "PROGRAMDATA",
    "HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH", "APPDATA", "LOCALAPPDATA", "USER", "LOGNAME", "USERNAME",
    "TMPDIR", "TEMP", "TMP", "LANG", "LANGUAGE", "LC_ALL", "LC_CTYPE", "TZ",
    "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS",
    "SSL_CERT_FILE", "SSL_CERT_DIR", "NODE_EXTRA_CA_CERTS", "HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY",
    "CLAUDE_CONFIG_DIR",
];
/// Removed explicitly (in addition to the allowlist) so the subscription login is always used.
const BLOCKED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY",
];

/// Allowlisted child environment (names compared case-insensitively, first wins).
fn sanitize_env<I: IntoIterator<Item = (String, String)>>(vars: I) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();
    for (name, value) in vars {
        let upper = name.to_ascii_uppercase();
        if !ALLOWED_ENV.contains(&upper.as_str())
            || BLOCKED_ENV.contains(&upper.as_str())
            || value.contains('\0')
            || result.iter().any(|(seen, _)| seen.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        result.push((name, value));
    }
    // Skip auto-update, telemetry and other traffic unrelated to the answer.
    result.push(("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into()));
    result
}

fn validate_model(model: Option<&str>) -> Result<Option<&str>, String> {
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else { return Ok(None) };
    let valid = model.len() <= 128
        && model.chars().all(|c| c.is_ascii_alphanumeric() || "._:/[]-".contains(c))
        && !model.starts_with('-');
    if valid { Ok(Some(model)) } else { Err("The selected model name is not valid.".into()) }
}

fn validate_messages(messages: &[Message]) -> Result<(), String> {
    let invalid = || INPUT_INVALID.to_string();
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err(invalid());
    }
    for message in messages {
        for part in &message.parts {
            match part {
                Part::Text(text) if text.len() > MAX_TEXT => return Err(invalid()),
                Part::Jpeg(bytes) if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES => {
                    return Err("An attached image is empty or too large.".into());
                }
                _ => {}
            }
        }
    }
    let last = messages.last().ok_or_else(invalid)?;
    let has_content = last.parts.iter().any(|part| match part {
        Part::Text(text) => !text.trim().is_empty(),
        Part::Jpeg(_) => true,
    });
    if last.role != Role::User || !has_content {
        return Err(invalid());
    }
    Ok(())
}

fn text_of(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.as_str()),
            Part::Jpeg(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Claude Code's stream-json input accepts user turns only, so earlier turns are folded
/// into a plain transcript. Only the current turn's images are attached.
fn fold_history(messages: &[Message]) -> Value {
    let Some((current, earlier)) = messages.split_last() else {
        return json!({ "type": "user", "message": { "role": "user", "content": [] } });
    };
    let transcript = earlier
        .iter()
        .map(|message| {
            let text = text_of(message);
            let images = message.parts.iter().filter(|part| matches!(part, Part::Jpeg(_))).count();
            let note = if images == 0 {
                String::new()
            } else {
                let separator = if text.is_empty() { "" } else { "\n\n" };
                let plural = if images == 1 { "" } else { "s" };
                format!("{separator}[{images} earlier image{plural} not repeated]")
            };
            let speaker = if message.role == Role::User { "User: " } else { "Assistant: " };
            format!("{speaker}{text}{note}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let current_text = text_of(current);
    let text = if transcript.is_empty() { current_text } else { format!("{transcript}\n\nUser: {current_text}") };
    let mut content = Vec::new();
    if !text.trim().is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    for part in &current.parts {
        if let Part::Jpeg(bytes) = part {
            content.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                },
            }));
        }
    }
    json!({ "type": "user", "message": { "role": "user", "content": content } })
}

/// What one stdout line means for the answer.
#[derive(Debug, PartialEq)]
enum LineEvent {
    Text(String),
    /// The `system`/`init` line: the model id the CLI resolved this session to.
    Model(String),
    /// Final `result`: the fallback answer text on success, or a user-safe error.
    Finished(Result<Option<String>, String>),
    Ignore,
}

fn classify(message: &Value) -> Result<LineEvent, String> {
    let object = message.as_object().ok_or_else(|| MALFORMED.to_string())?;
    match object.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            // Sub-agent output (never expected with tools disabled) is not part of the answer.
            if object.get("parent_tool_use_id").is_some_and(|id| !id.is_null()) {
                return Ok(LineEvent::Ignore);
            }
            let event = &object["event"];
            if event["type"] == "content_block_delta"
                && event["delta"]["type"] == "text_delta"
                && let Some(text) = event["delta"]["text"].as_str()
            {
                return Ok(LineEvent::Text(text.to_string()));
            }
            Ok(LineEvent::Ignore)
        }
        Some("result") => {
            if object.get("is_error") == Some(&Value::Bool(true)) || message["subtype"] != "success" {
                return Ok(LineEvent::Finished(Err(result_failure(message))));
            }
            if message["stop_reason"] == "refusal" {
                return Ok(LineEvent::Finished(Err(REFUSED.into())));
            }
            Ok(LineEvent::Finished(Ok(message["result"].as_str().map(String::from))))
        }
        Some("system") if message["subtype"] == "init" => Ok(match message["model"].as_str().filter(|id| plausible_model_id(id)) {
            Some(id) => LineEvent::Model(id.to_string()),
            None => LineEvent::Ignore,
        }),
        // System, assistant snapshots, thinking and tool events are never forwarded.
        _ => Ok(LineEvent::Ignore),
    }
}

/// Maps a failed `result` to a short message without echoing any of its text.
fn result_failure(message: &Value) -> String {
    let status = message["api_error_status"].as_u64();
    let mut detail: String = message["result"].as_str().unwrap_or_default().chars().take(4000).collect();
    if let Some(errors) = message["errors"].as_array() {
        for error in errors.iter().filter_map(Value::as_str) {
            detail.push(' ');
            detail.extend(error.chars().take(4000));
        }
    }
    let detail = detail.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| detail.contains(needle));
    if status == Some(429)
        || has(&[
            "usage limit", "rate limit", "rate-limit", "ratelimit", "limit reached", "limit exceeded", "quota",
            "out of usage", "out of extra usage", "weekly limit",
        ])
    {
        return USAGE_LIMIT.into();
    }
    if matches!(status, Some(401 | 403))
        || has(&[
            "not logged in", "/login", "login", "log in", "sign in", "signin", "authenticat", "unauthorized",
            "unauthorised", "invalid api key", "oauth", "token expired", "token has expired", "credential",
        ])
    {
        return AUTH_REQUIRED.into();
    }
    TURN_FAILED.into()
}

/// Incremental stream-json parser: splits stdout into lines across arbitrary chunk
/// boundaries, enforces the line and answer caps and accumulates the answer text.
struct StreamParser {
    buffer: Vec<u8>,
    text: String,
    /// The model the CLI reported in its `init` line.
    model: Option<String>,
    chars: usize,
    max_line: usize,
    max_chars: usize,
}

impl StreamParser {
    fn new() -> Self {
        Self::with_limits(MAX_LINE_BYTES, MAX_ANSWER_CHARS)
    }

    fn with_limits(max_line: usize, max_chars: usize) -> Self {
        Self { buffer: Vec::new(), text: String::new(), model: None, chars: 0, max_line, max_chars }
    }

    /// Returns `Ok(true)` once the final successful result has been seen.
    fn feed(&mut self, chunk: &[u8], on_delta: &mut dyn FnMut(&str)) -> Result<bool, String> {
        self.buffer.extend_from_slice(chunk);
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            let decoded = std::str::from_utf8(&line[..newline]).map_err(|_| MALFORMED.to_string())?.trim();
            if decoded.is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(decoded).map_err(|_| MALFORMED.to_string())?;
            match classify(&message)? {
                LineEvent::Ignore => {}
                LineEvent::Model(id) => self.model = Some(id),
                LineEvent::Text(text) => self.deliver(&text, on_delta)?,
                LineEvent::Finished(Err(message)) => return Err(message),
                LineEvent::Finished(Ok(fallback)) => {
                    if self.text.is_empty()
                        && let Some(fallback) = fallback
                    {
                        self.deliver(&fallback, on_delta)?;
                    }
                    if self.text.trim().is_empty() {
                        return Err(EMPTY.into());
                    }
                    return Ok(true);
                }
            }
        }
        if self.buffer.len() > self.max_line {
            return Err(LINE_TOO_LARGE.into());
        }
        Ok(false)
    }

    fn deliver(&mut self, text: &str, on_delta: &mut dyn FnMut(&str)) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        let count = text.chars().count();
        if self.chars + count > self.max_chars {
            return Err(TOO_LARGE.into());
        }
        self.chars += count;
        self.text.push_str(text);
        on_delta(text);
        Ok(())
    }
}

/// Signed in only for a claude.ai subscription on Anthropic's first-party API.
fn parse_status(output: &str) -> SubscriptionStatus {
    let mut status = SubscriptionStatus { installed: true, models: models(), ..SubscriptionStatus::default() };
    let parsed: Option<Value> = serde_json::from_str(output.trim()).ok();
    let Some(object) = parsed.as_ref().and_then(Value::as_object) else {
        status.error = Some(STATUS_FAILED.into());
        return status;
    };
    if object.get("loggedIn") != Some(&Value::Bool(true)) {
        return status;
    }
    let provider = object.get("apiProvider");
    let subscription = object.get("authMethod").and_then(Value::as_str) == Some("claude.ai")
        && (provider.is_none() || provider.and_then(Value::as_str) == Some("firstParty"));
    if !subscription {
        status.error = Some(UNSUPPORTED_AUTH.into());
        return status;
    }
    status.signed_in = true;
    let short = |key: &str, max: usize| {
        object.get(key).and_then(Value::as_str).filter(|text| !text.is_empty()).map(|text| text.chars().take(max).collect())
    };
    status.account = Some(short("email", 254).unwrap_or_else(|| "Claude subscription".to_string()));
    status.plan = short("subscriptionType", 64);
    status
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
}

/// Accepts only the native executable: `.exe` on Windows; no script launchers elsewhere.
fn native_executable(path: &Path) -> bool {
    if cfg!(windows) {
        has_extension(path, "exe")
    } else {
        !["js", "mjs", "cjs", "cmd", "bat", "ps1", "sh"].iter().any(|ext| has_extension(path, ext))
    }
}

/// Resolution order: the location chosen in Settings when it is usable, CLAUDE_PATH,
/// ~/.local/bin, then PATH. `usable` checks that a candidate is an existing native executable.
fn resolve_with(
    chosen: Option<&Path>,
    claude_path: Option<OsString>,
    home: Option<PathBuf>,
    path_var: Option<OsString>,
    usable: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    if let Some(chosen) = chosen.filter(|path| path.is_absolute() && usable(path)) {
        return Ok(chosen.to_path_buf());
    }
    if let Some(supplied) = claude_path.filter(|value| !value.is_empty()) {
        let supplied = PathBuf::from(supplied);
        let text = supplied.to_string_lossy();
        if !supplied.is_absolute() || text.contains(['\0', '\r', '\n']) {
            return Err(BAD_CLAUDE_PATH.into());
        }
        return if usable(&supplied) { Ok(supplied) } else { Err(NOT_FOUND.into()) };
    }
    let binary = if cfg!(windows) { "claude.exe" } else { "claude" };
    let accept = |candidate: &Path| usable(candidate).then(|| candidate.to_path_buf());
    let local = home.filter(|home| home.is_absolute()).map(|home| home.join(".local").join("bin").join(binary));
    local.and_then(|local| accept(&local))
        .or_else(|| cli_path::find_in(&path_var?, &[binary.to_string()], accept))
        .ok_or_else(|| NOT_FOUND.into())
}

// ---------------------------------------------------------------------------
// Process IO
// ---------------------------------------------------------------------------

fn usable_on_disk(path: &Path) -> bool {
    if !path.is_file() || !native_executable(path) {
        return false;
    }
    // A link must also point at a native executable.
    if let Ok(target) = std::fs::canonicalize(path)
        && !native_executable(&target)
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path).map(|meta| meta.permissions().mode() & 0o111 == 0).unwrap_or(true) {
            return false;
        }
    }
    true
}

fn resolve_executable() -> Result<PathBuf, String> {
    let resolve = || {
        let chosen = cli_path::chosen(Cli::Claude);
        resolve_with(chosen.as_deref(), std::env::var_os("CLAUDE_PATH"), dirs::home_dir(), Some(cli_path::search_path()), &usable_on_disk)
    };
    cli_path::resolve_with_retry(resolve, |message| message == NOT_FOUND)
}

/// An owned, empty temporary working directory, removed non-recursively on drop.
struct Workspace(PathBuf);

impl Workspace {
    fn create() -> Result<Self, String> {
        let base = std::env::temp_dir();
        for _ in 0..8 {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
            let counter = WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = base.join(format!("cluelyrs-claude-{}-{nanos}-{counter}", std::process::id()));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => break,
            }
        }
        Err(COULD_NOT_START.into())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Remove only the empty owned directory; never recurse.
        let _ = std::fs::remove_dir(&self.0);
    }
}

struct Running {
    child: Child,
    workspace: Workspace,
    /// The model the CLI reported for this process (from its `init` line), once seen.
    model: Option<String>,
}

fn launch(exe: &Path, args: &[String], stdin: bool, stdout: bool) -> Result<Running, String> {
    let workspace = Workspace::create()?;
    // PATH is the one `cli_path` searched, so the CLI finds the tools its install relies on.
    let vars = cli_path::with_search_path(std::env::vars_os()).into_iter().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    let mut command = Command::new(exe);
    command
        .args(args)
        .current_dir(&workspace.0)
        .env_clear()
        .envs(sanitize_env(vars))
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(if stdout { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound { NOT_FOUND.to_string() } else { COULD_NOT_START.to_string() }
    })?;
    Ok(Running { child, workspace, model: None })
}

/// Kills the child, then kills again after `KILL_GRACE` if it is still alive.
fn terminate(child: &mut Child) {
    let _ = child.kill();
    if !wait_until(child, KILL_GRACE) {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Returns true once the child has exited (or can no longer be observed).
fn wait_until(child: &mut Child, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return true,
            Ok(None) if Instant::now() >= deadline => return false,
            Ok(None) => thread::sleep(POLL.min(limit)),
        }
    }
}

/// Lets the child exit within `grace`, then terminates it, and removes its workspace,
/// all off the caller's thread.
fn reap(mut running: Running, grace: Duration) {
    if grace.is_zero() {
        let _ = running.child.kill();
    }
    thread::spawn(move || {
        if !wait_until(&mut running.child, grace) {
            terminate(&mut running.child);
        }
        drop(running.workspace);
    });
}

fn run_stream(running: &mut Running, line: String, cancel: &AtomicBool, on_delta: &mut dyn FnMut(&str)) -> Result<String, String> {
    let mut stdin = running.child.stdin.take().ok_or_else(|| COULD_NOT_START.to_string())?;
    let mut stdout = running.child.stdout.take().ok_or_else(|| COULD_NOT_START.to_string())?;
    // Written on its own thread so a large image payload cannot deadlock against stdout.
    // Dropping stdin afterwards ends the single-message input.
    thread::spawn(move || {
        let _ = stdin.write_all(line.as_bytes());
        let _ = stdin.flush();
    });
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if sender.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let started = Instant::now();
    let mut parser = StreamParser::new();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        if started.elapsed() > STREAM_TIMEOUT {
            return Err("Claude did not finish responding in time.".into());
        }
        match receiver.recv_timeout(POLL) {
            Ok(chunk) => {
                let finished = parser.feed(&chunk, on_delta);
                if let Some(id) = parser.model.take() { running.model = Some(id); }
                if finished? {
                    return Ok(parser.text);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(EXITED_EARLY.into()),
        }
    }
}

/// Runs a short command and returns bounded stdout (exit status is not required:
/// `auth status` reports a signed-out state as JSON too).
fn collect(exe: &Path, args: &[String], timeout: Duration) -> Result<String, String> {
    let mut running = launch(exe, args, false, true)?;
    let mut stdout = running.child.stdout.take().ok_or_else(|| STATUS_FAILED.to_string())?;
    let (sender, receiver) = mpsc::channel::<Option<Vec<u8>>>();
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    output.extend_from_slice(&buffer[..read]);
                    if output.len() > STATUS_OUTPUT_LIMIT {
                        let _ = sender.send(None);
                        return;
                    }
                }
            }
        }
        let _ = sender.send(Some(output));
    });
    let result = match receiver.recv_timeout(timeout) {
        Ok(Some(output)) => String::from_utf8(output).map_err(|_| STATUS_FAILED.to_string()),
        Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(STATUS_FAILED.to_string()),
        Err(mpsc::RecvTimeoutError::Timeout) => Err("Claude Code did not respond in time.".to_string()),
    };
    let grace = if result.is_ok() { EXIT_GRACE } else { Duration::ZERO };
    reap(running, grace);
    result
}

fn run_login() -> Result<(), String> {
    let exe = resolve_executable()?;
    let args = ["auth", "login", "--claudeai"].map(String::from);
    let mut running = launch(&exe, &args, false, false).map_err(|_| "Claude sign-in could not start.".to_string())?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        match running.child.try_wait() {
            Ok(Some(status)) => {
                drop(running);
                return if status.success() { Ok(()) } else { Err("Claude sign-in did not complete.".into()) };
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(200)),
            Ok(None) => {
                reap(running, Duration::ZERO);
                return Err("Claude sign-in did not finish in time.".into());
            }
            Err(_) => {
                reap(running, Duration::ZERO);
                return Err("Claude sign-in did not complete.".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str, images: usize) -> Message {
        let mut parts = vec![Part::Text(text.into())];
        parts.extend((0..images).map(|i| Part::Jpeg(vec![0xFF, 0xD8, i as u8])));
        Message { role: Role::User, parts }
    }

    fn assistant(text: &str) -> Message {
        Message { role: Role::Assistant, parts: vec![Part::Text(text.into())] }
    }

    fn line(value: Value) -> String {
        format!("{value}\n")
    }

    fn delta(text: &str) -> Value {
        json!({ "type": "stream_event", "event": { "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": text } } })
    }

    fn feed_all(parser: &mut StreamParser, input: &str, chunk: usize) -> (Result<bool, String>, Vec<String>) {
        let mut deltas = Vec::new();
        let mut result = Ok(false);
        for piece in input.as_bytes().chunks(chunk) {
            result = parser.feed(piece, &mut |text: &str| deltas.push(text.to_string()));
            if !matches!(result, Ok(false)) {
                break;
            }
        }
        (result, deltas)
    }

    #[test]
    fn argv_is_restricted_and_ordered() {
        let args = stream_args("Be brief.", Some("haiku"), Effort::Fast);
        assert_eq!(
            args,
            [
                "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
                "--include-partial-messages", "--tools", "", "--strict-mcp-config", "--safe-mode",
                "--disable-slash-commands", "--no-session-persistence", "--system-prompt", "Be brief.", "--model", "haiku",
            ]
        );
        let tools = args.iter().position(|arg| arg == "--tools").unwrap();
        assert_eq!(args[tools + 1], "", "an empty element disables every tool");
        assert!(!stream_args("x", None, Effort::Fast).contains(&"--model".to_string()));
        // Normal answers think least; Smart mode thinks more; Haiku keeps its default.
        let fast = stream_args("x", Some("sonnet"), Effort::Fast);
        assert_eq!(fast[fast.len() - 2..], ["--effort", "low"]);
        let smart = stream_args("x", None, Effort::Smart);
        assert_eq!(smart[smart.len() - 2..], ["--effort", "high"]);
        assert!(!args.contains(&"--effort".to_string()));
    }

    #[test]
    fn model_names_are_validated() {
        assert_eq!(validate_model(None), Ok(None));
        assert_eq!(validate_model(Some("  ")), Ok(None));
        assert_eq!(validate_model(Some("claude-sonnet-4-5[1m]")), Ok(Some("claude-sonnet-4-5[1m]")));
        assert!(validate_model(Some("--dangerously-skip-permissions")).is_err());
        assert!(validate_model(Some("a b")).is_err());
        assert!(validate_model(Some(&"x".repeat(129))).is_err());
    }

    #[test]
    fn command_line_length_is_guarded() {
        let exe = Path::new(r"C:\Users\me\.local\bin\claude.exe");
        assert!(check_command_line(exe, &stream_args("short", None, Effort::Fast), 30_000).is_ok());
        let error = check_command_line(exe, &stream_args(&"x".repeat(30_000), None, Effort::Fast), 30_000).unwrap_err();
        assert!(error.contains("too long"));
        // Quotes and backslashes count toward the escaped length.
        let plain = command_line_len(exe, &["aaaa".into()]);
        let escaped = command_line_len(exe, &["a\"\\a".into()]);
        assert_eq!(escaped, plain + 2);
        // Non-BMP characters take two UTF-16 units.
        assert_eq!(command_line_len(exe, &["\u{1F600}".into()]), command_line_len(exe, &["ab".into()]));
    }

    #[test]
    fn instructions_too_long_for_the_command_line_go_through_a_file() {
        let exe = Path::new(r"C:\Users\me\.local\bin\claude.exe");
        let dir = std::env::temp_dir().join(format!("cluelyrs-claude-prompts-{}", std::process::id()));
        let short = stream_args("Be brief.", Some("sonnet"), Effort::Fast);
        assert_eq!(fit_command_line(exe, short.clone(), 30_000, Some(&dir)).unwrap(), short, "short instructions stay inline");
        assert!(!dir.exists());

        let system = format!("Be brief.\n{}", "résumé ".repeat(6_000));
        let fitted = fit_command_line(exe, stream_args(&system, Some("sonnet"), Effort::Fast), 30_000, Some(&dir)).unwrap();
        assert!(!fitted.contains(&"--system-prompt".to_string()) && !fitted.contains(&system));
        let at = fitted.iter().position(|arg| arg == "--system-prompt-file").unwrap();
        let path = PathBuf::from(&fitted[at + 1]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), system);
        assert_eq!(fitted[..at], stream_args(&system, Some("sonnet"), Effort::Fast)[..at], "the other flags are unchanged");
        // The same instructions give the same arguments, so a spare started with them is reused.
        assert_eq!(fit_command_line(exe, stream_args(&system, Some("sonnet"), Effort::Fast), 30_000, Some(&dir)).unwrap(), fitted);
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).unwrap().permissions()) & 0o777, 0o600);
        // Without a folder to write to, the old error stands.
        assert!(fit_command_line(exe, stream_args(&system, None, Effort::Fast), 30_000, None).unwrap_err().contains("too long"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A spare is only used for a request with exactly its arguments, while it's still running,
    /// and is killed when dropped. A long-lived stand-in process plays the CLI waiting on stdin.
    #[cfg(windows)]
    #[test]
    fn a_spare_serves_only_matching_requests_and_dies_with_its_owner() {
        let waiting = |args: &[String]| {
            let exe = PathBuf::from(std::env::var_os("ComSpec").unwrap_or_else(|| "C:\\Windows\\System32\\cmd.exe".into()));
            let running = launch(&exe, &["/c".into(), "ping".into(), "-n".into(), "30".into(), "127.0.0.1".into()], true, true).unwrap();
            Spare { running: Some(running), args: args.to_vec(), started: Instant::now() }
        };
        let fast = stream_args("Be brief.", Some("sonnet"), Effort::Fast);
        let smart = stream_args("Be brief.", Some("sonnet"), Effort::Smart);
        let mut spare = waiting(&fast);
        assert!(spare.usable_for(&fast));
        assert!(!spare.usable_for(&smart), "Smart mode or another model needs another process");
        let mut stale = waiting(&fast);
        stale.started = Instant::now() - SPARE_MAX_AGE;
        assert!(!stale.usable_for(&fast));

        let child = spare.running.as_mut().unwrap();
        let _ = child.child.kill();
        let _ = child.child.wait();
        assert!(!spare.usable_for(&fast), "an exited process is never used");

        let mut owned = Some(waiting(&fast));
        let pid = owned.as_ref().unwrap().running.as_ref().unwrap().child.id();
        owned = None;
        drop(owned);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && process_alive(pid) { thread::sleep(Duration::from_millis(50)); }
        assert!(!process_alive(pid), "a dropped spare is killed");
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        let output = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().unwrap();
        String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
    }

    #[test]
    fn environment_is_allowlisted_and_scrubbed() {
        let source = [
            ("Path", r"C:\Windows"),
            ("PATH", r"C:\duplicate"),
            ("USERPROFILE", r"C:\Users\me"),
            ("ANTHROPIC_API_KEY", "SECRET"),
            ("anthropic_auth_token", "SECRET"),
            ("ANTHROPIC_BASE_URL", "SECRET"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "SECRET"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("CLAUDE_CODE_USE_VERTEX", "1"),
            ("CLAUDE_CODE_USE_FOUNDRY", "1"),
            ("NODE_OPTIONS", "--require x"),
            ("CLAUDE_CONFIG_DIR", r"C:\cfg"),
            ("TEMP", "bad\0value"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let env = sanitize_env(source);
        let get = |name: &str| env.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
        assert_eq!(get("PATH"), Some(r"C:\Windows"));
        assert_eq!(env.iter().filter(|(k, _)| k.eq_ignore_ascii_case("PATH")).count(), 1);
        assert_eq!(get("USERPROFILE"), Some(r"C:\Users\me"));
        assert_eq!(get("CLAUDE_CONFIG_DIR"), Some(r"C:\cfg"));
        assert_eq!(get("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"), Some("1"));
        for blocked in [
            "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY", "NODE_OPTIONS", "TEMP",
        ] {
            assert_eq!(get(blocked), None, "{blocked}");
        }
        assert!(env.iter().all(|(_, v)| !v.contains("SECRET")));
    }

    #[test]
    fn history_is_folded_with_current_images_only() {
        let messages = vec![user("First question", 1), assistant("First answer"), user("Second question", 2)];
        let sent = fold_history(&messages);
        assert_eq!(sent["type"], "user");
        assert_eq!(sent["message"]["role"], "user");
        let content = sent["message"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3, "text plus the two current images");
        assert_eq!(
            content[0],
            json!({ "type": "text", "text": "User: First question\n\n[1 earlier image not repeated]\n\nAssistant: First answer\n\nUser: Second question" })
        );
        assert_eq!(
            content[1],
            json!({ "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": "/9gA" } })
        );
        assert_eq!(content[2]["source"]["data"], "/9gB");
        assert!(!serde_json::to_string(&sent).unwrap().contains('\n'), "one stream-json line");
    }

    #[test]
    fn single_turn_is_not_prefixed_and_image_only_turns_have_no_text() {
        let sent = fold_history(&[user("Hi", 0)]);
        assert_eq!(sent["message"]["content"], json!([{ "type": "text", "text": "Hi" }]));
        let image_only = Message { role: Role::User, parts: vec![Part::Jpeg(vec![1, 2, 3])] };
        let sent = fold_history(&[image_only]);
        let content = sent["message"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "image");
        let earlier = Message { role: Role::User, parts: vec![Part::Jpeg(vec![1]), Part::Jpeg(vec![2])] };
        let sent = fold_history(&[earlier, assistant("Seen"), user("And now?", 0)]);
        assert_eq!(
            sent["message"]["content"][0]["text"],
            "User: [2 earlier images not repeated]\n\nAssistant: Seen\n\nUser: And now?"
        );
    }

    #[test]
    fn messages_are_validated() {
        assert!(validate_messages(&[]).is_err());
        assert!(validate_messages(&[user("  ", 0)]).is_err());
        assert!(validate_messages(&[user("Hi", 0), assistant("Hello")]).is_err());
        assert!(validate_messages(&[Message { role: Role::User, parts: vec![Part::Jpeg(Vec::new())] }]).is_err());
        assert!(validate_messages(&[user("Hi", 1)]).is_ok());
    }

    #[test]
    fn deltas_parse_across_split_chunks_and_non_text_events_are_ignored() {
        let input = [
            json!({ "type": "system", "subtype": "init", "tools": [] }),
            json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": "SECRET_REASONING" } } }),
            json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "input_json_delta", "partial_json": "{}" } } }),
            json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Bash" }] } }),
            json!({ "type": "stream_event", "parent_tool_use_id": "x", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "SUBAGENT" } } }),
            json!({ "type": "stream_event", "parent_tool_use_id": null, "event": { "type": "content_block_start" } }),
            delta("Héllo "),
            delta("wörld"),
            json!({ "type": "result", "subtype": "success", "is_error": false, "result": "Héllo wörld" }),
        ]
        .map(line)
        .concat();
        for chunk in [1, 3, 7, 4096] {
            let mut parser = StreamParser::new();
            let (result, deltas) = feed_all(&mut parser, &input, chunk);
            assert_eq!(result, Ok(true), "chunk size {chunk}");
            assert_eq!(parser.text, "Héllo wörld");
            assert_eq!(deltas, ["Héllo ", "wörld"]);
        }
    }

    #[test]
    fn crlf_and_blank_lines_are_tolerated() {
        let input = format!("\r\n{}\r\n\n{}", delta("ok"), line(json!({ "type": "result", "subtype": "success", "result": "ok" })));
        let mut parser = StreamParser::new();
        assert_eq!(feed_all(&mut parser, &input, 5).0, Ok(true));
        assert_eq!(parser.text, "ok");
    }

    #[test]
    fn result_text_is_the_fallback_when_no_deltas_arrived() {
        let mut parser = StreamParser::new();
        let input = line(json!({ "type": "result", "subtype": "success", "is_error": false, "result": "Whole answer" }));
        let (result, deltas) = feed_all(&mut parser, &input, 64);
        assert_eq!(result, Ok(true));
        assert_eq!(deltas, ["Whole answer"]);
    }

    #[test]
    fn result_errors_map_without_echoing_text() {
        let cases = [
            (json!({ "is_error": true, "subtype": "success", "result": "Claude AI usage limit reached|1700000000" }), USAGE_LIMIT),
            (json!({ "is_error": true, "subtype": "success", "result": "x", "api_error_status": 429 }), USAGE_LIMIT),
            (json!({ "is_error": true, "subtype": "success", "result": "Invalid API key · Please run /login" }), AUTH_REQUIRED),
            (json!({ "is_error": true, "subtype": "success", "result": "boom", "api_error_status": 401 }), AUTH_REQUIRED),
            (json!({ "is_error": false, "subtype": "error_during_execution", "errors": ["OAuth token has expired"] }), AUTH_REQUIRED),
            (json!({ "is_error": true, "subtype": "error_during_execution", "result": "PRIVATE C:\\path detail" }), TURN_FAILED),
            (json!({ "is_error": false, "subtype": "success", "stop_reason": "refusal", "result": "no" }), REFUSED),
        ];
        for (mut payload, expected) in cases {
            payload["type"] = json!("result");
            let mut parser = StreamParser::new();
            let (result, _) = feed_all(&mut parser, &line(payload.clone()), 4096);
            let error = result.unwrap_err();
            assert_eq!(error, expected, "{payload}");
            assert!(!error.contains("PRIVATE") && !error.contains("1700000000"));
        }
    }

    #[test]
    fn empty_malformed_and_oversized_output_are_rejected() {
        let mut parser = StreamParser::new();
        let empty = line(json!({ "type": "result", "subtype": "success", "is_error": false, "result": "" }));
        assert_eq!(feed_all(&mut parser, &empty, 4096).0, Err(EMPTY.into()));

        let mut parser = StreamParser::new();
        assert_eq!(feed_all(&mut parser, "not json\n", 4096).0, Err(MALFORMED.into()));
        let mut parser = StreamParser::new();
        assert_eq!(parser.feed(b"[1]\n", &mut |_| {}), Err(MALFORMED.into()));
        let mut parser = StreamParser::new();
        assert_eq!(parser.feed(b"\"\xff\"\n", &mut |_| {}), Err(MALFORMED.into()));

        let mut parser = StreamParser::with_limits(16, 1000);
        assert_eq!(parser.feed(&[b'x'; 17], &mut |_| {}), Err(LINE_TOO_LARGE.into()));

        let mut parser = StreamParser::with_limits(1024, 5);
        let input = format!("{}{}", line(delta("abc")), line(delta("def")));
        let (result, deltas) = feed_all(&mut parser, &input, 4096);
        assert_eq!(result, Err(TOO_LARGE.into()));
        assert_eq!(deltas, ["abc"]);
    }

    #[test]
    fn status_distinguishes_subscription_console_and_logged_out() {
        let subscription = parse_status(
            r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"me@example.com","subscriptionType":"max","configDirectory":"C:\\private"}"#,
        );
        assert!(subscription.installed && subscription.signed_in);
        assert_eq!(subscription.account.as_deref(), Some("me@example.com"));
        assert_eq!(subscription.plan.as_deref(), Some("max"));
        assert_eq!(subscription.error, None);
        assert_eq!(
            subscription.models,
            [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")].map(|(a, b)| (a.to_string(), b.to_string()))
        );

        let no_email = parse_status(r#"{"loggedIn":true,"authMethod":"claude.ai"}"#);
        assert!(no_email.signed_in);
        assert_eq!(no_email.account.as_deref(), Some("Claude subscription"));

        let console = parse_status(r#"{"loggedIn":true,"authMethod":"console","apiProvider":"firstParty"}"#);
        assert!(!console.signed_in);
        assert_eq!(console.error.as_deref(), Some(UNSUPPORTED_AUTH));
        let bedrock = parse_status(r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"bedrock"}"#);
        assert!(!bedrock.signed_in && bedrock.error.is_some());

        let logged_out = parse_status(r#"{"loggedIn":false,"authMethod":"none"}"#);
        assert!(logged_out.installed && !logged_out.signed_in);
        assert_eq!(logged_out.error, None);

        let garbage = parse_status("Error: something C:\\private");
        assert_eq!(garbage.error.as_deref(), Some(STATUS_FAILED));
    }

    #[test]
    fn executable_resolution_accepts_native_binaries_only() {
        let root = PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
        let binary = if cfg!(windows) { "claude.exe" } else { "claude" };
        let home = root.join("home");
        let local = home.join(".local").join("bin").join(binary);
        let bin = root.join("bin");
        let on_path = bin.join(binary);
        let path_var = std::env::join_paths([PathBuf::from("relative"), bin]).unwrap();

        let only = |target: PathBuf| move |path: &Path| path == target;
        assert_eq!(resolve_with(None, None, Some(home.clone()), None, &only(local.clone())), Ok(local.clone()));
        assert_eq!(resolve_with(None, None, Some(home.clone()), Some(path_var.clone()), &only(on_path.clone())), Ok(on_path));
        assert_eq!(resolve_with(None, None, Some(home.clone()), Some(path_var), &|_| true), Ok(local), "~/.local/bin wins");
        assert_eq!(resolve_with(None, None, None, None, &|_| true), Err(NOT_FOUND.into()));
        assert_eq!(resolve_with(None, None, Some(PathBuf::from("rel")), None, &|_| true), Err(NOT_FOUND.into()));

        let supplied = root.join("tools").join(binary);
        assert_eq!(resolve_with(None, Some(supplied.clone().into()), Some(home), None, &|_| true), Ok(supplied));
        assert_eq!(resolve_with(None, Some("claude".into()), None, None, &|_| true), Err(BAD_CLAUDE_PATH.into()));
        let script = root.join("tools").join("claude.cmd");
        assert_eq!(resolve_with(None, Some(script.clone().into()), None, None, &|path| native_executable(path)), Err(NOT_FOUND.into()));

        for name in ["claude.cmd", "claude.ps1", "cli.js", "claude.BAT"] {
            assert!(!native_executable(Path::new(name)), "{name}");
        }
        if cfg!(windows) {
            assert!(native_executable(Path::new(r"C:\x\CLAUDE.EXE")));
            assert!(!native_executable(Path::new(r"C:\x\claude")));
        }
    }

    #[test]
    fn a_usable_chosen_location_comes_first() {
        let root = PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
        let binary = if cfg!(windows) { "claude.exe" } else { "claude" };
        let chosen = root.join("chosen").join(binary);
        let supplied = root.join("tools").join(binary);
        let usable = |path: &Path| path != root.join("gone").join(binary);
        assert_eq!(resolve_with(Some(&chosen), Some(supplied.clone().into()), None, None, &usable), Ok(chosen));
        // Gone or relative: skipped for CLAUDE_PATH and the automatic search, not an error.
        assert_eq!(resolve_with(Some(&root.join("gone").join(binary)), Some(supplied.clone().into()), None, None, &usable), Ok(supplied.clone()));
        assert_eq!(resolve_with(Some(Path::new(binary)), Some(supplied.clone().into()), None, None, &usable), Ok(supplied));
    }

    #[cfg(unix)]
    #[test]
    fn a_native_executable_on_the_search_path_is_found() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("cluelyrs-claude-path-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, "#!/bin/sh\nexit 0\n").unwrap();
        let path_var = std::env::join_paths([dir.join("empty"), bin.clone()]).unwrap();
        assert_eq!(resolve_with(None, None, Some(dir.join("home")), Some(path_var.clone()), &usable_on_disk), Err(NOT_FOUND.into()), "not executable");
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_with(None, None, Some(dir.join("home")), Some(path_var), &usable_on_disk), Ok(claude));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_is_created_empty_and_removed() {
        let workspace = Workspace::create().unwrap();
        let path = workspace.0.clone();
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        drop(workspace);
        assert!(!path.exists());
    }

    /// Live end-to-end checks against the real CLI. They consume subscription usage, so
    /// they run only when explicitly requested with `CLUELYRS_CLAUDE_LIVE=1` and
    /// `--ignored` (`live_image` also needs `CLUELYRS_CLAUDE_LIVE_JPEG`).
    fn live_enabled() -> bool {
        std::env::var("CLUELYRS_CLAUDE_LIVE").as_deref() == Ok("1")
    }

    #[test]
    #[ignore = "live: consumes Claude subscription usage"]
    fn live_text() {
        if !live_enabled() {
            return;
        }
        let req = ChatRequest {
            system: "Answer in one short line.".into(),
            messages: vec![user("Reply with exactly: pong", 0)],
            model: Some("haiku".into()),
            effort: Effort::Fast,
        };
        let mut deltas = 0;
        let answer = ClaudeCli::stream(&req, &AtomicBool::new(false), &mut |_| deltas += 1);
        println!("live_text: {answer:?} ({deltas} deltas)");
        assert!(answer.is_ok());
    }

    #[test]
    #[ignore = "live: consumes Claude subscription usage"]
    fn live_image() {
        let Ok(path) = std::env::var("CLUELYRS_CLAUDE_LIVE_JPEG") else { return };
        if !live_enabled() {
            return;
        }
        let jpeg = std::fs::read(path).unwrap();
        let req = ChatRequest {
            system: "Answer in one short line.".into(),
            messages: vec![Message {
                role: Role::User,
                parts: vec![Part::Text("What colour is this image?".into()), Part::Jpeg(jpeg)],
            }],
            model: Some("haiku".into()),
            effort: Effort::Fast,
        };
        let answer = ClaudeCli::stream(&req, &AtomicBool::new(false), &mut |_| {});
        println!("live_image: {answer:?}");
        assert!(answer.is_ok());
    }

    #[test]
    fn model_ids_read_as_family_and_version() {
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-sonnet-5-5"), "Sonnet 5.5");
        assert_eq!(model_label("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_label("claude-sonnet-5-5[1m]"), "Sonnet 5.5");
        assert_eq!(model_label("claude-fable-5-1"), "Fable 5.1");
        for unknown in ["gpt-6.1-sol", "claude-", "claude-opus", "claude-opus-latest", "something"] {
            assert_eq!(model_label(unknown), unknown);
        }
    }

    #[test]
    fn the_init_line_reports_the_resolved_model_and_odd_ids_are_ignored() {
        let mut parser = StreamParser::new();
        let input = format!("{}{}{}", line(json!({ "type": "system", "subtype": "init", "model": "claude-opus-5-5", "tools": [] })),
            line(delta("Hi")), line(json!({ "type": "result", "subtype": "success", "is_error": false, "result": "Hi" })));
        assert_eq!(parser.feed(input.as_bytes(), &mut |_| {}), Ok(true));
        assert_eq!(parser.model.as_deref(), Some("claude-opus-5-5"));
        let odd = line(json!({ "type": "system", "subtype": "init", "model": "<script>alert(1)</script>" }));
        let mut parser = StreamParser::new();
        assert_eq!(parser.feed(odd.as_bytes(), &mut |_| {}), Ok(false));
        assert_eq!(parser.model, None);
        remember_model("opus", "claude-opus-5-5");
        remember_model("opus", "claude-opus-5-6");
        assert_eq!(resolved_models().iter().filter(|(alias, _)| alias == "opus").collect::<Vec<_>>(), [&("opus".to_string(), "claude-opus-5-6".to_string())]);
    }
}
