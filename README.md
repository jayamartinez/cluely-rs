# CluelyRS

A native, keyboard-driven assistant overlay for Windows, built in Rust on [GPUI](https://gpui.rs). Press Start, and every
question you ask is answered with a fresh screenshot of what you're looking at, by the model you choose: your ChatGPT
subscription, your Claude subscription, or your own API key.

## Requirements

- Windows 10 2004 or later (hiding from screen capture needs `WDA_EXCLUDEFROMCAPTURE`)
- Rust stable with the MSVC toolchain (Visual Studio 2022 Build Tools)
- Optional, for subscription answers:
  - [Codex CLI](https://github.com/openai/codex) signed in with ChatGPT, for **ChatGPT subscription**
  - [Claude Code](https://claude.com/claude-code) signed in with a Claude plan, for **Claude subscription**

## Run

```powershell
cargo run            # debug build with a console for logs
cargo run --release  # windowless release build
```

## Using it

| Shortcut | Action |
|---|---|
| `Ctrl Shift ↵` | Start / stop a Live session |
| `Ctrl Shift Space` | Jump into the text box (Enter sends and returns you to your app; Esc returns without sending) |
| `Ctrl ↵` | Assist: answer from your screen and the conversation (only while Live) |
| `Ctrl \` | Show / hide the overlay |
| `Ctrl Alt ←↑↓→` | Move the overlay (hold to glide) |
| `Ctrl Alt Shift ↑↓` | Scroll the answer |
| `Esc` | Close settings |

Only the pill, the quick actions and the text box take mouse clicks; answers and the space around the overlay are
click-through. Move and scroll shortcuts are claimed only while the overlay is visible, and Assist only during a Live
session, so other apps keep their shortcuts.

**Answer with** (Settings → Model):

- **ChatGPT subscription** through the official Codex app-server, with tools, shell, MCP and network disabled, a read-only
  sandbox and an empty working folder.
- **Claude subscription** through the official Claude Code CLI with tools disabled.
- **Your API key**: Anthropic, OpenAI, OpenRouter, Google Gemini, xAI, Groq, DeepSeek, Mistral, Together, local Ollama or
  LM Studio, or any OpenAI-compatible URL. Keys are stored in Windows Credential Manager.

**Sessions** (clock button in the pill) is a normal window for reviewing past Live sessions: an AI summary at three
lengths, timeline, transcript, answers and the screenshots they used, plus search and "Ask about this session".

## Where data lives

| What | Where |
|---|---|
| Settings | `%APPDATA%\CluelyRS\settings.json` |
| Sessions and screenshots | `%APPDATA%\CluelyRS\sessions\` (kept 7 days, 30 days or forever) |
| API keys | Windows Credential Manager, service `CluelyRS` |

Nothing is uploaded except the requests sent to the provider you choose.

## Development

```powershell
cargo test
cargo clippy --all-targets

# Review the Sessions window with synthetic data
.\dev\make-demo-sessions.ps1 -Out "$env:TEMP\cluelyrs-demo"
$env:CLUELYRS_DATA_DIR = "$env:TEMP\cluelyrs-demo"; cargo run
```

Latency: set `CLUELYRS_METRICS=1` to export pipeline timings as JSON Lines to `%LOCALAPPDATA%\CluelyRS\metrics\`,
then compare runs with `cargo run --example latency_report -- <file.jsonl>...` (grouped per provider).

`CLUELYRS_ALLOW_CAPTURE=1` lets the overlay appear in screenshots while developing. Live provider tests are ignored by
default (`cargo test -- --ignored` with the variables documented in `claude_cli.rs` and `codex.rs`).

## Status

Working: overlay, keybinds, click-through, capture hiding, providers, screenshot on send, sessions and AI notes.
Not built yet: live transcription (Parakeet v3 with Whisper fallback), so answers rely on the screenshot and your
typed question for now.
