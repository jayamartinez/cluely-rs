# CluelyRS

A native, keyboard-driven assistant overlay for Windows, built in Rust on [GPUI](https://gpui.rs). Press Start, and every
question you ask is answered with a fresh screenshot of what you're looking at, by the model you choose: your ChatGPT
subscription, your Claude subscription, or your own API key.

## Install (Windows)

Download `CluelyRS-<version>-setup.exe` from [Releases](https://github.com/jayamartinez/cluely-rs/releases) and run
it. It installs for your account only (no administrator prompt) into `%LOCALAPPDATA%\Programs\CluelyRS`, and needs
Windows 10 2004 or later on a 64-bit PC with AVX2 (any Intel or AMD CPU from about 2013 on). Install a newer version over
the old one; your settings, sessions and keys stay.

The installer isn't code-signed, so Windows warns about it:

- **"Windows protected your PC"** (SmartScreen): click **More info**, then **Run anyway**.
- **Microsoft Defender** may flag or remove CluelyRS, because hiding from screen capture and global shortcuts look like
  what malware does. Restore it from Windows Security → Virus & threat protection → Protection history (choose
  **Allow on device**), and add an exclusion for `%LOCALAPPDATA%\Programs\CluelyRS` if it happens again.

Uninstall from Settings → Apps → Installed apps. It asks whether to also delete your settings, sessions, speech models
and saved API keys; otherwise they're kept for a later reinstall.

## Requirements (building from source)

- Windows 10 2004 or later (hiding from screen capture needs `WDA_EXCLUDEFROMCAPTURE`)
- Rust stable with the MSVC toolchain (Visual Studio 2022 Build Tools, including its CMake and Ninja components)
- CMake 3.18 or later and [Git for Windows](https://git-scm.com/download/win) (its bash applies parakeet.cpp's ggml
  patches during the build)
- Optional, for subscription answers:
  - [Codex CLI](https://github.com/openai/codex) signed in with ChatGPT, for **ChatGPT subscription**
  - [Claude Code](https://claude.com/claude-code) signed in with a Claude plan, for **Claude subscription**

## Run

```powershell
git clone git@github.com:jayamartinez/cluely-rs.git
cd cluely-rs
git submodule update --init third_party/parakeet.cpp
git -C third_party/parakeet.cpp submodule update --init third_party/ggml

cargo run            # debug build with a console for logs
cargo run --release  # windowless release build
```

Only those two submodules are needed; parakeet.cpp's optional CED and voice-detect submodules aren't built. ggml's
paths are deep, so in a long folder path run `git config --global core.longpaths true` first.

## Using it

| Shortcut | Action |
|---|---|
| `Ctrl Shift Space` | Jump into the text box; press again to return to your app, keeping what you typed (Enter sends and returns you to your app; Esc returns without sending) |
| `Ctrl ↵` | Assist: answer from your screen and the conversation (starts Live if it's off) |
| `Ctrl \` | Show / hide the overlay |
| `Ctrl Alt ←↑↓→` | Move the overlay (hold to glide) |
| `Ctrl Alt Shift ↑↓` | Scroll the answer |
| `Esc` | Close settings |

Live starts when you click the waveform in the toolbar, type a question, press a quick action or press Assist, and stops
when you click the waveform again; no shortcut stops it.

Only the pill, the quick actions and the text box take mouse clicks; answers and the space around the overlay are
click-through. Move and scroll shortcuts are claimed only while the overlay is visible, so other apps keep them. Assist
(`Ctrl ↵`) is claimed for as long as CluelyRS runs, so other apps don't get that chord.

**Answer with** (Settings → Model):

- **ChatGPT subscription** through the official Codex app-server, with tools, shell, MCP and network disabled, a read-only
  sandbox and an empty working folder.
- **Claude subscription** through the official Claude Code CLI with tools disabled.
- **Your API key**: Anthropic, OpenAI, OpenRouter, Google Gemini, xAI, Groq, DeepSeek, Mistral, Together, local Ollama or
  LM Studio, or any OpenAI-compatible URL. Keys are stored in Windows Credential Manager or the macOS Keychain.

Answers are short by default (one to three sentences, longer only when the question is genuinely complex) and use
the least reasoning the provider offers, so the first words stream within a few seconds. When Live starts, the ChatGPT
subscription opens its thread and the Claude subscription starts a Claude Code process for the next answer, so neither
pays its start-up when you press Assist. **Smart mode** (Settings → Model) asks for deeper reasoning and allows longer
answers when speed matters less.

**Listening** (Settings → Listening): a Live session transcribes desktop audio as **Them** and the microphone as **Me**
on this PC with NVIDIA Parakeet Realtime (English). The Live panel shows the last two lines and what each side is
saying right now; words the recognizer may still change are muted and italic, and a blue **?** marks a likely question.
The model (176 MB) downloads from Settings the first time, verified against a pinned checksum. Each source has its own
toggle and device picker (the system default unless you choose one). On macOS (13 or later), desktop audio is everything
the system plays, captured with ScreenCaptureKit, so its picker has the single option **System audio**; it needs the
Screen Recording permission (System Settings → Privacy & Security → Screen & System Audio Recording), and without it the
Live panel says so while the microphone keeps working. Turn **Transcribe conversations** off to keep Live to the screen
and typed questions.

**Deepgram** (optional): choose it under *Transcribe with* and save a Deepgram API key (kept in Windows Credential
Manager or the macOS Keychain). Live audio is then streamed to Deepgram's Nova-3 instead of being transcribed on
this PC; everything after the recognizer (transcript, endpointing, the overlay) is the same, so the two can be compared
with the latency report.

**Sessions** (clock button in the pill) is a normal window for reviewing past Live sessions: an AI summary at three
lengths, timeline, transcript, answers and the screenshots they used, plus search and "Ask about this session".

## Where data lives

| What | Windows | macOS |
|---|---|---|
| Settings | `%APPDATA%\CluelyRS\settings.json` | `~/Library/Application Support/CluelyRS/settings.json` |
| Sessions and screenshots | `%APPDATA%\CluelyRS\sessions\` | `~/Library/Application Support/CluelyRS/sessions/` |
| API keys | Windows Credential Manager, service `CluelyRS` | login Keychain, service `CluelyRS` |
| Speech models | `%LOCALAPPDATA%\CluelyRS\models\` | `~/Library/Application Support/CluelyRS/models/` |
| Logs (errors and crash reports) | `%LOCALAPPDATA%\CluelyRS\logs\cluelyrs.log` | `~/Library/Logs/CluelyRS/cluelyrs.log` |

Sessions are kept 7 days, 30 days or forever. Speech models download when first needed (~176 MB for Parakeet).

Nothing is uploaded except the requests sent to the provider you choose.

## Development

```powershell
cargo test
cargo clippy --all-targets

# Review the Sessions window with synthetic data
.\dev\make-demo-sessions.ps1 -Out "$env:TEMP\cluelyrs-demo"
$env:CLUELYRS_DATA_DIR = "$env:TEMP\cluelyrs-demo"; cargo run
```

Latency: set `CLUELYRS_METRICS=1` to export each Live session's pipeline timings as JSON Lines to
`%LOCALAPPDATA%\CluelyRS\metrics\`, together with what each source captured (`live-<time>-me.wav` and `-them.wav`,
16 kHz mono). Compare runs with `cargo run --example latency_report -- <file.jsonl>...` (grouped per provider).
`cargo run --example answer_latency -- [--warm] [--smart]` times one Assist-shaped answer through your selected provider
(one real request each run, so it uses your subscription or key).

Transcription: `cargo run --example parakeet_bench -- download` installs the Parakeet model, and the same example
benchmarks speed, latency, CPU, memory and long-run reliability (usage at the top of `examples/parakeet_bench.rs`; test
speech from `dev/make-bench-audio.ps1`). `cargo run --example transcript_replay -- <wav>` replays a recording (the
bench clip, or a session's exported capture) through the whole pipeline at real-time pace and prints where speech
starts and stops against every partial, end-of-utterance and commit, which is how endpointing problems from real
sessions are reproduced. `cargo run --example endpoint_eval` scores endpointing over every capture in the metrics
folder plus the bench clip: commits, fragments, merged questions, commit latency after speech stops and the reason
mix, per file. It runs Parakeet once per file (cached) and replays on the audio clock, so it is fast, deterministic
and makes no paid calls; `--config` compares settings side by side and `--stall` / `--slow` / `--lag` simulate a
recognizer falling behind a busy CPU (usage at the top of `examples/endpoint_eval.rs`). Changes to third-party code
and builds are documented in [PATCHES.md](PATCHES.md).

`CLUELYRS_ALLOW_CAPTURE=1` lets the overlay appear in screenshots while developing. Live provider tests are ignored by
default (`cargo test -- --ignored` with the variables documented in `claude_cli.rs` and `codex.rs`).
`CLUELYRS_LOG_FILE=1` makes a debug build write the log file as a release build does, instead of the console.

### Releasing (Windows)

Set `version` in `Cargo.toml`, then push a matching tag (`git tag v0.1.0 && git push origin v0.1.0`). The
[Release (Windows)](.github/workflows/release-windows.yml) workflow builds the exe (static C runtime, so no Visual C++
Redistributable is needed), generates `THIRD_PARTY_LICENSES.txt` with [cargo-about](https://github.com/EmbarkStudios/cargo-about),
builds the [Inno Setup](https://jrsoftware.org/isinfo.php) installer and attaches it to a draft release; publish the
draft once the macOS build is attached too. Attach exactly one `.exe`: the update check offers the first one it finds.
The same build locally, with Inno Setup 6.5 or later (for the PNG wizard images in `installer/`) and `cargo install cargo-about --locked --features cli`:

```powershell
cargo build --release
.\dev\third-party-licenses.ps1
.\dev\build-installer.ps1   # target\installer\CluelyRS-<version>-setup.exe
```

gpui's release build compiles shaders with `fxc.exe` from Windows SDK 10.0.26100; with another SDK, set
`GPUI_FXC_PATH` to its `fxc.exe` (for example `C:\Program Files (x86)\Windows Kits\10\bin\10.0.22621.0\x64\fxc.exe`).

## Status

Working: overlay, keybinds, click-through, capture hiding, providers, screenshot on send, sessions and AI notes, live
on-device transcription of Me and Them with the transcript saved per session, and answers that read the conversation
heard so far (the last 10 minutes or 40 lines, plus what is being said right now). During a Live session the ChatGPT
provider keeps one restricted Codex thread, so each answer only sends what is new; a newer request cancels the one in
flight, and nothing from a cancelled request reaches the screen.
Transcription runs on this PC with Parakeet, or optionally through Deepgram with your own key. Endpointing is
evaluated on recorded sessions, and a recognizer that falls behind (a busy CPU) is endpointed on its own clock, so
it neither cuts lines into fragments nor merges questions while it catches up.
Not yet: speculative answers.

## Third-party

- [parakeet.cpp](https://github.com/mudler/parakeet.cpp) (MIT) and [ggml](https://github.com/ggml-org/ggml) (MIT),
  built from the pinned submodule; see [PATCHES.md](PATCHES.md).
- Setup's and Settings' icons and logos in `assets/`: [Lucide](https://lucide.dev) icons (ISC License) in `assets/icons/` (`archive`,
  `arrow-right`, `chevrons-up-down`, `circle-user`, `cloud`, `download`, `hourglass`, `key-round`, `laptop-minimal`,
  `link`, `lock`, `message-square`, `mic`, `mic-off`, `monitor`, `move`, `panel-bottom`, `plug`, `radio`, `send`,
  `server`, `shield`, `sparkles`, `terminal`, `type`, `volume-2`, `zap`); the Anthropic, Claude, Deepgram, Google Gemini, Ollama
  and OpenRouter logos from [Simple Icons](https://simpleicons.org) (CC0) and the OpenAI, xAI, Groq, DeepSeek,
  Mistral, Together AI and LM Studio logos from [Lobe Icons](https://github.com/lobehub/lobe-icons) (MIT) in
  `assets/logos/`. The logos are their owners' trademarks, shown
  only to name the services CluelyRS connects to.
- [NVIDIA Parakeet Realtime EOU 120M](https://huggingface.co/nvidia/parakeet_realtime_eou_120m-v1), NVIDIA Open Model
  License. Downloaded at runtime, not distributed with CluelyRS.

## License

CluelyRS is released under the [MIT License](LICENSE). The third-party components above keep their own licenses.
