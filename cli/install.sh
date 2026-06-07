#!/usr/bin/env bash
# Install the ldex CLI on PATH.
#
# Builds the release binary and either:
#   • symlinks it into ~/.cargo/bin/ldex (if it's on PATH), or
#   • symlinks it into ~/.local/bin/ldex
#
# The binary is self-contained: build.rs bakes an absolute RUNPATH
# pointing at `<repo>/mini-app/core/lib`, so the dynamic linker resolves
# libldex_amm_ffi.so + libwallet_ffi.so automatically - no
# LD_LIBRARY_PATH required at runtime. Move the repo and you must
# re-run this script so the rpath is updated.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
CLI="$REPO/cli"
LIB="$REPO/mini-app/core/lib"

if [ ! -d "$LIB" ] || [ ! -f "$LIB/libldex_amm_ffi.so" ] || [ ! -f "$LIB/libwallet_ffi.so" ]; then
    echo "✗ FFI libraries missing at $LIB" >&2
    echo "  Build the mini-app first (it vendors the FFI .so files there):" >&2
    echo "    cd $REPO/mini-app/core && nix build ." >&2
    exit 2
fi

if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        echo "✗ cargo not found. Install rustup from https://rustup.rs" >&2
        exit 2
    fi
fi

echo "→ building release binary..."
(cd "$CLI" && cargo build --release --quiet)
BIN="$CLI/target/release/ldex"
[ -x "$BIN" ] || { echo "✗ build did not produce $BIN" >&2; exit 2; }

# Pick the install target. Prefer ~/.cargo/bin (cargo install style) if
# present on PATH; fall back to ~/.local/bin (XDG default). If neither
# is on PATH, default to ~/.local/bin and warn.
target_dir=""
case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) target_dir="$HOME/.cargo/bin" ;;
    *":$HOME/.local/bin:"*) target_dir="$HOME/.local/bin" ;;
esac
if [ -z "$target_dir" ]; then
    target_dir="$HOME/.local/bin"
    on_path=0
else
    on_path=1
fi
mkdir -p "$target_dir"
ln -sf "$BIN" "$target_dir/ldex"

echo "✓ installed: $target_dir/ldex → $BIN"
if [ "$on_path" -eq 0 ]; then
    echo
    echo "⚠ $target_dir is not on your PATH yet. Add this to ~/.bashrc or ~/.zshrc:"
    echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

echo
echo "→ smoke test:"
"$target_dir/ldex" --version
echo
echo "→ try:  ldex --help"
