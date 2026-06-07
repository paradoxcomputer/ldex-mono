#!/usr/bin/env bash
# Run a local LEZ sequencer in standalone mode (no Bedrock/Indexer).
# Listens on 127.0.0.1:3040 - the address the mini-app's wallet config uses.
# Genesis accounts are pre-funded (no faucet needed for local dev).
#
# Leave this running in its own terminal while you use the mini-app's
# "Chain height" button (and later swap/liquidity ops).

set -euo pipefail

# LEZ source-tree location. Override with $LDEX_LEZ_DIR for non-default
# clones. Convention documented in SETUP.md is ~/ldex-spike/lez.
LEZ="${LDEX_LEZ_DIR:-$HOME/ldex-spike/lez}"
CFG="${LDEX_SEQUENCER_CONFIG:-$LEZ/sequencer/service/configs/debug/sequencer_config.json}"

export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"
. "$HOME/.cargo/env" 2>/dev/null || true
export LOGOS_BLOCKCHAIN_CIRCUITS="$HOME/.logos-blockchain-circuits"
unset RISC0_DEV_MODE   # real proofs

cd "$LEZ"
BIN="$LEZ/target/release/sequencer_service"
if [ ! -x "$BIN" ]; then
  echo ">> Building standalone sequencer (first run; slow)..."
  cargo build --features standalone -p sequencer_service --release
fi

echo ">> Starting sequencer (standalone) on 127.0.0.1:3040 - Ctrl-C to stop"
exec "$BIN" "$CFG"
