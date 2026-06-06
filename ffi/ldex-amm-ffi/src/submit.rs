//! Signed-submit AMM ops (embedded wallet backend — A/C).
//!
//! Builds AMM `PublicTransaction`s against *our* fee-tier `amm_core` +
//! deployed AMM program id, signs with the user's wallet keys, submits to
//! the sequencer. Modeled on `wallet/src/program_facades/amm.rs` but using
//! our fork's instruction shape (`fees`/`deadline`) and required account
//! order (incl. `lp_lock_holding` for `NewDefinition`). Every op takes the
//! `fees` tier so it targets the correct (pair, fee-tier) pool.
//!
//! Stateless: each call opens the already-onboarded wallet
//! (`WalletCore::new_update_chain`). Wallet creation/import (A/C) is
//! onboarding, handled elsewhere.

use std::ffi::{c_char, CStr};
use std::path::PathBuf;

use amm_core::{
    compute_liquidity_token_pda, compute_lp_lock_holding_pda, compute_pool_pda, compute_vault_pda,
    Instruction, PoolDefinition, CLOCK_01,
};
// Monolithic single-STARK swap support (task #37). The methods crate
use std::collections::HashMap;

use common::transaction::NSSATransaction;
use nssa::privacy_preserving_transaction::circuit::ProgramWithDependencies;
use nssa::program::Program;
use nssa_core::account::AccountId;
use sequencer_service_rpc::RpcClient as _;
use token_core::TokenHolding;
use wallet::{AccDecodeData, ExecutionFailureKind, PrivacyPreservingAccount, WalletCore};

use crate::{
    program_id_from_bytes, read_id, LDEX_AMM_ERR_ACCOUNT, LDEX_AMM_ERR_KEY, LDEX_AMM_ERR_NULL,
    LDEX_AMM_ERR_SUBMIT, LDEX_AMM_ERR_UTF8, LDEX_AMM_ERR_WALLET, LDEX_AMM_OK,
};

unsafe fn c_str(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(str::to_owned)
}

/// Common prep: open wallet, resolve token definitions from the user's
/// holdings, derive the fee-tier pool + vault PDAs.
struct Prep {
    wallet: WalletCore,
    def_a: AccountId,
    def_b: AccountId,
    pool: AccountId,
    vault_a: AccountId,
    vault_b: AccountId,
}

async fn prep(
    cfg: &str,
    store: &str,
    amm_pid: nssa_core::program::ProgramId,
    uha: AccountId,
    uhb: AccountId,
    fees: u128,
) -> Result<Prep, i32> {
    let wallet = WalletCore::new_update_chain(PathBuf::from(cfg), PathBuf::from(store), None)
        .map_err(|_| LDEX_AMM_ERR_WALLET)?;
    let ua = wallet
        .get_account_public(uha)
        .await
        .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
    let ub = wallet
        .get_account_public(uhb)
        .await
        .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
    let def_a = TokenHolding::try_from(&ua.data)
        .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?
        .definition_id();
    let def_b = TokenHolding::try_from(&ub.data)
        .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?
        .definition_id();
    let pool = compute_pool_pda(amm_pid, def_a, def_b, fees);
    let vault_a = compute_vault_pda(amm_pid, pool, def_a);
    let vault_b = compute_vault_pda(amm_pid, pool, def_b);
    Ok(Prep {
        wallet,
        def_a,
        def_b,
        pool,
        vault_a,
        vault_b,
    })
}

/// Privacy-path prep: open the wallet and derive pool/vault PDAs from
/// **explicit token definition ids** — never reads the user holdings via
/// `get_account_public` (private/`PrivateOwned` holdings are
/// commitment-based and have no public state). The caller passes the
/// definition ids it already knows (bootstrap `LDEX_DEF_A/B`).
struct PrepP {
    wallet: WalletCore,
    pool: AccountId,
    vault_a: AccountId,
    vault_b: AccountId,
}

async fn prep_private(
    cfg: &str,
    store: &str,
    amm_pid: nssa_core::program::ProgramId,
    def_a: AccountId,
    def_b: AccountId,
    fees: u128,
) -> Result<PrepP, i32> {
    let mut wallet = WalletCore::new_update_chain(PathBuf::from(cfg), PathBuf::from(store), None)
        .map_err(|_| LDEX_AMM_ERR_WALLET)?;

    // Mode-2 disposable router proofs use composite receipts (router +
    // AMM + 2× token::Transfer = 4 STARK assumptions, ~20-30 MB total)
    // which exceed jsonrpsee's default 10 MiB body cap, hence the
    // "Sequencer client error" we saw at the post-prove submit step.
    // `wallet.sequencer_client` is a `pub` field on `WalletCore`, so we
    // build a fresh HttpClient with 100 MiB max sizes on both directions
    // and swap it in *after* construction. The wallet's internal poller
    // keeps its own 10 MiB clone of the client, but the poller is only
    // used for `sync_to_block` — the privacy-tx submit path goes through
    // `self.sequencer_client.send_transaction(...)` directly (see
    // `wallet/src/lib.rs:430` in v0.2.0-rc3), which is the field we just
    // replaced. The server-side cap is bumped to match in
    // `lez/sequencer/service/src/lib.rs:22` (LDEX-local edit).
    //
    // The alternative — `NSSA_RECEIPT=succinct` to fold all assumptions
    // into one final STARK — keeps receipts small but adds ~2-3× to
    // proof generation time. The body-cap bump is the better trade.
    const RPC_BODY_MAX_BYTES: u32 = 100 * 1024 * 1024;
    let wallet_cfg = wallet::config::WalletConfig::from_path_or_initialize_default(
        std::path::Path::new(cfg),
    )
    .map_err(|_| LDEX_AMM_ERR_WALLET)?;
    let bumped_client = sequencer_service_rpc::SequencerClientBuilder::default()
        .max_request_size(RPC_BODY_MAX_BYTES)
        .max_response_size(RPC_BODY_MAX_BYTES)
        .build(wallet_cfg.sequencer_addr.clone())
        .map_err(|_| LDEX_AMM_ERR_WALLET)?;
    wallet.sequencer_client = bumped_client;

    let pool = compute_pool_pda(amm_pid, def_a, def_b, fees);
    let vault_a = compute_vault_pda(amm_pid, pool, def_a);
    let vault_b = compute_vault_pda(amm_pid, pool, def_b);
    Ok(PrepP {
        wallet,
        pool,
        vault_a,
        vault_b,
    })
}

/// Returns `true` when the caller passed the token pair in the REVERSE
/// of the pool's canonical leg order — i.e. the pool's stored
/// `definition_token_a_id` is `def_b_passed`, not `def_a_passed`.
///
/// The pool PDA is order-INDEPENDENT (`compute_pool_pda_seed` sorts the
/// two ids), so a pool can be looked up with the defs in either order.
/// The vault PDAs and the amm/amm_v2 handlers, however, bind *position*
/// A to the pool's stored `definition_token_a_id`: every handler asserts
/// `vault_a.account_id == pool_def.vault_a_id` and pairs `vault_a` with
/// `user_holding_a` (and, for liquidity, with `*_token_a`). So whenever
/// this returns `true` the caller must flip its
/// `(vault_a, holding_a, amount_a)` / `(vault_b, holding_b, amount_b)`
/// tuples before building the account list, or the guest panics on the
/// vault-id assertion. Reads the pool's `PoolDefinition` from chain.
async fn pool_needs_leg_flip(
    wallet: &WalletCore,
    pool: AccountId,
    def_a_passed: AccountId,
) -> Result<bool, i32> {
    let acc = wallet
        .get_account_public(pool)
        .await
        .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
    let pool_def = PoolDefinition::try_from(&acc.data)
        .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
    Ok(pool_def.definition_token_a_id != def_a_passed)
}

/// Align `prep_private`'s `vault_a`/`vault_b` with the pool's actual
/// `definition_token_a_id` ordering, fetching the pool from chain.
///
/// `prep_private` derives `vault_a = compute_vault_pda(pool, def_a_passed)`
/// — which only matches the pool's `vault_a_id` if the caller happens
/// to pass the defs in the same order they were used at pool-create
/// time. For swap paths against an existing pool (notably the WLEZ-
/// paired ones, where the FFI's `(wlez_def, token_def_out)` order
/// isn't the order the pool was created with), this silently swaps
/// vault_a/b. amm_v2 then asserts `vault_a.account_id ==
/// pool_def.vault_a_id` and panics inside the proof guest.
///
/// This helper swaps the prepped `vault_a`/`vault_b` if the pool's
/// canonical leg order is reversed relative to `def_a_passed`. Safe
/// no-op when the ordering already matches. NOTE: this only re-orders
/// the vaults — paths that also pass user holdings / per-side amounts
/// positionally (token↔token swap & liquidity) must flip those too; see
/// `pool_needs_leg_flip`. It is correct on its own only for the WLEZ
/// disposable paths, whose handlers pick the in/out vault by definition
/// and carry the user holdings as definition-tagged (not position-tagged)
/// accounts.
async fn align_prep_to_pool(p: &mut PrepP, def_a_passed: AccountId) -> Result<(), i32> {
    if pool_needs_leg_flip(&p.wallet, p.pool, def_a_passed).await? {
        // Pool's def_a is the OTHER one. `vault_a` was derived from
        // `def_a_passed` and `vault_b` from `def_b_passed`, so swapping
        // them is exactly `vault_a = vault(pool.token_a)`,
        // `vault_b = vault(pool.token_b)`.
        std::mem::swap(&mut p.vault_a, &mut p.vault_b);
    }
    Ok(())
}

/// Build → sign → submit a public AMM tx; return the 32-byte tx hash.
async fn finalize<I: serde::Serialize>(
    wallet: &WalletCore,
    amm_pid: nssa_core::program::ProgramId,
    account_ids: Vec<AccountId>,
    signers: &[AccountId],
    instruction: I,
) -> Result<[u8; 32], i32> {
    let nonces = wallet
        .get_accounts_nonces(signers.to_vec())
        .await
        .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
    let mut keys = Vec::with_capacity(signers.len());
    for s in signers {
        keys.push(
            wallet
                .storage()
                .user_data
                .get_pub_account_signing_key(*s)
                .ok_or(LDEX_AMM_ERR_KEY)?,
        );
    }
    let message =
        nssa::public_transaction::Message::try_new(amm_pid, account_ids, nonces, instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
    let witness_set = nssa::public_transaction::WitnessSet::for_message(&message, &keys);
    let tx = nssa::PublicTransaction::new(message, witness_set);
    let hash = wallet
        .sequencer_client
        .send_transaction(NSSATransaction::Public(tx))
        .await
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
    // Block until the tx is observably included in a block. Without this,
    // FFIs returned "success" the moment the mempool accepted the submit,
    // and any sequencer-side rejection (proof/dev-mode mismatch, conflict,
    // insufficient gas, etc.) looked indistinguishable from success to
    // the UI. With poll, an inclusion timeout or sequencer reject surfaces
    // as LDEX_AMM_ERR_SUBMIT — the UI's busy spinner clears only after
    // the chain has actually accepted the tx.
    wallet
        .poll_native_token_transfer(hash)
        .await
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
    let mut out = [0u8; 32];
    let h: &[u8] = hash.as_ref();
    if h.len() == 32 {
        out.copy_from_slice(h);
    }
    Ok(out)
}

// Shared multi-thread tokio runtime.
//
// Previously this function built a fresh `new_multi_thread().enable_all()`
// runtime PER FFI CALL — spawning worker threads + IO driver + timer
// driver, doing one piece of work, then tearing all of it down. The LDEX
// CLI never noticed because it makes one call per process, but the
// mini-app plugin makes many calls per UI refresh, and the cost (plus
// possible cross-runtime deadlocks when the Logos host's Qt event loop
// has its own pool) drove every callModule past the QtRO bridge's 20 s
// timeout. UI saw "Failed to invoke callRemoteMethod" / "invalid response".
//
// Sharing one process-wide runtime is the standard pattern for an
// FFI cdylib that's called many times in-process.
fn runtime() -> Result<&'static tokio::runtime::Runtime, i32> {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    Ok(RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime build failed (LDEX FFI init)")
    }))
}

fn hex32s(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

unsafe fn write_json(out: *mut u8, cap: usize, s: &str) -> i32 {
    let b = s.as_bytes();
    if out.is_null() || b.len() + 1 > cap {
        return LDEX_AMM_ERR_SUBMIT;
    }
    std::ptr::copy_nonoverlapping(b.as_ptr(), out, b.len());
    *out.add(b.len()) = 0;
    LDEX_AMM_OK
}

/// Read a pool's on-chain state. Writes JSON to `out`:
/// `{"exists":bool,"reserve_a":"..","reserve_b":"..","lp_supply":"..","fees":N,
/// "cum_volume_a":"..","cum_volume_b":"..","cum_fees_a":"..","cum_fees_b":".."}`.
/// `cum_volume_*` and `cum_fees_*` are EXACT lifetime on-chain accumulators
/// maintained by the AMM swap path (RFP Usability #3).
///
/// # Safety
/// `*_path` NUL-terminated; id ptrs are 32 bytes; `out` has `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_pool_info(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    token_a_def: *const u8,
    token_b_def: *const u8,
    fees: u128,
    out: *mut u8,
    cap: usize,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(a), Some(b)) = (
        read_id(amm_program_id),
        read_id(token_a_def),
        read_id(token_b_def),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let amm_pid = program_id_from_bytes(pid);
    let pool = compute_pool_pda(amm_pid, AccountId::new(a), AccountId::new(b), fees);
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let json = rt.block_on(async move {
        let w = match WalletCore::new_update_chain(PathBuf::from(&cfg), PathBuf::from(&store), None)
        {
            Ok(w) => w,
            Err(_) => return Err(LDEX_AMM_ERR_WALLET),
        };
        match w.get_account_public(pool).await {
            Ok(acc) => match PoolDefinition::try_from(&acc.data) {
                Ok(p) if p.liquidity_pool_supply > 0 => Ok(format!(
                    "{{\"exists\":true,\"reserve_a\":\"{}\",\"reserve_b\":\"{}\",\
                     \"lp_supply\":\"{}\",\"fees\":{},\
                     \"cum_volume_a\":\"{}\",\"cum_volume_b\":\"{}\",\
                     \"cum_fees_a\":\"{}\",\"cum_fees_b\":\"{}\",\
                     \"token_a_def\":\"{}\",\"token_b_def\":\"{}\"}}",
                    p.reserve_a, p.reserve_b, p.liquidity_pool_supply, p.fees,
                    p.cum_volume_a, p.cum_volume_b, p.cum_fees_a, p.cum_fees_b,
                    hex32s(p.definition_token_a_id.value()),
                    hex32s(p.definition_token_b_id.value())
                )),
                _ => Ok("{\"exists\":false}".to_string()),
            },
            Err(_) => Ok("{\"exists\":false}".to_string()),
        }
    });
    match json {
        Ok(s) => write_json(out, cap, &s),
        Err(e) => e,
    }
}

