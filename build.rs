//! Builds parakeet.cpp (pinned submodule) as static libraries and links them, plus the small
//! shim in `native/`. See PATCHES.md for what is changed relative to upstream.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=native/parakeet_shim.cpp");
    println!("cargo:rerun-if-env-changed=CLUELYRS_NINJA");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    google_client(&manifest);
    let source = manifest.join("third_party/parakeet.cpp");
    if !source.join("CMakeLists.txt").exists() || !source.join("third_party/ggml/CMakeLists.txt").exists() {
        panic!(
            "parakeet.cpp sources are missing. Run:\n  git submodule update --init third_party/parakeet.cpp\n  \
             git -C third_party/parakeet.cpp submodule update --init third_party/ggml"
        );
    }

    #[cfg(windows)]
    windows_resources();

    // ScreenCaptureKit (macOS desktop audio) is weak-linked so CluelyRS still launches on macOS
    // before 12.3, which doesn't have it; desktop audio checks the OS version before touching it.
    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos") {
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
    }

    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    let apple = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos");
    // .cargo/config.toml links the C runtime statically on MSVC (no Visual C++ Redistributable
    // needed on the user's PC); parakeet must use the same runtime or the link fails (LNK2038).
    let static_crt = msvc && env::var("CARGO_CFG_TARGET_FEATURE").is_ok_and(|f| f.split(',').any(|f| f == "crt-static"));

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
        // CMake picks the runtime from CMAKE_MSVC_RUNTIME_LIBRARY (policy CMP0091), which would
        // otherwise add /MD after the cmake crate's own /MT.
        config.static_crt(static_crt)
            .define("CMAKE_POLICY_DEFAULT_CMP0091", "NEW")
            .define("CMAKE_MSVC_RUNTIME_LIBRARY", if static_crt { "MultiThreaded" } else { "MultiThreadedDLL" })
            .cflag("/DWIN32 /D_WINDOWS")
            .cxxflag("/DWIN32 /D_WINDOWS /GR /EHsc")
            // Upstream v0.6.0 uses POSIX fseeko/ftello, which MSVC spells _fseeki64/_ftelli64.
            .cxxflag("/Dfseeko=_fseeki64")
            .cxxflag("/Dftello=_ftelli64");
    }
    if apple {
        // Streams decode on Metal (about a ninth of the CPU's load at similar latency; PATCHES.md),
        // with the CPU as fallback (PARAKEET_DEVICE=cpu), and never on ggml's BLAS backend, so it
        // isn't built. Accelerate stays on (upstream's default): ggml-cpu uses it for vector math.
        // The Metal shaders are embedded in the binary, so nothing has to ship beside it. All set
        // explicitly so values cached by an earlier configure can't win.
        config.define("GGML_BLAS", "OFF")
            .define("PARAKEET_GGML_METAL", "ON")
            .define("GGML_METAL_EMBED_LIBRARY", "ON");
        // Build for the oldest supported macOS (.cargo/config.toml), not the build machine's SDK,
        // or the objects can need a newer macOS than the binary claims to run on.
        println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
        let deployment_target = env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "13.0".into());
        config.define("CMAKE_OSX_DEPLOYMENT_TARGET", deployment_target);
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
    // Link order: each library before the ones it depends on.
    let libs: &[&str] = if apple { &["parakeet", "ggml", "ggml-metal", "ggml-cpu", "ggml-base"] } else { &["parakeet", "ggml", "ggml-cpu", "ggml-base"] };
    let mut dirs: Vec<PathBuf> = libs.iter()
        .map(|lib| find_file(&out.join("build"), &file_name(lib)).unwrap_or_else(|| panic!("{} was not produced by the parakeet.cpp build", file_name(lib))))
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs { println!("cargo:rustc-link-search=native={}", dir.display()); }
    for lib in libs { println!("cargo:rustc-link-lib=static={lib}"); }
    if msvc { println!("cargo:rustc-link-lib=advapi32"); }
    if apple {
        for framework in ["Accelerate", "Foundation", "Metal", "MetalKit"] { println!("cargo:rustc-link-lib=framework={framework}"); }
        // ggml-metal's `@available` checks call compiler-rt's __isPlatformVersionAtLeast, which clang
        // links on its own but rustc doesn't. std's weak copy of it is gone under the release
        // profile's LTO (nothing Rust uses it), so link clang's runtime for it.
        match clang_runtime() {
            Some(runtime) => println!("cargo:rustc-link-arg={}", runtime.display()),
            None => println!("cargo:warning=libclang_rt.osx.a not found; release builds may fail to link"),
        }
    }

    cc::Build::new()
        .cpp(true)
        .file("native/parakeet_shim.cpp")
        .include(source.join("src"))
        .flag_if_supported("/std:c++17")
        .flag_if_supported("-std=c++17")
        .compile("cluelyrs_parakeet_shim");
}

