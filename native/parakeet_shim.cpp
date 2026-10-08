// CluelyRS shim over parakeet.cpp (see PATCHES.md).
//
// Exports two of the library's internal C++ functions, which its C API doesn't expose, under
// CluelyRS-prefixed C names. Upstream sources are not modified.
//
// - pk::set_num_threads: v0.6.0 runs inference with a hard-coded default of 8 threads; upstream's
//   own CLI sets it for --threads.
// - pk::shutdown_backend: frees the process-wide compute backend. With Metal it must be freed
//   after every model and before the process exits; ggml aborts during exit otherwise. Upstream's
//   CLI and server call it before returning from main.
//
// Remove each export once parakeet.cpp's C API offers an equivalent.

namespace pk {
void set_num_threads(int n);
void shutdown_backend();
}

extern "C" void cluelyrs_parakeet_set_threads(int n_threads) {
    pk::set_num_threads(n_threads);
}

extern "C" void cluelyrs_parakeet_shutdown_backend() {
    pk::shutdown_backend();
}
