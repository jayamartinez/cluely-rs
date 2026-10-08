//! Summarize one or more exported metrics files, per provider, then speculative answers: how
//! often a prepared answer was shown, and how long asked-for answers took to appear with and
//! without one. Sessions recorded without speculation get an estimate of what it would have done.
//! `cargo run --example latency_report -- <session.jsonl>...`
use std::collections::BTreeSet;

use cluely_rs::metrics::{KEY_SPANS, Speculation, Summary, durations, read_jsonl, speculation, summarize, what_if};

fn main() -> anyhow::Result<()> {
    // Utterance ids and times restart in every session, so pairing happens per file.
    let mut sessions = Vec::new();
    for path in std::env::args().skip(1) { sessions.push(read_jsonl(&std::fs::read_to_string(&path)?)?); }
    let marks: Vec<_> = sessions.iter().flatten().cloned().collect();
    anyhow::ensure!(!marks.is_empty(), "usage: latency_report <session.jsonl>...");
    let providers: BTreeSet<Option<String>> = marks.iter().map(|m| m.provider.clone()).filter(Option::is_some).collect();
    let groups: Vec<Option<String>> = if providers.is_empty() { vec![None] } else { providers.into_iter().collect() };
    for provider in groups {
        println!("\n{}", provider.as_deref().unwrap_or("all marks"));
        for (label, from, to) in KEY_SPANS {
            print_summary(label, summarize(durations(&marks, *from, *to, provider.as_deref())));
        }
    }

    let mut summary = Speculation::default();
    for session in &sessions {
        let one = speculation(session);
        summary.started += one.started;
        summary.hits += one.hits;
        summary.misses += one.misses;
        summary.cancelled += one.cancelled;
        summary.with_ms.extend(one.with_ms);
        summary.without_ms.extend(one.without_ms);
    }
    println!("\nspeculative answers");
    println!("  started {}, shown on request (hits) {}, misses {}, cancelled unshown {}", summary.started, summary.hits, summary.misses, summary.cancelled);
    match summary.hit_rate() {
        Some(rate) => println!("  hit rate {:.0}% of requests that had a speculative answer running", rate * 100.0),
        None => println!("  hit rate: no request had a speculative answer running"),
    }
    println!("  asked → first visible words");
    print_summary("    with a speculative answer", summarize(summary.with_ms));
    print_summary("    without", summarize(summary.without_ms));

    let (mut hits, mut considered, mut pairs) = (0, 0, Vec::new());
    for session in &sessions {
        let (h, c, p) = what_if(session);
        (hits, considered) = (hits + h, considered + c);
        pairs.extend(p);
    }
    if considered > 0 {
        println!("\nestimate for requests recorded without speculation (answers to a committed question, nothing said since)");
        println!("  {hits} of {considered} requests would have been hits");
        print_summary("    request → first words, as recorded", summarize(pairs.iter().map(|p| p.0).collect()));
        print_summary("    estimated with speculation", summarize(pairs.iter().map(|p| p.1).collect()));
    }
    Ok(())
}

fn print_summary(label: &str, summary: Option<Summary>) {
    match summary {
        Some(s) => println!("  {label:<40} n={:<4} p50={:>7.1} ms  p95={:>7.1} ms  max={:>7.1} ms", s.count, s.p50_ms, s.p95_ms, s.max_ms),
        None => println!("  {label:<40} no data"),
    }
}
