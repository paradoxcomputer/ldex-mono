#!/usr/bin/env bash
# Bootstrap a fresh checkout for local Phase-0 development. Idempotent:
# re-running on a working setup is a no-op (a build + test).
#
# Installs/checks:
#   - Rust toolchain via rustup, if cargo is missing
#   - RISC Zero toolchain via rzup, if cargo risczero is missing
#   - exports RECURSION_SRC_PATH to the verified zkr shipped in scripts/
#
# Then builds the Phase-0 stack and runs the unit-test subset that doesn't
# require docker buildx (i.e. skips the LEZ guest ELFs — see CONTRIBUTING.md).
#
# Usage:  ./scripts/bootstrap-dev.sh
# Env:    SKIP_INSTALL=1     skip toolchain installs (assume present)
#         SKIP_E2E=1         skip the dev-mode e2e smoke test at the end

set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[!] %s\033[0m\n' "$*" >&2; }

# 1. Rust toolchain --------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
    if [[ "${SKIP_INSTALL:-0}" == "1" ]]; then
        warn "cargo not found; SKIP_INSTALL=1 set, aborting"
        exit 1
    fi
    step "Installing Rust via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --default-toolchain stable
    . "$HOME/.cargo/env"
fi
[[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
export PATH="$HOME/.cargo/bin:$PATH"
step "Rust toolchain: $(cargo --version)"

# 2. RISC Zero toolchain ---------------------------------------------------
# We need `cargo risczero` and the riscv32im-risc0-zkvm-elf target for the
# guest builds in guests/{hello,composed,heavy}-methods.
if ! command -v cargo-risczero >/dev/null 2>&1; then
    if [[ "${SKIP_INSTALL:-0}" == "1" ]]; then
        warn "cargo-risczero not found; SKIP_INSTALL=1 set, aborting"
        exit 1
    fi
    step "Installing RISC Zero toolchain via rzup"
    if ! command -v rzup >/dev/null 2>&1; then
        curl -L https://risczero.com/install | bash
        export PATH="$HOME/.risc0/bin:$PATH"
    fi
    rzup install rust
    rzup install cargo-risczero
fi
export PATH="$HOME/.risc0/bin:$PATH"
step "RISC Zero: $(cargo risczero --version 2>/dev/null || echo 'cargo risczero not on PATH')"

# 3. Recursion zkr sidecar -------------------------------------------------
# The upstream risc0-circuit-recursion build script downloads a precomputed
# zkr from S3 and verifies its SHA. On some networks the download is mangled
# and verification panics. The verified copy ships in scripts/.
if [[ -f scripts/recursion_zkr.zip ]]; then
    export RECURSION_SRC_PATH="$PWD/scripts/recursion_zkr.zip"
    step "RECURSION_SRC_PATH = $RECURSION_SRC_PATH"
else
    warn "scripts/recursion_zkr.zip missing — the recursion build may fall back to S3 and could fail"
fi

# 4. Build the Phase-0 + state-machine crates ------------------------------
# We deliberately do NOT build the LEZ guest crates here. Those produce
# riscv32im-risc0-zkvm-elf ELFs via cargo risczero build (docker buildx
# required) and aren't part of the laptop dev loop. See docs in BUILD.md.
step "Building Phase-0 stack"
cargo build --release \
    -p psychopomp-types -p psychopomp-attest -p psychopomp-hwclass \
    -p psychopomp-client -p psychopomp-prover -p psychopomp-e2e \
    -p psychopomp-registry-core -p psychopomp-escrow-core \
    -p psychopomp-registry-program -p psychopomp-escrow-program

# 5. Unit tests ------------------------------------------------------------
step "Running unit tests"
cargo test --no-fail-fast \
    -p psychopomp-types -p psychopomp-attest -p psychopomp-hwclass \
    -p psychopomp-client -p psychopomp-prover -p psychopomp-e2e \
    -p psychopomp-registry-core -p psychopomp-escrow-core \
    -p psychopomp-registry-program -p psychopomp-escrow-program

# 6. (Optional) dev-mode e2e smoke ----------------------------------------
if [[ "${SKIP_E2E:-0}" != "1" ]]; then
    step "Running dev-mode e2e smoke (RISC0_DEV_MODE=1)"
    RISC0_DEV_MODE=1 ./scripts/e2e-localhost.sh --release
fi

cat <<'EOF'

==> bootstrap-dev done.

Next steps:
  - Real-STARK localhost run:  ./scripts/e2e-localhost.sh --release
  - Full Phase-0 protocol matrix (3 provers, ~30s in DEV mode):
        RISC0_DEV_MODE=1 ./scripts/e2e-all.sh
  - Heavy synthetic workload (sized for GPU vs CPU comparison):
        ./scripts/e2e-localhost.sh --release \\
            --test heavy --rounds 10000
  - Phase-1 on-chain (requires LEZ_HOME pointing at a logos-execution-zone
    checkout): see BUILD.md > "Phase 1 — on-chain registry/escrow".
  - Ship to a rented GPU box:
        ./scripts/bundle-for-runpod.sh   # secrets-safe by construction
EOF
