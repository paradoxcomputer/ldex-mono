#!/usr/bin/env bash
# Post-bootstrap driver for the mode-2 disposable monolithic swap measurement.
#
# Steps:
#   1. Source bootstrap.env (created by scripts/bootstrap.sh)
#   2. Create a fresh AMM pool from HOLD_A + HOLD_B (initial reserves 100_000 each)
#   3. Allocate + init two fresh public account-A holdings (a_a for TOKENA, a_b for TOKENB)
#   4. Pre-fund a_a so the deshield credit doesn't exceed `is_authorized` (the
#      router-side `shift_balance` requires Fungible; init sets balance=0 which
#      is fine for net-zero round-trip - no actual minted balance needed).
#   5. Run `e2e_testnet mono2 …` and capture wall-clock
#   6. Tail seq.log for "Validated transaction" or "InvalidPrivacyPreservingProof"
#
# Run after bootstrap completes:
#   bash scripts/run-mono2.sh
set -euo pipefail
export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"

REPO="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
ENV_FILE="${LDEX_ENV_FILE:-$REPO/scripts/bootstrap.env}"
[ -f "$ENV_FILE" ] || { echo "FAIL: $ENV_FILE missing - bootstrap didn't finish?" >&2; exit 1; }
# shellcheck disable=SC1090
source "$ENV_FILE"

LEZ="${LDEX_LEZ_DIR:-$HOME/ldex-spike/lez}"
WALLET="${LDEX_WALLET_BIN:-$LEZ/target/release/wallet}"
AMM_FFI="$REPO/ffi/ldex-amm-ffi"
E2E="$AMM_FFI/target/release/examples/e2e_testnet"
PW="${LDEX_WALLET_PW:-ldexdev}"
export NSSA_WALLET_HOME_DIR

w() { printf '%s\n' "$PW" | "$WALLET" "$@"; }
new_pub() { w account new public 2>&1 | grep -oE 'Public/[1-9A-HJ-NP-Za-km-z]{32,44}' | head -1; }

# 1. Create a fresh pool with 100_000 of each token at 30 bps fees.
#    Public AMM tx (no ZK).
echo ">> creating pool (HOLD_A=$LDEX_HOLD_A, HOLD_B=$LDEX_HOLD_B, LP=$LDEX_USER_HOLDING_LP, init=100000, fees=30)"
$E2E pool "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_PROGRAM_ID" "$LDEX_HOLD_A" "$LDEX_HOLD_B" "$LDEX_USER_HOLDING_LP" \
    100000 30
sleep 18

# 2. Allocate + init fresh disposable account-A holdings (mode-2 RFP-literal).
echo ">> allocating fresh A holding for TOKENA"
A_A=$(new_pub)
echo "   a_holding_a = $A_A"
$E2E init "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_ATA_PROGRAM_ID" "$LDEX_DEF_A" "$A_A"
sleep 18

echo ">> allocating fresh A holding for TOKENB"
A_B=$(new_pub)
echo "   a_holding_b = $A_B"
$E2E init "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_ATA_PROGRAM_ID" "$LDEX_DEF_B" "$A_B"
sleep 18

# 3. Sync private holdings so AccountManager can fetch up-to-date
#    membership proofs (the bootstrap's sync covered the initial
#    shieldings; pool creation issued public block(s) on top so the
#    commitment tree's `last_synced_block` is current).
echo ">> syncing private accounts"
w account sync-private >/dev/null 2>&1 || true

# 4. Mark the start time, fire the mono2 swap, capture stdout.
echo ">> firing mode-2 monolithic disposable swap (real ZK, CPU)"
echo "   $LDEX_PRIV_A → $LDEX_PRIV_B via $A_A/$A_B in pool($LDEX_DEF_A, $LDEX_DEF_B), 100→?"
T0=$(date +%s)
unset RISC0_DEV_MODE
$E2E mono2 "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_PROGRAM_ID" \
    "$LDEX_PRIV_A" "$LDEX_PRIV_B" \
    "$A_A" "$A_B" \
    "$LDEX_DEF_A" "$LDEX_DEF_B" \
    "$LDEX_DEF_A" \
    100 1 30
T1=$(date +%s)
echo ">> wall-clock: $((T1 - T0))s"
echo ">> seq.log relevant lines:"
tail -100 /tmp/ldex-dev/seq.log 2>/dev/null \
  | grep -E 'Validated transaction|InvalidPrivacyPreservingProof|InvalidProof|panic|error' \
  | tail -20
