#!/usr/bin/env bash
# Full end-to-end test of amm_v2 routing for mode 0/1/2.
#
# Steps:
#  1. Pre-create the user's LP ATA so amm_v2 can mint into it.
#  2. Create amm_v2 pool via ATA-only path (100_000 / 100_000 at 30 bps).
#  3. Mode 0 - public swap via amm_v2 ATA path (v2pubswap_ata, ~15 s).
#  4. Mode 1 - PrivateOwned private swap via amm_v2 (v2swap1, ~10 m CPU).
#  5. Mode 2 - Disposable swap via amm_v2 (v2disp, ~14 m CPU).
#  6. Verify on-chain deltas via wallet `account get` for each step.

set -euo pipefail
export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"
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

show() { printf '%s\n' "$PW" | NSSA_WALLET_HOME_DIR=/tmp/ldex-bootstrap/wallet "$WALLET" account get --account-id "$1" 2>&1 | tail -2; }

echo "==[1] create amm_v2 pool (LP→ATA): 100_000 / 100_000 at 30 bps"
"$E2E" v2pool_ata "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_V2_PROGRAM_ID" \
    "$LDEX_ATA_PROGRAM_ID" "$LDEX_USER_OWNER" \
    "$LDEX_USER_HOLDING_A" "$LDEX_USER_HOLDING_B" \
    100000 30
sleep 18
echo "    HOLD_A after pool create:"; show "$LDEX_USER_HOLDING_A"
echo "    HOLD_B after pool create:"; show "$LDEX_USER_HOLDING_B"

echo "==[3] MODE 0 (ATA-only swap via amm_v2) - 10 TOKENA → ?"
"$E2E" v2pubswap_ata "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_V2_PROGRAM_ID" \
    "$LDEX_ATA_PROGRAM_ID" "$LDEX_USER_OWNER" \
    "$LDEX_DEF_A" "$LDEX_DEF_B" "$LDEX_DEF_A" \
    10 1 30
sleep 18
echo "    ATA_A after mode-0:"; show "$LDEX_ATA_A"
echo "    ATA_B after mode-0:"; show "$LDEX_ATA_B"

echo "==[4] sync wallet private state before private swaps"
w account sync-private >/dev/null 2>&1 || true

echo "==[5] MODE 1 (PrivateOwned via amm_v2) - 100 TOKENA → ?"
T0=$(date +%s)
unset RISC0_DEV_MODE
"$E2E" v2swap1 "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_V2_PROGRAM_ID" "$LDEX_PRIV_A" "$LDEX_PRIV_B" \
    "$LDEX_DEF_A" "$LDEX_DEF_B" "$LDEX_DEF_A" \
    100 1 30
T1=$(date +%s)
echo "    mode-1 wall-clock: $((T1 - T0))s"
# Wait for the privacy tx to be validated into a block before the
# next privacy tx (otherwise mode-2 constructs its witness from a
# pre-mode-1 commitment-tree snapshot → "Commitment already seen").
sleep 30
w account sync-private >/dev/null 2>&1 || true
echo "    PRIV_A after mode-1:"; show "$LDEX_PRIV_A"
echo "    PRIV_B after mode-1:"; show "$LDEX_PRIV_B"

echo "==[6] allocate + init fresh A holdings for mode-2"
A_A=$(new_pub)
A_B=$(new_pub)
"$E2E" init "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" "$LDEX_ATA_PROGRAM_ID" "$LDEX_DEF_A" "$A_A"
sleep 18
"$E2E" init "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" "$LDEX_ATA_PROGRAM_ID" "$LDEX_DEF_B" "$A_B"
sleep 18

echo "==[7] MODE 2 (Disposable via amm_v2) - 100 TOKENA → ?"
T2=$(date +%s)
"$E2E" v2disp "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
    "$LDEX_AMM_V2_PROGRAM_ID" "$LDEX_PRIV_A" "$LDEX_PRIV_B" \
    "$A_A" "$A_B" "$LDEX_DEF_A" "$LDEX_DEF_B" "$LDEX_DEF_A" \
    100 1 30
T3=$(date +%s)
echo "    mode-2 wall-clock: $((T3 - T2))s"
# Mode-2 disposable mints two new commitments (PRIV_A drain + PRIV_B
# credit) and the wallet's private-scanner needs the tx to actually
# be in a sealed block before sync-private will pick them up. The FFI
# returns at mempool-accept; one sleep + sync usually isn't enough on
# CPU-paced dev sequencers. Sleep 30s + double-sync to be sure.
sleep 30
w account sync-private >/dev/null 2>&1 || true
sleep 5
w account sync-private >/dev/null 2>&1 || true
echo "    PRIV_A after mode-2:"; show "$LDEX_PRIV_A"
echo "    PRIV_B after mode-2:"; show "$LDEX_PRIV_B"
echo "    a_holding_a (net-zero):"; show "$A_A"
echo "    a_holding_b (net-zero):"; show "$A_B"

echo "==[8] seq.log error scan"
grep -E 'InvalidPrivacyPreservingProof|InvalidProof|panicked|failed execution check' /tmp/ldex-dev/seq.log | tail -10 || echo "    (none)"

echo "==[9] WLEZ + ATA: pool create TOKEN_A/WLEZ via new_pool_ata"
if [ -n "${LDEX_HOLD_W:-}" ] && [ -n "${LDEX_ATA_W:-}" ]; then
    # 2000 of each - must exceed amm_v2's MINIMUM_LIQUIDITY=1000
    # (initial_lp = sqrt(amount_a · amount_b)). Bootstrap pre-wraps
    # 5000 native LEZ → ~2000 to ATA_W (40%) and ~3000 to HOLD_W (60%);
    # HOLD_W has enough headroom to seed this pool.
    "$E2E" v2pool_ata "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
        "$LDEX_AMM_V2_PROGRAM_ID" \
        "$LDEX_ATA_PROGRAM_ID" "$LDEX_USER_OWNER" \
        "$LDEX_USER_HOLDING_A" "$LDEX_HOLD_W" \
        2000 30
    sleep 18
    echo "    HOLD_A after WLEZ pool:"; show "$LDEX_USER_HOLDING_A"
    echo "    HOLD_W after WLEZ pool:"; show "$LDEX_HOLD_W"

    echo "==[10] mode-0 ATA swap: 5 WLEZ → TOKENA via ATA(USER, WLEZ_DEF)"
    "$E2E" v2pubswap_ata "$LDEX_WALLET_CONFIG" "$LDEX_WALLET_STORAGE" \
        "$LDEX_AMM_V2_PROGRAM_ID" \
        "$LDEX_ATA_PROGRAM_ID" "$LDEX_USER_OWNER" \
        "$LDEX_DEF_A" "$LDEX_WLEZ_DEF" "$LDEX_WLEZ_DEF" \
        5 1 30
    sleep 18
    echo "    ATA_W after WLEZ swap:"; show "$LDEX_ATA_W"
    echo "    ATA_A after WLEZ swap:"; show "$LDEX_ATA_A"
else
    echo "    (skipped - LDEX_HOLD_W / LDEX_ATA_W not in env)"
fi

echo "==[DONE]"
