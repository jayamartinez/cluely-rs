//! Shows where CluelyRS finds the Codex and Claude Code CLIs, and that each starts through that
//! path with the PATH CluelyRS gives it. Only `--version` is run: no sign-in, no prompt.
//!
//! cargo run --example cli_paths
//! env PATH=/usr/bin:/bin:/usr/sbin:/sbin cargo run --example cli_paths   (as if opened from Finder)

use std::process::{Command, Stdio};
use std::time::Instant;

use cluely_rs::{claude_cli::ClaudeCli, cli_path, codex};

fn main() {
    println!("process PATH: {}", std::env::var("PATH").unwrap_or_default());
    let started = Instant::now();
    let search = cli_path::search_path();
    println!("search PATH ({} ms): {}", started.elapsed().as_millis(), search.to_string_lossy());
    for (name, found) in [("codex", codex::executable()), ("claude", ClaudeCli::executable())] {
        let path = match found {
            Ok(path) => path,
            Err(message) => { println!("{name}: not found: {message}"); continue; }
        };
        println!("{name}: {}", path.display());
        let output = Command::new(&path).arg("--version").env("PATH", &search).stdin(Stdio::null()).output();
        match output {
            Ok(output) => println!("  --version ({}): {}", output.status, String::from_utf8_lossy(&output.stdout).trim()),
            Err(error) => println!("  --version failed: {error}"),
        }
    }
}
