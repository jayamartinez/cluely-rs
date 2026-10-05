//! Streaming chat over HTTP for the Anthropic Messages API and OpenAI-compatible Chat Completions.
//!
//! Everything here is blocking; callers run [`stream`] and [`list_models`] on a background thread.
//! Request bodies and SSE/event parsing are pure functions so they can be tested without a network.
//! Errors never carry the API key, request bodies or full response bodies.

use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wire {
    Anthropic,
    OpenAiCompatible,
}

pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub wire: Wire,
    pub base_url: &'static str,
    pub needs_key: bool,
    pub local: bool,
    pub key_page: &'static str,
    pub suggested_models: &'static [&'static str],
}

const fn remote(id: &'static str, label: &'static str, wire: Wire, base_url: &'static str, key_page: &'static str, suggested_models: &'static [&'static str]) -> Preset {
    Preset { id, label, wire, base_url, needs_key: true, local: false, key_page, suggested_models }
}

const fn local(id: &'static str, label: &'static str, base_url: &'static str) -> Preset {
    Preset { id, label, wire: Wire::OpenAiCompatible, base_url, needs_key: false, local: true, key_page: "", suggested_models: &[] }
}

/// Built-in providers. Model IDs are only listed where verified against official docs (2026-10-05);
/// otherwise the UI offers "Load models" via [`list_models`].
pub const PRESETS: &[Preset] = &[
    remote("anthropic", "Anthropic", Wire::Anthropic, "https://api.anthropic.com/v1", "https://platform.claude.com/settings/keys",
        &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"]),
    remote("openai", "OpenAI", Wire::OpenAiCompatible, "https://api.openai.com/v1", "https://platform.openai.com/settings/organization/api-keys",
        &["gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"]),
    remote("openrouter", "OpenRouter", Wire::OpenAiCompatible, "https://openrouter.ai/api/v1", "https://openrouter.ai/settings/keys", &[]),
    remote("gemini", "Google Gemini", Wire::OpenAiCompatible, "https://generativelanguage.googleapis.com/v1beta/openai", "https://aistudio.google.com/apikey",
        &["gemini-3.8-flash", "gemini-3.5-flash"]),
    remote("xai", "xAI Grok", Wire::OpenAiCompatible, "https://api.x.ai/v1", "https://console.x.ai", &["grok-4.7"]),
    remote("groq", "Groq", Wire::OpenAiCompatible, "https://api.groq.com/openai/v1", "https://console.groq.com/keys",
        &["llama-3.3-70b-versatile", "openai/gpt-oss-120b", "llama-3.1-8b-instant"]),
    remote("deepseek", "DeepSeek", Wire::OpenAiCompatible, "https://api.deepseek.com", "https://platform.deepseek.com/api_keys",
        &["deepseek-flash", "deepseek-v4-pro"]),
    remote("mistral", "Mistral", Wire::OpenAiCompatible, "https://api.mistral.ai/v1", "https://console.mistral.ai/api-keys", &[]),
    remote("together", "Together AI", Wire::OpenAiCompatible, "https://api.together.ai/v1", "https://api.together.ai/settings/projects/~current/api-keys",
        &["meta-llama/Llama-3.3-70B-Instruct-Turbo", "Qwen/Qwen3.5-9B"]),
    local("ollama", "Ollama (local)", "http://localhost:11434/v1"),
    local("lmstudio", "LM Studio (local)", "http://localhost:1234/v1"),
    Preset { id: "custom", label: "Custom (OpenAI-compatible)", wire: Wire::OpenAiCompatible, base_url: "", needs_key: false, local: false, key_page: "", suggested_models: &[] },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    Assistant,
}

/// A message part. `Jpeg` holds raw JPEG bytes; they are base64-encoded when the body is built.
#[derive(Clone, Debug)]
pub enum Part {
    Text(String),
    Jpeg(Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub wire: Wire,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

#[derive(Debug)]
pub enum ProviderError {
    MissingKey,
    MissingModel,
    Unauthorized,
    RateLimited,
    NotFound,
    BadRequest(String),
    Server(u16),
    Network(String),
    Cancelled,
    Empty,
    TooLarge,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey => write!(f, "Add an API key for this provider in Settings."),
            Self::MissingModel => write!(f, "Choose a model in Settings."),
            Self::Unauthorized => write!(f, "The provider rejected the API key."),
            Self::RateLimited => write!(f, "The provider is rate limiting requests. Try again shortly."),
            Self::NotFound => write!(f, "The provider could not find that model or endpoint."),
            Self::BadRequest(message) if message.is_empty() => write!(f, "The provider rejected the request."),
            Self::BadRequest(message) => write!(f, "The provider rejected the request: {message}"),
            Self::Server(code) => write!(f, "The provider had a server error ({code}). Try again shortly."),
            Self::Network(message) => write!(f, "Network error: {message}"),
            Self::Cancelled => write!(f, "Cancelled."),
            Self::Empty => write!(f, "The provider returned an empty answer."),
            Self::TooLarge => write!(f, "The answer was too large and was stopped."),
        }
    }
}

impl std::error::Error for ProviderError {}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_TIMEOUT: Duration = Duration::from_secs(180);
const MODELS_TIMEOUT: Duration = Duration::from_secs(8);
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_ANSWER_CHARS: usize = 200_000;
const MAX_SSE_LINE: usize = 1 << 20;
const MAX_SSE_EVENT: usize = 4 << 20;
const MAX_ERROR_BODY: u64 = 64 * 1024;
const MAX_MODELS_BODY: u64 = 8 << 20;
const MAX_MODELS: usize = 200;
const MAX_PROVIDER_MESSAGE: usize = 200;

// ---------------------------------------------------------------------------------------------
// Public entry points

/// Streams one answer. `on_delta` receives each text fragment; the full answer is returned.
/// `cancel` is checked before sending, after every read and between SSE events.
pub fn stream(req: &Request, cancel: &AtomicBool, on_delta: &mut dyn FnMut(&str)) -> Result<String, ProviderError> {
    let base = validate_base_url(&req.base_url)?;
    if req.model.trim().is_empty() {
        return Err(ProviderError::MissingModel);
    }
    let key = req.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty());
    if req.wire == Wire::Anthropic && key.is_none() {
        return Err(ProviderError::MissingKey);
    }
    let (url, body) = match req.wire {
        Wire::Anthropic => (format!("{base}/messages"), anthropic_body(req)),
        Wire::OpenAiCompatible => (format!("{base}/chat/completions"), openai_body(req, &base)),
    };
    let body = serde_json::to_string(&body).map_err(|_| ProviderError::BadRequest("Could not encode the request.".into()))?;
    if cancel.load(Ordering::Relaxed) {
        return Err(ProviderError::Cancelled);
    }

    let agent = agent(STREAM_TIMEOUT);
    let mut request = auth_headers(agent.post(&url), req.wire, key)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream");
    if is_openrouter(&base) {
        request = request.header("X-Title", "CluelyRS");
    }
    let response = request.send(body.as_str()).map_err(|error| transport_error(&error))?;
    let status = response.status().as_u16();
    if status >= 300 {
        return Err(status_error(status, response.into_body(), key));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(ProviderError::Cancelled);
    }

    let mut reader = response.into_body().into_with_config().limit(u64::MAX).reader();
    let mut parser = SseParser::default();
    let mut answer = Answer::default();
    let mut chunk = [0u8; 8192];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(read) => read,
            Err(_) if cancel.load(Ordering::Relaxed) => return Err(ProviderError::Cancelled),
            Err(error) => return Err(io_error(&error)),
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(ProviderError::Cancelled);
        }
        let events = if read == 0 { parser.finish() } else { parser.feed(&chunk[..read])? };
        for event in events {
            if cancel.load(Ordering::Relaxed) {
                return Err(ProviderError::Cancelled);
            }
            match parse_event(req.wire, &event, key)? {
                StreamItem::Text(text) => answer.push(&text, on_delta)?,
                StreamItem::Done => return answer.finish(),
                StreamItem::Ignore => {}
            }
        }
        if read == 0 {
            return answer.finish();
        }
    }
}

