//! Builds parakeet.cpp (pinned submodule) as static libraries and links them, plus the small
//! thread-count shim in `native/`. See PATCHES.md for what is changed relative to upstream.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=native/parakeet_threads.cpp");
    println!("cargo:rerun-if-env-changed=CLUELYRS_NINJA");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest.join("third_party/parakeet.cpp");
    if !source.join("CMakeLists.txt").exists() || !source.join("third_party/ggml/CMakeLists.txt").exists() {
        panic!(
            "parakeet.cpp sources are missing. Run:\n  git submodule update --init third_party/parakeet.cpp\n  \
             git -C third_party/parakeet.cpp submodule update --init third_party/ggml"
        );
    }

    // ScreenCaptureKit (macOS desktop audio) is weak-linked so CluelyRS still launches on macOS
    // before 12.3, which doesn't have it; desktop audio checks the OS version before touching it.
    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos") {
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
    }

    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    let apple = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos");

    let mut config = cmake::Config::new(&source);
    // Always optimize: a debug ggml is far too slow for real-time inference, even in dev builds.
    config.profile("Release")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("PARAKEET_SHARED", "OFF")
        .define("PARAKEET_BUILD_CLI", "OFF")
        .define("PARAKEET_BUILD_SERVER", "OFF")
        .define("PARAKEET_WITH_CED", "OFF")
        .define("PARAKEET_WITH_VOICEDETECT", "OFF")
        // Portable rather than tuned to the build machine: x86-64 with AVX2/FMA/F16C on Windows,
        // the macOS arm64 baseline (every Apple silicon Mac) on macOS.
        .define("GGML_NATIVE", "OFF")
        // MSVC's OpenMP runtime spins idle threads between decodes: ~5x the CPU of ggml's own
        // thread pool for two live streams, with no latency benefit (PATCHES.md).
        .define("GGML_OPENMP", "OFF");
    if msvc {
        // Setting flags replaces CMake's MSVC defaults, so restore them (exceptions and RTTI matter:
        // parakeet.cpp reports load errors with C++ exceptions).
        config.cflag("/DWIN32 /D_WINDOWS")
            .cxxflag("/DWIN32 /D_WINDOWS /GR /EHsc")
            // Upstream v0.6.0 uses POSIX fseeko/ftello, which MSVC spells _fseeki64/_ftelli64.
            .cxxflag("/Dfseeko=_fseeki64")
            .cxxflag("/Dftello=_ftelli64");
    }
    if apple {
        // Streaming decodes run on the CPU backend, never ggml's BLAS backend, so don't build it.
        // Accelerate stays on (upstream's default): ggml-cpu uses it for vector math. Metal is off
        // for now (PATCHES.md), set explicitly so a value cached by an earlier configure can't win.
        config.define("GGML_BLAS", "OFF").define("PARAKEET_GGML_METAL", "OFF");
    }
    config.build_target("parakeet");
    // Upstream applies its in-tree ggml patches (one is a CPU matmul speedup) with bash at
    // configure time. On Windows, point it at Git's bash: a `bash` on PATH may be WSL's, which
    // can't run against Windows paths. Elsewhere CMake finds the system bash itself. Without bash
    // the build still works, just without that speedup.
    if cfg!(windows) {
        match git_bash() {
            Some(bash) => { config.define("BASH_EXECUTABLE", bash); }
            None => println!("cargo:warning=Git bash not found; building parakeet.cpp without its ggml CPU patches (slower)"),
        }
    }
    if let Some(ninja) = find_ninja() {
        // Ninja avoids MSBuild's MAX_PATH failures on deep target directories.
        config.generator("Ninja").define("CMAKE_MAKE_PROGRAM", ninja);
    }
    let out = config.build();

    let file_name = |lib: &str| if msvc { format!("{lib}.lib") } else { format!("lib{lib}.a") };
    let mut dirs: Vec<PathBuf> = ["parakeet", "ggml", "ggml-base", "ggml-cpu"].iter()
        .map(|lib| find_file(&out.join("build"), &file_name(lib)).unwrap_or_else(|| panic!("{} was not produced by the parakeet.cpp build", file_name(lib))))
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs { println!("cargo:rustc-link-search=native={}", dir.display()); }
    for lib in ["parakeet", "ggml", "ggml-cpu", "ggml-base"] { println!("cargo:rustc-link-lib=static={lib}"); }
    if msvc { println!("cargo:rustc-link-lib=advapi32"); }
    if apple { println!("cargo:rustc-link-lib=framework=Accelerate"); }

    cc::Build::new()
        .cpp(true)
        .file("native/parakeet_threads.cpp")
        .flag_if_supported("/std:c++17")
        .compile("cluelyrs_parakeet_shim");
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) { return Some(found); }
        } else if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return Some(path);
        }
    }
    None
}

fn git_bash() -> Option<PathBuf> {
    let git = Command::new("where").arg("git").output().ok()?;
    let git = String::from_utf8_lossy(&git.stdout).lines().next()?.trim().to_string();
    // git.exe lives in Git\cmd, Git\bin or Git\mingw64\bin; bash is Git\bin\bash.exe.
    Path::new(&git).ancestors().map(|dir| dir.join("bin").join("bash.exe")).find(|bash| bash.exists())
}

fn find_ninja() -> Option<PathBuf> {
    if let Some(path) = env::var_os("CLUELYRS_NINJA") { return Some(PathBuf::from(path)); }
    if Command::new("ninja").arg("--version").output().is_ok_and(|o| o.status.success()) { return Some(PathBuf::from("ninja")); }
    let vswhere = Path::new(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
    let install = Command::new(vswhere).args(["-latest", "-products", "*", "-property", "installationPath"]).output().ok()?;
    let install = String::from_utf8_lossy(&install.stdout).trim().to_string();
    let ninja = Path::new(&install).join(r"Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\ninja.exe");
    ninja.exists().then_some(ninja)
}
