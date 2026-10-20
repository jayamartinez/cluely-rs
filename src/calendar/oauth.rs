//! Google sign-in with the installed-app OAuth flow: PKCE (S256), a loopback redirect to
//! 127.0.0.1 on a random port, and the system browser. Read-only calendar scope only. No server
//! of CluelyRS's own is involved.
//!
//! The listener binds to 127.0.0.1 only and accepts a single request that carries this sign-in's
//! `state`. It waits for as long as Settings shows "Signing in…" and stops at once on Cancel. After
//! a sign-in it answers late or repeated callbacks for a few minutes with an "expired" page that
//! takes no code (`Loopback::linger`). Its pages lead back to the app with a `cluelyrs://calendar`
//! link (`crate::app_link`). Tokens are never logged, formatted into errors or `Debug` output.
//!
//! The client ID comes from the build (`CLUELYRS_GOOGLE_CLIENT_ID`, `CLUELYRS_GOOGLE_CLIENT_SECRET`;
//! a Desktop client's secret isn't confidential, but it's still kept out of the repository) or from
//! Settings › Calendar › Advanced, which overrides it.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::google::{agent, read_json};

pub const SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
/// The refresh token's entry in the credential store.
pub const REFRESH_TOKEN_ID: &str = "google-calendar";
/// The secret of the user's own client ID (Advanced), in the credential store.
pub const OWN_SECRET_ID: &str = "google-client-secret";
/// How long a finished sign-in's listener still answers callbacks with the "expired" page.
pub const LINGER: Duration = Duration::from_secs(5 * 60);
/// The last sign-in's port. A new sign-in listens there again when it can, so a tab left over from
/// an earlier attempt in this run reaches it and gets the "expired" page rather than no page at all.
static LAST_PORT: AtomicU16 = AtomicU16::new(0);
/// A redirect that doesn't send its request line promptly is dropped.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST: usize = 16 * 1024;
const POLL: Duration = Duration::from_millis(100);

/// Google's OAuth addresses; the tests point these at local fakes.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub revoke: String,
}

impl Endpoints {
    pub fn google() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
        }
    }
}

/// An OAuth client: the built-in one or the user's own.
#[derive(Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub secret: Option<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("id", &self.id).field("secret", &self.secret.as_ref().map(|_| "…")).finish()
    }
}

/// The client ID this build was made with, if any.
pub fn built_in() -> Option<Client> {
    let id = option_env!("CLUELYRS_GOOGLE_CLIENT_ID").map(str::trim).filter(|id| !id.is_empty())?;
    let secret = option_env!("CLUELYRS_GOOGLE_CLIENT_SECRET").map(str::trim).filter(|secret| !secret.is_empty());
    Some(Client { id: id.to_owned(), secret: secret.map(str::to_owned) })
}

/// The client to sign in with: the user's own ID when one is set, otherwise the built-in one.
pub fn resolve_client(own_id: &str, own_secret: impl FnOnce() -> Option<String>) -> Option<Client> {
    let own_id = own_id.trim();
    if own_id.is_empty() { return built_in(); }
    Some(Client { id: own_id.to_owned(), secret: own_secret() })
}

/// Whether sign-in has a client ID to use, without reading the credential store.
pub fn has_client(own_id: &str) -> bool { !own_id.trim().is_empty() || built_in().is_some() }

/// A plausible Desktop client ID ("….apps.googleusercontent.com").
pub fn plausible_client_id(id: &str) -> bool {
    let id = id.trim();
    id.len() <= 200 && id.ends_with(".apps.googleusercontent.com") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
}