/// Windows: the app icon as icon resource 1, which GPUI gives its windows (the taskbar and Alt+Tab
/// show it) and Explorer shows for the exe, plus the version information Explorer, Task Manager
/// and the installer show. FileVersion and ProductVersion default to the package version. Without
/// a resource compiler the build goes on with the generic icon and no version information.
/// The Google OAuth client built into CluelyRS (`calendar::oauth::built_in` reads it with
/// `option_env!`). Release builds get it from the environment (the workflow passes repository
/// secrets); a local build can put it in a git-ignored `.env` in the repository root instead, as
/// `CLUELYRS_GOOGLE_CLIENT_ID=…` and `CLUELYRS_GOOGLE_CLIENT_SECRET=…` lines. A variable set in the
/// environment wins over `.env`, and nothing else in `.env` is read. The values are never printed.
fn google_client(manifest: &Path) {
    const KEYS: [&str; 2] = ["CLUELYRS_GOOGLE_CLIENT_ID", "CLUELYRS_GOOGLE_CLIENT_SECRET"];
    for key in KEYS { println!("cargo:rerun-if-env-changed={key}"); }
    // Cargo counts a watched file that doesn't exist as changed on every build, which would rebuild
    // CluelyRS each time, so `.env` is watched only once it's there: touch build.rs after first creating it.
    let Ok(text) = std::fs::read_to_string(manifest.join(".env")) else { return };
    println!("cargo:rerun-if-changed=.env");
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.strip_prefix("export ").unwrap_or(line).split_once('=') else { continue };
        let key = key.trim();
        if !KEYS.contains(&key) || env::var_os(key).is_some_and(|set| !set.is_empty()) { continue; }
        let value = value.trim();
        let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))).unwrap_or(value);
        // Handed to rustc only (Cargo keeps it out of the build's console output).
        if !value.is_empty() { println!("cargo:rustc-env={key}={value}"); }
    }
}

#[cfg(windows)]
fn windows_resources() {
    println!("cargo:rerun-if-changed=assets/app-icon/app-icon.ico");
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon_with_id("assets/app-icon/app-icon.ico", "1")
        .set("ProductName", "CluelyRS")
        .set("FileDescription", "CluelyRS")
        .set("CompanyName", "Jay Martinez")
        .set("LegalCopyright", "Copyright (c) 2026 Jay Martinez")
        .set("OriginalFilename", "CluelyRS.exe")
        .set("InternalName", "CluelyRS");
    if let Err(error) = resource.compile() { println!("cargo:warning=Windows resources not embedded: {error}"); }
}

/// macOS: the Xcode toolchain's compiler runtime (`libclang_rt.osx.a`).
fn clang_runtime() -> Option<PathBuf> {
    let output = Command::new("xcrun").args(["clang", "-print-file-name=libclang_rt.osx.a"]).output().ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    path.is_absolute().then_some(path).filter(|path| path.exists())
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