/// Lists model IDs via `GET {base}/models`: sorted, deduplicated and capped at 200 entries.
pub fn list_models(wire: Wire, base_url: &str, api_key: Option<&str>) -> Result<Vec<String>, ProviderError> {
    let base = validate_base_url(base_url)?;
    let key = api_key.map(str::trim).filter(|key| !key.is_empty());
    if wire == Wire::Anthropic && key.is_none() {
        return Err(ProviderError::MissingKey);
    }
    let url = match wire {
        Wire::Anthropic => format!("{base}/models?limit=1000"),
        Wire::OpenAiCompatible => format!("{base}/models"),
    };
    let response = auth_headers(agent(MODELS_TIMEOUT).get(&url), wire, key)
        .header("accept", "application/json")
        .call()
        .map_err(|error| transport_error(&error))?;
    let status = response.status().as_u16();
    if status >= 300 {
        return Err(status_error(status, response.into_body(), key));
    }
    let text = response.into_body().into_with_config().limit(MAX_MODELS_BODY).read_to_string()
        .map_err(|error| match error {
            ureq::Error::BodyExceedsLimit(_) => ProviderError::TooLarge,
            other => transport_error(&other),
        })?;
    parse_model_list(&text)
}

// ---------------------------------------------------------------------------------------------
// URL validation and HTTP plumbing