/// `count` random bytes from the OS, base64url without padding.
fn random_token(count: usize) -> String {
    let mut bytes = vec![0u8; count];
    getrandom::fill(&mut bytes).expect("the OS random number generator is available");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A sign-in's `state`: 32 random bytes, checked on the redirect.
pub fn new_state() -> String { random_token(32) }

/// The PKCE pair: the verifier stays here, its S256 challenge goes in the browser URL.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// A fresh verifier of 43 characters (32 random bytes), RFC 7636's minimum and Google's preference.
    pub fn generate() -> Self { Self::from_verifier(random_token(32)) }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

/// The address the browser is sent to.
pub fn auth_url(endpoints: &Endpoints, client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let query = [
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        // A refresh token, issued again on every sign-in (an earlier one may have been revoked).
        ("access_type", "offline"),
        ("prompt", "consent"),
    ];
    let query: Vec<String> = query.iter().map(|(name, value)| format!("{name}={}", encode(value))).collect();
    format!("{}?{}", endpoints.auth, query.join("&"))
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
pub(super) fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') { out.push(byte as char); }
        else { out.push_str(&format!("%{byte:02X}")); }
    }
    out
}

/// Decodes a query value (`+` is a space). Malformed escapes are kept as they are.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let hex = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let digit = |at: usize| bytes.get(at).and_then(|byte| hex(*byte));
        let escaped = if bytes[index] == b'%' { digit(index + 1).zip(digit(index + 2)) } else { None };
        match (bytes[index], escaped) {
            (b'%', Some((high, low))) => { out.push(high << 4 | low); index += 3; }
            (b'+', _) => { out.push(b' '); index += 1; }
            (byte, _) => { out.push(byte); index += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Cancel was pressed while the browser was open.
    Cancelled,
    /// Access was refused on Google's consent screen.
    Denied,
    /// Calendar access was unticked on the consent screen.
    MissingScope,
    /// The loopback listener couldn't start.
    NoListener,
    /// The refresh token couldn't be saved to the credential store.
    NotSaved,
    /// No connection, DNS failure or timeout.
    Offline,
    /// The refresh token was revoked or expired: sign in again.
    SignInAgain,
    /// Google refused the client ID or secret.
    BadClient,
    /// Any other unexpected status.
    Status(u16),
    /// A reply that isn't the expected JSON.
    Invalid,
}

impl AuthError {
    /// A short line for Settings › Calendar.
    pub fn message(&self) -> String {
        match self {
            Self::Cancelled => "Sign-in was cancelled.".into(),
            Self::Denied => "Google sign-in was declined.".into(),
            Self::MissingScope => "Calendar access wasn't allowed. Sign in again and tick it.".into(),
            Self::NoListener => "Couldn't start sign-in on this PC.".into(),
            Self::NotSaved => format!("Couldn't save the sign-in to {}.", crate::secrets::STORE_NAME),
            Self::Offline => "Couldn't reach Google. Check your connection.".into(),
            Self::SignInAgain => "Google signed CluelyRS out. Sign in again.".into(),
            Self::BadClient => "Google refused the client ID. Check it under Advanced.".into(),
            Self::Status(status) => format!("Google replied with an error ({status})."),
            Self::Invalid => "Google's reply couldn't be read.".into(),
        }
    }
}

/// The loopback redirect's listener for one sign-in.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    /// Listen on 127.0.0.1 (never other interfaces): at the last sign-in's port when it's free,
    /// otherwise at one the OS picks.
    pub fn bind() -> Result<Self, AuthError> {
        let loopback = Self::bind_preferring(LAST_PORT.load(Ordering::Relaxed))?;
        LAST_PORT.store(loopback.port, Ordering::Relaxed);
        Ok(loopback)
    }

    fn bind_preferring(port: u16) -> Result<Self, AuthError> {
        let listener = (port != 0).then(|| TcpListener::bind(("127.0.0.1", port)).ok()).flatten()
            .map_or_else(|| TcpListener::bind(("127.0.0.1", 0)), Ok).map_err(|_| AuthError::NoListener)?;
        listener.set_nonblocking(true).map_err(|_| AuthError::NoListener)?;
        let port = listener.local_addr().map_err(|_| AuthError::NoListener)?.port();
        Ok(Self { listener, port })
    }

    pub fn redirect_uri(&self) -> String { format!("http://127.0.0.1:{}", self.port) }

    /// Wait, until `cancel` is set, for the browser's redirect carrying `state`, and return its
    /// authorization code. Requests without this sign-in's `state` (a stale tab, a forgery) get the
    /// "expired" page and waiting goes on; the first one with it ends the wait either way.
    pub fn wait(&self, state: &str, cancel: &AtomicBool) -> Result<String, AuthError> {
        loop {
            if cancel.load(Ordering::SeqCst) { return Err(AuthError::Cancelled); }
            if let Some(outcome) = self.next(Some(state)) { return outcome; }
        }
    }

    /// After a sign-in: answer every callback with the "expired" page for `duration`, then close.
    /// No code is ever accepted here.
    pub fn linger(self, duration: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until { let _ = self.next(None); }
    }

    /// Answer one waiting request, if any (otherwise pause briefly).
    fn next(&self, state: Option<&str>) -> Option<Result<String, AuthError>> {
        match self.listener.accept() {
            Ok((stream, peer)) if peer.ip().is_loopback() => answer(stream, state),
            Ok(_) => None,
            Err(_) => { std::thread::sleep(POLL); None }
        }
    }
}

