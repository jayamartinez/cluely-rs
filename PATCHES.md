# Third-party changes

CluelyRS carries no edits to third-party source files. This file records every place where its build or code departs from
how upstream ships, and how to undo each one.

## parakeet.cpp

- **Upstream:** [mudler/parakeet.cpp](https://github.com/mudler/parakeet.cpp) (MIT), a git submodule at
  `third_party/parakeet.cpp`.
- **Pinned:** `v0.6.0`, commit `da8bb4c45d08e081e759849bc6ee0a38720eb0b9`. Its ggml submodule is `v0.13.0`
  (`e705c5fed490514458bdd2eaddc43bd098fcce9b`).
- **Model:** `realtime_eou_120m-v1-q8_0.gguf` from
  [mudler/parakeet-cpp-gguf](https://huggingface.co/mudler/parakeet-cpp-gguf), revision
  `741158ae71e64ef5c89385862c18f777d07a97a1` (SHA-256 `62616b914d6f5a683a5dea672df055b57de5c49dddf871b8b44b9c814dc3d896`). The model is NVIDIA's
  [parakeet_realtime_eou_120m-v1](https://huggingface.co/nvidia/parakeet_realtime_eou_120m-v1), under the NVIDIA Open
  Model License. It is downloaded at runtime to `%LOCALAPPDATA%\CluelyRS\models\parakeet\` and never bundled or
  committed.

### 1. Inference thread count (shim)

- **What:** `native/parakeet_shim.cpp` exports `cluelyrs_parakeet_set_threads(int)`. It is a one-line C wrapper over
  parakeet.cpp's existing internal `pk::set_num_threads`, which upstream's own CLI uses for `--threads`. No upstream file
  is modified.
- **Why:** v0.6.0 decodes streams with a hard-coded default of 8 threads (`kDefaultThreads` in `src/ggml_graph.cpp`). Its
  C API has no way to change that for streaming. `parakeet_capi_set_concurrency` exists, but it is documented not to
  affect streaming sessions. Mic and desktop audio each run a stream all the time, so the thread count directly sets
  how much of the machine transcription takes.
- **CluelyRS default:** 4 threads, shared by all streams (`stt::parakeet::DEFAULT_THREADS`). See the measurements below.
- **To remove:** when upstream exposes thread configuration in its C API, call that from `stt::parakeet::ffi::set_threads`.
  Then delete its export from `native/parakeet_shim.cpp` (and the file and the `cc::Build` block in `build.rs` once
  section 4's export is gone too). If the shim stops linking after a
  version bump (for example because `pk::set_num_threads` was renamed), check upstream for an equivalent before
  restoring it.

### 2. Build configuration (`build.rs`)

These are CMake options and compiler flags, not source changes. To undo one, delete its line in `build.rs`. The flags
and `advapi32` below are MSVC-only; macOS differences are listed after them.

- **Removable on upgrade:** `/Dfseeko=_fseeki64 /Dftello=_ftelli64`. v0.6.0 calls the POSIX `fseeko`/`ftello`, which
  MSVC doesn't provide; this maps them to MSVC's 64-bit equivalents. Remove it once upstream builds with MSVC as-is.
- **Defaults restored:** `/DWIN32 /D_WINDOWS /GR /EHsc`. Passing any C++ flags replaces CMake's MSVC defaults, so the
  defaults are restored here. Exception unwinding matters because parakeet.cpp reports load errors with C++ exceptions.
- **`GGML_OPENMP=OFF`:** ggml enables OpenMP by default. With MSVC's OpenMP runtime, idle worker threads spin between
  decodes. Two live streams at real-time pace then used **5.3 cores** on average with OpenMP, against **1.0 core** with
  ggml's own thread pool. Latency was equal or better, and dropping OpenMP also removes the `VCOMP140.dll` dependency.
- **Smaller build:** `GGML_NATIVE=OFF`, plus CED, voice-detect, the CLI and the server off. `GGML_NATIVE=OFF` gives a
  portable x86-64 build instead of one tuned to the build machine; the others build only the libraries CluelyRS links.
- **Git's bash:** `BASH_EXECUTABLE` is pointed at Git for Windows' bash. Upstream applies its own in-tree ggml patches
  with bash at configure time, and one of them speeds up CPU matrix multiplication. Applying them modifies files inside
  `third_party/parakeet.cpp/third_party/ggml`, so `.gitmodules` sets `ignore = dirty` for the submodule.
- **macOS (Apple clang):** no compiler flags are needed. CMake finds the system bash, so upstream's ggml patches apply as
  they do on Windows. `GGML_NATIVE=OFF` builds for the macOS arm64 baseline, which every Apple silicon Mac supports.
  `GGML_BLAS=OFF`: streaming decodes run on Metal or ggml's CPU backend, never its BLAS backend. Accelerate stays on (upstream's
  default) and is linked, because ggml-cpu uses it for vector math.
- **macOS, Metal on:** `PARAKEET_GGML_METAL=ON` (upstream ships it off) with `GGML_METAL_EMBED_LIBRARY=ON`, so the
  Metal shaders are compiled into the binary and nothing has to ship beside it. `ggml-metal` and the Foundation, Metal and
  MetalKit frameworks are linked. On an M1, streams decode at about the CPU's latency with a fraction of its CPU time
  (measurements below). parakeet.cpp picks the GPU by itself; `PARAKEET_DEVICE=cpu` still forces the CPU backend. Metal
  needs the backend shutdown in section 4. To undo, set `PARAKEET_GGML_METAL` back to `OFF` and drop `ggml-metal`, the
  three frameworks and `GGML_METAL_EMBED_LIBRARY`; the shutdown can stay, it is harmless on the CPU.

### 3. Stream reset after each utterance (provider behaviour)

- **What:** `stt::parakeet::session` replaces the parakeet.cpp stream after every end-of-utterance. It also replaces a
  stream that has run 20 s and then gone quiet for 1 s. On each reset, the last few seconds of audio after the cut are
  replayed into the new stream from a rolling buffer, so nothing that was still being decoded is lost. This logic lives
  only in the Parakeet provider; nothing else knows streams are replaced.
- **Why:** with v0.6.0, a stream kept running across utterances misses many end-of-utterance signals and drifts in its
  output. In the run below, the single stream got 5 of the 8 EOUs per pass, from the first pass on, and its text varied
  between identical passes.
- **To remove:** when a parakeet.cpp release keeps EOU detection on long streams, set `MAX_STREAM_MS` to infinity and
  stop resetting on events in `Session::feed_decoder`. Then rerun `parakeet_bench long` to confirm.

### 4. Backend shutdown before exit (shim, macOS)

- **What:** `native/parakeet_shim.cpp` also exports `cluelyrs_parakeet_shutdown_backend()`, a C wrapper over
  parakeet.cpp's internal `pk::shutdown_backend`, which upstream's CLI and server call before returning from `main`.
  `stt::parakeet::ffi` counts models from before they load until after they are freed (a stream keeps its model alive).
  `stt::parakeet::shutdown()` waits up to 3 s for the last one, then frees the backend; from then on model loads fail,
  so nothing can bring the backend back. It is safe with no model loaded and when called twice. If models are still
  alive after 3 s, it leaves the backend alone (freeing it under them would be a use-after-free) and logs that.
- **Where it runs:** on every macOS quit (⌘Q, the app menu, Settings › About › Quit, a quit Apple Event, logout): all of
  them reach GPUI's `on_app_quit`, where the overlay first stops listening and then calls `stt::parakeet::shutdown()`.
  The `parakeet_bench`, `transcript_replay` and `endpoint_eval` examples call it after `run` returns, once every model,
  stream and worker thread is gone. Windows keeps its quit path unchanged; the examples call it there too, harmlessly.
- **Why:** parakeet.cpp keeps a process-wide compute backend (`g_backend` in `src/ggml_graph.cpp`) that holds Metal
  buffers. Left to static destructors at exit, ggml aborts in `ggml_metal_rsets_free` ("you haven't deallocated all
  Metal resources before exiting").
- **To remove:** when upstream's C API frees the backend itself (for example with the last context), call that from
  `ffi::shutdown_backend`, or drop the call if nothing is needed, and delete the export from the shim.

### Measurements

All measurements were taken on 2026-10-05, on a 16-thread desktop running Windows 11, with a Rust debug build (the
native code is always optimized). The input was `dev/make-bench-audio.ps1` output: 33.6 s, 8 utterance ends per pass.

**Thread sweep.** `cargo run --example parakeet_bench -- <wav> sweep`. RTF is decode time divided by audio time; lower
is faster.

| Threads | RTF, 1 stream | RTF, 2 streams | p95 per-block decode, 2 streams |
|---|---|---|---|
| 2 | 0.201 | 0.401 | 63.6 ms |
| 4 | 0.129 | 0.247 | 38.9 ms |
| 8 | 0.107 | 0.216 | 35.0 ms |

**Real time, Me and Them together.** `parakeet_bench <wav> realtime`. Latency runs from the EOU point in the audio to
its event.

| Threads | CPU (cores) | EOU latency p50 / p95 | Peak working set |
|---|---|---|---|
| 2 | 0.84 | ~157 / ~200 ms | 216 MB |
| 4 | 1.03 | ~145 / ~165 ms | 225 MB |
| 8 | 1.71 | ~142 / ~166 ms | 216 MB |

With OpenMP on, the same 4-thread run used 5.31 cores, at a p95 of ~171 ms.

**Long run.** `parakeet_bench <wav> long --reps 20`: 11.5 minutes of the same audio repeated.

| | EOUs (160 expected) | Words per quarter |
|---|---|---|
| One raw stream | 93 (58%) | 326 / 311 / 333 / 314 |
| Provider (with resets) | 158 (98.8%) | 315 / 315 / 315 / 315 |

The provider's two missed EOUs fell in different passes, and its words were identical in every quarter, so no text was
lost. Endpointing commits on silence when an EOU doesn't arrive.

### Measurements on macOS

Taken on 2026-10-08 on an M1 MacBook Air (4 performance and 4 efficiency cores, no fan), macOS 26.3, with a Rust debug
build. The input was `dev/make-bench-audio.sh` output (the same questions as the Windows script, in the macOS voice): 30.9 s.
Numbers are not comparable with the Windows table, because the voice and the machine differ.

**Thread sweep, CPU.** 8 threads spill onto the efficiency cores and are much slower, so the 4-thread default holds.

| Threads | RTF, 1 stream | RTF, 2 streams | p95 per-block decode, 2 streams |
|---|---|---|---|
| 2 | 0.212 | 0.360 | 57.5 ms |
| 4 | 0.187 | 0.368 | 57.7 ms |
| 8 | 0.591 | 1.367 | 199.5 ms |

**Real time, Me and Them together, 4 threads, 60 s.** `PARAKEET_DEVICE=cpu` forced the CPU in a Metal build. The first
run is the original measurement, from before Metal was enabled. The second is this build (embedded shaders, backend freed
at exit), under `/usr/bin/time -l`, while another build may have been compiling on the machine. Latency is the worse of Me
and Them.

| Backend | CPU (cores) | EOU latency p50 / p95 | Peak resident memory |
|---|---|---|---|
| CPU, first run | 1.18 | ~172 / ~205 ms | 210 MB |
| Metal, first run | 0.13 | ~183 / ~193 ms | 249 MB |
| CPU, second run | 1.65 | ~189 / ~231 ms | 223 MB |
| Metal, second run | 0.10 | ~169 / ~188 ms | 409 MB |

The second run's memory is the process's peak as `time` reports it; sampled while streaming, it was 251 MB on Metal and
212 MB on the CPU. Both second runs exited with 0; the Metal one logged `ggml_metal_free: deallocating` and left no crash
report.

**Thread sweep, second run.** `parakeet_bench <wav> sweep --threads 2,4`; RTF per stream.

| Backend, threads | RTF, 1 stream | RTF, 2 streams | p95 per-block decode, 2 streams |
|---|---|---|---|
| Metal, 2 | 0.144 | 0.241 | 38.0 ms |
| Metal, 4 | 0.124 | 0.254 | 40.4 ms |
| CPU, 2 | 0.243 | 0.435 | 68.5 ms |
| CPU, 4 | 0.289 | 0.516 | 83.3 ms |

On Metal the thread count matters little. The very first Metal run after the
build took about 13 s to start decoding while macOS compiled the shaders. Later runs, including one from a copy of the
binary at another path, started at once.