/// Trims and normalizes a base URL. `http://` is only allowed for loopback hosts.
pub fn validate_base_url(raw: &str) -> Result<String, ProviderError> {
    let url = raw.trim().trim_end_matches('/');
    if url.is_empty() {
        return Err(ProviderError::BadRequest("Enter a base URL for this provider.".into()));
    }
    let (scheme, rest) = url.split_once("://").ok_or_else(|| ProviderError::BadRequest("The base URL must start with https://.".into()))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err(ProviderError::BadRequest("The base URL must start with https://.".into()));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || url.contains(['?', '#']) || url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err(ProviderError::BadRequest("The base URL is not valid.".into()));
    }
    let host = host_of(authority).to_ascii_lowercase();
    if host.is_empty() {
        return Err(ProviderError::BadRequest("The base URL is not valid.".into()));
    }
    if scheme == "http" && !is_loopback(&host) {
        return Err(ProviderError::BadRequest("Use https:// (plain http:// is only allowed for localhost).".into()));
    }
    Ok(format!("{scheme}://{rest}"))
}

fn host_of(authority: &str) -> &str {
    if let Some(stripped) = authority.strip_prefix('[') {
        return stripped.split(']').next().unwrap_or_default();
    }
    authority.split(':').next().unwrap_or_default()
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn is_openrouter(base: &str) -> bool {
    base.starts_with("https://openrouter.ai/")
}

fn is_openai(base: &str) -> bool {
    base.starts_with("https://api.openai.com/")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        // Never forward credentials across redirects.
        .max_redirects(0)
        .user_agent("CluelyRS")
        .build()
        .new_agent()
}

fn auth_headers<B>(request: ureq::RequestBuilder<B>, wire: Wire, key: Option<&str>) -> ureq::RequestBuilder<B> {
    match (wire, key) {
        (Wire::Anthropic, Some(key)) => request.header("x-api-key", key).header("anthropic-version", ANTHROPIC_VERSION),
        (Wire::Anthropic, None) => request.header("anthropic-version", ANTHROPIC_VERSION),
        (Wire::OpenAiCompatible, Some(key)) => request.header("authorization", format!("Bearer {key}")),
        (Wire::OpenAiCompatible, None) => request,
    }
}

fn transport_error(error: &ureq::Error) -> ProviderError {
    let message = match error {
        ureq::Error::Timeout(_) => "the request timed out",
        ureq::Error::HostNotFound => "could not find the server",
        ureq::Error::ConnectionFailed => "could not connect to the server",
        ureq::Error::Io(error) => return io_error(error),
        ureq::Error::BadUri(_) => "the base URL is not valid",
        ureq::Error::Tls(_) | ureq::Error::Pem(_) | ureq::Error::Rustls(_) => "a secure connection could not be established",
        ureq::Error::BodyExceedsLimit(_) => return ProviderError::TooLarge,
        _ => "the request failed",
    };
    ProviderError::Network(message.into())
}

fn io_error(error: &std::io::Error) -> ProviderError {
    use std::io::ErrorKind;
    let message = match error.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => "the connection timed out",
        ErrorKind::ConnectionRefused => "the server refused the connection",
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe => "the connection was interrupted",
        _ => "the connection failed",
    };
    ProviderError::Network(message.into())
}

fn status_error(status: u16, body: ureq::Body, key: Option<&str>) -> ProviderError {
    let text = body.into_with_config().limit(MAX_ERROR_BODY).read_to_string().unwrap_or_default();
    map_status(status, &text, key)
}

/// Maps an HTTP status (and, for 400/422, the provider's error *message*) to a user-safe error.
fn map_status(status: u16, body: &str, key: Option<&str>) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Unauthorized,
        404 => ProviderError::NotFound,
        429 => ProviderError::RateLimited,
        500..=599 => ProviderError::Server(status),
        300..=399 => ProviderError::Network("the server redirected the request; check the base URL".into()),
        _ => ProviderError::BadRequest(provider_message(body, key)),
    }
}

/// Extracts `error.message` (or a top-level `message`/string `error`) and sanitizes it.
fn provider_message(body: &str, key: Option<&str>) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else { return String::new() };
    // Some providers wrap the error in a one-element array.
    let value = value.as_array().and_then(|items| items.first()).unwrap_or(&value);
    let message = value.pointer("/error/message").and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or_default();
    sanitize_message(message, key)
}

/// Redacts anything key-like, collapses whitespace and truncates to 200 characters.
fn sanitize_message(message: &str, key: Option<&str>) -> String {
    let mut text = message.to_owned();
    if let Some(key) = key.filter(|key| key.len() >= 4) {
        text = text.replace(key, "[redacted]");
    }
    let words: Vec<String> = text.split_whitespace().map(redact_word).collect();
    let joined = words.join(" ");
    if joined.chars().count() <= MAX_PROVIDER_MESSAGE {
        return joined;
    }
    let mut truncated: String = joined.chars().take(MAX_PROVIDER_MESSAGE - 1).collect();
    truncated.push('…');
    truncated
}

