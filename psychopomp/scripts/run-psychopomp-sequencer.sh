#!/usr/bin/env bash
# Run a dedicated psychopomp LEZ sequencer (standalone, no L1).
# Listens on :3050 by default; writes its rocksdb state into ./sequencer-state.
#
# Leave this running while doing on-chain registry / escrow operations
# (see docs/PHASE1-onchain.md).
#
# Requires: a Logos Execution Zone (LEZ) checkout that produces a
# `sequencer_service` binary under <LEZ_HOME>/target/release/. Set LEZ_HOME
# to point at it. Default: $HOME/lez

set -euo pipefail

LEZ="${LEZ_HOME:-$HOME/lez}"
if [[ ! -d "$LEZ" ]]; then
    echo "LEZ checkout not found at $LEZ" >&2
    echo "Set LEZ_HOME=/path/to/your/lez checkout (see docs/PHASE1-onchain.md)." >&2
    exit 1
fi
HERE="$(cd "$(dirname "$0")/.." && pwd)"
CFG="$HERE/sequencer-state/psychopomp_sequencer_config.json"
BIN="$LEZ/target/release/sequencer_service"
PORT="${PORT:-3050}"

export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"
. "$HOME/.cargo/env" 2>/dev/null || true
export LOGOS_BLOCKCHAIN_CIRCUITS="$HOME/.logos-blockchain-circuits"
unset RISC0_DEV_MODE

if [[ ! -x "$BIN" ]]; then
    echo ">> Building standalone sequencer (first run; slow)..."
    cd "$LEZ" && cargo build --features standalone -p sequencer_service --release
fi

mkdir -p "$HERE/sequencer-state"
echo ">> Starting psychopomp sequencer (standalone) on 127.0.0.1:$PORT"
echo "   state dir: $HERE/sequencer-state"
echo "   config:    $CFG"
echo "   Ctrl-C to stop"
exec "$BIN" --port "$PORT" "$CFG"