/// The page a callback gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page { SignedIn, Declined, Failed, Expired, NotFound }

impl Page {
    fn status(self) -> &'static str {
        match self { Self::SignedIn | Self::Declined => "200 OK", Self::Failed | Self::Expired => "400 Bad Request", Self::NotFound => "404 Not Found" }
    }

    /// A small page in the app's colours, with "Back to CluelyRS" (a `cluelyrs://calendar` link).
    /// Nothing from the request is echoed into it.
    fn html(self) -> String {
        let (title, detail) = match self {
            Self::SignedIn => ("Signed in to CluelyRS", "You can close this tab."),
            Self::Declined => ("Sign-in was declined", "CluelyRS can't read your calendar. You can close this tab."),
            Self::Failed => ("Sign-in didn't finish", "Try again from CluelyRS › Settings › Calendar."),
            Self::Expired => ("This sign-in expired", "Start again from CluelyRS › Settings › Calendar."),
            Self::NotFound => return "<!doctype html><meta charset=utf-8><title>CluelyRS</title>Not found.".into(),
        };
        format!("<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>{title}</title>\
            <style>body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;background:#0f1012;color:#f2efe8;font:15px/1.5 'Segoe UI',system-ui,sans-serif}}\
            main{{background:#17191c;border:1px solid #2c2f33;border-radius:12px;padding:32px 36px;max-width:420px;text-align:center}}\
            h1{{font-size:22px;font-weight:600;margin:0 0 8px}}p{{color:#9c9890;margin:0 0 24px}}\
            a{{display:inline-block;background:#4c8dff;color:#061230;font-weight:600;text-decoration:none;padding:10px 18px;border-radius:8px}}</style>\
            <main><h1>{title}</h1><p>{detail}</p><a href=\"{}\">Back to CluelyRS</a></main></html>", crate::app_link::CALENDAR)
    }
}

/// Read one callback and reply to it. Only a request carrying `state` (when there is one) ends the
/// sign-in; anything else gets the "expired" (or "not found") page and `None`.
fn answer(stream: TcpStream, state: Option<&str>) -> Option<Result<String, AuthError>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));
    let mut reader = BufReader::new(std::io::Read::take(&stream, MAX_REQUEST as u64));
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    // Drain the headers, so the browser reads the reply instead of a reset connection.
    let mut header = String::new();
    while reader.read_line(&mut header).is_ok_and(|read| read > 2) { header.clear(); }
    drop(reader);
    let (page, outcome) = match (parse_redirect(&line), state) {
        (Some(redirect), Some(state)) if constant_eq(redirect.state.as_deref().unwrap_or(""), state) => match (redirect.code, redirect.error) {
            (Some(code), None) if !code.is_empty() => (Page::SignedIn, Some(Ok(code))),
            (_, Some(error)) if error == "access_denied" => (Page::Declined, Some(Err(AuthError::Denied))),
            _ => (Page::Failed, Some(Err(AuthError::Invalid))),
        },
        (Some(_), _) => (Page::Expired, None),
        (None, _) => (Page::NotFound, None),
    };
    let html = page.html();
    let reply = format!("HTTP/1.1 {}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\ncache-control: no-store\r\nreferrer-policy: no-referrer\r\n\
        content-security-policy: default-src 'none'; style-src 'unsafe-inline'\r\nx-frame-options: DENY\r\nconnection: close\r\n\r\n{html}", page.status(), html.len());
    let mut stream = &stream;
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
    outcome
}

