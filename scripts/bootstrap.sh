#!/usr/bin/env bash
# Dev/testnet bootstrap for LDEX e2e: deploy token/ata/amm/router to the
# target LEZ L2 sequencer, create user A/B/LP holdings, mint two test
# tokens.
#
# Endpoint is configurable - same script for local-debug and the
# self-hosted testnet (the LEZ L2 sequencer genesis-funds initial_accounts
# identically, so no faucet is needed for the L2 setup; the
# logos-blockchain faucet funds the L1, not used here):
#
#   LDEX_SEQUENCER_ADDR   L2 sequencer RPC   (default http://127.0.0.1:3040)
#   LDEX_WALLET_HOME      wallet home dir    (default /tmp/ldex-bootstrap/wallet)
#   LDEX_WALLET_PW        wallet password    (default ldexdev)
#
# Program ids are deterministic RISC0 image ids → identical on any L1.
#
# Prereqs: target sequencer reachable; prebuilt guest .bin in
# programs/target/riscv-guest/...; wallet CLI built. Emits
# scripts/bootstrap.env with the ids the e2e test / shim need.

set -euo pipefail

# prog_id() shells out to `cargo`; ensure the rust/risc0 toolchains are on
# PATH even under a minimal non-login shell.
export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"

# Discover repo root from this script's own location (scripts/bootstrap.sh
# → parent dir = repo root). Lets a fresh clone in any location bootstrap
# without code edits.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"

# LEZ source-tree location (contains the wallet + sequencer source).
# Override with $LDEX_LEZ_DIR for non-default clones. The convention
# documented in SETUP.md is ~/ldex-spike/lez.
LEZ="${LDEX_LEZ_DIR:-$HOME/ldex-spike/lez}"
WALLET="${LDEX_WALLET_BIN:-$LEZ/target/release/wallet}"
PROG="$REPO/programs"
HOME_DIR="${LDEX_WALLET_HOME:-/tmp/ldex-bootstrap/wallet}"
PW="${LDEX_WALLET_PW:-ldexdev}"
SEQ_ADDR="${LDEX_SEQUENCER_ADDR:-http://127.0.0.1:3040}"
OUT="${LDEX_BOOTSTRAP_OUT:-$REPO/scripts/bootstrap.env}"

# host:port for the reachability pre-check (strip scheme).
SEQ_HP="${SEQ_ADDR#http://}"; SEQ_HP="${SEQ_HP#https://}"; SEQ_HP="${SEQ_HP%%/*}"
SEQ_HOST="${SEQ_HP%%:*}"; SEQ_PORT="${SEQ_HP##*:}"

export NSSA_WALLET_HOME_DIR="$HOME_DIR"
[ -x "$WALLET" ] || { echo "wallet CLI not built: $WALLET" >&2; exit 1; }
timeout 3 bash -c "</dev/tcp/${SEQ_HOST}/${SEQ_PORT}" 2>/dev/null \
  || { echo "sequencer not reachable at $SEQ_ADDR ($SEQ_HOST:$SEQ_PORT)" >&2; exit 1; }

mkdir -p "$HOME_DIR"
cp -f "$LEZ/wallet/configs/debug/wallet_config.json" "$HOME_DIR/wallet_config.json"
# Point the wallet at the chosen sequencer (debug config hardcodes :3040).
python3 - "$HOME_DIR/wallet_config.json" "$SEQ_ADDR" <<'PY'
import json,sys
p,addr=sys.argv[1],sys.argv[2]
d=json.load(open(p))
d["sequencer_addr"]=addr
d["seq_poll_timeout"]="2s"
json.dump(d,open(p,"w"),indent=4)
PY
echo ">> target sequencer: $SEQ_ADDR"

# Password is read from stdin (read_password_from_stdin); pipe it every call.
w() { printf '%s\n' "$PW" | "$WALLET" "$@"; }

# Parse "Generated new account with account_id Public/<base58>" -> Public/<b58>
new_pub() { w account new public 2>&1 | grep -oE 'Public/[1-9A-HJ-NP-Za-km-z]{32,44}' | head -1; }

