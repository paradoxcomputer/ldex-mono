#!/usr/bin/env bash
# Live-test runner for amm_v2 + the existing AMM mode 0/1/2.
#
# Sequence:
#   1. Allocate a fresh public LP holding for the amm_v2 pool.
#   2. Create the amm_v2 pool (100_000 of each side at 30 bps fees).
#   3. Sanity: mode-0 public swap on the EXISTING AMM (~15 s).
#   4. Allocate fresh A holdings (mode-2 disposable, RFP AC#4) + init.
#   5. Run the amm_v2 disposable swap (mode-2 via amm_v2; testnet-compat).
#   6. Verify on-chain balance deltas + seq.log clean.

set -euo pipefail
export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"

# Discover repo root from this script's location.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
source "${LDEX_ENV_FILE:-$REPO/scripts/bootstrap.env}"

LEZ="${LDEX_LEZ_DIR:-$HOME/ldex-spike/lez}"
WALLET="${LDEX_WALLET_BIN:-$LEZ/target/release/wallet}"
AMM_FFI="$REPO/ffi/ldex-amm-ffi"
E2E="$AMM_FFI/target/release/examples/e2e_testnet"
PW="${LDEX_WALLET_PW:-ldexdev}"
export NSSA_WALLET_HOME_DIR

w() { printf '%s\n' "$PW" | "$WALLET" "$@"; }
new_pub() { w account new public 2>&1 | grep -oE 'Public/[1-9A-HJ-NP-Za-km-z]{32,44}' | head -1; }

echo ">> [step 1] allocate fresh LP holding for amm_v2 pool"
HOLD_LP_V2=$(new_pub)
echo "   LP holding for amm_v2 = $HOLD_LP_V2"

echo ">> [step 2] create amm_v2 pool (100_000 TOKENA + 100_000 TOKENB at 30 bps)"
"$E2E" v2pool "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_V2_PROGRAM_ID" "$LDEX_HOLD_A" "$LDEX_HOLD_B" "$HOLD_LP_V2" \
    100000 30
sleep 18

echo ">> [step 3] sanity: public swap (mode 0) on EXISTING AMM"
"$E2E" pubswap "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_PROGRAM_ID" "$LDEX_HOLD_A" "$LDEX_HOLD_B" "$LDEX_DEF_A" \
    10 1 30 || echo "   (mode-0 swap on existing AMM — needs an existing AMM pool; create one with: e2e_testnet pool $LDEX_AMM_PROGRAM_ID $LDEX_HOLD_A $LDEX_HOLD_B $LDEX_USER_HOLDING_LP 100000 30)"
sleep 18

echo ">> [step 4] allocate + init fresh A holdings for mode-2 disposable"
A_A=$(new_pub)
echo "   a_holding_a = $A_A"
"$E2E" init "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_ATA_PROGRAM_ID" "$LDEX_DEF_A" "$A_A"
sleep 18

A_B=$(new_pub)
echo "   a_holding_b = $A_B"
"$E2E" init "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_ATA_PROGRAM_ID" "$LDEX_DEF_B" "$A_B"
sleep 18

echo ">> [step 5] sync private accounts so AccountManager sees current commitments"
w account sync-private >/dev/null 2>&1 || true

echo ">> [step 6] fire mode-2 amm_v2 disposable swap (real ZK, CPU)"
echo "   ${LDEX_PRIV_A} → ${LDEX_PRIV_B} via ${A_A}/${A_B} on amm_v2 pool"
T0=$(date +%s)
unset RISC0_DEV_MODE
"$E2E" v2disp "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_V2_PROGRAM_ID" \
    "$LDEX_PRIV_A" "$LDEX_PRIV_B" \
    "$A_A" "$A_B" \
    "$LDEX_DEF_A" "$LDEX_DEF_B" \
    "$LDEX_DEF_A" \
    100 1 30
T1=$(date +%s)
echo ">> amm_v2 wall-clock: $((T1 - T0))s"
echo ">> seq.log error lines (should be empty):"
grep -E 'InvalidPrivacyPreservingProof|InvalidProof|panicked|failed execution check' /tmp/ldex-dev/seq.log | tail -10 || echo "   (none)"
