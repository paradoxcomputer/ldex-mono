#!/usr/bin/env bash
# Phase-0 end-to-end localhost test.
#
# Launches the prover on 127.0.0.1:8088, runs the e2e harness against it,
# tears down. Pass `--release` to use the optimised binaries.
#
# Modes:
#   RISC0_DEV_MODE=1 (default for CI)  → fake receipts, ~seconds, no GPU needed
#   RISC0_DEV_MODE=0                   → real STARK on whatever default_prover() picks

set -euo pipefail

PROFILE=debug
CARGO_FLAGS=()
if [[ "${1:-}" == "--release" ]]; then
    PROFILE=release
    CARGO_FLAGS+=("--release")
    shift
fi

cd "$(dirname "$0")/.."

# Use the verified recursion zkr shipped in scripts/ to bypass the broken
# S3 verifier hash dance (see BUILD.md > Build notes).
if [[ -z "${RECURSION_SRC_PATH:-}" && -f scripts/recursion_zkr.zip ]]; then
    export RECURSION_SRC_PATH="$PWD/scripts/recursion_zkr.zip"
fi

echo "[e2e] profile=$PROFILE  RISC0_DEV_MODE=${RISC0_DEV_MODE:-unset}"

# Build first so the launch is instant.
cargo build "${CARGO_FLAGS[@]}" -p psychopomp-prover -p psychopomp-e2e

PORT=${PORT:-8088}
PROVER_BIN=target/$PROFILE/psychopomp-prover
E2E_BIN=target/$PROFILE/psychopomp-e2e
LOG=$(mktemp /tmp/psychopomp-prover.XXXXXX.log)

echo "[e2e] launching prover (log=$LOG)"
"$PROVER_BIN" --bind "127.0.0.1:$PORT" >"$LOG" 2>&1 &
PROVER_PID=$!
trap 'kill $PROVER_PID 2>/dev/null || true; echo "[e2e] prover log:"; tail -50 "$LOG" 2>/dev/null || true' EXIT

# Wait for /v0/health to respond (up to 10s).
for i in $(seq 1 50); do
    if curl -fsS "http://127.0.0.1:$PORT/v0/health" >/dev/null 2>&1; then
        break
    fi
    sleep 0.2
done
if ! curl -fsS "http://127.0.0.1:$PORT/v0/health" >/dev/null 2>&1; then
    echo "[e2e] prover failed to come up" >&2
    exit 1
fi
echo "[e2e] prover up"

"$E2E_BIN" --endpoint "http://127.0.0.1:$PORT" "$@"

echo "[e2e] success"
