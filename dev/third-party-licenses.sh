#!/usr/bin/env bash
# Generates THIRD_PARTY_LICENSES.txt for the macOS app (the counterpart of third-party-licenses.ps1):
# cargo-about's notices for the Rust crates (about.toml, about.hbs), then the hand-maintained ones in
# licenses/ (parakeet.cpp, ggml, assets). Needs cargo-about on PATH:
#   cargo install cargo-about --locked --features cli
# Usage: dev/third-party-licenses.sh [out]   (default target/THIRD_PARTY_LICENSES.txt)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
out=${1:-target/THIRD_PARTY_LICENSES.txt}

command -v cargo-about >/dev/null || { echo "error: cargo-about not found on PATH. Install with: cargo install cargo-about --locked --features cli" >&2; exit 1; }

mkdir -p "$(dirname "$out")"
generated=$(mktemp "${TMPDIR:-/tmp}/cluelyrs-about.XXXXXX")
trap 'rm -f "$generated"' EXIT
cargo about generate --fail about.hbs -o "$generated"

separator=$(printf '=%.0s' {1..80})
{
    cat "$generated"
    for notice in "parakeet.cpp.txt|parakeet.cpp (MIT License)" "ggml.txt|ggml (MIT License)" "assets.txt|Icons and logos (Lucide, Lobe Icons, Simple Icons)"; do
        file=licenses/${notice%%|*}
        [[ -f "$file" ]] || { echo "error: missing license notice: $file" >&2; exit 1; }
        printf '\n\n%s\n%s\n%s\n' "$separator" "${notice#*|}" "$separator"
        cat "$file"
    done
} > "$out"
echo "Wrote $out"
