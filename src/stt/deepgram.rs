//! Deepgram streaming transcription (Nova-3) over a WebSocket, as the optional cloud and
//! reference provider. It sits behind the same [`StreamingAsr`] interface as Parakeet, so
//! transcript state, endpointing and the overlay are shared; only the audio leaves the PC.
//!
//! The API key lives in Windows Credential Manager under the provider id `deepgram` (or
//! `DEEPGRAM_API_KEY` for the replay example). It is never logged or formatted into errors.
//!
//! Live test (paid; only runs when you set the variable):
//!   DEEPGRAM_API_KEY=... cargo test --lib stt::deepgram -- --ignored

use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::audio::{AudioChunk, SAMPLE_RATE};
use crate::stt::{AsrError, Availability, Capabilities, EventKind, EventSink, Locality, StreamingAsr, StreamingAsrSession};

pub const PROVIDER_ID: &str = "deepgram";
const ENDPOINT: &str = "wss://api.deepgram.com/v1/listen";
const MODEL: &str = "nova-3";
/// Audio is sent in blocks this long, not per 10 ms chunk.
const SEND_EVERY_MS: f64 = 100.0;
/// Deepgram closes a stream that is silent for ~10 s; a KeepAlive every few seconds prevents it.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(5);
/// How long `finish` waits for the trailing results after CloseStream.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

pub struct Deepgram {
    key: Option<String>,
}

impl Deepgram {
    /// Uses the key in Credential Manager.
    pub fn from_store() -> Self { Self { key: crate::secrets::get(PROVIDER_ID) } }

    pub fn with_key(key: impl Into<String>) -> Self { Self { key: Some(key.into()) } }
}

/// The listen URL with the streaming parameters: raw 16 kHz mono PCM, interim results, and
/// Deepgram's own endpointing and utterance-end signals (one input to our endpointer).
pub fn listen_url() -> String {
    format!("{ENDPOINT}?model={MODEL}&encoding=linear16&sample_rate={SAMPLE_RATE}&channels=1&interim_results=true\
        &endpointing=300&utterance_end_ms=1000&vad_events=true&punctuate=true&smart_format=false")
}

impl StreamingAsr for Deepgram {
    fn capabilities(&self) -> Capabilities {
        Capabilities { id: PROVIDER_ID, label: "Deepgram (cloud)", summary: "Deepgram Nova-3 over the network. Needs an API key; audio is sent to Deepgram.",
            locality: Locality::Cloud, requires_api_key: true, emits_end_of_utterance: true, languages: &[],
            // Interim results arrive about once a second while speech continues.
            text_lag_ms: 1500.0 }
    }

    fn availability(&self) -> Availability {
        if self.key.is_some() { Availability::Ready } else { Availability::NeedsApiKey }
    }