fn redact_word(word: &str) -> String {
    let core = word.trim_matches(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'));
    if looks_like_key(core) { word.replace(core, "[redacted]") } else { word.to_owned() }
}

fn looks_like_key(token: &str) -> bool {
    const PREFIXES: &[&str] = &["sk-", "sk_", "xai-", "gsk_", "AIza", "pk-", "rk-", "key-", "Bearer"];
    if token.len() >= 12 && PREFIXES.iter().any(|prefix| token.starts_with(prefix)) {
        return true;
    }
    // Long opaque tokens mixing letters and digits (no ordinary words are this shape).
    token.len() >= 24
        && token.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        && token.chars().any(|ch| ch.is_ascii_digit())
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
}

// ---------------------------------------------------------------------------------------------
// Request bodies (pure)

fn encode_jpeg(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn joined_text(parts: &[Part]) -> String {
    parts.iter().filter_map(|part| match part { Part::Text(text) => Some(text.as_str()), Part::Jpeg(_) => None }).collect::<Vec<_>>().join("\n\n")
}

fn anthropic_body(req: &Request) -> Value {
    let messages: Vec<Value> = req.messages.iter().filter_map(|message| match message.role {
        Role::User => {
            let content: Vec<Value> = message.parts.iter().filter_map(|part| match part {
                Part::Jpeg(bytes) if !bytes.is_empty() => Some(json!({"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": encode_jpeg(bytes)}})),
                Part::Text(text) if !text.is_empty() => Some(json!({"type": "text", "text": text})),
                _ => None,
            }).collect();
            (!content.is_empty()).then(|| json!({"role": "user", "content": content}))
        }
        Role::Assistant => {
            let text = joined_text(&message.parts);
            (!text.is_empty()).then(|| json!({"role": "assistant", "content": text}))
        }
    }).collect();
    let mut body = json!({"model": req.model.trim(), "max_tokens": req.max_tokens, "stream": true, "messages": messages});
    if !req.system.trim().is_empty() {
        body["system"] = json!(req.system);
    }
    body
}

fn openai_body(req: &Request, base: &str) -> Value {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if !req.system.trim().is_empty() {
        messages.push(json!({"role": "system", "content": req.system}));
    }
    for message in &req.messages {
        match message.role {
            Role::User => {
                let has_image = message.parts.iter().any(|part| matches!(part, Part::Jpeg(bytes) if !bytes.is_empty()));
                if has_image {
                    let content: Vec<Value> = message.parts.iter().filter_map(|part| match part {
                        Part::Jpeg(bytes) if !bytes.is_empty() => Some(json!({"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", encode_jpeg(bytes))}})),
                        Part::Text(text) if !text.is_empty() => Some(json!({"type": "text", "text": text})),
                        _ => None,
                    }).collect();
                    messages.push(json!({"role": "user", "content": content}));
                } else {
                    // Plain strings are the most widely supported shape for text-only turns.
                    let text = joined_text(&message.parts);
                    if !text.is_empty() {
                        messages.push(json!({"role": "user", "content": text}));
                    }
                }
            }
            Role::Assistant => {
                let text = joined_text(&message.parts);
                if !text.is_empty() {
                    messages.push(json!({"role": "assistant", "content": text}));
                }
            }
        }
    }
    let mut body = json!({"model": req.model.trim(), "stream": true, "messages": messages});
    // OpenAI's current models reject `max_tokens`; other compatible servers expect it.
    let limit_field = if is_openai(base) { "max_completion_tokens" } else { "max_tokens" };
    body[limit_field] = json!(req.max_tokens);
    body
}

// ---------------------------------------------------------------------------------------------
// SSE parsing (pure)

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SseEvent {
    event: String,
    data: String,
}

/// Incremental Server-Sent Events parser: accepts arbitrary byte chunks, `\n`, `\r\n` or `\r`
/// line endings, comments and multi-line `data:` fields. Lines and events are size-bounded.
#[derive(Default)]
struct SseParser {
    line: Vec<u8>,
    skip_lf: bool,
    event: String,
    data: String,
    has_data: bool,
}

