//! Summarize one or more exported metrics files, per provider.
//! `cargo run --example latency_report -- <session.jsonl>...`
use std::collections::BTreeSet;

use cluely_rs::metrics::{KEY_SPANS, durations, read_jsonl, summarize};

fn main() -> anyhow::Result<()> {
    let mut marks = Vec::new();
    for path in std::env::args().skip(1) { marks.extend(read_jsonl(&std::fs::read_to_string(&path)?)?); }
    anyhow::ensure!(!marks.is_empty(), "usage: latency_report <session.jsonl>...");
    let providers: BTreeSet<Option<String>> = marks.iter().map(|m| m.provider.clone()).filter(Option::is_some).collect();
    let groups: Vec<Option<String>> = if providers.is_empty() { vec![None] } else { providers.into_iter().collect() };
    for provider in groups {
        println!("\n{}", provider.as_deref().unwrap_or("all marks"));
        for (label, from, to) in KEY_SPANS {
            match summarize(durations(&marks, *from, *to, provider.as_deref())) {
                Some(s) => println!("  {label:<40} n={:<4} p50={:>7.1} ms  p95={:>7.1} ms  max={:>7.1} ms", s.count, s.p50_ms, s.p95_ms, s.max_ms),
                None => println!("  {label:<40} no data"),
            }
        }
    }
    Ok(())
}
