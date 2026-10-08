// CluelyRS shim over parakeet.cpp (see PATCHES.md).
//
// Exports internal C++ functions of the library, which its C API doesn't expose, under
// CluelyRS-prefixed C names. Upstream sources are not modified; its headers are included.
//
// - pk::set_num_threads: v0.6.0 runs inference with a hard-coded default of 8 threads; upstream's
//   own CLI sets it for --threads.
// - pk::shutdown_backend: frees the process-wide compute backend. With Metal it must be freed
//   after every model and before the process exits; ggml aborts during exit otherwise. Upstream's
//   CLI and server call it before returning from main.
// - The process backend's device name ("cpu", or a GPU such as "MTL0"), to tell when a GPU that
//   was asked for couldn't be used.
//
// Remove each export once parakeet.cpp's C API offers an equivalent.

#include "backend.hpp"
#include "ggml_graph.hpp"

extern "C" void cluelyrs_parakeet_set_threads(int n_threads) {
    pk::set_num_threads(n_threads);
}

extern "C" void cluelyrs_parakeet_shutdown_backend() {
    pk::shutdown_backend();
}

// Creates the backend if there is none, so call it after a model load. The string lives in the
// backend: copy it before the backend can be freed.
extern "C" const char* cluelyrs_parakeet_device_name() {
    return pk::global_backend().device_name();
}
