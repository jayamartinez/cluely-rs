//! Answers during a Live session. A [`ReasoningSession`] lives as long as the session, keeps
//! the selected provider warm, runs one request at a time with cancellation and
//! stale-generation protection, and gives every request the conversation heard so far.
//!
//! What "warm" means per provider:
//! - **ChatGPT subscription (Codex app-server):** one restricted thread is opened for the
//!   session and every request is a turn on it, so the conversation accumulates server-side
//!   instead of being replayed with each request. If the app-server restarts, the next turn
//!   opens a new thread with the same restrictions and folds the history in.
//! - **Claude subscription (Claude Code CLI):** one process per request. The CLI runs with
//!   session persistence disabled on purpose, so there is nothing to resume; the request
//!   context is prebuilt and the history is replayed.
//! - **API providers:** the HTTP agent is shared for the app's lifetime, so requests reuse the
//!   TLS connection; the history is replayed.
//!
//! Answers can also be prepared before they're asked for (`speculation`): preparation (no model
//! requests) for every question heard, and, when Settings turns it on, a speculative answer: a
//! real request to the selected model that runs on its own thread or process and is shown the
//! moment Assist is pressed if the conversation still matches.

pub mod context;
pub mod session;
pub mod speculation;

pub use context::{Conversation, Line, Now};
pub use session::{Claimed, Generation, Reply, ReasoningSession, Request};
