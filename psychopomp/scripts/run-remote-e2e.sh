#!/usr/bin/env bash
# Laptop-side: run the e2e harness against a remotely-deployed prover.
#
# Usage:  ENDPOINT=https://<runpod-tcp-proxy>:8088 ./scripts/run-remote-e2e.sh
#         (or pass --endpoint URL to the binary directly)

set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -z "${ENDPOINT:-}" ]]; then
    echo "Set ENDPOINT=http://your.prover:8088" >&2
    exit 1
fi

if [[ -z "${RECURSION_SRC_PATH:-}" && -f scripts/recursion_zkr.zip ]]; then
    export RECURSION_SRC_PATH="$PWD/scripts/recursion_zkr.zip"
fi

cargo build --release -p psychopomp-e2e

# RISC0_DEV_MODE only affects local proving (we have none here); we always
# verify the remote receipt against HELLO_ID, so DEV-mode-fake receipts
# returned by a misconfigured remote prover will fail verification.
exec ./target/release/psychopomp-e2e --endpoint "$ENDPOINT" "$@"
