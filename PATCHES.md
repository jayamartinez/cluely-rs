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

- **What:** `native/parakeet_threads.cpp` exports `cluelyrs_parakeet_set_threads(int)`. It is a one-line C wrapper over
  parakeet.cpp's existing internal `pk::set_num_threads`, which upstream's own CLI uses for `--threads`. No upstream file
  is modified.
- **Why:** v0.6.0 decodes streams with a hard-coded default of 8 threads (`kDefaultThreads` in `src/ggml_graph.cpp`). Its
  C API has no way to change that for streaming. `parakeet_capi_set_concurrency` exists, but it is documented not to
  affect streaming sessions. Mic and desktop audio each run a stream all the time, so the thread count directly sets
  how much of the machine transcription takes.
- **CluelyRS default:** 4 threads, shared by all streams (`stt::parakeet::DEFAULT_THREADS`). See the measurements below.
- **To remove:** when upstream exposes thread configuration in its C API, call that from `stt::parakeet::ffi::set_threads`.
  Then delete `native/parakeet_threads.cpp` and the `cc::Build` block in `build.rs`. If the shim stops linking after a
  version bump (for example because `pk::set_num_threads` was renamed), check upstream for an equivalent before
  restoring it.

### 2. Build configuration (`build.rs`)

These are CMake options and compiler flags, not source changes. To undo one, delete its line in `build.rs`.

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
