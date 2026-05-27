#!/usr/bin/env bash
# Build the registry + escrow LEZ guest binaries via cargo risczero build
# (needs docker buildx; verified present on this box).
#
# Produces (inside each guest target dir):
#   target/riscv32im-risc0-zkvm-elf/docker/<bin>.bin   <- deployable ELF

set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"

export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"
. "$HOME/.cargo/env" 2>/dev/null || true

for g in psychopomp-registry psychopomp-escrow; do
    dir="$HERE/Phase1-onchain/$g/methods/guest"
    echo ">> Building guest: $g"
    cd "$dir"
    cargo risczero build --manifest-path Cargo.toml
done

echo
echo ">> Built ELFs:"
find "$HERE/Phase1-onchain" -path "*docker/*.bin" -type f -newer "$HERE/Cargo.toml" 2>/dev/null || true