# deploy-program prints nothing; fire it, then wait so this deploy tx lands
# in its OWN block. Multiple large program-deploy txs in one block exceed
# the L1 channel-inscription cap (MAX_BLOCK_SIZE*7/8 = 896 KiB) and panic
# the sequencer. block_create_timeout is 15s → wait 20s. (Same practice on
# the real testnet: deploy one program per block.)
deploy() {
  echo "   deploy $(basename "$1") ($(stat -c%s "$1") B) - own block" >&2
  w deploy-program "$1" >&2 2>&1 || true
  sleep 20
}

# Deterministic program id (RISC0 image id) of a guest .bin, as 64 hex chars.
AMM_FFI="$REPO/ffi/ldex-amm-ffi"
prog_id() {
  ( cd "$AMM_FFI" && cargo run -q --release --example amm_program_id -- "$1" )
}

echo ">> deploying programs"
TOKEN_BIN="$PROG/target/riscv-guest/token-methods/token-guest/riscv32im-risc0-zkvm-elf/release/token.bin"
ATA_BIN="$PROG/target/riscv-guest/ata-methods/ata-guest/riscv32im-risc0-zkvm-elf/release/ata.bin"
AMM_BIN="$PROG/target/riscv-guest/amm-methods/amm-guest/riscv32im-risc0-zkvm-elf/release/amm.bin"
ROUTER_BIN="$PROG/target/riscv-guest/private-swap-router-methods/private-swap-router-guest/riscv32im-risc0-zkvm-elf/release/private_swap_router.bin"
# WLEZ - wraps the native LEZ gas token 1:1 into an SPL-style token so
# the AMM can take it as a pool side. UI hides the wrap; user just sees
# "LEZ" in pickers and submits one combined wrap+swap behind the scenes.
WLEZ_BIN="$PROG/target/riscv-guest/wlez-methods/wlez-guest/riscv32im-risc0-zkvm-elf/release/wlez.bin"
# amm_v2: combined private-swap program. Replaces the
# (router + amm + 4× token::Transfer) recursive tree for mode-2
# disposable swaps with (amm_v2 + 4× token::Transfer). Same on-chain
# observable shape, ~5-10% wall-clock reduction. Testnet-compatible
# (just a regular deployed LEZ program; no nssa change).
AMM_V2_BIN="$PROG/target/riscv-guest/amm-v2-methods/amm-v2-guest/riscv32im-risc0-zkvm-elf/release/amm_v2.bin"
for b in "$TOKEN_BIN" "$ATA_BIN" "$AMM_BIN" "$ROUTER_BIN" "$WLEZ_BIN" "$AMM_V2_BIN"; do [ -f "$b" ] || { echo "missing $b" >&2; exit 1; }; done
# One program per block (each deploy() waits a block).
deploy "$TOKEN_BIN"; deploy "$ATA_BIN"; deploy "$AMM_BIN"; deploy "$ROUTER_BIN"; deploy "$WLEZ_BIN"; deploy "$AMM_V2_BIN"
echo ">> computing deterministic program ids"
TOKEN_ID=$(prog_id "$TOKEN_BIN")
ATA_ID=$(prog_id "$ATA_BIN")
AMM_ID=$(prog_id "$AMM_BIN")
ROUTER_ID=$(prog_id "$ROUTER_BIN")
WLEZ_ID=$(prog_id "$WLEZ_BIN")
AMM_V2_ID=$(prog_id "$AMM_V2_BIN")
echo "   amm program id = $AMM_ID"
echo "   router program id = $ROUTER_ID"
echo "   wlez program id = $WLEZ_ID"
echo "   amm_v2 program id = $AMM_V2_ID"

# Token universe. 10 tokens (TOKENA..TOKENJ) are minted, each with a
# distinct definition account and a keypair-signed user holding (HOLD_X)
# that receives the supply. ATAs (deterministic per (owner, def)) are
# created for the first 8 (A..H) and funded out of HOLD_X - these are
# the tokens that are "in the dev wallet" and immediately usable for
# F8 ATA-based swaps. TOKENI and TOKENJ exist as defs + supply but
# without funded ATAs, demonstrating the "token-agnostic, any token on
# chain" path: the user can still pick them in the catalog, create a
# pool against another token, and swap via the keypair-holding leg.
TOKENS=(A B C D E F G H I J)
FUND_LIMIT=8   # how many tokens to ATA-fund into the dev wallet