/// **On-chain** price history (design.md §5.11③). Reads the pool
/// account's `PoolDefinition.obs` ring directly from chain and derives
/// gapless per-interval TWAP price points — no off-chain indexer, no
/// observer that can miss trades (every swap/liquidity tx pushed an
/// observation by construction). Writes JSON
/// `[{"t":unix_ms,"p":price_b_per_a}, ...]` (oldest→newest).
///
/// # Safety
/// `*_path` NUL-terminated; id ptrs 32 bytes; `out` has `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_onchain_price_history(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    token_a_def: *const u8,
    token_b_def: *const u8,
    fees: u128,
    out: *mut u8,
    cap: usize,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(a), Some(b)) = (
        read_id(amm_program_id),
        read_id(token_a_def),
        read_id(token_b_def),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let amm_pid = program_id_from_bytes(pid);
    let pool = compute_pool_pda(amm_pid, AccountId::new(a), AccountId::new(b), fees);
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let json = rt.block_on(async move {
        let w = match WalletCore::new_update_chain(PathBuf::from(&cfg), PathBuf::from(&store), None)
        {
            Ok(w) => w,
            Err(_) => return Err(LDEX_AMM_ERR_WALLET),
        };
        let acc = match w.get_account_public(pool).await {
            Ok(acc) => acc,
            Err(_) => return Ok("[]".to_string()),
        };
        let pd = match PoolDefinition::try_from(&acc.data) {
            Ok(p) => p,
            Err(_) => return Ok("[]".to_string()),
        };
        // Per-interval TWAP of price(A in B), Q64.64 → f64. Gapless: each
        // observation was pushed by an on-chain mutating tx.
        let mut s = String::from("[");
        let mut first = true;
        for w2 in pd.obs.windows(2) {
            let (o0, o1) = (&w2[0], &w2[1]);
            let dt = o1.ts.saturating_sub(o0.ts);
            if dt == 0 {
                continue;
            }
            let dcum = o1.cum_a.wrapping_sub(o0.cum_a);
            let twap_q64 = dcum / (dt as u128);
            // Q64.64 → f64 (price B per A).
            let price = (twap_q64 as f64) / (2f64.powi(64));
            if !first {
                s.push(',');
            }
            first = false;
            s.push_str(&format!("{{\"t\":{},\"p\":{:.10}}}", o1.ts, price));
        }
        s.push(']');
        Ok(s)
    });
    match json {
        Ok(s) => write_json(out, cap, &s),
        Err(e) => e,
    }
}

/// Read a token holding balance. Writes `{"balance":"..","definition":"<hex>"}`.
///
/// # Safety
/// `*_path` NUL-terminated; `account_id` is 32 bytes; `out` has `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_token_balance(
    config_path: *const c_char,
    storage_path: *const c_char,
    account_id: *const u8,
    out: *mut u8,
    cap: usize,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let Some(acct) = read_id(account_id) else {
        return LDEX_AMM_ERR_NULL;
    };
    let id = AccountId::new(acct);
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let json = rt.block_on(async move {
        let w = match WalletCore::new_update_chain(PathBuf::from(&cfg), PathBuf::from(&store), None)
        {
            Ok(w) => w,
            Err(_) => return Err(LDEX_AMM_ERR_WALLET),
        };
        match w.get_account_public(id).await {
            Ok(acc) => match TokenHolding::try_from(&acc.data) {
                Ok(TokenHolding::Fungible { definition_id, balance }) => Ok(format!(
                    "{{\"balance\":\"{}\",\"definition\":\"{}\"}}",
                    balance,
                    hex32s(definition_id.value())
                )),
                _ => Ok("{\"balance\":\"0\"}".to_string()),
            },
            Err(_) => Ok("{\"balance\":\"0\"}".to_string()),
        }
    });
    match json {
        Ok(s) => write_json(out, cap, &s),
        Err(e) => e,
    }
}

/// Read the persisted on-chain price history for a pool (design.md
/// §5.11 layer ②). Pure filesystem read of the `price_indexer` daemon's
/// CSV — no chain call, non-blocking, safe to poll from the chart.
/// Path is derived identically to the daemon
/// (`${LDEX_PRICE_DIR:-$HOME/.ldex/price}/<amm8>_<a8>_<b8>_<fee>.csv`).
/// Writes JSON `[{"b":block,"t":unix_ms,"p":price_b_per_a}, ...]`
/// (oldest→newest, at most `max_points`). Empty array if no history yet.
///
/// # Safety
/// id ptrs are 32 bytes; `out` has `cap` writable bytes.
/// Read the off-chain reserve-feed CSV for (amm, a, b, fees) → parsed rows
/// `(block_id, ms, reserve_a, reserve_b)`; empty if the file is absent/unreadable.
/// Callers apply their own filtering/derivation. Shared by `ldex_amm_price_history`
/// and `ldex_amm_volume_estimate` (was duplicated verbatim in both).
fn read_reserve_feed(amm: &[u8], a: &[u8], b: &[u8], fees: u128) -> Vec<(u64, u128, f64, f64)> {
    let h4 = |x: &[u8]| x[..4].iter().map(|y| format!("{y:02x}")).collect::<String>();
    let dir = std::env::var("LDEX_PRICE_DIR").unwrap_or_else(|_| {
        format!("{}/.ldex/price", std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
    });
    let path = format!("{}/{}_{}_{}_{}.csv", dir, h4(amm), h4(a), h4(b), fees);
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split(',');
            let bid: u64 = it.next()?.parse().ok()?;
            let ms: u128 = it.next()?.parse().ok()?;
            let ra: f64 = it.next()?.parse().ok()?;
            let rb: f64 = it.next()?.parse().ok()?;
            Some((bid, ms, ra, rb))
        })
        .collect()
}

#[no_mangle]
pub unsafe extern "C" fn ldex_amm_price_history(
    amm_program_id: *const u8,
    token_a_def: *const u8,
    token_b_def: *const u8,
    fees: u128,
    max_points: u32,
    out: *mut u8,
    cap: usize,
) -> i32 {
    let (Some(amm), Some(a), Some(b)) = (
        read_id(amm_program_id),
        read_id(token_a_def),
        read_id(token_b_def),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let mut rows: Vec<(u64, u128, f64)> = read_reserve_feed(&amm, &a, &b, fees)
        .into_iter()
        .filter_map(|(bid, ms, ra, rb)| (ra != 0.0).then_some((bid, ms, rb / ra)))
        .collect();
    let n = max_points.max(1) as usize;
    if rows.len() > n {
        rows.drain(0..rows.len() - n);
    }
    let mut s = String::from("[");
    for (i, (bid, ms, p)) in rows.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{{\"b\":{bid},\"t\":{ms},\"p\":{p:.8}}}"));
    }
    s.push(']');
    write_json(out, cap, &s)
}

/// Pool analytics estimate (RFP Usability #3) — **aggregate-only**, no
/// individual positions. Derived from the on-chain-sourced reserve feed
/// (`block_id,unix_ms,reserve_a,reserve_b,lp_supply`):
///
/// * `tvlA`/`tvlB`     — latest on-chain reserves (exact: pool TVL legs).
/// * `volA`/`volB`     — Σ|Δreserve| across samples ≈ cumulative throughput
///                       (approximate: a swap moves both legs; this is a
///                       reserve-delta proxy, not an on-chain volume
///                       accumulator — labelled as approximate in the UI).
/// * `feeRevA`/`feeRevB` — `vol · fees/10000` ≈ LP fee revenue (approx).
/// * `samples`         — number of feed rows used.
///
/// Returns JSON `{"tvlA":..,"tvlB":..,"volA":..,"volB":..,"feeRevA":..,
/// "feeRevB":..,"samples":N,"feeBps":F}`. Reads no per-account state.
///
/// # Safety
/// `*_id` args = 32 readable bytes; `out` = `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_volume_estimate(
    amm_program_id: *const u8,
    token_a_def: *const u8,
    token_b_def: *const u8,
    fees: u128,
    out: *mut u8,
    cap: usize,
) -> i32 {
    let (Some(amm), Some(a), Some(b)) = (
        read_id(amm_program_id),
        read_id(token_a_def),
        read_id(token_b_def),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let res: Vec<(f64, f64)> = read_reserve_feed(&amm, &a, &b, fees)
        .into_iter()
        .map(|(_bid, _ms, ra, rb)| (ra, rb))
        .collect();
    let (mut va, mut vb) = (0f64, 0f64);
    for w in res.windows(2) {
        va += (w[1].0 - w[0].0).abs();
        vb += (w[1].1 - w[0].1).abs();
    }
    let (tvl_a, tvl_b) = res.last().copied().unwrap_or((0.0, 0.0));
    let fr = fees as f64 / 10_000.0;
    let s = format!(
        "{{\"tvlA\":{tvl_a:.4},\"tvlB\":{tvl_b:.4},\"volA\":{va:.4},\
         \"volB\":{vb:.4},\"feeRevA\":{:.4},\"feeRevB\":{:.4},\
         \"samples\":{},\"feeBps\":{}}}",
        va * fr,
        vb * fr,
        res.len(),
        fees
    );
    write_json(out, cap, &s)
}

unsafe fn out32(result: Result<[u8; 32], i32>, out: *mut u8) -> i32 {
    match result {
        Ok(h) => {
            std::ptr::copy_nonoverlapping(h.as_ptr(), out, 32);
            LDEX_AMM_OK
        }
        Err(code) => code,
    }
}

macro_rules! ids4 {
    ($pid:ident,$a:ident,$b:ident,$c:ident,$d:ident) => {{
        let (Some(p), Some(a), Some(b), Some(c)) =
            (read_id($pid), read_id($a), read_id($b), read_id($c))
        else {
            return LDEX_AMM_ERR_NULL;
        };
        let d = read_id($d);
        (p, a, b, c, d)
    }};
}

/// `NewDefinition` — create a fee-tier pool.
/// Accounts: pool, vault_a, vault_b, lp_def, lp_lock, user_a, user_b, user_lp.
/// Signers: user_a, user_b (+ user_lp when its key is known).
///
/// # Safety
/// Strings NUL-terminated UTF-8; `*_id` args = 32 readable bytes;
/// `out_tx_hash` = 32 writable bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_new_pool(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    amount_a: u128,
    amount_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, lp_b, _) = ids4!(
        amm_program_id,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        user_holding_lp
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(lp_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        let lp_lock = compute_lp_lock_holding_pda(amm_pid, p.pool);
        let account_ids = vec![
            p.pool, p.vault_a, p.vault_b, lp_def, lp_lock, uha, uhb, uhlp, CLOCK_01,
        ];
        // user_lp signs only if the wallet holds its key (fresh LP holding
        // must be user-authorized; otherwise a+b suffice).
        let mut signers = vec![uha, uhb];
        if p.wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(uhlp)
            .is_some()
        {
            signers.push(uhlp);
        }
        let instruction = Instruction::NewDefinition {
            token_a_amount: amount_a,
            token_b_amount: amount_b,
            fees,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &signers, instruction).await
    });
    out32(res, out_tx_hash)
}

/// `NewDefinitionAta` — create a v1 fee-tier pool that PINS the deployed
/// ATA program id, making the pool's ATA-routed ops
/// (`ldex_amm_swap_exact_in_ata` / `_out_ata` / `_add_liquidity_ata`)
/// reachable. Without this, a pool created via `ldex_amm_new_pool` pins
/// `ProgramId::default()` (zero) and every v1 ATA op fails the
/// `ata_program_id == pinned id` assertion. Mirrors `ldex_amm_new_pool`
/// exactly (same keypair-holding deposit/LP-lock/LP-mint legs and account
/// order) plus the pinned `ata_program_id`, derived — like the other F8
/// ATA FFIs — from the `LDEX_ATA_PROGRAM_ID` env var. The amm_v2 analogue
/// is `ldex_amm_v2_new_pool_ata`.
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_new_pool_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    amount_a: u128,
    amount_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, lp_b, _) = ids4!(
        amm_program_id,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        user_holding_lp
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(lp_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        // Only the pinned ATA program id is needed (the user side still
        // uses keypair holdings, exactly like NewDefinition); the derived
        // ATAs are unused. Same env-var source as `ata_env_ctx` callers.
        let (ata_pid, _, _) = ata_env_ctx(uha, uha, uha)?;
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        let lp_lock = compute_lp_lock_holding_pda(amm_pid, p.pool);
        let account_ids = vec![
            p.pool, p.vault_a, p.vault_b, lp_def, lp_lock, uha, uhb, uhlp, CLOCK_01,
        ];
        let mut signers = vec![uha, uhb];
        if p.wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(uhlp)
            .is_some()
        {
            signers.push(uhlp);
        }
        let instruction = Instruction::NewDefinitionAta {
            token_a_amount: amount_a,
            token_b_amount: amount_b,
            fees,
            ata_program_id: ata_pid,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &signers, instruction).await
    });
    out32(res, out_tx_hash)
}

/// `SwapExactInput`. Accounts: pool, vault_a, vault_b, user_a, user_b.
/// Signer: the user holding whose token definition == `token_definition_in`.
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_swap_exact_in(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, in_b, _) = ids4!(
        amm_program_id,
        user_holding_a,
        user_holding_b,
        token_definition_in,
        token_definition_in
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, tok_in) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(in_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let signer = if p.def_a == tok_in {
            uha
        } else if p.def_b == tok_in {
            uhb
        } else {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        };
        let account_ids = vec![p.pool, p.vault_a, p.vault_b, uha, uhb, CLOCK_01];
        let instruction = Instruction::SwapExactInput {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: tok_in,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[signer], instruction).await
    });
    out32(res, out_tx_hash)
}

/// `SwapExactInputAta` — RFP Func #8 swap with the user side using
/// Associated Token Accounts. Owner authorises the spend (signer); the
/// ATA program internally PDA-authorises the sender ATA.
/// Accounts: `[pool, vault_a, vault_b, owner, ata_a, ata_b, CLOCK_01]`.
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_swap_exact_in_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    owner: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(da), Some(db), Some(tin)) = (
        read_id(amm_program_id),
        read_id(owner),
        read_id(token_def_a),
        read_id(token_def_b),
        read_id(token_definition_in),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let def_a = AccountId::new(da);
    let def_b = AccountId::new(db);
    let tok_in = AccountId::new(tin);
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        // Need an ATA program id to derive ata_a / ata_b. The deployed
        // ATA program's id is fetched via the env var (set by
        // bootstrap.sh: LDEX_ATA_PROGRAM_ID).
        let ata_pid_hex = std::env::var("LDEX_ATA_PROGRAM_ID")
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        let mut ata_pid_bytes = [0u8; 32];
        if ata_pid_hex.len() != 64 {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        }
        for i in 0..32 {
            ata_pid_bytes[i] = u8::from_str_radix(&ata_pid_hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        }
        let ata_pid = program_id_from_bytes(ata_pid_bytes);
        let ata_a_id = ata_core::get_associated_token_account_id(
            &ata_pid, &ata_core::compute_ata_seed(owner_id, def_a));
        let ata_b_id = ata_core::get_associated_token_account_id(
            &ata_pid, &ata_core::compute_ata_seed(owner_id, def_b));

        let p = prep(&cfg, &store, amm_pid, ata_a_id, ata_b_id, fees).await?;
        let _ = def_b;
        // Align the (vault, ata) legs to the pool's canonical token-A: the
        // handler asserts vault_a == pool.vault_a_id and pairs vault_a with
        // ata_a, so if the caller passed the pair reversed, flip both
        // together (token_definition_id_in disambiguates the direction).
        let (vault_a, vault_b, ata_a_id, ata_b_id) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, ata_b_id, ata_a_id)
            } else {
                (p.vault_a, p.vault_b, ata_a_id, ata_b_id)
            };
        let account_ids = vec![
            p.pool, vault_a, vault_b, owner_id, ata_a_id, ata_b_id, CLOCK_01,
        ];
        let instruction = Instruction::SwapExactInputAta {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: tok_in,
            ata_program_id: ata_pid,
            deadline,
        };
        // Only the owner signs (authorises the ATA spend); the ATA program
        // takes it from there via the deterministic PDA seed.
        finalize(&p.wallet, amm_pid, account_ids, &[owner_id], instruction).await
    });
    out32(res, out_tx_hash)
}

