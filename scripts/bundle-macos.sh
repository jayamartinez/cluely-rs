#!/usr/bin/env bash
# Builds dist/CluelyRS.app and the drag-to-Applications dist/CluelyRS-<version>.dmg (plus its
# .sha256) for Apple silicon. Uses Xcode command line tools, plus dmgbuild (pinned, installed into a
# venv under target/ on first use; needs python3) to lay out the DMG window without Finder.
#
# The app is signed with a stable identity (by default the free "Apple Development" certificate in
# the login keychain), not notarized and without the hardened runtime. A stable identity keeps the
# Keychain items and the Microphone/Screen Recording grants valid across updates; first opens get
# Gatekeeper's "Open Anyway".
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/bundle-macos.sh [--skip-build] [--help]

Builds dist/CluelyRS.app and dist/CluelyRS-<version>.dmg (+ .sha256) for Apple silicon.

  --skip-build   Package the existing target/release/cluely-rs instead of running
                 `cargo build --release --locked` first.
  -h, --help     Show this help.

Environment:
  CLUELYRS_SIGN_IDENTITY   codesign identity (name or SHA-1). Defaults to the first
                           "Apple Development" identity in the keychain, else ad-hoc.
  CLUELYRS_GOOGLE_CLIENT_ID, CLUELYRS_GOOGLE_CLIENT_SECRET
                           The Google OAuth client for Calendar sign-in, built into the
                           app (build.rs; it also reads them from a git-ignored .env in
                           the repository root). cargo inherits them from this script's
                           environment; the script never prints them.
EOF
}

skip_build=0
for arg in "$@"; do
    case "$arg" in
        --skip-build) skip_build=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown option: $arg" >&2; usage >&2; exit 2 ;;
    esac
done

die() { echo "error: $*" >&2; exit 1; }
warn() { printf '\n*** WARNING: %s\n\n' "$*" >&2; }

# Work from the repository root whatever directory the script was started in.
cd "$(dirname "${BASH_SOURCE[0]}")/.."
root=$(pwd)

[[ "$(uname -s)" == Darwin ]] || die "macOS only"
[[ "$(uname -m)" == arm64 ]] || die "Apple silicon (arm64) only"

app_name=CluelyRS
bundle_id=io.github.jayamartinez.cluelyrs
exe_name=cluely-rs # CFBundleExecutable in packaging/macos/Info.plist
min_macos=13.0
plist_template=packaging/macos/Info.plist
icon=assets/app-icon/app-icon.icns
dmg_settings=packaging/macos/dmg_settings.py
dmg_background=packaging/macos/dmg-background.tiff # rendered from dmg-background.svg
dmgbuild_version=1.6.7
dmgbuild_venv=target/dmgbuild-venv

