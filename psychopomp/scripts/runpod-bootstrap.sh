#!/usr/bin/env bash
# One-shot bootstrap for a fresh CUDA pod (RunPod / Vast / Lambda).
# Tested target: nvidia/cuda:12.4.1-devel-ubuntu22.04 base image with an
# NVIDIA GPU (RTX 4090 / A100 / H100). Run as root.
#
# After this script completes:
#   /opt/psychopomp/target/release/psychopomp-prover --bind 0.0.0.0:8088
# starts the operator daemon with GPU-accelerated proving enabled.

set -euo pipefail

REPO_TARBALL=${REPO_TARBALL:-psychopomp.tar.gz}
TARGET_DIR=${TARGET_DIR:-/opt/psychopomp}
LISTEN=${LISTEN:-0.0.0.0:8088}

echo "[bootstrap] target=$TARGET_DIR  listen=$LISTEN"

# 1. OS deps -----------------------------------------------------------------
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y --no-install-recommends \
    build-essential curl ca-certificates pkg-config \
    libssl-dev clang cmake git

# 2. Rust toolchain ----------------------------------------------------------
if ! command -v cargo >/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
fi
. "$HOME/.cargo/env"
rustup default stable

# 3. Place the source --------------------------------------------------------
mkdir -p "$TARGET_DIR"
if [[ -f "$REPO_TARBALL" ]]; then
    echo "[bootstrap] extracting $REPO_TARBALL"
    tar -xzf "$REPO_TARBALL" -C "$TARGET_DIR" --strip-components=1
elif [[ ! -f "$TARGET_DIR/Cargo.toml" ]]; then
    echo "[bootstrap] no source found; pass REPO_TARBALL=path or pre-mount $TARGET_DIR" >&2
    exit 1
fi

cd "$TARGET_DIR"

# 4. Recursion-zkr sidecar ---------------------------------------------------
# risc0-circuit-recursion's build.rs downloads a precomputed zkr from S3 and
# verifies its SHA. On some networks (transparent proxies / regional CDN
# corruption) the download is mangled and verification panics. If the user
# pre-stages the verified blob and exports RECURSION_SRC_PATH, the build
# script picks it up instead. We ship a fallback under scripts/.
if [[ -z "${RECURSION_SRC_PATH:-}" && -f scripts/recursion_zkr.zip ]]; then
    export RECURSION_SRC_PATH="$(pwd)/scripts/recursion_zkr.zip"
    echo "[bootstrap] RECURSION_SRC_PATH=$RECURSION_SRC_PATH"
fi

# 5. CUDA visibility ---------------------------------------------------------
# The risc0 cuda feature builds against nvcc on PATH and links libcudart from
# /usr/local/cuda/lib64. The cuda:12.4-devel image already has both.
if ! command -v nvcc >/dev/null; then
    echo "[bootstrap] nvcc not found — install CUDA toolkit before running, or use the cuda-devel image" >&2
    exit 1
fi
nvidia-smi || { echo "[bootstrap] nvidia-smi failed — no GPU visible to container" >&2; exit 1; }

# 6. Build --------------------------------------------------------------------
echo "[bootstrap] cargo build --release --features gpu -p psychopomp-prover"
cargo build --release --features gpu -p psychopomp-prover

# 7. systemd-style runit unit (or just print the command) --------------------
cat <<EOF

[bootstrap] ready.

Start the prover:

  cd $TARGET_DIR && \\
  RECURSION_SRC_PATH=${RECURSION_SRC_PATH:-/opt/psychopomp/scripts/recursion_zkr.zip} \\
  RISC0_DEV_MODE=0 \\
  RUST_LOG=info,psychopomp_prover=info,risc0_zkvm=info \\
  ./target/release/psychopomp-prover --bind $LISTEN

Expose the chosen port via RunPod's TCP proxy. The endpoint URL goes into the
client (see scripts/run-remote-e2e.sh on your laptop).

EOF