/// Helper for the F8 ATA-side FFIs: read `LDEX_ATA_PROGRAM_ID` from
/// process env, derive both ATAs deterministically, and return everything
/// callers need to build the privacy/public tx.
unsafe fn ata_env_ctx(
    owner_id: AccountId, def_a: AccountId, def_b: AccountId,
) -> Result<(nssa_core::program::ProgramId, AccountId, AccountId), i32> {
    let ata_pid_hex = std::env::var("LDEX_ATA_PROGRAM_ID")
        .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
    if ata_pid_hex.len() != 64 {
        return Err(LDEX_AMM_ERR_ACCOUNT);
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&ata_pid_hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
    }
    let ata_pid = program_id_from_bytes(bytes);
    let ata_a = ata_core::get_associated_token_account_id(
        &ata_pid, &ata_core::compute_ata_seed(owner_id, def_a));
    let ata_b = ata_core::get_associated_token_account_id(
        &ata_pid, &ata_core::compute_ata_seed(owner_id, def_b));
    Ok((ata_pid, ata_a, ata_b))
}

/// `SwapExactOutputAta` — RFP Func #8 swap-out via ATAs.
/// Accounts: `[pool, vault_a, vault_b, owner, ata_a, ata_b, CLOCK_01]`.
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_swap_exact_out_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    owner: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    exact_amount_out: u128,
    max_amount_in: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(da), Some(db), Some(tin)) = (
        read_id(amm_program_id), read_id(owner),
        read_id(token_def_a), read_id(token_def_b), read_id(token_definition_in),
    ) else { return LDEX_AMM_ERR_NULL };
    if out_tx_hash.is_null() { return LDEX_AMM_ERR_NULL }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let def_a = AccountId::new(da);
    let def_b = AccountId::new(db);
    let tok_in = AccountId::new(tin);
    let rt = match runtime() { Ok(r) => r, Err(e) => return e };
    let res = rt.block_on(async move {
        let (ata_pid, ata_a_id, ata_b_id) = ata_env_ctx(owner_id, def_a, def_b)?;
        let p = prep(&cfg, &store, amm_pid, ata_a_id, ata_b_id, fees).await?;
        // Align (vault, ata) legs to the pool's canonical token-A; see
        // ldex_amm_swap_exact_in_ata.
        let (vault_a, vault_b, ata_a_id, ata_b_id) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, ata_b_id, ata_a_id)
            } else {
                (p.vault_a, p.vault_b, ata_a_id, ata_b_id)
            };
        let account_ids = vec![p.pool, vault_a, vault_b, owner_id, ata_a_id, ata_b_id, CLOCK_01];
        let instruction = Instruction::SwapExactOutputAta {
            exact_amount_out, max_amount_in,
            token_definition_id_in: tok_in,
            ata_program_id: ata_pid, deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[owner_id], instruction).await
    });
    out32(res, out_tx_hash)
}

/// `AddLiquidityAta` — RFP Func #8 add-liquidity via ATAs.
/// Accounts: `[pool, vault_a, vault_b, lp_def, owner, ata_a, ata_b, ata_lp, CLOCK_01]`.
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_add_liquidity_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    owner: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    min_amount_liquidity: u128,
    max_amount_to_add_token_a: u128,
    max_amount_to_add_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(da), Some(db)) = (
        read_id(amm_program_id), read_id(owner),
        read_id(token_def_a), read_id(token_def_b),
    ) else { return LDEX_AMM_ERR_NULL };
    if out_tx_hash.is_null() { return LDEX_AMM_ERR_NULL }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let def_a = AccountId::new(da);
    let def_b = AccountId::new(db);
    let rt = match runtime() { Ok(r) => r, Err(e) => return e };
    let res = rt.block_on(async move {
        let (ata_pid, ata_a_id, ata_b_id) = ata_env_ctx(owner_id, def_a, def_b)?;
        let p = prep(&cfg, &store, amm_pid, ata_a_id, ata_b_id, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        // ATA_LP derivation: same scheme but with lp_def as the "definition".
        let ata_lp_id = ata_core::get_associated_token_account_id(
            &ata_pid, &ata_core::compute_ata_seed(owner_id, lp_def));
        // Align (vault, ata, max-amount) legs to the pool's canonical
        // token-A: add_liquidity_ata keys vault_a/ata_a/max_a all to
        // reserve_a, so a reversed-order call must flip all three (ata_lp
        // is the LP leg, unaffected).
        let (vault_a, vault_b, ata_a_id, ata_b_id, max_a, max_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, ata_b_id, ata_a_id,
                 max_amount_to_add_token_b, max_amount_to_add_token_a)
            } else {
                (p.vault_a, p.vault_b, ata_a_id, ata_b_id,
                 max_amount_to_add_token_a, max_amount_to_add_token_b)
            };
        let account_ids = vec![
            p.pool, vault_a, vault_b, lp_def, owner_id,
            ata_a_id, ata_b_id, ata_lp_id, CLOCK_01,
        ];
        let instruction = Instruction::AddLiquidityAta {
            min_amount_liquidity, max_amount_to_add_token_a: max_a,
            max_amount_to_add_token_b: max_b, ata_program_id: ata_pid, deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[owner_id], instruction).await
    });
    out32(res, out_tx_hash)
}

/// `RemoveLiquidityAta` — RFP Func #8 remove-liquidity via ATAs.
/// No new AMM instruction needed: the existing `RemoveLiquidity` already
/// works with ATAs because the vault → user transfers are vault-PDA-
/// authorised and the LP burn is `lp_def`-PDA-authorised — no user-side
/// signing is required by the AMM logic. Only the FFI changes: signer is
/// the **owner** (which provides the outer-tx nonce), and the user-holding
/// account ids are the deterministic ATAs derived from (owner, definition).
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_remove_liquidity_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    owner: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    remove_liquidity_amount: u128,
    min_amount_to_remove_token_a: u128,
    min_amount_to_remove_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(da), Some(db)) = (
        read_id(amm_program_id), read_id(owner),
        read_id(token_def_a), read_id(token_def_b),
    ) else { return LDEX_AMM_ERR_NULL };
    if out_tx_hash.is_null() { return LDEX_AMM_ERR_NULL }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let def_a = AccountId::new(da);
    let def_b = AccountId::new(db);
    let rt = match runtime() { Ok(r) => r, Err(e) => return e };
    let res = rt.block_on(async move {
        let (ata_pid, ata_a_id, ata_b_id) = ata_env_ctx(owner_id, def_a, def_b)?;
        let p = prep(&cfg, &store, amm_pid, ata_a_id, ata_b_id, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        let ata_lp_id = ata_core::get_associated_token_account_id(
            &ata_pid, &ata_core::compute_ata_seed(owner_id, lp_def));
        // Align (vault, ata, min-amount) legs to the pool's canonical
        // token-A: remove keys vault_a/user_holding_a(ata_a)/min_a all to
        // reserve_a, so a reversed-order call must flip all three (ata_lp
        // is the LP leg, unaffected).
        let (vault_a, vault_b, ata_a_id, ata_b_id, min_a, min_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, ata_b_id, ata_a_id,
                 min_amount_to_remove_token_b, min_amount_to_remove_token_a)
            } else {
                (p.vault_a, p.vault_b, ata_a_id, ata_b_id,
                 min_amount_to_remove_token_a, min_amount_to_remove_token_b)
            };
        let account_ids = vec![
            p.pool, vault_a, vault_b, lp_def,
            ata_a_id, ata_b_id, ata_lp_id, CLOCK_01,
        ];
        let instruction = Instruction::RemoveLiquidity {
            remove_liquidity_amount,
            min_amount_to_remove_token_a: min_a,
            min_amount_to_remove_token_b: min_b,
            deadline,
        };
        // Owner signs (provides nonce); the AMM's RemoveLiquidity path
        // does not require user-holding authorisation, so no ATA signer
        // is needed.
        finalize(&p.wallet, amm_pid, account_ids, &[owner_id], instruction).await
    });
    out32(res, out_tx_hash)
}

/// `SwapExactOutput`. Same accounts/signer rule as swap-in.
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_swap_exact_out(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    token_definition_in: *const u8,
    exact_amount_out: u128,
    max_amount_in: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, in_b, _) = ids4!(
        amm_program_id,
        user_holding_a,
        user_holding_b,
        token_definition_in,
        token_definition_in
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, tok_in) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(in_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let signer = if p.def_a == tok_in {
            uha
        } else if p.def_b == tok_in {
            uhb
        } else {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        };
        let account_ids = vec![p.pool, p.vault_a, p.vault_b, uha, uhb, CLOCK_01];
        let instruction = Instruction::SwapExactOutput {
            exact_amount_out,
            max_amount_in,
            token_definition_id_in: tok_in,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[signer], instruction).await
    });
    out32(res, out_tx_hash)
}

/// `AddLiquidity`. Accounts: pool, vault_a, vault_b, lp_def, user_a, user_b,
/// user_lp. Signers: user_a, user_b.
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_add_liquidity(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    min_amount_liquidity: u128,
    max_amount_to_add_token_a: u128,
    max_amount_to_add_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, lp_b, _) = ids4!(
        amm_program_id,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        user_holding_lp
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(lp_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        let account_ids = vec![p.pool, p.vault_a, p.vault_b, lp_def, uha, uhb, uhlp, CLOCK_01];
        let instruction = Instruction::AddLiquidity {
            min_amount_liquidity,
            max_amount_to_add_token_a,
            max_amount_to_add_token_b,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[uha, uhb], instruction).await
    });
    out32(res, out_tx_hash)
}

/// `RemoveLiquidity`. Accounts: pool, vault_a, vault_b, lp_def, user_a,
/// user_b, user_lp. Signer: user_lp (authorizes the LP burn).
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_remove_liquidity(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    remove_liquidity_amount: u128,
    min_amount_to_remove_token_a: u128,
    min_amount_to_remove_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, lp_b, _) = ids4!(
        amm_program_id,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        user_holding_lp
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(lp_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        let account_ids = vec![p.pool, p.vault_a, p.vault_b, lp_def, uha, uhb, uhlp, CLOCK_01];
        let instruction = Instruction::RemoveLiquidity {
            remove_liquidity_amount,
            min_amount_to_remove_token_a,
            min_amount_to_remove_token_b,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[uhlp], instruction).await
    });
    out32(res, out_tx_hash)
}

/// Resolve a deployed guest ELF. Lookup order:
///   1. env var `env_key` (explicit override)
///   2. `$LDEX_REPO/programs/target/riscv-guest/<rel>`
///   3. `<CARGO_MANIFEST_DIR>/../../programs/target/riscv-guest/<rel>`
///      — works when the FFI was built in-tree, since CARGO_MANIFEST_DIR
///      is baked at compile time and points at `ffi/ldex-amm-ffi`
/// No `$HOME/Documents/...` default — the LDEX repo is public, so the
/// binary must not assume a particular user's directory layout.
/// The on-disk `.bin` is the exact artifact `bootstrap.sh` inscribed,
/// so `Program::new(bytes).id()` is deterministic and equals the
/// bootstrapped program id.
fn load_deployed_program(env_key: &str, rel: &str) -> Result<Program, i32> {
    let path = if let Ok(p) = std::env::var(env_key) {
        if !p.trim().is_empty() {
            PathBuf::from(p)
        } else {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        }
    } else if let Ok(repo) = std::env::var("LDEX_REPO") {
        PathBuf::from(repo)
            .join("programs/target/riscv-guest")
            .join(rel)
    } else {
        // CARGO_MANIFEST_DIR is the FFI crate's source dir (ffi/ldex-amm-ffi),
        // baked at compile time. Walk two up to the repo root.
        let baked: &str = concat!(env!("CARGO_MANIFEST_DIR"),
                                   "/../../programs/target/riscv-guest");
        PathBuf::from(baked).join(rel)
    };
    let bytes = std::fs::read(&path).map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
    Program::new(bytes).map_err(|_| LDEX_AMM_ERR_ACCOUNT)
}

