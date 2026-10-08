//! Lightweight latency instrumentation. Every pipeline stage records a [`Mark`]; sessions can
//! be exported (opt-in, `CLUELYRS_METRICS=1`) and compared with `examples/latency_report.rs`.

pub mod recorder;
pub mod report;

use std::path::PathBuf;

pub use recorder::{Context, LatencyRecorder, Mark, SourceTag, Stage, export_enabled, read_jsonl};
pub use report::{KEY_SPANS, Speculation, Summary, durations, speculation, summarize, what_if};

/// Where exported metrics go: `%LOCALAPPDATA%\CluelyRS\metrics` (machine-local, never synced).
pub fn metrics_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("CluelyRS").join("metrics"))
}