echo ">> creating accounts (USER + 10 (def, hold) pairs + LP holding)"
USER=$(new_pub)
HOLD_LP=$(new_pub)

# Fund USER from a preconfigured genesis account so it has native LEZ
# for the WLEZ wrap (and any future native-paying ops). Without this,
# auth_transfer's `assert!(sender.balance >= amount)` panics inside the
# zkVM and the FFI's rc=0 hides the failure (the tx is submitted but
# rejected during execution - see seq.log; this is the recurring
# "verify via seq.log not rc" pattern). The two preconfigured accounts
# `CbgR6tj…` (10000 LEZ) and `2RHZhw…` (20000 LEZ) are seeded in the
# sequencer's genesis from `wallet_config.json:initial_accounts` and
# their signing keys are baked into the wallet config. The sequencer
# config field MUST be `initial_public_accounts` (not `initial_accounts`)
# - older configs had the wrong name and serde silently ignored them.
# 8000 of the preconfigured 10000 - leaves 2000 in the source for any
# future ops (it's the only natively-funded account in dev).
LDEX_USER_NATIVE_FUND="${LDEX_USER_NATIVE_FUND:-8000}"
echo ">> funding USER from preconfigured genesis account ($LDEX_USER_NATIVE_FUND LEZ)"
# Use 2RHZ... as funder. Cbg... fails with "Can not pay for operation"
# (the LDEX-fork wallet doesn't carry private keys for it on this sequencer
# setup, only the public key for verification - handed via initial_accounts).
# 2RHZ... is in the wallet's signable set + has plenty of native LEZ.
w auth-transfer send \
  --from "Public/2RHZhw9h534Zr3eq2RGhQete2Hh667foECzXPmSkGni2" \
  --to "$USER" \
  --amount "$LDEX_USER_NATIVE_FUND" >&2 || \
  echo "   (genesis fund failed - check preconfigured account balance)" >&2
sleep 14
declare -A DEF
declare -A HOLD
declare -A ATA
for t in "${TOKENS[@]}"; do
  DEF[$t]=$(new_pub)
  HOLD[$t]=$(new_pub)
done

echo ">> minting 10 tokens"
for t in "${TOKENS[@]}"; do
  echo "   TOKEN${t}: def=${DEF[$t]}  hold=${HOLD[$t]}" >&2
  w token new --name "TOKEN${t}" --total-supply 1000000 \
    --definition-account-id "${DEF[$t]}" --supply-account-id "${HOLD[$t]}" >&2
done

