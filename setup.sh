#!/usr/bin/env bash
# Per-user setup for LDEX. Run this once after `git clone`.
#
# Creates a `_lez` symlink in the repo root pointing at your LEZ source
# checkout. Every Cargo.toml patches the upstream nssa/wallet/common
# deps via this symlink, so the build works on any machine without
# modifying tracked files.
#
# Override the LEZ location:
#   LDEX_LEZ_DIR=/path/to/lez bash setup.sh
#
# The convention is to clone LEZ at ~/ldex-spike/lez — see SETUP.md.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
LEZ_DIR="${LDEX_LEZ_DIR:-$HOME/ldex-spike/lez}"

if [ ! -d "$LEZ_DIR" ]; then
    echo "✗ LEZ source tree not found at $LEZ_DIR" >&2
    echo "  Clone it first (see SETUP.md):" >&2
    echo "    mkdir -p ~/ldex-spike && cd ~/ldex-spike" >&2
    echo "    git clone --branch v0.2.0-rc3 https://github.com/logos-co/lez.git" >&2
    echo "    cd lez && cargo build --release" >&2
    echo "  Or set LDEX_LEZ_DIR=/path/to/your/lez and re-run." >&2
    exit 2
fi

LEZ_DIR="$(cd "$LEZ_DIR" && pwd)"   # canonicalise
LINK="$REPO/_lez"

if [ -L "$LINK" ]; then
    current="$(readlink -f "$LINK")"
    if [ "$current" = "$LEZ_DIR" ]; then
        echo "✓ _lez already points at $LEZ_DIR"
    else
        echo "→ updating _lez: $current → $LEZ_DIR"
        rm "$LINK"
        ln -s "$LEZ_DIR" "$LINK"
    fi
elif [ -e "$LINK" ]; then
    echo "✗ $LINK exists and is not a symlink — refusing to overwrite" >&2
    exit 2
else
    ln -s "$LEZ_DIR" "$LINK"
    echo "✓ created _lez → $LEZ_DIR"
fi

# Add _lez to .gitignore if it isn't already.
GITIGNORE="$REPO/.gitignore"
touch "$GITIGNORE"
if ! grep -qxF "_lez" "$GITIGNORE"; then
    echo "_lez" >> "$GITIGNORE"
    echo "✓ added _lez to .gitignore"
fi

# Sanity-check key sub-paths exist.
for sub in nssa nssa/core wallet common; do
    if [ ! -d "$LINK/$sub" ]; then
        echo "✗ Expected $LINK/$sub does not exist." >&2
        echo "  Your LEZ checkout layout differs from v0.2.0-rc3. Make sure you" >&2
        echo "  cloned the right branch (see SETUP.md)." >&2
        exit 2
    fi
done

# Drop risc0-zkvm's `bonsai` default feature in the LEZ workspace. The remote
# Bonsai prover client (bonsai-sdk -> reqwest -> rustls -> ring) is never used,
# and ring's C build can't cross-compile to the riscv32 zkVM guest target
# (it emits a host-only `-m64`), so every guest fails to build with it on.
# Idempotent: only rewrites the pristine rc3 line.
LEZ_CARGO="$LINK/Cargo.toml"
PRISTINE="risc0-zkvm = { version = \"3.0.5\", features = ['std'] }"
FIXED="risc0-zkvm = { version = \"3.0.5\", default-features = false, features = ['std', 'client'] }"
if grep -qF "$PRISTINE" "$LEZ_CARGO" 2>/dev/null; then
    sed -i "s|$PRISTINE|$FIXED|" "$LEZ_CARGO"
    echo "✓ patched _lez risc0-zkvm: dropped 'bonsai' default feature (no ring in guests)"
elif grep -qF "$FIXED" "$LEZ_CARGO" 2>/dev/null; then
    echo "✓ _lez risc0-zkvm already bonsai-free"
else
    echo "⚠ could not find the expected risc0-zkvm line in $LEZ_CARGO —" >&2
    echo "  if guest builds fail with a ring '-m64' error, drop the 'bonsai'" >&2
    echo "  default feature from risc0-zkvm there manually." >&2
fi

echo
echo "Setup complete. Next:"
echo "  1. cd $REPO"
echo "  2. cargo build --release                    # programs + FFI"
echo "  3. bash run-sequencer.sh                    # start dev sequencer"
echo "  4. bash scripts/bootstrap.sh                # deploy + fund (in another terminal)"
echo "  5. bash cli/install.sh                      # install ldex on PATH"
echo "  6. ldex status                              # smoke test"