/// Compares without stopping at the first difference.
fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[derive(Debug, Default, PartialEq)]
struct Redirect {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// The query of a `GET /?… HTTP/1.1` request line; `None` for any other request.
fn parse_redirect(request_line: &str) -> Option<Redirect> {
    let mut parts = request_line.split_whitespace();
    if parts.next()? != "GET" { return None; }
    let target = parts.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" { return None; }
    let mut redirect = Redirect::default();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = Some(decode(value));
        match name { "code" => redirect.code = value, "state" => redirect.state = value, "error" => redirect.error = value, _ => {} }
    }
    Some(redirect)
}

/// A short-lived access token. Kept in memory only.
#[derive(Clone)]
pub struct AccessToken {
    pub token: String,
    pub expires: Instant,
}

impl AccessToken {
    /// Still good for at least another minute.
    pub fn fresh(&self) -> bool { Instant::now() + Duration::from_secs(60) < self.expires }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("AccessToken(…)") }
}

/// What a finished sign-in returns.
pub struct Tokens {
    pub access: AccessToken,
    pub refresh: String,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct ErrorReply {
    #[serde(default)]
    error: String,
}

/// Trade the authorization code for tokens.
pub fn exchange_code(endpoints: &Endpoints, client: &Client, code: &str, verifier: &str, redirect_uri: &str) -> Result<Tokens, AuthError> {
    let mut form = vec![("code", code), ("client_id", client.id.as_str()), ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"), ("code_verifier", verifier)];
    if let Some(secret) = &client.secret { form.push(("client_secret", secret)); }
    let reply = token_request(&endpoints.token, &form)?;
    // Google's consent screen lets each scope be unticked.
    if !reply.scope.as_deref().unwrap_or(SCOPE).split(' ').any(|scope| scope == SCOPE) { return Err(AuthError::MissingScope); }
    let refresh = reply.refresh_token.clone().filter(|token| !token.is_empty()).ok_or(AuthError::Invalid)?;
    Ok(Tokens { access: access_token(reply), refresh })
}

/// A new access token from the refresh token.
pub fn refresh(endpoints: &Endpoints, client: &Client, refresh_token: &str) -> Result<AccessToken, AuthError> {
    let mut form = vec![("client_id", client.id.as_str()), ("refresh_token", refresh_token), ("grant_type", "refresh_token")];
    if let Some(secret) = &client.secret { form.push(("client_secret", secret)); }
    token_request(&endpoints.token, &form).map(access_token)
}

/// Revoke the grant (the refresh token and its access tokens). One Google no longer knows counts as revoked.
pub fn revoke(endpoints: &Endpoints, token: &str) -> Result<(), AuthError> {
    let response = agent().post(&endpoints.revoke).send_form([("token", token)]).map_err(|_| AuthError::Offline)?;
    match response.status().as_u16() {
        200 | 400 => Ok(()),
        status => Err(AuthError::Status(status)),
    }
}

fn access_token(reply: TokenReply) -> AccessToken {
    AccessToken { token: reply.access_token, expires: Instant::now() + Duration::from_secs(reply.expires_in.unwrap_or(3600).min(24 * 3600)) }
}

fn token_request(url: &str, form: &[(&str, &str)]) -> Result<TokenReply, AuthError> {
    let response = agent().post(url).send_form(form.iter().copied()).map_err(|_| AuthError::Offline)?;
    let status = response.status().as_u16();
    if status == 200 { return read_json::<TokenReply>(response).map_err(|_| AuthError::Invalid); }
    let error = read_json::<ErrorReply>(response).map(|reply| reply.error).unwrap_or_default();
    Err(match (status, error.as_str()) {
        (400, "invalid_grant") => AuthError::SignInAgain,
        (400 | 401, "invalid_client" | "unauthorized_client") => AuthError::BadClient,
        (_, "access_denied") => AuthError::Denied,
        (status, _) => AuthError::Status(status),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;

    use super::*;
    use crate::calendar::google::tests::serve;

    #[test]
    fn pkce_uses_s256_of_a_43_character_verifier() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier.len(), 43);
        assert!(pkce.verifier.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(Pkce::generate().verifier, pkce.verifier, "each sign-in has its own verifier");
        // RFC 7636 appendix B.
        let known = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(known.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn the_auth_url_asks_for_read_only_calendar_access_with_pkce() {
        let url = auth_url(&Endpoints::google(), "123-abc.apps.googleusercontent.com", "http://127.0.0.1:50123", "CHALLENGE", "STATE");
        let (base, query) = url.split_once('?').unwrap();
        assert_eq!(base, "https://accounts.google.com/o/oauth2/v2/auth");
        let params: Vec<(&str, String)> = query.split('&').map(|pair| { let (k, v) = pair.split_once('=').unwrap(); (k, decode(v)) }).collect();
        let get = |name: &str| params.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str());
        assert_eq!(get("client_id"), Some("123-abc.apps.googleusercontent.com"));
        assert_eq!(get("redirect_uri"), Some("http://127.0.0.1:50123"));
        assert_eq!(get("response_type"), Some("code"));
        assert_eq!(get("scope"), Some("https://www.googleapis.com/auth/calendar.readonly"));
        assert_eq!((get("code_challenge"), get("code_challenge_method")), (Some("CHALLENGE"), Some("S256")));
        assert_eq!(get("state"), Some("STATE"));
        assert_eq!(get("access_type"), Some("offline"));
        assert!(query.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A50123"), "{query}");
        assert!(!query.contains("client_secret"), "the secret never goes to the browser");
    }

    #[test]
    fn query_values_round_trip() {
        for value in ["4/0Ab_c-d.e~f", "a b&c=d/é", "%zz"] { assert_eq!(decode(&encode(value)), value); }
        assert_eq!(decode("a+b%2Fc%"), "a b/c%");
    }

    #[test]
    fn redirects_are_read_from_the_request_line() {
        assert_eq!(parse_redirect("GET /?state=s&code=4%2F0Ab&scope=x HTTP/1.1\r\n"),
            Some(Redirect { code: Some("4/0Ab".into()), state: Some("s".into()), error: None }));
        assert_eq!(parse_redirect("GET /?error=access_denied&state=s HTTP/1.1"), Some(Redirect { code: None, state: Some("s".into()), error: Some("access_denied".into()) }));
        assert_eq!(parse_redirect("GET /favicon.ico HTTP/1.1"), None);
        assert_eq!(parse_redirect("POST /?code=x HTTP/1.1"), None);
        assert_eq!(parse_redirect(""), None);
    }

    /// Plays the browser: sends `target` to the listener and returns the whole reply.
    fn fetch(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/html\r\n\r\n").unwrap();
        let mut reply = String::new();
        std::io::Read::read_to_string(&mut stream, &mut reply).unwrap();
        reply
    }

    /// The status line of `fetch`'s reply.
    fn browse(port: u16, target: &str) -> String { fetch(port, target).lines().next().unwrap_or_default().to_owned() }

    fn listen() -> (u16, String, Arc<AtomicBool>, mpsc::Receiver<Result<String, AuthError>>) {
        // A fresh port each: `bind` would share the last one between tests running at once.
        let loopback = Loopback::bind_preferring(0).unwrap();
        let redirect = loopback.redirect_uri();
        let port = loopback.port;
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, result) = mpsc::channel();
        let flag = cancel.clone();
        std::thread::spawn(move || { let _ = sender.send(loopback.wait("expected-state", &flag)); });
        (port, redirect, cancel, result)
    }

    #[test]
    fn the_loopback_takes_only_this_sign_ins_redirect_once() {
        let (port, redirect, _cancel, result) = listen();
        assert_eq!(redirect, format!("http://127.0.0.1:{port}"));
        assert_eq!(browse(port, "/favicon.ico"), "HTTP/1.1 404 Not Found");
        let forged = fetch(port, "/?state=forged&code=attacker");
        assert!(forged.starts_with("HTTP/1.1 400 Bad Request") && forged.contains("This sign-in expired"), "{forged}");
        assert!(!forged.contains("attacker") && !forged.contains("forged"), "nothing from the request is echoed");
        assert!(result.try_recv().is_err(), "a forged or stale state doesn't end the sign-in");
        let signed_in = fetch(port, "/?state=expected-state&code=4%2F0Ab&scope=x");
        assert!(signed_in.starts_with("HTTP/1.1 200 OK") && signed_in.contains("Signed in to CluelyRS"), "{signed_in}");
        assert!(signed_in.contains("href=\"cluelyrs://calendar\"") && signed_in.contains("Back to CluelyRS"));
        assert!(signed_in.contains("content-security-policy: default-src 'none'"));
        assert!(!signed_in.contains("4/0Ab") && !signed_in.contains("4%2F0Ab"), "the code isn't shown");
        assert_eq!(result.recv().unwrap(), Ok("4/0Ab".into()));
        // Single use: the listener has closed.
        std::thread::sleep(Duration::from_millis(50));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    #[test]
    fn the_loopback_reports_a_declined_consent() {
        let (port, _, _cancel, result) = listen();
        assert_eq!(browse(port, "/?error=access_denied&state=expected-state"), "HTTP/1.1 200 OK");
        assert_eq!(result.recv().unwrap(), Err(AuthError::Denied));
    }

    #[test]
    fn cancelling_stops_the_loopback_listener() {
        let (port, _, cancel, result) = listen();
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(result.recv_timeout(Duration::from_secs(2)).unwrap(), Err(AuthError::Cancelled));
        std::thread::sleep(Duration::from_millis(50));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "nothing listens after Cancel");
    }

    #[test]
    fn after_a_sign_in_late_callbacks_get_the_expired_page_for_a_while() {
        let loopback = Loopback::bind_preferring(0).unwrap();
        let port = loopback.port;
        let lingering = std::thread::spawn(move || loopback.linger(Duration::from_millis(600)));
        let late = fetch(port, "/?state=expected-state&code=4%2F0Ab");
        assert!(late.starts_with("HTTP/1.1 400 Bad Request") && late.contains("This sign-in expired") && late.contains("cluelyrs://calendar"), "{late}");
        lingering.join().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "then it closes");
    }

    #[test]
    fn a_new_sign_in_listens_on_the_last_port_when_it_is_free() {
        let first = Loopback::bind_preferring(0).unwrap();
        let port = first.port;
        // Still taken: another port.
        let second = Loopback::bind_preferring(port).unwrap();
        assert_ne!(second.port, port);
        drop((first, second));
        assert_eq!(Loopback::bind_preferring(port).unwrap().port, port);
    }

    fn endpoints(token: &str) -> Endpoints { Endpoints { auth: String::new(), token: format!("{token}/token"), revoke: format!("{token}/revoke") } }

    fn client() -> Client { Client { id: "id.apps.googleusercontent.com".into(), secret: Some("not-really-secret".into()) } }

    #[test]
    fn the_code_is_exchanged_with_its_verifier() {
        let (server, requests) = serve("200 OK", r#"{"access_token":"ya29.a","expires_in":3599,"refresh_token":"1//r","scope":"https://www.googleapis.com/auth/calendar.readonly","token_type":"Bearer"}"#);
        let tokens = exchange_code(&endpoints(&server), &client(), "4/0Ab", "VERIFIER", "http://127.0.0.1:5").unwrap();
        assert_eq!((tokens.access.token.as_str(), tokens.refresh.as_str()), ("ya29.a", "1//r"));
        assert!(tokens.access.fresh());
        let request = requests.recv().unwrap();
        assert!(request.starts_with("POST /token HTTP/1.1"), "{request}");
        for part in ["code=4%2F0Ab", "code_verifier=VERIFIER", "grant_type=authorization_code", "redirect_uri=http%3A%2F%2F127.0.0.1%3A5", "client_secret=not-really-secret"] {
            assert!(request.contains(part), "{part} in {request}");
        }
        assert!(!format!("{:?}", tokens.access).contains("ya29"), "Debug never shows a token");
        assert!(!format!("{:?}", client()).contains("not-really-secret"));

        let (server, _) = serve("200 OK", r#"{"access_token":"a","expires_in":3599,"refresh_token":"r","scope":"openid"}"#);
        assert_eq!(exchange_code(&endpoints(&server), &client(), "c", "v", "r").err(), Some(AuthError::MissingScope));
        let (server, _) = serve("401 Unauthorized", r#"{"error":"invalid_client"}"#);
        assert_eq!(exchange_code(&endpoints(&server), &client(), "c", "v", "r").err(), Some(AuthError::BadClient));
    }

    #[test]
    fn refreshing_gets_a_new_access_token_or_asks_to_sign_in_again() {
        let (server, requests) = serve("200 OK", r#"{"access_token":"ya29.new","expires_in":3599,"scope":"https://www.googleapis.com/auth/calendar.readonly"}"#);
        let access = refresh(&endpoints(&server), &client(), "1//r").unwrap();
        assert_eq!(access.token, "ya29.new");
        let request = requests.recv().unwrap();
        assert!(request.contains("grant_type=refresh_token") && request.contains("refresh_token=1%2F%2Fr"), "{request}");
        let (server, _) = serve("400 Bad Request", r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#);
        assert_eq!(refresh(&endpoints(&server), &client(), "1//r").err(), Some(AuthError::SignInAgain));
        let (server, _) = serve("503 Service Unavailable", "");
        assert_eq!(refresh(&endpoints(&server), &client(), "1//r").err(), Some(AuthError::Status(503)));
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        assert_eq!(refresh(&endpoints(&address), &client(), "1//r").err(), Some(AuthError::Offline));
    }

    #[test]
    fn revoking_an_unknown_token_counts_as_revoked() {
        let (server, requests) = serve("200 OK", "{}");
        assert_eq!(revoke(&endpoints(&server), "1//r"), Ok(()));
        assert!(requests.recv().unwrap().contains("token=1%2F%2Fr"));
        let (server, _) = serve("400 Bad Request", r#"{"error":"invalid_token"}"#);
        assert_eq!(revoke(&endpoints(&server), "1//r"), Ok(()));
    }

    #[test]
    fn client_ids_come_from_settings_before_the_build() {
        let own = resolve_client("mine.apps.googleusercontent.com", || Some("s".into())).unwrap();
        assert_eq!((own.id.as_str(), own.secret.as_deref()), ("mine.apps.googleusercontent.com", Some("s")));
        assert_eq!(resolve_client("  ", || panic!("the store isn't read without an own ID")), built_in());
        assert_eq!(has_client(""), built_in().is_some());
        assert!(plausible_client_id(" 123-abc.apps.googleusercontent.com "));
        assert!(!plausible_client_id("a-client-secret") && !plausible_client_id("a b.apps.googleusercontent.com"));
    }
}
