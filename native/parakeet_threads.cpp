// CluelyRS shim over parakeet.cpp (see PATCHES.md).
//
// parakeet.cpp v0.6.0 runs inference with a hard-coded default of 8 threads and its C API has
// no way to change it. The library already has an internal setter, pk::set_num_threads, used by
// its own CLI; this exports it under a CluelyRS-prefixed C name. Upstream sources are not
// modified. Remove this file once parakeet.cpp exposes an equivalent C function.

namespace pk {
void set_num_threads(int n);
}

extern "C" void cluelyrs_parakeet_set_threads(int n_threads) {
    pk::set_num_threads(n_threads);
}