# The [package] version, without needing cargo metadata or jq.
version=$(awk '
    /^\[/ { in_package = ($0 == "[package]") }
    in_package && /^version[[:space:]]*=/ { gsub(/^version[[:space:]]*=[[:space:]]*"|".*$/, ""); print; exit }
' Cargo.toml)
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || die "couldn't read the [package] version from Cargo.toml (got '$version')"

[[ -f "$plist_template" ]] || die "missing $plist_template"
[[ -f "$icon" ]] || die "missing $icon"
# Checked up front, before the long build: the license notices need it.
command -v cargo-about >/dev/null || die "cargo-about is needed for the license notices: cargo install cargo-about --locked --features cli"
[[ -f "$dmg_settings" ]] || die "missing $dmg_settings"
[[ -f "$dmg_background" ]] || die "missing $dmg_background"

# dmgbuild writes the DMG window's .DS_Store (background, icon positions) directly, which plain
# hdiutil can't, and avoids scripting Finder. Set it up before building so a failure is quick.
dmgbuild_installed() {
    "$dmgbuild_venv/bin/python" -c 'import importlib.metadata as m, sys; sys.exit(m.version("dmgbuild") != sys.argv[1])' \
        "$dmgbuild_version" 2>/dev/null
}
if ! dmgbuild_installed; then
    echo "==> Installing dmgbuild $dmgbuild_version into $dmgbuild_venv"
    command -v python3 >/dev/null || die "python3 is required to build the DMG (dmgbuild)"
    rm -rf "$dmgbuild_venv"
    python3 -m venv "$dmgbuild_venv" || die "couldn't create a Python venv at $dmgbuild_venv"
    # ds_store and mac_alias are dmgbuild's only dependencies; pinned too so the layout is reproducible.
    "$dmgbuild_venv/bin/python" -m pip install --quiet --disable-pip-version-check \
        "dmgbuild==$dmgbuild_version" "ds_store==1.3.3" "mac_alias==2.2.3" \
        || die "couldn't install dmgbuild $dmgbuild_version into $dmgbuild_venv"
    dmgbuild_installed || die "dmgbuild $dmgbuild_version is not importable from $dmgbuild_venv"
fi

if (( skip_build )); then
    echo "==> Skipping build (--skip-build)"
else
    echo "==> cargo build --release --locked"
    cargo build --release --locked
fi
binary=target/release/$exe_name
[[ -x "$binary" ]] || die "no executable at $binary (build first, or drop --skip-build)"

# Only arm64 is shipped; a universal or x86_64 binary means the wrong target was built.
archs=$(lipo -archs "$binary")
[[ "$archs" == arm64 ]] || die "$binary is '$archs', expected arm64 only"

# The deployment target baked into the binary must not exceed LSMinimumSystemVersion, or the app
# would refuse to start on the oldest macOS the plist (and README) promise.
minos=$(vtool -show-build "$binary" | awk '$1 == "minos" { print $2; exit }')
[[ -n "$minos" ]] || die "couldn't read minos from $binary"
version_gt() { # $1 > $2, comparing major.minor.patch numerically
    awk -v a="$1" -v b="$2" 'BEGIN {
        split(a, x, "."); split(b, y, ".")
        for (i = 1; i <= 3; i++) { if (x[i] + 0 > y[i] + 0) exit 0; if (x[i] + 0 < y[i] + 0) exit 1 }
        exit 1 }'
}
if version_gt "$minos" "$min_macos"; then
    die "$binary has minos $minos, above LSMinimumSystemVersion $min_macos: it wouldn't launch on macOS $min_macos. Build with MACOSX_DEPLOYMENT_TARGET=$min_macos (.cargo/config.toml)."
fi

echo "==> Assembling dist/$app_name.app ($version)"
dist=$root/dist
app=$dist/$app_name.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/$exe_name"
cp "$icon" "$app/Contents/Resources/app-icon.icns"
# The licenses go with the app, as the Windows installer ships them: CluelyRS's own and the notices
# of everything built into it.
cp LICENSE "$app/Contents/Resources/LICENSE.txt"
dev/third-party-licenses.sh target/THIRD_PARTY_LICENSES.txt >/dev/null
cp target/THIRD_PARTY_LICENSES.txt "$app/Contents/Resources/THIRD_PARTY_LICENSES.txt"
sed "s/@VERSION@/$version/g" "$plist_template" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
plutil -lint "$app/Contents/Info.plist" >/dev/null || die "Info.plist is invalid"
# codesign rejects files carrying Finder info or other extended attributes.
xattr -cr "$app"

# Signing identity: explicit, else the first Apple Development certificate, else ad-hoc.
if [[ -n "${CLUELYRS_SIGN_IDENTITY:-}" ]]; then
    sign_identity=$CLUELYRS_SIGN_IDENTITY
    sign_label=$CLUELYRS_SIGN_IDENTITY
    [[ "$sign_identity" == - ]] && sign_label="ad-hoc"
else
    # Lines look like:  1) <SHA-1> "Apple Development: name (TEAMID)"
    dev_line=$(security find-identity -v -p codesigning | grep -m1 '"Apple Development:' || true)
    if [[ -n "$dev_line" ]]; then
        sign_identity=$(awk '{ print $2 }' <<<"$dev_line") # the hash is unambiguous if names repeat
        sign_label=$(sed -E 's/^[^"]*"(.*)"$/\1/' <<<"$dev_line")
    else
        sign_identity=-
        sign_label="ad-hoc"
        warn "no Apple Development identity found; signing ad-hoc. Each build then looks like a new app to macOS: Keychain access and the Microphone/Screen Recording grants reset after every update."
    fi
fi

echo "==> Signing with: $sign_label"
# No hardened runtime and no entitlements: the app isn't notarized, and it uses no restricted APIs.
codesign --force --sign "$sign_identity" --identifier "$bundle_id" --timestamp=none "$app"
codesign --verify --strict --verbose=2 "$app"

echo "==> Building the DMG"
dmg=$dist/$app_name-$version.dmg
rm -f "$dmg" "$dmg.sha256"
# The app is copied with ditto (signature intact) next to an Applications symlink; the window
# layout (size, background, icon positions, hidden chrome) lives in $dmg_settings.
"$dmgbuild_venv/bin/python" -m dmgbuild -s "$dmg_settings" \
    -D app="$app" -D icon="$root/$icon" -D background="$root/$dmg_background" \
    "$app_name $version" "$dmg" || die "dmgbuild failed"
[[ -f "$dmg" ]] || die "dmgbuild produced no $dmg"
hdiutil verify "$dmg" >/dev/null
# File name only, so `shasum -c` works next to the downloaded DMG.
(cd "$dist" && shasum -a 256 "$(basename "$dmg")" > "$(basename "$dmg").sha256")

size() { du -sh "$1" | awk '{ print $1 }'; }
cat <<EOF

CluelyRS $version packaged
  App:      $app ($(size "$app"))
  DMG:      $dmg ($(size "$dmg"))
  SHA-256:  $(awk '{ print $1 }' "$dmg.sha256")  ($dmg.sha256)
  Signed:   $sign_label
  minos:    $minos (LSMinimumSystemVersion $min_macos)
EOF
if [[ "$sign_identity" == - ]]; then echo "  (ad-hoc signature: permissions and Keychain access reset with every build)"; fi
echo "Not notarized: on first open users need System Settings > Privacy & Security > Open Anyway."