    fn start_session(&self, sink: EventSink) -> Result<Box<dyn StreamingAsrSession>, AsrError> {
        let key = self.key.as_deref().ok_or(AsrError::NotReady(Availability::NeedsApiKey))?;
        let socket = connect(key).map_err(AsrError::Failed)?;
        Ok(Box::new(Session { sink, socket, origin_ms: None, pending: Vec::new(), last_sent: Instant::now(), closed: false }))
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Open the stream. Error text never includes the key: tungstenite's handshake errors can echo
/// the response, so they are reduced to a status code or a fixed message.
fn connect(key: &str) -> Result<Socket, String> {
    let mut request = listen_url().into_client_request().map_err(|_| "Couldn't build the Deepgram request.".to_string())?;
    let auth = format!("Token {key}").parse().map_err(|_| "The Deepgram API key has characters that can't be sent.".to_string())?;
    request.headers_mut().insert("Authorization", auth);
    let (mut socket, _) = tungstenite::connect(request).map_err(|error| connect_error(&error))?;
    // Reads happen between audio pushes, so they must not block.
    set_nonblocking(&mut socket, true)?;
    Ok(socket)
}

fn connect_error(error: &tungstenite::Error) -> String {
    match error {
        tungstenite::Error::Http(response) => match response.status().as_u16() {
            401 | 403 => "Deepgram rejected the API key.".into(),
            402 => "The Deepgram account has no credit left.".into(),
            status => format!("Deepgram refused the connection (HTTP {status})."),
        },
        tungstenite::Error::Io(_) | tungstenite::Error::Tls(_) => "Couldn't reach Deepgram. Check the network connection.".into(),
        _ => "Couldn't connect to Deepgram.".into(),
    }
}

fn set_nonblocking(socket: &mut Socket, on: bool) -> Result<(), String> {
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(tls) => tls.get_mut(),
        _ => return Err("Unexpected Deepgram connection type.".into()),
    };
    stream.set_nonblocking(on).map_err(|_| "Couldn't configure the Deepgram connection.".to_string())?;
    if !on { let _ = stream.set_read_timeout(Some(CLOSE_WAIT)); }
    Ok(())
}

struct Session {
    sink: EventSink,
    socket: Socket,
    /// Audio time of the first sample sent; Deepgram's times are seconds from there.
    origin_ms: Option<f64>,
    pending: Vec<u8>,
    last_sent: Instant,
    closed: bool,
}

impl StreamingAsrSession for Session {
    fn push(&mut self, chunk: &AudioChunk) -> Result<(), AsrError> {
        if self.closed { return Ok(()); }
        if self.origin_ms.is_none() { self.origin_ms = Some(chunk.start_ms); }
        self.pending.extend(chunk.samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()));
        if self.pending.len() as f64 >= SEND_EVERY_MS * SAMPLE_RATE as f64 / 1000.0 * 2.0 {
            let block = std::mem::take(&mut self.pending);
            self.send(Message::Binary(block.into()))?;
            self.last_sent = Instant::now();
        } else if self.last_sent.elapsed() >= KEEPALIVE_EVERY {
            self.send(Message::Text(r#"{"type":"KeepAlive"}"#.into()))?;
            self.last_sent = Instant::now();
        }
        self.drain()
    }

    fn finish(&mut self) -> Result<(), AsrError> {
        if self.closed { return Ok(()); }
        if !self.pending.is_empty() {
            let block = std::mem::take(&mut self.pending);
            self.send(Message::Binary(block.into()))?;
        }
        self.send(Message::Text(r#"{"type":"CloseStream"}"#.into()))?;
        // Trailing finals arrive within a moment; wait for them with a bounded blocking read.
        set_nonblocking(&mut self.socket, false).map_err(AsrError::Failed)?;
        let deadline = Instant::now() + CLOSE_WAIT;
        while Instant::now() < deadline && !self.closed {
            match self.socket.read() {
                Ok(message) => self.handle(message),
                Err(_) => break,
            }
        }
        self.closed = true;
        let _ = self.socket.close(None);
        Ok(())
    }
}

impl Session {
    fn send(&mut self, message: Message) -> Result<(), AsrError> {
        match self.socket.send(message) {
            Ok(()) => Ok(()),
            // The socket is non-blocking: a full buffer is flushed on a later send.
            Err(tungstenite::Error::Io(error)) if error.kind() == ErrorKind::WouldBlock => Ok(()),
            Err(error) => { self.fail(&error); Err(AsrError::Failed("The Deepgram connection was lost.".into())) }
        }
    }

    /// Handle whatever has arrived without waiting.
    fn drain(&mut self) -> Result<(), AsrError> {
        loop {
            match self.socket.read() {
                Ok(message) => self.handle(message),
                Err(tungstenite::Error::Io(error)) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) => { self.fail(&error); return Err(AsrError::Failed("The Deepgram connection was lost.".into())); }
            }
            if self.closed { return Ok(()); }
        }
    }

    fn fail(&mut self, error: &tungstenite::Error) {
        if self.closed { return; }
        self.closed = true;
        let message = match error {
            tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => "Deepgram closed the stream.".to_string(),
            _ => "The Deepgram connection was lost.".to_string(),
        };
        let at = self.now_ms();
        self.sink.emit(at, at, EventKind::Error { message, fatal: true });
    }

    fn now_ms(&self) -> f64 { self.origin_ms.unwrap_or(0.0) + self.pending.len() as f64 / 2.0 * 1000.0 / SAMPLE_RATE as f64 }

    fn handle(&mut self, message: Message) {
        let origin = self.origin_ms.unwrap_or(0.0);
        match message {
            Message::Text(text) => match parse_message(&text) {
                Parsed::Results { transcript, start, duration, is_final, speech_final } => {
                    let (start_ms, end_ms) = (origin + start * 1000.0, origin + (start + duration) * 1000.0);
                    if transcript.trim().is_empty() && !speech_final { return; }
                    if speech_final { self.sink.emit(start_ms, end_ms, EventKind::EndOfUtterance { text: transcript }); }
                    else if is_final { self.sink.emit(start_ms, end_ms, EventKind::Final { text: transcript }); }
                    else { self.sink.emit(start_ms, end_ms, EventKind::Partial { text: transcript, stable_hint: None }); }
                }
                Parsed::UtteranceEnd { last_word_end } => {
                    let at = origin + last_word_end * 1000.0;
                    self.sink.emit(at, at, EventKind::EndOfUtterance { text: String::new() });
                }
                Parsed::Error(message) => {
                    self.closed = true;
                    let at = self.now_ms();
                    self.sink.emit(at, at, EventKind::Error { message, fatal: true });
                }
                Parsed::Other => {}
            },
            Message::Close(_) => self.closed = true,
            _ => {}
        }
    }
}

/// What a message from Deepgram means for the pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Results { transcript: String, start: f64, duration: f64, is_final: bool, speech_final: bool },
    UtteranceEnd { last_word_end: f64 },
    /// A user-safe description; Deepgram's own text is reduced to its code.
    Error(String),
    /// Metadata, SpeechStarted and anything unknown.
    Other,
}

pub fn parse_message(text: &str) -> Parsed {
    let Ok(value) = serde_json::from_str::<Value>(text) else { return Parsed::Other };
    match value.get("type").and_then(Value::as_str) {
        Some("Results") => Parsed::Results {
            transcript: value.pointer("/channel/alternatives/0/transcript").and_then(Value::as_str).unwrap_or_default().to_string(),
            start: value.get("start").and_then(Value::as_f64).unwrap_or(0.0),
            duration: value.get("duration").and_then(Value::as_f64).unwrap_or(0.0),
            is_final: value.get("is_final").and_then(Value::as_bool).unwrap_or(false),
            speech_final: value.get("speech_final").and_then(Value::as_bool).unwrap_or(false),
        },
        Some("UtteranceEnd") => Parsed::UtteranceEnd { last_word_end: value.get("last_word_end").and_then(Value::as_f64).unwrap_or(0.0) },
        Some("Error") | None if value.get("err_code").is_some() || value.get("error").is_some() => {
            let code = value.get("err_code").and_then(Value::as_str).unwrap_or("unknown");
            Parsed::Error(format!("Deepgram reported an error ({code})."))
        }
        _ => Parsed::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_interim_final_and_speech_final_are_told_apart() {
        let interim = r#"{"type":"Results","start":1.5,"duration":0.8,"is_final":false,"speech_final":false,"channel":{"alternatives":[{"transcript":"how would you","confidence":0.9}]}}"#;
        assert_eq!(parse_message(interim), Parsed::Results { transcript: "how would you".into(), start: 1.5, duration: 0.8, is_final: false, speech_final: false });
        let is_final = r#"{"type":"Results","start":1.5,"duration":1.2,"is_final":true,"speech_final":false,"channel":{"alternatives":[{"transcript":"how would you design"}]}}"#;
        assert!(matches!(parse_message(is_final), Parsed::Results { is_final: true, speech_final: false, .. }));
        let speech_final = r#"{"type":"Results","start":2.7,"duration":0.9,"is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"a cache?"}]}}"#;
        assert!(matches!(parse_message(speech_final), Parsed::Results { speech_final: true, .. }));
    }

    #[test]
    fn utterance_end_metadata_errors_and_junk_are_classified() {
        assert_eq!(parse_message(r#"{"type":"UtteranceEnd","last_word_end":3.61,"channel":[0,1]}"#), Parsed::UtteranceEnd { last_word_end: 3.61 });
        assert_eq!(parse_message(r#"{"type":"Metadata","request_id":"abc","model_info":{}}"#), Parsed::Other);
        assert_eq!(parse_message(r#"{"type":"SpeechStarted","timestamp":0.5}"#), Parsed::Other);
        assert_eq!(parse_message(r#"{"type":"Error","err_code":"DATA-0000","err_msg":"secret stuff sk-1234567890abcdef"}"#),
            Parsed::Error("Deepgram reported an error (DATA-0000).".into()));
        assert_eq!(parse_message("not json"), Parsed::Other);
    }

    #[test]
    fn the_listen_url_pins_the_audio_format_and_endpointing_options() {
        let url = listen_url();
        assert!(url.starts_with("wss://api.deepgram.com/v1/listen?model=nova-3"));
        for param in ["encoding=linear16", "sample_rate=16000", "channels=1", "interim_results=true", "endpointing=300", "utterance_end_ms=1000", "vad_events=true"] {
            assert!(url.contains(param), "{param}");
        }
    }

    #[test]
    fn connection_errors_never_carry_the_key() {
        let key = "dg_secret_1234567890abcdef1234";
        let provider = Deepgram::with_key(key);
        assert_eq!(provider.availability(), Availability::Ready);
        assert_eq!(Deepgram { key: None }.availability(), Availability::NeedsApiKey);
        let text = connect_error(&tungstenite::Error::Io(std::io::Error::other(key)));
        assert!(!text.contains(key) && !text.contains("secret"), "{text}");
        let not_ready = Deepgram { key: None }.start_session(EventSink::new(crate::audio::Source::Me, crate::stt::Generation(1), std::sync::mpsc::channel().0));
        assert!(matches!(not_ready, Err(AsrError::NotReady(Availability::NeedsApiKey))));
    }

    #[test]
    fn results_map_onto_the_session_audio_clock() {
        // A session whose first chunk starts at 10 s: Deepgram's 1.5 s means 11.5 s of audio time.
        let (tx, rx) = std::sync::mpsc::channel();
        let mut session = Session { sink: EventSink::new(crate::audio::Source::Them, crate::stt::Generation(1), tx),
            socket: unsafe_dummy_socket(), origin_ms: Some(10_000.0), pending: Vec::new(), last_sent: Instant::now(), closed: false };
        session.handle(Message::Text(r#"{"type":"Results","start":1.5,"duration":0.5,"is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"ok"}]}}"#.into()));
        session.handle(Message::Text(r#"{"type":"UtteranceEnd","last_word_end":2.0}"#.into()));
        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!((events[0].start_ms, events[0].end_ms), (11_500.0, 12_000.0));
        assert_eq!(events[0].kind, EventKind::EndOfUtterance { text: "ok".into() });
        assert_eq!((events[1].start_ms, events[1].kind.clone()), (12_000.0, EventKind::EndOfUtterance { text: String::new() }));
        std::mem::forget(session);
    }

    /// A socket that is never read or written: `handle` only parses messages.
    fn unsafe_dummy_socket() -> Socket {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        WebSocket::from_raw_socket(MaybeTlsStream::Plain(stream), tungstenite::protocol::Role::Client, None)
    }

    /// Streams the bench clip and expects at least one final. Paid; needs DEEPGRAM_API_KEY.
    #[test]
    #[ignore]
    fn live_stream_produces_finals() {
        let Ok(key) = std::env::var("DEEPGRAM_API_KEY") else { return };
        let wav = std::env::var("CLUELYRS_BENCH_WAV").expect("CLUELYRS_BENCH_WAV=<16 kHz mono wav from dev/make-bench-audio.ps1>");
        let bytes = std::fs::read(wav).unwrap();
        // The data chunk follows a fmt chunk whose size varies (System.Speech writes 18 bytes).
        let data = bytes.windows(4).position(|w| w == b"data").expect("no data chunk") + 8;
        let pcm: Vec<f32> = bytes[data..].chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let provider = Deepgram::with_key(key);
        let mut session = provider.start_session(EventSink::new(crate::audio::Source::Them, crate::stt::Generation(1), tx)).unwrap();
        for (i, block) in pcm.chunks(320).enumerate() {
            session.push(&AudioChunk { source: crate::audio::Source::Them, start_ms: i as f64 * 20.0, samples: block.to_vec() }).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        session.finish().unwrap();
        let events: Vec<_> = rx.try_iter().collect();
        let settled: Vec<&str> = events.iter().filter_map(|e| match &e.kind {
            EventKind::Final { text } | EventKind::EndOfUtterance { text } if !text.is_empty() => Some(text.as_str()), _ => None }).collect();
        let ends = events.iter().filter(|e| matches!(e.kind, EventKind::EndOfUtterance { .. })).count();
        println!("{} settled segments, {ends} end-of-utterance signals: {settled:?}", settled.len());
        assert!(settled.iter().any(|text| text.to_lowercase().contains("cache")), "{settled:?}");
        // The clip has six questions with pauses between them.
        assert!(ends >= 6, "{ends} end-of-utterance signals");
        assert!(events.iter().all(|e| !matches!(e.kind, EventKind::Error { .. })), "{events:?}");
    }
}