# RFP Func #8 - derive and create ATAs for the first FUND_LIMIT tokens.
E2E() { ( cd "$AMM_FFI" && PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH" \
  RISC0_SKIP_BUILD=1 cargo run -q --release --example e2e_testnet -- "$@" ) ; }
ata_addr() { E2E ataid x x "$ATA_ID" "$1" "$2" 2>/dev/null | head -1; }
LDEX_ATA_FUND="${LDEX_ATA_FUND:-50000}"
i=0
for t in "${TOKENS[@]}"; do
  ATA[$t]=$(ata_addr "$USER" "${DEF[$t]}")
  echo "   ATA(USER,TOKEN${t})=${ATA[$t]}"
  if [ $i -lt $FUND_LIMIT ]; then
    E2E atacreate "$HOME_DIR/wallet_config.json" "$HOME_DIR/storage.json" \
        "$ATA_ID" "$USER" "${DEF[$t]}" >&2 || true
    sleep 14
  fi
  i=$((i+1))
done

if [ "$LDEX_ATA_FUND" -gt 0 ] 2>/dev/null; then
  echo ">> funding the first $FUND_LIMIT ATAs (${LDEX_ATA_FUND} each)"
  i=0
  for t in "${TOKENS[@]}"; do
    if [ $i -lt $FUND_LIMIT ]; then
      w account get --account-id "${HOLD[$t]}" >/dev/null 2>&1 || true
      w token send --from "${HOLD[$t]}" --to "${ATA[$t]}" \
          --amount "$LDEX_ATA_FUND" 2>&1 \
        | grep -iE "new_commitments|panic|error" | tail -1 >&2 || true
      sleep 18
    fi
    i=$((i+1))
  done
fi

# Shielded balances for private modes - the privacy modes (1/2/3) need
# PrivateOwned accounts the user has already shielded into. Create one
# private account per funded token (A..H), shield a slice of HOLD_<L>
# into it. The mini-app dispatches private modes against LDEX_PRIV_<L>.
new_priv() { printf '%s\n' "$PW" | "$WALLET" account new private --label "$1" 2>&1 | grep -oE 'Private/[1-9A-HJ-NP-Za-km-z]{32,44}' | head -1; }
LDEX_PRIV_FUND="${LDEX_PRIV_FUND:-100000}"
declare -A PRIV
echo ">> creating private accounts + shielding ${LDEX_PRIV_FUND} of each funded token"
i=0
for t in "${TOKENS[@]}"; do
  if [ $i -lt $FUND_LIMIT ]; then
    PRIV[$t]=$(new_priv "priv${t}-$$")
    echo "   PRIV(TOKEN${t})=${PRIV[$t]}"
    w account get --account-id "${HOLD[$t]}" >/dev/null 2>&1 || true
    w token send --from "${HOLD[$t]}" --to "${PRIV[$t]}" \
        --amount "$LDEX_PRIV_FUND" 2>&1 \
      | grep -iE "new_commitments|panic|error" | tail -1 >&2 || true
    sleep 18
  fi
  i=$((i+1))
done
# Sync the wallet's private-commitment view so later private ops see the
# fresh commitments (last_synced_block was 0 otherwise).
printf '%s\n' "$PW" | "$WALLET" account sync-private >/dev/null 2>&1 || true

# ── WLEZ (wrapped native gas token) ────────────────────────────────
# The mini-app shows "LEZ" as a normal catalog entry but the AMM only
# trades token-program holdings - so under the hood we wrap native into
# a 1:1 WLEZ token. Steps:
#   1. WLEZ_DEF / WLEZ_VAULT - deterministic PDAs derived from the
#      WLEZ program id (no chain call).
#   2. wlez_admin initialize - claims the vault + creates the WLEZ
#      token definition (chained NewFungibleDefinition with total_supply=0).
#      Idempotent - re-running this on a deployed-and-init'd WLEZ is a no-op.
#   3. HOLD_W - fresh keypair-derived account that becomes the user's
#      WLEZ holding. `init_token_holding` converts it to a valid
#      TokenHolding for WLEZ_DEF.
#   4. wlez_admin wrap - pre-locks `LDEX_WLEZ_FUND` native LEZ so the
#      user has WLEZ to trade with on first launch. (Skipped silently if
#      the user has no native balance to spare - e.g. a wallet that
#      hasn't been faucet-funded yet.)
echo ">> initialising WLEZ (wrapped native)"
WLEZ_ADMIN() { ( cd "$AMM_FFI" && PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH" \
  RISC0_SKIP_BUILD=1 cargo run -q --release --example wlez_admin -- "$@" ) ; }
# Pre-wrap default: 5000 of USER's ~8000 native LEZ. Leaves ~3000
# native so the user can still pay fees + wrap more from the UI.
# Sized so that ~40% of the wrap goes to ATA_W (≥2000 for live ATA
# swaps) and ~60% stays in HOLD_W (≥3000 so it can seed an AMM pool
# above MINIMUM_LIQUIDITY=1000 in step [9] of run-amm-v2-full-test.sh).
# Was 100000 - bigger than USER's whole genesis fund, so it silently
# failed at `assert!(sender.balance >= amount)`.
LDEX_WLEZ_FUND="${LDEX_WLEZ_FUND:-5000}"

# 1. Pure derivations.
WLEZ_DEF_HEX=$(WLEZ_ADMIN defid "$WLEZ_ID" 2>/dev/null | head -1)
WLEZ_VAULT_HEX=$(WLEZ_ADMIN vaultid "$WLEZ_ID" 2>/dev/null | head -1)
echo "   WLEZ def    (hex32) = $WLEZ_DEF_HEX"
echo "   WLEZ vault  (hex32) = $WLEZ_VAULT_HEX"

# 2. Initialize (idempotent). USER is the payer/signer for fees.
echo "   submitting wlez::Initialize"
WLEZ_ADMIN initialize "$HOME_DIR/wallet_config.json" "$HOME_DIR/storage.json" \
    "$WLEZ_ID" "${DEF[A]}" "$USER" >&2 || \
  echo "   (initialize failed - assuming already initialised)" >&2
sleep 14

# 3. Fresh user WLEZ holding + init.
HOLD_W=$(new_pub)
echo "   user WLEZ holding   = $HOLD_W"
E2E init "$HOME_DIR/wallet_config.json" "$HOME_DIR/storage.json" \
    "$ATA_ID" "$WLEZ_DEF_HEX" "$HOLD_W" >&2 || true
sleep 14

# 4. Pre-wrap a small amount so the UI's LEZ row is non-empty.
echo "   pre-wrapping $LDEX_WLEZ_FUND native LEZ into WLEZ"
WLEZ_ADMIN wrap "$HOME_DIR/wallet_config.json" "$HOME_DIR/storage.json" \
    "$WLEZ_ID" "$USER" "$HOLD_W" "$LDEX_WLEZ_FUND" >&2 || \
  echo "   (wrap failed - user may have insufficient native balance)" >&2
sleep 14

# 5. RFP Func #8 (WLEZ side) - create + fund USER's WLEZ ATA so
#    WLEZ-paired ATA flows (pool create / swap_exact_in_ata against
#    a TOKEN/WLEZ pair) have a populated source ATA.
ATA_W=$(E2E ataid x x "$ATA_ID" "$USER" "$WLEZ_DEF_HEX" 2>/dev/null | head -1)
echo "   ATA(USER, WLEZ_DEF) = $ATA_W"
E2E atacreate "$HOME_DIR/wallet_config.json" "$HOME_DIR/storage.json" \
    "$ATA_ID" "$USER" "$WLEZ_DEF_HEX" >&2 || true
sleep 14
# Split: ~40% to the ATA (immediately tradeable via mode-0 ATA swap)
# and ~60% stays in HOLD_W so it can seed an AMM pool with WLEZ as
# one side (sqrt(amount²) ≥ MINIMUM_LIQUIDITY=1000 is required).
WLEZ_ATA_FUND=$(( LDEX_WLEZ_FUND * 2 / 5 ))
if [ "$WLEZ_ATA_FUND" -gt 0 ]; then
  printf '%s\n' "$PW" | "$WALLET" token send --from "$HOLD_W" --to "$ATA_W" \
      --amount "$WLEZ_ATA_FUND" 2>&1 \
    | grep -iE "new_commitments|panic|error" | tail -1 >&2 || true
  sleep 14
fi

# ── Seed a default pool ─────────────────────────────────────────────
# Without this, a fresh-bootstrap user opens the mini-app's Pools tab and
# sees "No pools exist yet" - every swap then fails with `{exists:false}`.
# Seed TOKENA/TOKENB at fee=5 from HOLD_A/HOLD_B so the first launch has
# a working market. Skippable via LDEX_SKIP_POOL_SEED=1 (CI / re-runs).
#
# Goes through the FFI binary because the wallet CLI's `amm` subcommand
# is disabled in the LDEX wallet fork; the `v2pool_ata` example binary
# wraps `ldex_amm_v2_new_pool_ata` (the same FFI the mini-app's
# createPoolFor uses).
SEED_POOL_AMOUNT="${LDEX_SEED_POOL_AMOUNT:-100000}"
SEED_POOL_FEE="${LDEX_SEED_POOL_FEE:-5}"
if [ "${LDEX_SKIP_POOL_SEED:-0}" != "1" ]; then
  E2E_BIN="${LDEX_E2E_BIN:-$REPO/ffi/ldex-amm-ffi/target/release/examples/e2e_testnet}"
  if [ -x "$E2E_BIN" ]; then
    echo ">> seeding pool TOKENA/TOKENB @ fee=${SEED_POOL_FEE} (${SEED_POOL_AMOUNT}/${SEED_POOL_AMOUNT})"
    "$E2E_BIN" v2pool_ata \
        "$HOME_DIR/wallet_config.json" \
        "$HOME_DIR/storage.json" \
        "$AMM_V2_ID" \
        "$ATA_ID" \
        "$USER" \
        "${HOLD[A]}" \
        "${HOLD[B]}" \
        "$SEED_POOL_AMOUNT" \
        "$SEED_POOL_FEE" 2>&1 | tail -1 >&2 || \
      echo "   (pool seed failed - first-launch UI will show empty pools)" >&2
    sleep 5
  else
    echo "   (skipping pool seed - e2e_testnet binary not built at $E2E_BIN)" >&2
  fi
fi

# Emit the env. Back-compat: LDEX_DEF_A/B + LDEX_USER_HOLDING_A/B +
# LDEX_ATA_A/B still alias to TOKENA/TOKENB so existing tooling keeps
# working. The new LDEX_TOKENS array drives the mini-app's catalog.
{
  echo "# LDEX bootstrap output ($(date -u +%FT%TZ))"
  echo "export NSSA_WALLET_HOME_DIR=\"$HOME_DIR\""
  echo "export LDEX_WALLET_CONFIG=\"$HOME_DIR/wallet_config.json\""
  echo "export LDEX_WALLET_STORAGE=\"$HOME_DIR/storage.json\""
  echo "export LDEX_WALLET_PW=\"$PW\""
  echo "export LDEX_TOKEN_PROGRAM_ID=\"$TOKEN_ID\""
  echo "export LDEX_ATA_PROGRAM_ID=\"$ATA_ID\""
  echo "export LDEX_AMM_PROGRAM_ID=\"$AMM_ID\""
  echo "export LDEX_ROUTER_PROGRAM_ID=\"$ROUTER_ID\""
  echo "export LDEX_WLEZ_PROGRAM_ID=\"$WLEZ_ID\""
  echo "export LDEX_AMM_V2_PROGRAM_ID=\"$AMM_V2_ID\""
  echo "export LDEX_WLEZ_DEF=\"$WLEZ_DEF_HEX\""
  echo "export LDEX_WLEZ_VAULT=\"$WLEZ_VAULT_HEX\""
  echo "export LDEX_HOLD_W=\"$HOLD_W\""
  echo "export LDEX_ATA_W=\"$ATA_W\""
  echo "export LDEX_WLEZ_FUND=\"$LDEX_WLEZ_FUND\""
  echo "export LDEX_SEQUENCER_ADDR=\"$SEQ_ADDR\""
  echo "export LDEX_USER_OWNER=\"$USER\""
  echo "export LDEX_USER_HOLDING_LP=\"$HOLD_LP\""
  echo "export LDEX_TOKENS=\"${TOKENS[*]}\""
  echo "export LDEX_FUND_LIMIT=\"$FUND_LIMIT\""
  for t in "${TOKENS[@]}"; do
    echo "export LDEX_DEF_${t}=\"${DEF[$t]}\""
    echo "export LDEX_HOLD_${t}=\"${HOLD[$t]}\""
    echo "export LDEX_ATA_${t}=\"${ATA[$t]}\""
    if [ -n "${PRIV[$t]:-}" ]; then
      echo "export LDEX_PRIV_${t}=\"${PRIV[$t]}\""
    fi
  done
  # Back-compat aliases (TOKENA/B mirror DEF_A/B).
  echo "export LDEX_DEF_A=\"${DEF[A]}\""
  echo "export LDEX_USER_HOLDING_A=\"${HOLD[A]}\""
  echo "export LDEX_DEF_B=\"${DEF[B]}\""
  echo "export LDEX_USER_HOLDING_B=\"${HOLD[B]}\""
  echo "export LDEX_ATA_A=\"${ATA[A]}\""
  echo "export LDEX_ATA_B=\"${ATA[B]}\""
} > "$OUT"
echo ">> wrote $OUT"; cat "$OUT"