impl SseParser {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, ProviderError> {
        let mut events = Vec::new();
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.skip_lf = true;
                    self.end_line(&mut events)?;
                }
                b'\n' => self.end_line(&mut events)?,
                _ => {
                    if self.line.len() >= MAX_SSE_LINE {
                        return Err(ProviderError::TooLarge);
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(events)
    }

    /// Flushes a trailing event when the stream ends without a final blank line.
    fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            let _ = self.end_line(&mut events);
        }
        self.dispatch(&mut events);
        events
    }

    fn end_line(&mut self, events: &mut Vec<SseEvent>) -> Result<(), ProviderError> {
        let line = String::from_utf8_lossy(&self.line).into_owned();
        self.line.clear();
        if line.is_empty() {
            self.dispatch(events);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_str(), ""),
        };
        match field {
            "data" => {
                if self.data.len() + value.len() + 1 > MAX_SSE_EVENT {
                    return Err(ProviderError::TooLarge);
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => self.event = value.to_owned(),
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        if self.has_data {
            events.push(SseEvent { event: std::mem::take(&mut self.event), data: std::mem::take(&mut self.data) });
        }
        self.event.clear();
        self.data.clear();
        self.has_data = false;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StreamItem {
    Text(String),
    Done,
    Ignore,
}

fn parse_event(wire: Wire, event: &SseEvent, key: Option<&str>) -> Result<StreamItem, ProviderError> {
    match wire {
        Wire::Anthropic => parse_anthropic_event(event, key),
        Wire::OpenAiCompatible => parse_openai_event(event, key),
    }
}

/// Anthropic Messages streaming: text comes from `content_block_delta` / `text_delta`;
/// thinking and tool deltas are ignored; `error` events become errors; `message_stop` ends the stream.
fn parse_anthropic_event(event: &SseEvent, key: Option<&str>) -> Result<StreamItem, ProviderError> {
    let Ok(value) = serde_json::from_str::<Value>(&event.data) else { return Ok(StreamItem::Ignore) };
    let kind = value.get("type").and_then(Value::as_str).unwrap_or(event.event.as_str());
    match kind {
        "content_block_delta" => {
            let delta = &value["delta"];
            if delta.get("type").and_then(Value::as_str) == Some("text_delta")
                && let Some(text) = delta.get("text").and_then(Value::as_str)
            {
                return Ok(StreamItem::Text(text.to_owned()));
            }
            Ok(StreamItem::Ignore)
        }
        "message_stop" => Ok(StreamItem::Done),
        "error" => {
            let error_type = value.pointer("/error/type").and_then(Value::as_str).unwrap_or_default();
            let message = value.pointer("/error/message").and_then(Value::as_str).unwrap_or_default();
            Err(match error_type {
                "authentication_error" | "permission_error" => ProviderError::Unauthorized,
                "rate_limit_error" => ProviderError::RateLimited,
                "not_found_error" => ProviderError::NotFound,
                "overloaded_error" => ProviderError::Server(529),
                "api_error" | "timeout_error" => ProviderError::Server(500),
                _ => ProviderError::BadRequest(sanitize_message(message, key)),
            })
        }
        _ => Ok(StreamItem::Ignore),
    }
}

/// OpenAI-compatible streaming: text from `choices[0].delta.content`; `[DONE]` ends the stream;
/// in-band `error` objects become errors. Reasoning fields (e.g. `reasoning_content`) are ignored.
fn parse_openai_event(event: &SseEvent, key: Option<&str>) -> Result<StreamItem, ProviderError> {
    let data = event.data.trim();
    if data == "[DONE]" {
        return Ok(StreamItem::Done);
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else { return Ok(StreamItem::Ignore) };
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let message = error.get("message").and_then(Value::as_str).or_else(|| error.as_str()).unwrap_or_default();
        let code = error.get("code").and_then(|code| code.as_u64().or_else(|| code.as_str().and_then(|code| code.parse().ok())));
        return Err(match code {
            Some(401 | 403) => ProviderError::Unauthorized,
            Some(404) => ProviderError::NotFound,
            Some(429) => ProviderError::RateLimited,
            Some(code @ 500..=599) => ProviderError::Server(code as u16),
            _ => ProviderError::BadRequest(sanitize_message(message, key)),
        });
    }
    match value.pointer("/choices/0/delta/content").and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Ok(StreamItem::Text(text.to_owned())),
        _ => Ok(StreamItem::Ignore),
    }
}

/// Accumulates streamed text and enforces the answer size cap.
#[derive(Default)]
struct Answer {
    text: String,
    chars: usize,
}

impl Answer {
    fn push(&mut self, delta: &str, on_delta: &mut dyn FnMut(&str)) -> Result<(), ProviderError> {
        if delta.is_empty() {
            return Ok(());
        }
        self.chars += delta.chars().count();
        if self.chars > MAX_ANSWER_CHARS {
            return Err(ProviderError::TooLarge);
        }
        self.text.push_str(delta);
        on_delta(delta);
        Ok(())
    }

    fn finish(self) -> Result<String, ProviderError> {
        if self.text.trim().is_empty() { Err(ProviderError::Empty) } else { Ok(self.text) }
    }
}

// ---------------------------------------------------------------------------------------------
// Model listing (pure)

/// Accepts `{"data":[{"id":..}]}`, `{"models":[..]}` or a bare array; strips Gemini's `models/` prefix.
fn parse_model_list(text: &str) -> Result<Vec<String>, ProviderError> {
    let value: Value = serde_json::from_str(text).map_err(|_| ProviderError::BadRequest("The provider returned an unreadable model list.".into()))?;
    let items = value.as_array()
        .or_else(|| value.get("data").and_then(Value::as_array))
        .or_else(|| value.get("models").and_then(Value::as_array))
        .ok_or_else(|| ProviderError::BadRequest("The provider returned an unreadable model list.".into()))?;
    let mut ids: Vec<String> = items.iter()
        .filter_map(|item| item.get("id").or_else(|| item.get("name")).and_then(Value::as_str).or_else(|| item.as_str()))
        .map(|id| id.trim().strip_prefix("models/").unwrap_or(id.trim()).to_owned())
        .filter(|id| !id.is_empty() && id.len() <= 200 && !id.chars().any(char::is_control))
        .collect();
    ids.sort();
    ids.dedup();
    ids.truncate(MAX_MODELS);
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(wire: Wire, base: &str) -> Request {
        Request {
            wire,
            base_url: base.into(),
            api_key: Some("sk-test-0123456789abcdef".into()),
            model: "model-x".into(),
            system: "Be brief.".into(),
            messages: vec![
                Message { role: Role::User, parts: vec![Part::Text("What is this?".into()), Part::Jpeg(vec![0xFF, 0xD8, 0xFF])] },
                Message { role: Role::Assistant, parts: vec![Part::Text("A picture.".into())] },
                Message { role: Role::User, parts: vec![Part::Text("Thanks".into())] },
            ],
            max_tokens: 1024,
        }
    }

    /// Feeds `input` split at every `step` bytes and collects the stream items.
    fn run(wire: Wire, input: &str, step: usize) -> (Vec<StreamItem>, Option<ProviderError>) {
        let mut parser = SseParser::default();
        let mut items = Vec::new();
        let bytes = input.as_bytes();
        let mut events = Vec::new();
        for chunk in bytes.chunks(step.max(1)) {
            events.extend(parser.feed(chunk).unwrap());
        }
        events.extend(parser.finish());
        for event in events {
            match parse_event(wire, &event, None) {
                Ok(StreamItem::Ignore) => {}
                Ok(item) => items.push(item),
                Err(error) => return (items, Some(error)),
            }
        }
        (items, None)
    }

    fn text_of(items: &[StreamItem]) -> String {
        items.iter().filter_map(|item| match item { StreamItem::Text(text) => Some(text.as_str()), _ => None }).collect()
    }

    const ANTHROPIC_STREAM: &str = "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\r\n\r\n\
event: content_block_start\r\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\r\n\r\n\
event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"secret\"}}\r\n\r\n\
event: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\n\
event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Héllo \"}}\r\n\r\n\
event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"wörld 😀\"}}\r\n\r\n\
event: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\r\n\r\n\
event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";

    const OPENAI_STREAM: &str = ": OPENROUTER PROCESSING\n\n\
data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hidden\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Héllo \"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"wörld 😀\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

    #[test]
    fn anthropic_stream_survives_any_chunking() {
        for step in 1..=ANTHROPIC_STREAM.len() {
            let (items, error) = run(Wire::Anthropic, ANTHROPIC_STREAM, step);
            assert!(error.is_none());
            assert_eq!(text_of(&items), "Héllo wörld 😀", "step {step}");
            assert_eq!(items.last(), Some(&StreamItem::Done));
        }
    }

    #[test]
    fn openai_stream_survives_any_chunking() {
        for step in 1..=OPENAI_STREAM.len() {
            let (items, error) = run(Wire::OpenAiCompatible, OPENAI_STREAM, step);
            assert!(error.is_none());
            assert_eq!(text_of(&items), "Héllo wörld 😀", "step {step}");
            assert_eq!(items.last(), Some(&StreamItem::Done));
        }
    }

    #[test]
    fn sse_handles_multiline_data_bare_cr_and_missing_trailing_blank_line() {
        let mut parser = SseParser::default();
        let events = parser.feed(b"event: x\rdata: a\r\ndata:b\n: comment\n\n").unwrap();
        assert_eq!(events, vec![SseEvent { event: "x".into(), data: "a\nb".into() }]);
        assert!(parser.feed(b"data: tail").unwrap().is_empty());
        assert_eq!(parser.finish(), vec![SseEvent { event: String::new(), data: "tail".into() }]);
    }

    #[test]
    fn sse_lines_are_bounded() {
        let mut parser = SseParser::default();
        let big = vec![b'a'; MAX_SSE_LINE + 1];
        assert!(matches!(parser.feed(&big), Err(ProviderError::TooLarge)));
    }

    #[test]
    fn anthropic_error_events_map_to_errors() {
        let stream = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        assert!(matches!(run(Wire::Anthropic, stream, 7).1, Some(ProviderError::Server(529))));
        let stream = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"bad sk-ant-api03-abcdefghijklmnop\"}}\n\n";
        match run(Wire::Anthropic, stream, 5).1 {
            Some(ProviderError::BadRequest(message)) => assert_eq!(message, "bad [redacted]"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn openai_in_band_errors_map_to_errors() {
        let stream = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: {\"error\":{\"code\":429,\"message\":\"slow down\"},\"choices\":[{\"finish_reason\":\"error\"}]}\n\n";
        let (items, error) = run(Wire::OpenAiCompatible, stream, 3);
        assert_eq!(text_of(&items), "Hi");
        assert!(matches!(error, Some(ProviderError::RateLimited)));
        let stream = "data: {\"error\":{\"message\":\"context too long\"}}\n\n";
        assert!(matches!(run(Wire::OpenAiCompatible, stream, 4).1, Some(ProviderError::BadRequest(m)) if m == "context too long"));
    }

    #[test]
    fn answer_is_capped() {
        let mut answer = Answer::default();
        let mut sink = |_: &str| {};
        let chunk = "x".repeat(100_000);
        answer.push(&chunk, &mut sink).unwrap();
        answer.push(&chunk, &mut sink).unwrap();
        assert!(matches!(answer.push("y", &mut sink), Err(ProviderError::TooLarge)));
        assert!(matches!(Answer::default().finish(), Err(ProviderError::Empty)));
    }

    #[test]
    fn anthropic_body_shape() {
        let body = anthropic_body(&request(Wire::Anthropic, "https://api.anthropic.com/v1"));
        assert_eq!(body["model"], "model-x");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["system"], "Be brief.");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0], json!({"type": "text", "text": "What is this?"}));
        assert_eq!(messages[0]["content"][1], json!({"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "/9j/"}}));
        assert_eq!(messages[1], json!({"role": "assistant", "content": "A picture."}));
        assert_eq!(messages[2]["content"][0]["text"], "Thanks");
        // No system field when empty.
        let mut req = request(Wire::Anthropic, "https://api.anthropic.com/v1");
        req.system.clear();
        assert!(anthropic_body(&req).get("system").is_none());
    }

    #[test]
    fn openai_body_shape() {
        let base = "https://openrouter.ai/api/v1";
        let body = openai_body(&request(Wire::OpenAiCompatible, base), base);
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 1024);
        assert!(body.get("max_completion_tokens").is_none());
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0], json!({"role": "system", "content": "Be brief."}));
        assert_eq!(messages[1]["content"][0], json!({"type": "text", "text": "What is this?"}));
        assert_eq!(messages[1]["content"][1], json!({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,/9j/"}}));
        assert_eq!(messages[2], json!({"role": "assistant", "content": "A picture."}));
        assert_eq!(messages[3], json!({"role": "user", "content": "Thanks"}));

        let base = "https://api.openai.com/v1";
        let body = openai_body(&request(Wire::OpenAiCompatible, base), base);
        assert_eq!(body["max_completion_tokens"], 1024);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn status_mapping() {
        assert!(matches!(map_status(401, "", None), ProviderError::Unauthorized));
        assert!(matches!(map_status(403, "", None), ProviderError::Unauthorized));
        assert!(matches!(map_status(404, "", None), ProviderError::NotFound));
        assert!(matches!(map_status(429, "", None), ProviderError::RateLimited));
        assert!(matches!(map_status(503, "{}", None), ProviderError::Server(503)));
        assert!(matches!(map_status(529, "", None), ProviderError::Server(529)));
        let body = r#"{"error":{"message":"Invalid model 'foo'","type":"invalid_request_error"}}"#;
        assert!(matches!(map_status(400, body, None), ProviderError::BadRequest(m) if m == "Invalid model 'foo'"));
        assert!(matches!(map_status(422, r#"[{"error":{"message":"nope"}}]"#, None), ProviderError::BadRequest(m) if m == "nope"));
        // Non-JSON bodies are never echoed.
        assert!(matches!(map_status(400, "<html>secret page</html>", None), ProviderError::BadRequest(m) if m.is_empty()));
    }

    #[test]
    fn provider_messages_are_redacted_and_truncated() {
        let key = "my-custom-key-value";
        let body = format!(r#"{{"error":{{"message":"Key {key} invalid; also sk-proj-ABCDEFGH12345678 and AIzaSyA1234567890abcdefghij and token abcdefghijklmnopqrstuvwx1234."}}}}"#);
        let message = provider_message(&body, Some(key));
        assert!(!message.contains(key));
        assert!(!message.contains("sk-proj"));
        assert!(!message.contains("AIza"));
        assert!(!message.contains("abcdefghijklmnopqrstuvwx1234"));
        assert!(message.contains("[redacted]"));

        let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "word ".repeat(100));
        assert_eq!(provider_message(&long, None).chars().count(), 200);
        // Ordinary words survive.
        assert_eq!(sanitize_message("max_tokens must be less than 8192", None), "max_tokens must be less than 8192");
    }

    #[test]
    fn display_never_contains_key_material() {
        let error = map_status(400, r#"{"error":{"message":"bad key sk-live-0123456789abcdef"}}"#, None);
        assert!(!error.to_string().contains("0123456789"));
    }

    #[test]
    fn base_url_validation() {
        assert_eq!(validate_base_url(" https://api.openai.com/v1/ ").unwrap(), "https://api.openai.com/v1");
        assert_eq!(validate_base_url("http://localhost:11434/v1").unwrap(), "http://localhost:11434/v1");
        assert!(validate_base_url("http://127.0.0.1:1234/v1").is_ok());
        assert!(validate_base_url("http://[::1]:8080/v1").is_ok());
        for bad in ["", "api.openai.com/v1", "http://example.com/v1", "http://localhost.evil.com/v1", "ftp://x/y",
            "https://user:pass@host/v1", "https:///v1", "https://host/v1?x=1", "https://ho st/v1", "http://127.0.0.1@evil.com/"] {
            assert!(matches!(validate_base_url(bad), Err(ProviderError::BadRequest(_))), "{bad:?}");
        }
    }

    #[test]
    fn stream_validates_before_network() {
        let cancel = AtomicBool::new(false);
        let mut sink = |_: &str| {};
        let mut req = request(Wire::OpenAiCompatible, "http://example.com/v1");
        assert!(matches!(stream(&req, &cancel, &mut sink), Err(ProviderError::BadRequest(_))));
        req.base_url = "https://example.com/v1".into();
        req.model = " ".into();
        assert!(matches!(stream(&req, &cancel, &mut sink), Err(ProviderError::MissingModel)));
        let mut req = request(Wire::Anthropic, "https://api.anthropic.com/v1");
        req.api_key = Some("  ".into());
        assert!(matches!(stream(&req, &cancel, &mut sink), Err(ProviderError::MissingKey)));
        let req = request(Wire::OpenAiCompatible, "https://example.invalid/v1");
        let cancelled = AtomicBool::new(true);
        assert!(matches!(stream(&req, &cancelled, &mut sink), Err(ProviderError::Cancelled)));
        assert!(matches!(list_models(Wire::OpenAiCompatible, "http://example.com", None), Err(ProviderError::BadRequest(_))));
        assert!(matches!(list_models(Wire::Anthropic, "https://api.anthropic.com/v1", None), Err(ProviderError::MissingKey)));
    }

    #[test]
    fn model_lists_parse_all_shapes() {
        let openai = r#"{"object":"list","data":[{"id":"b"},{"id":"a"},{"id":"a"}]}"#;
        assert_eq!(parse_model_list(openai).unwrap(), vec!["a", "b"]);
        let together = r#"[{"id":"meta/x"},{"id":"qwen/y"}]"#;
        assert_eq!(parse_model_list(together).unwrap(), vec!["meta/x", "qwen/y"]);
        let gemini = r#"{"data":[{"id":"models/gemini-3.8-flash"}]}"#;
        assert_eq!(parse_model_list(gemini).unwrap(), vec!["gemini-3.8-flash"]);
        let many: Vec<Value> = (0..500).map(|index| json!({"id": format!("m{index:03}")})).collect();
        assert_eq!(parse_model_list(&json!({"data": many}).to_string()).unwrap().len(), 200);
        assert!(parse_model_list("not json").is_err());
    }

    #[test]
    fn presets_are_ordered_and_consistent() {
        let ids: Vec<&str> = PRESETS.iter().map(|preset| preset.id).collect();
        assert_eq!(ids, ["anthropic", "openai", "openrouter", "gemini", "xai", "groq", "deepseek", "mistral", "together", "ollama", "lmstudio", "custom"]);
        for preset in PRESETS {
            if !preset.base_url.is_empty() {
                assert!(validate_base_url(preset.base_url).is_ok(), "{}", preset.id);
            }
            assert_eq!(preset.local, preset.base_url.starts_with("http://"), "{}", preset.id);
            assert!(!(preset.local && preset.needs_key));
        }
        assert_eq!(preset("anthropic").unwrap().wire, Wire::Anthropic);
        assert!(preset("nope").is_none());
    }
}