/// **Private** `SwapExactInput` — RFP-goal-conformant privacy path
/// (design.md §5.2/§5.10, "Private" mode). One `send_privacy_preserving_tx`
/// invoking the **existing deployed AMM** with the user's two token
/// holdings as `PrivateOwned`: the privacy circuit deshields them into the
/// program's view, the AMM swap runs (its chained token transfers hit the
/// public vaults), and the post-states are re-shielded — **no public
/// originating address ever exists on-chain**. Re-shield is structural
/// (both holdings `PrivateOwned`); the shielded input balance is checked
/// pre-submission. No native/gas leg: privacy txs are feeless on rc3.
///
/// Accounts (order = AMM `swap_exact_input` guest signature):
/// `[Public(pool), Public(vault_a), Public(vault_b),
///   PrivateOwned(user_a), PrivateOwned(user_b)]`.
///
/// Proving runs in-process (risc0). Dev: `RISC0_DEV_MODE=1`. Real proofs
/// need `LOGOS_BLOCKCHAIN_CIRCUITS` + the risc0 toolchain (~270 s).
///
/// # Safety
/// As `ldex_amm_swap_exact_in`.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_private_swap_exact_in(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(a_b), Some(b_b)) = (
        read_id(amm_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(da_b), Some(db_b), Some(in_b)) = (
        read_id(token_def_a),
        read_id(token_def_b),
        read_id(token_definition_in),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, tok_in) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(in_b));
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    // Load the exact deployed AMM + token guest ELFs (in-circuit execution
    // needs bytecode, not just ids). AMM id must match the caller's
    // program id; token is declared as the AMM's chained-call dependency.
    let amm_prog = match load_deployed_program(
        "LDEX_AMM_ELF",
        "amm-methods/amm-guest/riscv32im-risc0-zkvm-elf/release/amm.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT; // deployed ELF != requested AMM program
    }
    // The token program in play is nssa's BUILT-IN `Program::token()` —
    // the wallet's `token new/send` (and the AMM's chained transfers)
    // use it, so all holdings/vaults are owned by it. (Our separately
    // deployed token.bin has a different image id and is unused here.)
    let token_prog = Program::token();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(token_prog.id(), token_prog);
    let program = ProgramWithDependencies::new(amm_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_pid, def_a, def_b, fees).await?;
        // Align (vault, holding) legs to the pool's canonical token-A:
        // swap_exact_input_circuit asserts vault_a == pool.vault_a_id and
        // pairs vault_a (idx1) with user_holding_a (idx3), so if the caller
        // passed the pair reversed, flip both vaults and holdings together
        // (token_definition_id_in disambiguates the direction).
        let flip = pool_needs_leg_flip(&p.wallet, p.pool, def_a).await?;
        let (vault_a, vault_b, h_a, h_b, slot_a_def, slot_b_def) = if flip {
            (p.vault_b, p.vault_a, uhb, uha, def_b, def_a)
        } else {
            (p.vault_a, p.vault_b, uha, uhb, def_a, def_b)
        };
        // pre_states order = accounts order below; input holding is h_a
        // (idx 3) when the input token is the slot-A def, else h_b (idx 4).
        let input_idx = if slot_a_def == tok_in {
            3usize
        } else if slot_b_def == tok_in {
            4usize
        } else {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        };

        let accounts = vec![
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(vault_a),
            PrivacyPreservingAccount::Public(vault_b),
            PrivacyPreservingAccount::PrivateOwned(h_a),
            PrivacyPreservingAccount::PrivateOwned(h_b),
            // No clock — `SwapExactInputCircuit` skips the TWAP oracle
            // update, so the proof's pre-state set has no CLOCK_01
            // entry to drift during slow CPU proving. See
            // `amm_core::Instruction::SwapExactInputCircuit` doc for
            // the full rationale (same fix as the mode-2 router path).
        ];
        let instruction = Instruction::SwapExactInputCircuit {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: tok_in,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        // RFP Usability #7: shielded input balance must cover the swap
        // (no gas leg to check — privacy txs are feeless on rc3).
        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let acc = pre.get(input_idx).ok_or(ExecutionFailureKind::AmountMismatchError)?;
            let bal = match TokenHolding::try_from(&acc.data) {
                Ok(TokenHolding::Fungible { balance, .. }) => balance,
                _ => 0,
            };
            if bal < swap_amount_in {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Wait for chain inclusion. Without this, the FFI returns "success"
        // the moment the mempool accepts the submit — a sequencer rejection
        // (proof invalid, conflict, etc.) is then indistinguishable from a
        // successful op to the caller. With poll, rejection → ERR_SUBMIT.
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// **Private-Disposable** swap — the RFP-literal account-A model
/// (design.md §5.10 mode 2, §5.2 router). One privacy-preserving tx whose
/// top program is the deployed **account-A router**, with the AMM and
/// token programs declared as chained-call dependencies. The user's
/// private input holding is deshielded into a **fresh single-use public
/// account A**, A swaps in the public pool, and A's output is re-shielded
/// to the user's private output holding — atomically, one proof. A is
/// caller-created (one fresh public account per pool token) and never
/// reused. Weaker than the routerless `ldex_amm_private_swap_exact_in`
/// (an ephemeral public address is visible) but matches RFP Privacy AC #4
/// verbatim.
///
/// `user_holding_a/b` = the user's private holdings for the pool's token
/// A / token B (def order from prep). `a_holding_a/b` = the two fresh
/// public account-A holdings (created by the caller via wallet-ffi,
/// uninitialized — the token transfers initialize them in-tx).
///
/// # Safety
/// Strings NUL-terminated UTF-8; every `*_id`/`*_holding_*` arg is 32
/// readable bytes; `out_tx_hash` is 32 writable bytes.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_disposable_swap_exact_in(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    router_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    a_holding_a: *const u8,
    a_holding_b: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(amm_b), Some(rtr_b), Some(ua_b), Some(ub_b)) = (
        read_id(amm_program_id),
        read_id(router_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(aa_b), Some(ab_b), Some(in_b)) = (
        read_id(a_holding_a),
        read_id(a_holding_b),
        read_id(token_definition_in),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(da_b), Some(db_b)) = (read_id(token_def_a), read_id(token_def_b)) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(amm_b);
    let router_pid = program_id_from_bytes(rtr_b);
    let (uha, uhb) = (AccountId::new(ua_b), AccountId::new(ub_b));
    let (a_a, a_b) = (AccountId::new(aa_b), AccountId::new(ab_b));
    let tok_in = AccountId::new(in_b);
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    // Load the three deployed guest ELFs (in-circuit execution needs
    // bytecode). Router is the top program; AMM + token are its declared
    // chained-call dependencies. Ids must match what was deployed.
    let router_prog = match load_deployed_program(
        "LDEX_ROUTER_ELF",
        "private-swap-router-methods/private-swap-router-guest/\
         riscv32im-risc0-zkvm-elf/release/private_swap_router.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if router_prog.id() != router_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let amm_prog = match load_deployed_program(
        "LDEX_AMM_ELF",
        "amm-methods/amm-guest/riscv32im-risc0-zkvm-elf/release/amm.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    // Built-in token program (see note in the mode-1 export) — what the
    // AMM's chained transfers actually invoke.
    let token_prog = Program::token();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(amm_prog.id(), amm_prog);
    deps.insert(token_prog.id(), token_prog);
    let program = ProgramWithDependencies::new(router_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        // LOW: debug-only tracing. These lines print account ids and the
        // shielded input balance, which must NOT leak to stderr in a release
        // build of a privacy tool. Compiled out unless debug_assertions.
        macro_rules! disp_trace { ($($a:tt)*) => {{ #[cfg(debug_assertions)] eprintln!($($a)*); }} }
        disp_trace!("[disp] step:prep_private");
        let p = prep_private(&cfg, &store, amm_pid, def_a, def_b, fees).await?;
        disp_trace!("[disp] pool={:?} vault_a={:?} vault_b={:?}", p.pool, p.vault_a, p.vault_b);
        // Orient user in/out holdings by the input token.
        let (user_in, user_out) = if def_a == tok_in {
            (uha, uhb)
        } else if def_b == tok_in {
            (uhb, uha)
        } else {
            disp_trace!("[disp] FAIL: tok_in matches neither def_a nor def_b");
            return Err(LDEX_AMM_ERR_ACCOUNT);
        };

        // Account order = router guest `private_swap` signature.
        // No CLOCK_01 — the AMM chained call uses
        // `SwapExactInputCircuit` (no oracle update) so the proof's
        // pre-state set has nothing that drifts with new blocks.
        // Public swaps (mode 0) keep using `SwapExactInput` with the
        // clock account; the TWAP oracle is still fed by those.
        let accounts = vec![
            PrivacyPreservingAccount::PrivateOwned(user_in),
            PrivacyPreservingAccount::Public(a_a),
            PrivacyPreservingAccount::Public(a_b),
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(p.vault_a),
            PrivacyPreservingAccount::Public(p.vault_b),
            PrivacyPreservingAccount::PrivateOwned(user_out),
        ];
        let instruction = private_swap_router_core::Instruction::PrivateSwap {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: tok_in,
            fees,
            deadline,
        };
        disp_trace!("[disp] step:serialize_instruction");
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_e| { disp_trace!("[disp] FAIL serialize: {_e}"); LDEX_AMM_ERR_SUBMIT })?;

        // RFP Usability #7: shielded input balance must cover the swap
        // (user_in is pre_state index 0; no gas leg on rc3 privacy txs).
        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let acc = pre.first().ok_or(ExecutionFailureKind::AmountMismatchError)?;
            let bal = match TokenHolding::try_from(&acc.data) {
                Ok(TokenHolding::Fungible { balance, .. }) => balance,
                _ => 0,
            };
            disp_trace!("[disp] pre_check user_in_balance={bal} swap_amount_in={swap_amount_in}");
            if bal < swap_amount_in {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        disp_trace!("[disp] step:send_privacy_preserving_tx");
        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_e| { disp_trace!("[disp] FAIL send: {_e}  --debug-- {_e:?}"); LDEX_AMM_ERR_SUBMIT })?;
        disp_trace!("[disp] step:poll_native_token_transfer");
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_e| { disp_trace!("[disp] FAIL poll: {_e}  --debug-- {_e:?}"); LDEX_AMM_ERR_SUBMIT })?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}



/// **Private add-liquidity** (RFP Func #2: LP via deshield→interact→
/// re-shield from a private account). One `send_privacy_preserving_tx`
/// over the deployed AMM `AddLiquidity`: the user's two token holdings
/// are deshielded in-circuit, liquidity is added in the public pool, and
/// the minted LP is re-shielded to the user's private LP holding — no
/// public address ever exists on-chain. The LP *position* is public
/// pool state; which private account provided/owns it is not traceable.
/// Same mechanism as the validated private swap (mode-1).
///
/// # Safety
/// As `ldex_amm_private_swap_exact_in`.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_private_add_liquidity(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    min_amount_liquidity: u128,
    max_amount_to_add_token_a: u128,
    max_amount_to_add_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(a_b), Some(b_b)) = (
        read_id(amm_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(lp_b), Some(da_b), Some(db_b)) = (
        read_id(user_holding_lp),
        read_id(token_def_a),
        read_id(token_def_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (
        AccountId::new(a_b),
        AccountId::new(b_b),
        AccountId::new(lp_b),
    );
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    let amm_prog = match load_deployed_program(
        "LDEX_AMM_ELF",
        "amm-methods/amm-guest/riscv32im-risc0-zkvm-elf/release/amm.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let token_prog = Program::token();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(token_prog.id(), token_prog);
    let program = ProgramWithDependencies::new(amm_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_pid, def_a, def_b, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        // Align (vault, holding, max-amount) legs to the pool's canonical
        // token-A: add_liquidity keys vault_a/user_holding_a(idx4)/max_a all
        // to reserve_a, so a reversed-order call must flip all three (uhlp
        // is the LP leg, unaffected).
        let (vault_a, vault_b, h_a, h_b, max_a, max_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, uhb, uha,
                 max_amount_to_add_token_b, max_amount_to_add_token_a)
            } else {
                (p.vault_a, p.vault_b, uha, uhb,
                 max_amount_to_add_token_a, max_amount_to_add_token_b)
            };
        // Account order = AMM `add_liquidity` guest signature
        // (clock threaded last per §5.11③).
        let accounts = vec![
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(vault_a),
            PrivacyPreservingAccount::Public(vault_b),
            PrivacyPreservingAccount::Public(lp_def),
            PrivacyPreservingAccount::PrivateOwned(h_a),
            PrivacyPreservingAccount::PrivateOwned(h_b),
            PrivacyPreservingAccount::PrivateOwned(uhlp),
            PrivacyPreservingAccount::Public(CLOCK_01),
        ];
        let instruction = Instruction::AddLiquidity {
            min_amount_liquidity,
            max_amount_to_add_token_a: max_a,
            max_amount_to_add_token_b: max_b,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        // Shielded inputs must cover what's being added (RFP Usability #7).
        // h_a = pre_state idx 4, h_b = idx 5 (aligned to slot, with max_a/b).
        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let bal = |i: usize| -> u128 {
                pre.get(i)
                    .and_then(|a| TokenHolding::try_from(&a.data).ok())
                    .map(|h| match h {
                        TokenHolding::Fungible { balance, .. } => balance,
                        _ => 0,
                    })
                    .unwrap_or(0)
            };
            if bal(4) < max_a || bal(5) < max_b {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Wait for chain inclusion. Without this, the FFI returns "success"
        // the moment the mempool accepts the submit — a sequencer rejection
        // (proof invalid, conflict, etc.) is then indistinguishable from a
        // successful op to the caller. With poll, rejection → ERR_SUBMIT.
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// **Private remove-liquidity** (RFP Func #2). One
/// `send_privacy_preserving_tx` over the deployed AMM `RemoveLiquidity`:
/// the user's private LP holding is deshielded in-circuit, liquidity is
/// withdrawn from the public pool, and both token outputs are re-shielded
/// to the user's private holdings. Same mechanism as the validated
/// private swap.
///
/// # Safety
/// As `ldex_amm_private_swap_exact_in`.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_private_remove_liquidity(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    remove_liquidity_amount: u128,
    min_amount_to_remove_token_a: u128,
    min_amount_to_remove_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(a_b), Some(b_b)) = (
        read_id(amm_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(lp_b), Some(da_b), Some(db_b)) = (
        read_id(user_holding_lp),
        read_id(token_def_a),
        read_id(token_def_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (
        AccountId::new(a_b),
        AccountId::new(b_b),
        AccountId::new(lp_b),
    );
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    let amm_prog = match load_deployed_program(
        "LDEX_AMM_ELF",
        "amm-methods/amm-guest/riscv32im-risc0-zkvm-elf/release/amm.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let token_prog = Program::token();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(token_prog.id(), token_prog);
    let program = ProgramWithDependencies::new(amm_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_pid, def_a, def_b, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        // Align (vault, holding, min-amount) legs to the pool's canonical
        // token-A: remove keys vault_a/user_holding_a(idx4)/min_a all to
        // reserve_a, so a reversed-order call must flip all three (uhlp is
        // the LP leg, the pre_check target, unaffected).
        let (vault_a, vault_b, h_a, h_b, min_a, min_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, uhb, uha,
                 min_amount_to_remove_token_b, min_amount_to_remove_token_a)
            } else {
                (p.vault_a, p.vault_b, uha, uhb,
                 min_amount_to_remove_token_a, min_amount_to_remove_token_b)
            };
        let accounts = vec![
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(vault_a),
            PrivacyPreservingAccount::Public(vault_b),
            PrivacyPreservingAccount::Public(lp_def),
            PrivacyPreservingAccount::PrivateOwned(h_a),
            PrivacyPreservingAccount::PrivateOwned(h_b),
            PrivacyPreservingAccount::PrivateOwned(uhlp),
            PrivacyPreservingAccount::Public(CLOCK_01),
        ];
        let instruction = Instruction::RemoveLiquidity {
            remove_liquidity_amount,
            min_amount_to_remove_token_a: min_a,
            min_amount_to_remove_token_b: min_b,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        // Shielded LP holding (pre_state idx 6) must cover the burn.
        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let bal = pre
                .get(6)
                .and_then(|a| TokenHolding::try_from(&a.data).ok())
                .map(|h| match h {
                    TokenHolding::Fungible { balance, .. } => balance,
                    _ => 0,
                })
                .unwrap_or(0);
            if bal < remove_liquidity_amount {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Wait for chain inclusion. Without this, the FFI returns "success"
        // the moment the mempool accepts the submit — a sequencer rejection
        // (proof invalid, conflict, etc.) is then indistinguishable from a
        // successful op to the caller. With poll, rejection → ERR_SUBMIT.
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// Initialize a fresh public account as a token holding for `token_def`
/// (public `token::InitializeAccount` tx — no proof, fast). Used by the
/// Private-Disposable path to make the router's freshly-created account-A
/// holdings valid token holdings *before* the AMM validates them
/// upfront (the AMM rejects an uninitialized user holding —
/// `swap.rs` "must be owned by the vault's Token Program"). Idempotent
/// in effect: re-initializing an already-init holding just fails
/// harmlessly at the sequencer (caller treats non-OK as best-effort).
///
/// # Safety
/// `*_path` NUL-terminated; `token_def`/`holding` are 32 bytes;
/// `out_tx_hash` is 32 writable bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_init_token_holding(
    config_path: *const c_char,
    storage_path: *const c_char,
    token_def: *const u8,
    holding: *const u8,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(def_b), Some(hold_b)) = (read_id(token_def), read_id(holding)) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let (def_id, hold_id) = (AccountId::new(def_b), AccountId::new(hold_b));
    let token_pid = Program::token().id();
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let wallet = WalletCore::new_update_chain(
            PathBuf::from(&cfg),
            PathBuf::from(&store),
            None,
        )
        .map_err(|_| LDEX_AMM_ERR_WALLET)?;
        // The fresh holding account is the only signer (uninitialized,
        // authorized); the token definition is a read-only input.
        let signers = [hold_id];
        let nonces = wallet
            .get_accounts_nonces(signers.to_vec())
            .await
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        let key = wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(hold_id)
            .ok_or(LDEX_AMM_ERR_KEY)?;
        let message = nssa::public_transaction::Message::try_new(
            token_pid,
            vec![def_id, hold_id],
            nonces,
            token_core::Instruction::InitializeAccount,
        )
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let witness_set =
            nssa::public_transaction::WitnessSet::for_message(&message, &[key]);
        let tx = nssa::PublicTransaction::new(message, witness_set);
        let hash = wallet
            .sequencer_client
            .send_transaction(NSSATransaction::Public(tx))
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Same poll-for-inclusion guard as `finalize()`. Mempool accept
        // != ledger inclusion; without this, rc=0 was being returned for
        // txs the sequencer eventually rejected.
        wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// Create the Associated Token Account for `(owner, token_def)` via the
/// deployed ATA program (RFP-004 Func #8). Public tx, idempotent (no-op
/// if the ATA already exists). The ATA id is derived deterministically
/// (`sha256(owner ‖ def)` PDA) — the caller can also get it from
/// `ldex_ata_id`. `out_tx_hash` ← 32-byte tx hash.
///
/// # Safety
/// `*_path` NUL-terminated; `*_id` args 32 bytes; `out_tx_hash` 32
/// writable bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_ata_create(
    config_path: *const c_char,
    storage_path: *const c_char,
    ata_program_id: *const u8,
    owner: *const u8,
    token_def: *const u8,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(o_b), Some(d_b)) = (
        read_id(ata_program_id),
        read_id(owner),
        read_id(token_def),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let ata_pid = program_id_from_bytes(pid_b);
    let (owner_id, def_id) = (AccountId::new(o_b), AccountId::new(d_b));
    let seed = ata_core::compute_ata_seed(owner_id, def_id);
    let ata_id = ata_core::get_associated_token_account_id(&ata_pid, &seed);
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let wallet = WalletCore::new_update_chain(
            PathBuf::from(&cfg),
            PathBuf::from(&store),
            None,
        )
        .map_err(|_| LDEX_AMM_ERR_WALLET)?;
        // Owner is the only key signer; token_def is read-only; the ATA
        // is a program-derived account (Claim::Authorized handled by the
        // ATA program when it's still default).
        let nonces = wallet
            .get_accounts_nonces(vec![owner_id])
            .await
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        let key = wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(owner_id)
            .ok_or(LDEX_AMM_ERR_KEY)?;
        let message = nssa::public_transaction::Message::try_new(
            ata_pid,
            vec![owner_id, def_id, ata_id],
            nonces,
            ata_core::Instruction::Create,
        )
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let witness_set =
            nssa::public_transaction::WitnessSet::for_message(&message, &[key]);
        let tx = nssa::PublicTransaction::new(message, witness_set);
        let hash = wallet
            .sequencer_client
            .send_transaction(NSSATransaction::Public(tx))
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Same poll-for-inclusion guard as `finalize()`. Mempool accept
        // != ledger inclusion; without this, rc=0 was being returned for
        // txs the sequencer eventually rejected.
        wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}



// ============================================================================
//          amm_v2 combined disposable swap (mode-2, testnet-compat)
// ============================================================================
//
// Replaces (router + amm + 4× token::Transfer) with
// (amm_v2 + 4× token::Transfer). One fewer env::verify in the outer
// privacy STARK → ~5-10% wall-clock reduction on mode-2. Receipts
// verify under upstream PRIVACY_PRESERVING_CIRCUIT_ID (amm_v2 is a
// regular deployed program; no nssa change required).
//
// Pool layout: amm_v2 NewDefinition creates pools under amm_v2's
// ProgramId. Same `PoolDefinition` data shape as the canonical AMM;
// vault PDAs derive under amm_v2.

/// amm_v2 combined disposable swap — SINGLE-PROOF (in-circuit) variant.
///
/// Runs deshield + AMM swap + re-shield inside ONE privacy STARK. This
/// names the public pool PDA (+ vaults) as committed pre-states, so the
/// sequencer re-derives them from LIVE chain state at submit and verifies
/// the receipt against that: a competing swap that moves the pool during
/// the minutes-long proof invalidates the receipt
/// (`InvalidPrivacyPreservingProof`), forcing a re-prove. On a busy,
/// bidirectional DEX pool that stale-out is near-certain under load.
///
/// Kept for callers who want MAXIMAL privacy (the swap stays hidden inside
/// the proof) and can tolerate drift/re-proves. The default
/// `ldex_amm_v2_disposable_swap` is the drift-free 3-tx variant; prefer it
/// unless you specifically need the single-proof privacy profile.
///
/// Mirror of `ldex_amm_disposable_swap_exact_in` argument list MINUS
/// `router_program_id` (amm_v2 IS the router+AMM combined), PLUS
/// `amm_v2_program_id` to select the amm_v2 pool ecosystem.
///
/// # Safety
/// Strings NUL-terminated UTF-8; every `*_id`/`*_holding_*` arg is 32
/// readable bytes; `out_tx_hash` is 32 writable bytes.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_disposable_swap_inproof(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    a_holding_a: *const u8,
    a_holding_b: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(ua_b), Some(ub_b)) = (
        read_id(amm_v2_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(aa_b), Some(ab_b), Some(in_b)) = (
        read_id(a_holding_a),
        read_id(a_holding_b),
        read_id(token_definition_in),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(da_b), Some(db_b)) = (read_id(token_def_a), read_id(token_def_b)) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_v2_pid = program_id_from_bytes(pid_b);
    let (uha, uhb) = (AccountId::new(ua_b), AccountId::new(ub_b));
    let (a_a, a_b) = (AccountId::new(aa_b), AccountId::new(ab_b));
    let tok_in = AccountId::new(in_b);
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    let amm_v2_prog = match load_deployed_program(
        "LDEX_AMM_V2_ELF",
        "amm-v2-methods/amm-v2-guest/riscv32im-risc0-zkvm-elf/release/amm_v2.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_v2_prog.id() != amm_v2_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let token_prog = Program::token();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(token_prog.id(), token_prog);
    let program = ProgramWithDependencies::new(amm_v2_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_v2_pid, def_a, def_b, fees).await?;
        let (user_in, user_out) = if def_a == tok_in {
            (uha, uhb)
        } else if def_b == tok_in {
            (uhb, uha)
        } else {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        };
        // Align the (vault, a-holding) legs to the pool's canonical
        // token-A: disposable_swap asserts vault_a == pool.vault_a_id and
        // pairs a_holding_a with vault_a (token_definition_id_in picks the
        // swap direction), so flip both together on a reversed-order call.
        let (vault_a, vault_b, a_a, a_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, a_b, a_a)
            } else {
                (p.vault_a, p.vault_b, a_a, a_b)
            };

        let accounts = vec![
            PrivacyPreservingAccount::PrivateOwned(user_in),
            PrivacyPreservingAccount::Public(a_a),
            PrivacyPreservingAccount::Public(a_b),
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(vault_a),
            PrivacyPreservingAccount::Public(vault_b),
            PrivacyPreservingAccount::PrivateOwned(user_out),
        ];
        let instruction = amm_v2_core::Instruction::DisposableSwap {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: tok_in,
            fees,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let acc = pre.first().ok_or(ExecutionFailureKind::AmountMismatchError)?;
            let bal = match TokenHolding::try_from(&acc.data) {
                Ok(TokenHolding::Fungible { balance, .. }) => balance,
                _ => 0,
            };
            if bal < swap_amount_in {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Wait for chain inclusion. Without this, the FFI returns "success"
        // the moment the mempool accepts the submit — a sequencer rejection
        // (proof invalid, conflict, etc.) is then indistinguishable from a
        // successful op to the caller. With poll, rejection → ERR_SUBMIT.
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// amm_v2 combined disposable swap — DRIFT-FREE 3-transaction variant
/// (the default; replaces the former single-proof body, now preserved as
/// [`ldex_amm_v2_disposable_swap_inproof`]).
///
/// The former design ran the AMM swap *inside* the privacy proof, naming
/// the public pool PDA as a committed pre-state; a competing swap landing
/// during the minutes-long proof invalidated the receipt (stale pool
/// pre-state -> `InvalidPrivacyPreservingProof`). A shared DEX pool is
/// bidirectional and hot, so under load that drift was near-certain.
///
/// This variant splits the op into three transactions so the ONLY pool
/// interaction is a proofless PUBLIC swap, which the sequencer linearizes
/// against live state and therefore cannot go stale:
///   1. DESHIELD  user_in (PrivateOwned) -> a_in (Public)   [privacy proof, no pool]
///   2. SWAP      a_in -> a_out via public SwapExactInput    [public tx, atomic, min_out]
///   3. RESHIELD  a_out (Public) -> user_out (PrivateOwned)  [privacy proof, no pool]
/// Steps 1 and 3 prove but touch only the user's own notes / fresh A
/// holdings (no shared contention); step 2 prices against the live pool
/// with `min_amount_out` slippage protection. The fresh `a_out` holding is
/// auto-initialised by the swap's token `Transfer`
/// (`new_claimed_if_default`), so no separate init tx is needed.
///
/// PRIVACY TRADE-OFF (deliberate): a 3-tx flow links a deshield and a
/// reshield around an *observable* public swap, leaking more linkage than
/// the single-proof design hid. Callers who need maximal privacy and can
/// tolerate drift should call [`ldex_amm_v2_disposable_swap_inproof`].
///
/// ATOMICITY: three on-chain txs cannot be on-chain-atomic. This is
/// best-effort, no-loss: if the public swap fails (e.g. slippage), the
/// deshielded input is re-shielded back to `user_in` (rollback); if the
/// final re-shield fails, the swapped output stays in the public `a_out`
/// holding (recoverable via `ldex_token_shield`) and an error is returned.
/// Funds are never destroyed, but a crash mid-sequence requires a manual
/// resume from the A holdings.
///
/// Returns the re-shield (final) tx hash. Same argument shape as
/// [`ldex_amm_v2_disposable_swap_inproof`] — existing callers are fixed
/// transparently.
///
/// # Safety
/// Strings NUL-terminated UTF-8; every `*_id`/`*_holding_*` arg is 32
/// readable bytes; `out_tx_hash` is 32 writable bytes.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_disposable_swap(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    a_holding_a: *const u8,
    a_holding_b: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(ua_b), Some(ub_b)) = (
        read_id(amm_v2_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(aa_b), Some(ab_b), Some(in_b)) = (
        read_id(a_holding_a),
        read_id(a_holding_b),
        read_id(token_definition_in),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(da_b), Some(db_b)) = (read_id(token_def_a), read_id(token_def_b)) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_v2_pid = program_id_from_bytes(pid_b);
    let (uha, uhb) = (AccountId::new(ua_b), AccountId::new(ub_b));
    let (a_a, a_b) = (AccountId::new(aa_b), AccountId::new(ab_b));
    let tok_in = AccountId::new(in_b);
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_v2_pid, def_a, def_b, fees).await?;
        // Resolve swap direction: which user/A holdings are the input vs
        // output side. `a_a`/`a_b` are the fresh single-use account-A
        // holdings for `def_a`/`def_b` respectively (caller-created).
        let (user_in, user_out, a_in, a_out) = if def_a == tok_in {
            (uha, uhb, a_a, a_b)
        } else if def_b == tok_in {
            (uhb, uha, a_b, a_a)
        } else {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        };
        // Definition of the OUTPUT token (the side `a_out` receives).
        let def_out = if def_a == tok_in { def_b } else { def_a };
        let mut wallet = p.wallet;
        let token_prog: ProgramWithDependencies = Program::token().into();

        // --- tx1: DESHIELD user_in (PrivateOwned) -> a_in (Public). One
        // simple privacy proof; touches only the user's note + fresh A
        // holding, so it cannot drift on pool state. ---
        let deshield_data = Program::serialize_instruction(
            token_core::Instruction::Transfer { amount_to_transfer: swap_amount_in },
        )
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let (h1, secrets1) = wallet
            .send_privacy_preserving_tx(
                vec![
                    PrivacyPreservingAccount::PrivateOwned(user_in),
                    PrivacyPreservingAccount::Public(a_in),
                ],
                deshield_data,
                &token_prog,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let tx1 = wallet
            .poll_native_token_transfer(h1)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Fold the spend into the local cache so `user_in`'s private balance
        // is correct for any subsequent op (see `ldex_token_deshield`).
        if let NSSATransaction::PrivacyPreserving(ppt) = tx1 {
            if let Some(secret) = secrets1.into_iter().next() {
                wallet
                    .decode_insert_privacy_preserving_transaction_results(
                        &ppt,
                        &[AccDecodeData::Decode(secret, user_in)],
                    )
                    .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
                wallet
                    .store_persistent_data()
                    .await
                    .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
            }
        }

        // --- tx1b: INIT a_out as a token holding for the OUTPUT definition.
        // amm_v2's swap asserts BOTH trader holdings are already owned by the
        // token program; `a_in` was claimed by the deshield transfer above, but
        // `a_out` is a fresh single-use account and must be initialised first
        // (cheap public tx, no proof). ---
        {
            let token_pid = Program::token().id();
            let nonces = wallet
                .get_accounts_nonces(vec![a_out])
                .await
                .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
            let key = wallet
                .storage()
                .user_data
                .get_pub_account_signing_key(a_out)
                .ok_or(LDEX_AMM_ERR_KEY)?;
            let msg = nssa::public_transaction::Message::try_new(
                token_pid,
                vec![def_out, a_out],
                nonces,
                token_core::Instruction::InitializeAccount,
            )
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
            let ws = nssa::public_transaction::WitnessSet::for_message(&msg, &[key]);
            let h = wallet
                .sequencer_client
                .send_transaction(NSSATransaction::Public(nssa::PublicTransaction::new(msg, ws)))
                .await
                .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
            wallet
                .poll_native_token_transfer(h)
                .await
                .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        }

        // --- tx2: PUBLIC SwapExactInput a_in -> a_out. Proofless, atomic
        // against live pool state, slippage-bounded by `min_amount_out` —
        // this is the leg that makes the whole op drift-free. Signer is
        // `a_in` (deshielded, authorized); `a_out` is the now-initialised
        // output holding. 5-account list, NO Clock (amm_v2 skips the oracle). ---
        let swap_instr = amm_v2_core::Instruction::SwapExactInput {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: tok_in,
            deadline,
        };
        // Align the (vault, a-holding) legs to the pool's canonical
        // token-A: swap_exact_input asserts vault_a == pool.vault_a_id and
        // pairs a_a with vault_a, so flip both on a reversed-order call
        // (token_definition_id_in picks the direction).
        let (vault_a, vault_b, sw_a_a, sw_a_b) =
            if pool_needs_leg_flip(&wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, a_b, a_a)
            } else {
                (p.vault_a, p.vault_b, a_a, a_b)
            };
        if let Err(e) = finalize(
            &wallet,
            amm_v2_pid,
            vec![p.pool, vault_a, vault_b, sw_a_a, sw_a_b],
            &[a_in],
            swap_instr,
        )
        .await
        {
            // ROLLBACK: the public swap failed, so re-shield the deshielded
            // input back to the user's private holding rather than leave it
            // stranded in the public `a_in`. Best-effort: if this also
            // fails, funds remain in `a_in` (recoverable via
            // `ldex_token_shield`).
            let reshield_back = Program::serialize_instruction(
                token_core::Instruction::Transfer { amount_to_transfer: swap_amount_in },
            )
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
            if let Ok((hb, secrets_back)) = wallet
                .send_privacy_preserving_tx(
                    vec![
                        PrivacyPreservingAccount::Public(a_in),
                        PrivacyPreservingAccount::PrivateOwned(user_in),
                    ],
                    reshield_back,
                    &token_prog,
                )
                .await
            {
                // Fold the reshield credit back into the local cache:
                // tx1 already decremented `user_in`'s cached PRIV balance by
                // swap_amount_in, so without this the cache stays understated
                // until a full rescan and a later pre_check could falsely
                // fail. Best-effort, mirroring the tx1/tx3 success folds.
                if let Ok(NSSATransaction::PrivacyPreserving(ppt)) =
                    wallet.poll_native_token_transfer(hb).await
                {
                    if let Some(s) = secrets_back.into_iter().next() {
                        let _ = wallet.decode_insert_privacy_preserving_transaction_results(
                            &ppt,
                            &[AccDecodeData::Decode(s, user_in)],
                        );
                        let _ = wallet.store_persistent_data().await;
                    }
                }
            }
            return Err(e);
        }

        // --- read the realized swap output now sitting in the public a_out.
        // Drift means the realized amount is pool-dependent; reshield exactly
        // what landed (>= min_amount_out, enforced by the swap). ---
        let out_acc = wallet
            .get_account_public(a_out)
            .await
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        let out_amount = match TokenHolding::try_from(&out_acc.data) {
            Ok(TokenHolding::Fungible { balance, .. }) => balance,
            _ => 0,
        };
        if out_amount == 0 {
            return Err(LDEX_AMM_ERR_SUBMIT);
        }

        // --- tx3: RESHIELD a_out (Public) -> user_out (PrivateOwned). One
        // simple privacy proof; no pool, cannot drift. ---
        let reshield_data = Program::serialize_instruction(
            token_core::Instruction::Transfer { amount_to_transfer: out_amount },
        )
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let (h3, secrets3) = wallet
            .send_privacy_preserving_tx(
                vec![
                    PrivacyPreservingAccount::Public(a_out),
                    PrivacyPreservingAccount::PrivateOwned(user_out),
                ],
                reshield_data,
                &token_prog,
            )
            .await
            // Output is safe in the public `a_out` (recoverable via
            // `ldex_token_shield`); surface the failure to the caller.
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let tx3 = wallet
            .poll_native_token_transfer(h3)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        if let NSSATransaction::PrivacyPreserving(ppt) = tx3 {
            if let Some(secret) = secrets3.into_iter().next() {
                wallet
                    .decode_insert_privacy_preserving_transaction_results(
                        &ppt,
                        &[AccDecodeData::Decode(secret, user_out)],
                    )
                    .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
                wallet
                    .store_persistent_data()
                    .await
                    .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
            }
        }

        let mut out = [0u8; 32];
        let h: &[u8] = h3.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// Create a new amm_v2 pool (public tx, no proof). Mirror of
/// `ldex_amm_new_pool` argument shape, but the program id and pool
/// PDA derive under amm_v2 — the resulting pool is amm_v2-owned.
/// amm_v2 NewDefinition takes no Clock account (no on-chain TWAP
/// oracle; amm_v2 pools deliberately skip it so privacy proofs over
/// them are drift-free on slow CPU provers).
///
/// # Safety
/// As `ldex_amm_new_pool`.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_new_pool(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    amount_a: u128,
    amount_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, lp_b, _) = ids4!(
        amm_v2_program_id,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        user_holding_lp
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(lp_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        let lp_lock = compute_lp_lock_holding_pda(amm_pid, p.pool);
        // 8-account list, NO Clock — amm_v2 deliberately skips the oracle.
        let account_ids = vec![
            p.pool, p.vault_a, p.vault_b, lp_def, lp_lock, uha, uhb, uhlp,
        ];
        let mut signers = vec![uha, uhb];
        if p.wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(uhlp)
            .is_some()
        {
            signers.push(uhlp);
        }
        let instruction = amm_v2_core::Instruction::NewDefinition {
            token_a_amount: amount_a,
            token_b_amount: amount_b,
            fees,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &signers, instruction).await
    });
    out32(res, out_tx_hash)
}

// ============================================================================
//          amm_v2 — full AMM superset (mode 0 / 1 / 2 + liquidity)
// ============================================================================
//
// These FFI exports route mode-0 (public), mode-1 (private), and mode-2
// (disposable, token↔token AND native-LEZ) through amm_v2's pool
// ecosystem. amm_v2's pools are amm_v2-owned (separate PDAs from the
// canonical AMM); the cpp_plugin / UI uses LDEX_AMM_V2_PROGRAM_ID from
// bootstrap.env. Receipts verify under upstream PRIVACY_PRESERVING_
// CIRCUIT_ID — testnet-compatible.
//
// Public ops use `finalize()` (no Clock — amm_v2 skips the on-chain
// TWAP oracle on all swaps; analytics consumes cum_volume / cum_fees
// counters instead). Private ops use send_privacy_preserving_tx with
// amm_v2 loaded as the deployed top-level program.

/// amm_v2 add liquidity (public tx, no proof). Account list: pool,
/// vault_a, vault_b, lp_def, user_a, user_b, user_lp (NO clock).
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_add_liquidity(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    min_amount_liquidity: u128,
    max_amount_to_add_token_a: u128,
    max_amount_to_add_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, lp_b, _) = ids4!(
        amm_v2_program_id,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        user_holding_lp
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(lp_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        // Align (vault, holding, max-amount) legs to the pool's canonical
        // token-A: add_liquidity keys vault_a/user_holding_a/max_a all to
        // reserve_a, so a reversed-order call must flip all three (uhlp is
        // the LP leg, unaffected).
        let (vault_a, vault_b, h_a, h_b, max_a, max_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, p.def_a).await? {
                (p.vault_b, p.vault_a, uhb, uha,
                 max_amount_to_add_token_b, max_amount_to_add_token_a)
            } else {
                (p.vault_a, p.vault_b, uha, uhb,
                 max_amount_to_add_token_a, max_amount_to_add_token_b)
            };
        // 7-account list, NO Clock.
        let account_ids = vec![p.pool, vault_a, vault_b, lp_def, h_a, h_b, uhlp];
        let instruction = amm_v2_core::Instruction::AddLiquidity {
            min_amount_liquidity,
            max_amount_to_add_token_a: max_a,
            max_amount_to_add_token_b: max_b,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[h_a, h_b], instruction).await
    });
    out32(res, out_tx_hash)
}

/// amm_v2 remove liquidity (public tx). LP holding signs.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_remove_liquidity(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    remove_liquidity_amount: u128,
    min_amount_to_remove_token_a: u128,
    min_amount_to_remove_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, lp_b, _) = ids4!(
        amm_v2_program_id,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        user_holding_lp
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(lp_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        // Align (vault, holding, min-amount) legs to the pool's canonical
        // token-A: remove_liquidity keys vault_a/user_holding_a/min_a all to
        // reserve_a, so a reversed-order call must flip all three (uhlp is
        // the LP leg, the sole signer, unaffected).
        let (vault_a, vault_b, h_a, h_b, min_a, min_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, p.def_a).await? {
                (p.vault_b, p.vault_a, uhb, uha,
                 min_amount_to_remove_token_b, min_amount_to_remove_token_a)
            } else {
                (p.vault_a, p.vault_b, uha, uhb,
                 min_amount_to_remove_token_a, min_amount_to_remove_token_b)
            };
        let account_ids = vec![p.pool, vault_a, vault_b, lp_def, h_a, h_b, uhlp];
        let instruction = amm_v2_core::Instruction::RemoveLiquidity {
            remove_liquidity_amount,
            min_amount_to_remove_token_a: min_a,
            min_amount_to_remove_token_b: min_b,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[uhlp], instruction).await
    });
    out32(res, out_tx_hash)
}

/// amm_v2 mode-0 public swap. Same arg shape as `ldex_amm_swap_exact_in`
/// but uses amm_v2 pool + skips Clock.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_swap_exact_in(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (pid_b, a_b, b_b, in_b, _) = ids4!(
        amm_v2_program_id,
        user_holding_a,
        user_holding_b,
        token_definition_in,
        token_definition_in
    );
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, tok_in) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(in_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res = rt.block_on(async move {
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let signer = if p.def_a == tok_in {
            uha
        } else if p.def_b == tok_in {
            uhb
        } else {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        };
        // Align (vault, holding) legs to the pool's canonical token-A:
        // swap_exact_input asserts vault_a == pool.vault_a_id and pairs
        // vault_a with user_holding_a, so if the caller passed the pair
        // reversed, flip both vaults and holdings together
        // (token_definition_id_in / signer disambiguate the direction).
        let (vault_a, vault_b, h_a, h_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, p.def_a).await? {
                (p.vault_b, p.vault_a, uhb, uha)
            } else {
                (p.vault_a, p.vault_b, uha, uhb)
            };
        // 5-account list, NO Clock.
        let account_ids = vec![p.pool, vault_a, vault_b, h_a, h_b];
        let instruction = amm_v2_core::Instruction::SwapExactInput {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: tok_in,
            deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[signer], instruction).await
    });
    out32(res, out_tx_hash)
}

/// amm_v2 mode-1 PRIVATE PrivateOwned swap. The upstream privacy
/// circuit chains amm_v2.SwapExactInputCircuit as the top-level call.
/// Receipts verify under upstream PRIVACY_PRESERVING_CIRCUIT_ID.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_private_swap_exact_in(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(a_b), Some(b_b)) = (
        read_id(amm_v2_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(da_b), Some(db_b), Some(in_b)) = (
        read_id(token_def_a),
        read_id(token_def_b),
        read_id(token_definition_in),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, tok_in) = (AccountId::new(a_b), AccountId::new(b_b), AccountId::new(in_b));
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    let amm_v2_prog = match load_deployed_program(
        "LDEX_AMM_V2_ELF",
        "amm-v2-methods/amm-v2-guest/riscv32im-risc0-zkvm-elf/release/amm_v2.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_v2_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let token_prog = Program::token();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(token_prog.id(), token_prog);
    let program = ProgramWithDependencies::new(amm_v2_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_pid, def_a, def_b, fees).await?;
        // Align (vault, holding) legs to the pool's canonical token-A:
        // swap_exact_input_circuit asserts vault_a == pool.vault_a_id and
        // pairs vault_a (idx1) with user_holding_a (idx3), so if the
        // caller passed the pair reversed, flip both vaults and holdings
        // together. token_definition_id_in disambiguates the direction.
        let flip = pool_needs_leg_flip(&p.wallet, p.pool, def_a).await?;
        let (vault_a, vault_b, h_a, h_b, slot_a_def, slot_b_def) = if flip {
            (p.vault_b, p.vault_a, uhb, uha, def_b, def_a)
        } else {
            (p.vault_a, p.vault_b, uha, uhb, def_a, def_b)
        };
        // Input holding index in the (now canonical) account list.
        let input_idx = if slot_a_def == tok_in {
            3usize
        } else if slot_b_def == tok_in {
            4usize
        } else {
            return Err(LDEX_AMM_ERR_ACCOUNT);
        };
        // 5-account list, NO Clock.
        let accounts = vec![
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(vault_a),
            PrivacyPreservingAccount::Public(vault_b),
            PrivacyPreservingAccount::PrivateOwned(h_a),
            PrivacyPreservingAccount::PrivateOwned(h_b),
        ];
        let instruction = amm_v2_core::Instruction::SwapExactInputCircuit {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: tok_in,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let acc = pre.get(input_idx).ok_or(ExecutionFailureKind::AmountMismatchError)?;
            let bal = match TokenHolding::try_from(&acc.data) {
                Ok(TokenHolding::Fungible { balance, .. }) => balance,
                _ => 0,
            };
            if bal < swap_amount_in {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Wait for chain inclusion. Without this, the FFI returns "success"
        // the moment the mempool accepts the submit — a sequencer rejection
        // (proof invalid, conflict, etc.) is then indistinguishable from a
        // successful op to the caller. With poll, rejection → ERR_SUBMIT.
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// amm_v2 mode-1 PRIVATE add-liquidity. Mirrors
/// `ldex_amm_v2_private_swap_exact_in` but with the AddLiquidity variant
/// + an LP-def account. Replaces the v1 `ldex_amm_private_add_liquidity`
/// path which the chain rejects as InvalidPrivacyPreservingProof.
///
/// 7-account layout (NO Clock — amm_v2 skips oracle for privacy proofs):
///   0. pool (Public PDA)
///   1. vault_a (Public PDA)
///   2. vault_b (Public PDA)
///   3. lp_def (Public PDA — amm_v2's LP token definition for this pool)
///   4. user_holding_a (PrivateOwned)
///   5. user_holding_b (PrivateOwned)
///   6. user_holding_lp (PrivateOwned — wallet-owned, can be a fresh
///      private account on first deposit; the proof creates the
///      commitment for it)
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_private_add_liquidity(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    min_amount_liquidity: u128,
    max_amount_to_add_token_a: u128,
    max_amount_to_add_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(a_b), Some(b_b)) = (
        read_id(amm_v2_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(lp_b), Some(da_b), Some(db_b)) = (
        read_id(user_holding_lp),
        read_id(token_def_a),
        read_id(token_def_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (
        AccountId::new(a_b),
        AccountId::new(b_b),
        AccountId::new(lp_b),
    );
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    let amm_v2_prog = match load_deployed_program(
        "LDEX_AMM_V2_ELF",
        "amm-v2-methods/amm-v2-guest/riscv32im-risc0-zkvm-elf/release/amm_v2.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_v2_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let token_prog = Program::token();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(token_prog.id(), token_prog);
    let program = ProgramWithDependencies::new(amm_v2_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_pid, def_a, def_b, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        // Align (vault, holding, max-amount) legs to the pool's canonical
        // token-A: add_liquidity keys vault_a/user_holding_a/max_a all to
        // reserve_a, so a reversed-order call must flip all three (the LP
        // leg uhlp/lp_def is unaffected).
        let (vault_a, vault_b, h_a, h_b, max_a, max_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, uhb, uha,
                 max_amount_to_add_token_b, max_amount_to_add_token_a)
            } else {
                (p.vault_a, p.vault_b, uha, uhb,
                 max_amount_to_add_token_a, max_amount_to_add_token_b)
            };
        let accounts = vec![
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(vault_a),
            PrivacyPreservingAccount::Public(vault_b),
            PrivacyPreservingAccount::Public(lp_def),
            PrivacyPreservingAccount::PrivateOwned(h_a),
            PrivacyPreservingAccount::PrivateOwned(h_b),
            PrivacyPreservingAccount::PrivateOwned(uhlp),
        ];
        let instruction = amm_v2_core::Instruction::AddLiquidity {
            min_amount_liquidity,
            max_amount_to_add_token_a: max_a,
            max_amount_to_add_token_b: max_b,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        // Pre-check: token-A holding (idx 4) and token-B holding (idx 5)
        // must cover the deposit. Indices/amounts are in the aligned order.
        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let bal_at = |idx: usize| -> u128 {
                pre.get(idx)
                    .and_then(|a| TokenHolding::try_from(&a.data).ok())
                    .map(|h| match h {
                        TokenHolding::Fungible { balance, .. } => balance,
                        _ => 0,
                    })
                    .unwrap_or(0)
            };
            if bal_at(4) < max_a || bal_at(5) < max_b {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// amm_v2 mode-1 PRIVATE remove-liquidity. Mirror of `_private_add_liquidity`
/// using the RemoveLiquidity variant.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_private_remove_liquidity(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    user_holding_lp: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    remove_liquidity_amount: u128,
    min_amount_to_remove_token_a: u128,
    min_amount_to_remove_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(a_b), Some(b_b)) = (
        read_id(amm_v2_program_id),
        read_id(user_holding_a),
        read_id(user_holding_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(lp_b), Some(da_b), Some(db_b)) = (
        read_id(user_holding_lp),
        read_id(token_def_a),
        read_id(token_def_b),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let (uha, uhb, uhlp) = (
        AccountId::new(a_b),
        AccountId::new(b_b),
        AccountId::new(lp_b),
    );
    let (def_a, def_b) = (AccountId::new(da_b), AccountId::new(db_b));

    let amm_v2_prog = match load_deployed_program(
        "LDEX_AMM_V2_ELF",
        "amm-v2-methods/amm-v2-guest/riscv32im-risc0-zkvm-elf/release/amm_v2.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_v2_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let token_prog = Program::token();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(token_prog.id(), token_prog);
    let program = ProgramWithDependencies::new(amm_v2_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_pid, def_a, def_b, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        // Align (vault, holding, min-amount) legs to the pool's canonical
        // token-A: remove_liquidity keys vault_a/user_holding_a/min_a all
        // to reserve_a, so a reversed-order call must flip all three (the
        // LP leg uhlp/lp_def and remove_liquidity_amount are unaffected).
        let (vault_a, vault_b, h_a, h_b, min_a, min_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, uhb, uha,
                 min_amount_to_remove_token_b, min_amount_to_remove_token_a)
            } else {
                (p.vault_a, p.vault_b, uha, uhb,
                 min_amount_to_remove_token_a, min_amount_to_remove_token_b)
            };
        let accounts = vec![
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(vault_a),
            PrivacyPreservingAccount::Public(vault_b),
            PrivacyPreservingAccount::Public(lp_def),
            PrivacyPreservingAccount::PrivateOwned(h_a),
            PrivacyPreservingAccount::PrivateOwned(h_b),
            PrivacyPreservingAccount::PrivateOwned(uhlp),
        ];
        let instruction = amm_v2_core::Instruction::RemoveLiquidity {
            remove_liquidity_amount,
            min_amount_to_remove_token_a: min_a,
            min_amount_to_remove_token_b: min_b,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        // PRIV_LP (idx 6) must cover the burn.
        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let bal = pre
                .get(6)
                .and_then(|a| TokenHolding::try_from(&a.data).ok())
                .map(|h| match h {
                    TokenHolding::Fungible { balance, .. } => balance,
                    _ => 0,
                })
                .unwrap_or(0);
            if bal < remove_liquidity_amount {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// amm_v2 mode-2 disposable with native-LEZ input (LEZ → token).
/// 9-account layout, chains WLEZ::Wrap + 2 vault token::Transfer +
/// 1 reshield token::Transfer.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_disposable_swap_native_in(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    wlez_program_id: *const u8,
    user_native: *const u8,
    wlez_vault: *const u8,
    wlez_definition: *const u8,
    a_wlez_holding: *const u8,
    a_holding_out: *const u8,
    token_def_out: *const u8,
    user_holding_out: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(wlz_b)) = (
        read_id(amm_v2_program_id),
        read_id(wlez_program_id),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(un_b), Some(wv_b), Some(wd_b), Some(awh_b), Some(aho_b)) = (
        read_id(user_native),
        read_id(wlez_vault),
        read_id(wlez_definition),
        read_id(a_wlez_holding),
        read_id(a_holding_out),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(tdo_b), Some(uho_b)) = (read_id(token_def_out), read_id(user_holding_out)) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let wlez_pid = program_id_from_bytes(wlz_b);
    let user_native_id = AccountId::new(un_b);
    let wlez_vault_id_ = AccountId::new(wv_b);
    let wlez_def_id_ = AccountId::new(wd_b);
    let a_wlez_holding_id = AccountId::new(awh_b);
    let a_holding_out_id = AccountId::new(aho_b);
    let token_def_out_id = AccountId::new(tdo_b);
    let user_holding_out_id = AccountId::new(uho_b);

    let amm_v2_prog = match load_deployed_program(
        "LDEX_AMM_V2_ELF",
        "amm-v2-methods/amm-v2-guest/riscv32im-risc0-zkvm-elf/release/amm_v2.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_v2_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let wlez_prog = match load_deployed_program(
        "LDEX_WLEZ_ELF",
        "wlez-methods/wlez-guest/riscv32im-risc0-zkvm-elf/release/wlez.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if wlez_prog.id() != wlez_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let token_prog = Program::token();
    let auth_transfer_prog = Program::authenticated_transfer_program();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(wlez_prog.id(), wlez_prog);
    deps.insert(token_prog.id(), token_prog);
    deps.insert(auth_transfer_prog.id(), auth_transfer_prog);
    let program = ProgramWithDependencies::new(amm_v2_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        // Pool PDA derives from (amm_v2_pid, wlez_def, token_def_out, fees).
        let p = prep_private(&cfg, &store, amm_pid, wlez_def_id_, token_def_out_id, fees).await?;
        let mut p = p; align_prep_to_pool(&mut p, wlez_def_id_).await?;

        let accounts = vec![
            PrivacyPreservingAccount::Public(user_native_id),
            PrivacyPreservingAccount::Public(wlez_vault_id_),
            PrivacyPreservingAccount::Public(wlez_def_id_),
            PrivacyPreservingAccount::Public(a_wlez_holding_id),
            PrivacyPreservingAccount::Public(a_holding_out_id),
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(p.vault_a),
            PrivacyPreservingAccount::Public(p.vault_b),
            PrivacyPreservingAccount::PrivateOwned(user_holding_out_id),
        ];
        let instruction = amm_v2_core::Instruction::DisposableSwapNativeIn {
            swap_amount_in,
            min_amount_out,
            fees,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        // user_native is at pre_state index 0 — its native balance must
        // cover the wrap amount.
        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let acc = pre.first().ok_or(ExecutionFailureKind::AmountMismatchError)?;
            if acc.balance < swap_amount_in {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Wait for chain inclusion. Without this, the FFI returns "success"
        // the moment the mempool accepts the submit — a sequencer rejection
        // (proof invalid, conflict, etc.) is then indistinguishable from a
        // successful op to the caller. With poll, rejection → ERR_SUBMIT.
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// amm_v2 mode-2 disposable with native-LEZ output (token → LEZ).
/// 9-account layout, chains token::Transfer (deshield) + 2 vault
/// token::Transfer + WLEZ::Unwrap.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_disposable_swap_native_out(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    wlez_program_id: *const u8,
    user_holding_in: *const u8,
    a_holding_in: *const u8,
    a_wlez_holding: *const u8,
    wlez_definition: *const u8,
    wlez_vault: *const u8,
    user_native: *const u8,
    token_def_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(wlz_b)) = (
        read_id(amm_v2_program_id),
        read_id(wlez_program_id),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let (Some(uhi_b), Some(ahi_b), Some(awh_b), Some(wd_b), Some(wv_b), Some(un_b)) = (
        read_id(user_holding_in),
        read_id(a_holding_in),
        read_id(a_wlez_holding),
        read_id(wlez_definition),
        read_id(wlez_vault),
        read_id(user_native),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let Some(tdi_b) = read_id(token_def_in) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let amm_pid = program_id_from_bytes(pid_b);
    let wlez_pid = program_id_from_bytes(wlz_b);
    let user_holding_in_id = AccountId::new(uhi_b);
    let a_holding_in_id = AccountId::new(ahi_b);
    let a_wlez_holding_id = AccountId::new(awh_b);
    let wlez_def_id_ = AccountId::new(wd_b);
    let wlez_vault_id_ = AccountId::new(wv_b);
    let user_native_id = AccountId::new(un_b);
    let token_def_in_id = AccountId::new(tdi_b);

    let amm_v2_prog = match load_deployed_program(
        "LDEX_AMM_V2_ELF",
        "amm-v2-methods/amm-v2-guest/riscv32im-risc0-zkvm-elf/release/amm_v2.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if amm_v2_prog.id() != amm_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let wlez_prog = match load_deployed_program(
        "LDEX_WLEZ_ELF",
        "wlez-methods/wlez-guest/riscv32im-risc0-zkvm-elf/release/wlez.bin",
    ) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if wlez_prog.id() != wlez_pid {
        return LDEX_AMM_ERR_ACCOUNT;
    }
    let token_prog = Program::token();
    let auth_transfer_prog = Program::authenticated_transfer_program();
    let mut deps: HashMap<nssa_core::program::ProgramId, Program> = HashMap::new();
    deps.insert(wlez_prog.id(), wlez_prog);
    deps.insert(token_prog.id(), token_prog);
    deps.insert(auth_transfer_prog.id(), auth_transfer_prog);
    let program = ProgramWithDependencies::new(amm_v2_prog, deps);

    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let p = prep_private(&cfg, &store, amm_pid, token_def_in_id, wlez_def_id_, fees).await?;
        let mut p = p; align_prep_to_pool(&mut p, token_def_in_id).await?;

        let accounts = vec![
            PrivacyPreservingAccount::PrivateOwned(user_holding_in_id),
            PrivacyPreservingAccount::Public(a_holding_in_id),
            PrivacyPreservingAccount::Public(a_wlez_holding_id),
            PrivacyPreservingAccount::Public(p.pool),
            PrivacyPreservingAccount::Public(p.vault_a),
            PrivacyPreservingAccount::Public(p.vault_b),
            PrivacyPreservingAccount::Public(wlez_def_id_),
            PrivacyPreservingAccount::Public(wlez_vault_id_),
            PrivacyPreservingAccount::Public(user_native_id),
        ];
        let instruction = amm_v2_core::Instruction::DisposableSwapNativeOut {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: token_def_in_id,
            fees,
            deadline,
        };
        let instruction_data = Program::serialize_instruction(instruction)
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;

        let pre_check = move |pre: &[&nssa_core::account::Account]| {
            let acc = pre.first().ok_or(ExecutionFailureKind::AmountMismatchError)?;
            let bal = match TokenHolding::try_from(&acc.data) {
                Ok(TokenHolding::Fungible { balance, .. }) => balance,
                _ => 0,
            };
            if bal < swap_amount_in {
                return Err(ExecutionFailureKind::InsufficientFundsError);
            }
            Ok(())
        };

        let (hash, _secrets) = p
            .wallet
            .send_privacy_preserving_tx_with_pre_check(
                accounts,
                instruction_data,
                &program,
                pre_check,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Wait for chain inclusion. Without this, the FFI returns "success"
        // the moment the mempool accepts the submit — a sequencer rejection
        // (proof invalid, conflict, etc.) is then indistinguishable from a
        // successful op to the caller. With poll, rejection → ERR_SUBMIT.
        p.wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

// ============================================================================
//                  amm_v2 ATA variants (RFP Func #8 mode-0)
// ============================================================================
//
// Trader-side ATA-based mode-0 swaps + add liquidity through amm_v2. Same
// derivation rule as the canonical AMM (ATA per (owner, def) computed via
// the ATA program's PDA scheme), but the pool/vaults live under amm_v2's
// ProgramId and the account list omits Clock (amm_v2 pools skip the
// on-chain oracle).

#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_swap_exact_in_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    owner: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(da), Some(db), Some(tin)) = (
        read_id(amm_v2_program_id), read_id(owner),
        read_id(token_def_a), read_id(token_def_b),
        read_id(token_definition_in),
    ) else { return LDEX_AMM_ERR_NULL; };
    if out_tx_hash.is_null() { return LDEX_AMM_ERR_NULL; }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let def_a = AccountId::new(da);
    let def_b = AccountId::new(db);
    let tok_in = AccountId::new(tin);
    let rt = match runtime() { Ok(r) => r, Err(e) => return e };
    let res = rt.block_on(async move {
        let (ata_pid, ata_a, ata_b) = ata_env_ctx(owner_id, def_a, def_b)?;
        let p = prep(&cfg, &store, amm_pid, ata_a, ata_b, fees).await?;
        // Align the (vault, ata) legs to the pool's canonical token-A.
        // The handler asserts vault_a == pool.vault_a_id and pairs
        // vault_a with ata_a; if the caller passed the pair reversed,
        // flip both together (token_definition_id_in disambiguates the
        // swap direction independently).
        let (vault_a, vault_b, ata_a, ata_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, ata_b, ata_a)
            } else {
                (p.vault_a, p.vault_b, ata_a, ata_b)
            };
        let account_ids = vec![p.pool, vault_a, vault_b, owner_id, ata_a, ata_b];
        let instruction = amm_v2_core::Instruction::SwapExactInputAta {
            swap_amount_in, min_amount_out,
            token_definition_id_in: tok_in,
            ata_program_id: ata_pid, deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[owner_id], instruction).await
    });
    out32(res, out_tx_hash)
}

#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_swap_exact_out_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    owner: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    token_definition_in: *const u8,
    exact_amount_out: u128,
    max_amount_in: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(da), Some(db), Some(tin)) = (
        read_id(amm_v2_program_id), read_id(owner),
        read_id(token_def_a), read_id(token_def_b),
        read_id(token_definition_in),
    ) else { return LDEX_AMM_ERR_NULL; };
    if out_tx_hash.is_null() { return LDEX_AMM_ERR_NULL; }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let def_a = AccountId::new(da);
    let def_b = AccountId::new(db);
    let tok_in = AccountId::new(tin);
    let rt = match runtime() { Ok(r) => r, Err(e) => return e };
    let res = rt.block_on(async move {
        let (ata_pid, ata_a, ata_b) = ata_env_ctx(owner_id, def_a, def_b)?;
        let p = prep(&cfg, &store, amm_pid, ata_a, ata_b, fees).await?;
        // Align (vault, ata) legs to the pool's canonical token-A; see
        // ldex_amm_v2_swap_exact_in_ata.
        let (vault_a, vault_b, ata_a, ata_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, ata_b, ata_a)
            } else {
                (p.vault_a, p.vault_b, ata_a, ata_b)
            };
        let account_ids = vec![p.pool, vault_a, vault_b, owner_id, ata_a, ata_b];
        let instruction = amm_v2_core::Instruction::SwapExactOutputAta {
            exact_amount_out, max_amount_in,
            token_definition_id_in: tok_in,
            ata_program_id: ata_pid, deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[owner_id], instruction).await
    });
    out32(res, out_tx_hash)
}

#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_add_liquidity_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    owner: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    min_amount_liquidity: u128,
    max_amount_to_add_token_a: u128,
    max_amount_to_add_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(da), Some(db)) = (
        read_id(amm_v2_program_id), read_id(owner),
        read_id(token_def_a), read_id(token_def_b),
    ) else { return LDEX_AMM_ERR_NULL; };
    if out_tx_hash.is_null() { return LDEX_AMM_ERR_NULL; }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let def_a = AccountId::new(da);
    let def_b = AccountId::new(db);
    let rt = match runtime() { Ok(r) => r, Err(e) => return e };
    let res = rt.block_on(async move {
        let (ata_pid, ata_a, ata_b) = ata_env_ctx(owner_id, def_a, def_b)?;
        let p = prep(&cfg, &store, amm_pid, ata_a, ata_b, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        // ATA-LP holding derives from (owner, lp_def) under the same ATA program.
        let ata_lp = ata_core::get_associated_token_account_id(
            &ata_pid, &ata_core::compute_ata_seed(owner_id, lp_def));
        // Align (vault, ata, max-amount) legs to the pool's canonical
        // token-A: add_liquidity_ata keys vault_a/ata_a/max_a all to
        // reserve_a, so a reversed-order call must flip all three (ata_lp
        // is the LP leg, unaffected).
        let (vault_a, vault_b, ata_a, ata_b, max_a, max_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, ata_b, ata_a,
                 max_amount_to_add_token_b, max_amount_to_add_token_a)
            } else {
                (p.vault_a, p.vault_b, ata_a, ata_b,
                 max_amount_to_add_token_a, max_amount_to_add_token_b)
            };
        let account_ids = vec![p.pool, vault_a, vault_b, lp_def, owner_id, ata_a, ata_b, ata_lp];
        let instruction = amm_v2_core::Instruction::AddLiquidityAta {
            min_amount_liquidity, max_amount_to_add_token_a: max_a, max_amount_to_add_token_b: max_b,
            ata_program_id: ata_pid, deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[owner_id], instruction).await
    });
    out32(res, out_tx_hash)
}

/// amm_v2 NewDefinitionAta — create a new amm_v2 pool whose initial
/// user-side LP is minted straight into the user's deterministic
/// `ATA(owner, lp_def)` (RFP Func #8 on the LP holding). Token
/// deposits come from the user's keypair `user_holding_a/b` via
/// canonical `token::Transfer` (the brand-new vaults start default
/// and only the token program's PDA-claim path initialises them — an
/// `ata::Transfer` to a default destination is rejected at the ATA
/// program level).
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_new_pool_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    owner: *const u8,
    user_holding_a: *const u8,
    user_holding_b: *const u8,
    amount_a: u128,
    amount_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(ha), Some(hb)) = (
        read_id(amm_v2_program_id), read_id(owner),
        read_id(user_holding_a), read_id(user_holding_b),
    ) else { return LDEX_AMM_ERR_NULL; };
    if out_tx_hash.is_null() { return LDEX_AMM_ERR_NULL; }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let uha = AccountId::new(ha);
    let uhb = AccountId::new(hb);
    let rt = match runtime() { Ok(r) => r, Err(e) => return e };
    let res = rt.block_on(async move {
        let (ata_pid, _, _) = ata_env_ctx(owner_id, owner_id, owner_id)?;
        let p = prep(&cfg, &store, amm_pid, uha, uhb, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        let lp_lock = compute_lp_lock_holding_pda(amm_pid, p.pool);
        let ata_lp = ata_core::get_associated_token_account_id(
            &ata_pid, &ata_core::compute_ata_seed(owner_id, lp_def));
        // 9-account list (no Clock — amm_v2 skips oracle).
        let account_ids = vec![
            p.pool, p.vault_a, p.vault_b, lp_def, lp_lock,
            owner_id, uha, uhb, ata_lp,
        ];
        let instruction = amm_v2_core::Instruction::NewDefinitionAta {
            token_a_amount: amount_a, token_b_amount: amount_b,
            fees, ata_program_id: ata_pid, deadline,
        };
        // user_holding_a/b sign (canonical token::Transfer drains).
        finalize(&p.wallet, amm_pid, account_ids, &[uha, uhb], instruction).await
    });
    out32(res, out_tx_hash)
}

/// amm_v2 RemoveLiquidityAta — chain `ata::Burn` (owner-auth) for the
/// LP, then `token::Transfer` (vault PDA-auth) to return underlying
/// tokens into the user's ATAs.
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_amm_v2_remove_liquidity_ata(
    config_path: *const c_char,
    storage_path: *const c_char,
    amm_v2_program_id: *const u8,
    owner: *const u8,
    token_def_a: *const u8,
    token_def_b: *const u8,
    remove_liquidity_amount: u128,
    min_amount_to_remove_token_a: u128,
    min_amount_to_remove_token_b: u128,
    fees: u128,
    deadline: u64,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid), Some(o), Some(da), Some(db)) = (
        read_id(amm_v2_program_id), read_id(owner),
        read_id(token_def_a), read_id(token_def_b),
    ) else { return LDEX_AMM_ERR_NULL; };
    if out_tx_hash.is_null() { return LDEX_AMM_ERR_NULL; }
    let amm_pid = program_id_from_bytes(pid);
    let owner_id = AccountId::new(o);
    let def_a = AccountId::new(da);
    let def_b = AccountId::new(db);
    let rt = match runtime() { Ok(r) => r, Err(e) => return e };
    let res = rt.block_on(async move {
        let (ata_pid, ata_a, ata_b) = ata_env_ctx(owner_id, def_a, def_b)?;
        let p = prep(&cfg, &store, amm_pid, ata_a, ata_b, fees).await?;
        let lp_def = compute_liquidity_token_pda(amm_pid, p.pool);
        let ata_lp = ata_core::get_associated_token_account_id(
            &ata_pid, &ata_core::compute_ata_seed(owner_id, lp_def));
        // Align (vault, ata, min-amount) legs to the pool's canonical
        // token-A: remove_liquidity_ata keys vault_a/ata_a/min_a all to
        // reserve_a and asserts ata_a == ATA(owner, pool.token_a), so a
        // reversed-order call must flip all three (ata_lp is the LP leg).
        let (vault_a, vault_b, ata_a, ata_b, min_a, min_b) =
            if pool_needs_leg_flip(&p.wallet, p.pool, def_a).await? {
                (p.vault_b, p.vault_a, ata_b, ata_a,
                 min_amount_to_remove_token_b, min_amount_to_remove_token_a)
            } else {
                (p.vault_a, p.vault_b, ata_a, ata_b,
                 min_amount_to_remove_token_a, min_amount_to_remove_token_b)
            };
        let account_ids = vec![
            p.pool, vault_a, vault_b, lp_def,
            owner_id, ata_a, ata_b, ata_lp,
        ];
        let instruction = amm_v2_core::Instruction::RemoveLiquidityAta {
            remove_liquidity_amount,
            min_amount_to_remove_token_a: min_a,
            min_amount_to_remove_token_b: min_b,
            ata_program_id: ata_pid, deadline,
        };
        finalize(&p.wallet, amm_pid, account_ids, &[owner_id], instruction).await
    });
    out32(res, out_tx_hash)
}

/// `ata::Transfer` as a top-level public tx. Drains `amount` of the
/// underlying token from `sender_ata` (PDA-owned by the ATA program)
/// into `recipient` (any initialised TokenHolding for the same
/// definition). The owner authorises via signature; the ATA program
/// internally chains `token::Transfer` with the sender's PDA-seed.
///
/// Primary use case: moving WLEZ from `ATA(USER, WLEZ_DEF)` into the
/// keypair `HOLD_W` so the user can unwrap it (WLEZ::Unwrap requires
/// `user_holding.is_authorized`, which the wallet can't satisfy for
/// the PDA-owned ATA).
#[no_mangle]
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub unsafe extern "C" fn ldex_ata_transfer(
    config_path: *const c_char,
    storage_path: *const c_char,
    ata_program_id: *const u8,
    owner: *const u8,
    sender_ata: *const u8,
    recipient: *const u8,
    amount: u128,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(o_b), Some(s_b), Some(r_b)) = (
        read_id(ata_program_id),
        read_id(owner),
        read_id(sender_ata),
        read_id(recipient),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let ata_pid = program_id_from_bytes(pid_b);
    let (owner_id, sender_id, recipient_id) =
        (AccountId::new(o_b), AccountId::new(s_b), AccountId::new(r_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let wallet = WalletCore::new_update_chain(
            PathBuf::from(&cfg),
            PathBuf::from(&store),
            None,
        )
        .map_err(|_| LDEX_AMM_ERR_WALLET)?;
        let nonces = wallet
            .get_accounts_nonces(vec![owner_id])
            .await
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        let key = wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(owner_id)
            .ok_or(LDEX_AMM_ERR_KEY)?;
        let message = nssa::public_transaction::Message::try_new(
            ata_pid,
            vec![owner_id, sender_id, recipient_id],
            nonces,
            ata_core::Instruction::Transfer { amount },
        )
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let witness_set =
            nssa::public_transaction::WitnessSet::for_message(&message, &[key]);
        let tx = nssa::PublicTransaction::new(message, witness_set);
        let hash = wallet
            .sequencer_client
            .send_transaction(NSSATransaction::Public(tx))
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Same poll-for-inclusion guard as `finalize()`. Mempool accept
        // != ledger inclusion; without this, rc=0 was being returned for
        // txs the sequencer eventually rejected.
        wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// Manual shield (Public → PrivateOwned) for a FUNGIBLE TOKEN. Wraps
/// `Token::send_transfer_transaction_shielded_owned_account` so the user
/// can top up a private balance without going through a swap.
///
/// IMPORTANT: this is NOT the same primitive as
/// `wallet_ffi_transfer_shielded_owned` — that one targets the native LEZ
/// `authenticated_transfer_program` and refuses with InsufficientFunds
/// when used on a token holding (it checks `account.balance`, the
/// account's native field, which is always 0 for token holdings).
/// Tokens hold their amount in `account.data` under the token program.
///
/// Generates a STARK proof; under `RISC0_DEV_MODE=0` this takes tens of
/// seconds (simple privacy tx, no chained calls).
#[no_mangle]
pub unsafe extern "C" fn ldex_token_shield(
    config_path: *const c_char,
    storage_path: *const c_char,
    sender: *const u8,
    recipient: *const u8,
    amount: u128,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(s_b), Some(r_b)) = (read_id(sender), read_id(recipient)) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let (sender_id, recipient_id) = (AccountId::new(s_b), AccountId::new(r_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let mut wallet = WalletCore::new_update_chain(
            PathBuf::from(&cfg),
            PathBuf::from(&store),
            None,
        )
        .map_err(|_| LDEX_AMM_ERR_WALLET)?;
        // Replicates `Token::send_transfer_transaction_shielded_owned_account`
        // inline because the wallet's `program_facades::token` is
        // disabled in the LDEX fork (it imports upstream token_core
        // which collides with LDEX's locally-patched token_core).
        let instruction_data = Program::serialize_instruction(
            token_core::Instruction::Transfer { amount_to_transfer: amount },
        )
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let program: ProgramWithDependencies = Program::token().into();
        let (hash, secrets) = wallet
            .send_privacy_preserving_tx(
                vec![
                    PrivacyPreservingAccount::Public(sender_id),
                    PrivacyPreservingAccount::PrivateOwned(recipient_id),
                ],
                instruction_data,
                &program,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Don't return until the tx is included in a block — otherwise the
        // FFI returns "success" the moment the mempool accepts the tx, and
        // the caller (UI) reads stale balances. If the sequencer rejects
        // (e.g. dev/real proof mode mismatch), this surfaces as a poll
        // error -> LDEX_AMM_ERR_SUBMIT instead of a silent "success".
        let tx = wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Decode the recipient's new commitment + balance into the wallet's
        // local cache so walletTokens() sees the new PRIV balance
        // immediately. Without this the wallet only learns about the
        // shielded receipt later, when syncPrivateBalances scans the new
        // block — and that race made the UI claim "no shielded balance"
        // even after a successful shield.
        if let NSSATransaction::PrivacyPreserving(ppt) = tx {
            let secret = secrets
                .into_iter()
                .next()
                .ok_or(LDEX_AMM_ERR_SUBMIT)?;
            wallet
                .decode_insert_privacy_preserving_transaction_results(
                    &ppt,
                    &[AccDecodeData::Decode(secret, recipient_id)],
                )
                .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
            wallet
                .store_persistent_data()
                .await
                .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        }
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}

/// Manual deshield (PrivateOwned → Public) for a fungible token. Same
/// caveat as above: token-program-aware variant, not the native-LEZ one.
#[no_mangle]
pub unsafe extern "C" fn ldex_token_deshield(
    config_path: *const c_char,
    storage_path: *const c_char,
    sender: *const u8,
    recipient: *const u8,
    amount: u128,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(s_b), Some(r_b)) = (read_id(sender), read_id(recipient)) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let (sender_id, recipient_id) = (AccountId::new(s_b), AccountId::new(r_b));
    let rt = match runtime() {
        Ok(r) => r,
        Err(e) => return e,
    };
    let res: Result<[u8; 32], i32> = rt.block_on(async move {
        let mut wallet = WalletCore::new_update_chain(
            PathBuf::from(&cfg),
            PathBuf::from(&store),
            None,
        )
        .map_err(|_| LDEX_AMM_ERR_WALLET)?;
        let instruction_data = Program::serialize_instruction(
            token_core::Instruction::Transfer { amount_to_transfer: amount },
        )
        .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        let program: ProgramWithDependencies = Program::token().into();
        let (hash, secrets) = wallet
            .send_privacy_preserving_tx(
                vec![
                    PrivacyPreservingAccount::PrivateOwned(sender_id),
                    PrivacyPreservingAccount::Public(recipient_id),
                ],
                instruction_data,
                &program,
            )
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        // Wait for inclusion + update wallet's local cache for the sender
        // PRIV (its new post-spend balance). See ldex_token_shield for the
        // full reasoning; same race applies here.
        let tx = wallet
            .poll_native_token_transfer(hash)
            .await
            .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        if let NSSATransaction::PrivacyPreserving(ppt) = tx {
            let secret = secrets
                .into_iter()
                .next()
                .ok_or(LDEX_AMM_ERR_SUBMIT)?;
            wallet
                .decode_insert_privacy_preserving_transaction_results(
                    &ppt,
                    &[AccDecodeData::Decode(secret, sender_id)],
                )
                .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
            wallet
                .store_persistent_data()
                .await
                .map_err(|_| LDEX_AMM_ERR_SUBMIT)?;
        }
        let mut out = [0u8; 32];
        let h: &[u8] = hash.as_ref();
        if h.len() == 32 {
            out.copy_from_slice(h);
        }
        Ok(out)
    });
    out32(res, out_tx_hash)
}
