//! C-ABI shim for the Wrapped LEZ (WLEZ) program. Mirrors the structure
//! of `submit.rs` for the AMM. Four entry points:
//!
//!   - `ldex_wlez_definition_id` - pure derivation (no chain call).
//!   - `ldex_wlez_vault_id`      - pure derivation (no chain call).
//!   - `ldex_wlez_initialize`    - submits `wlez::Initialize` (one-shot at deploy time).
//!   - `ldex_wlez_wrap`          - submits `wlez::Wrap{amount}` (user signs).
//!   - `ldex_wlez_unwrap`        - submits `wlez::Unwrap{amount}` (user signs).
//!
//! All ids are 32-byte raw bytes. Return codes match the LDEX_AMM_* enum
//! in lib.rs so the mini-app's `rcMessage` helper produces consistent UI
//! text across both the AMM and WLEZ paths.

use std::ffi::{c_char, CStr};
use std::path::PathBuf;

use common::transaction::NSSATransaction;
use nssa_core::account::AccountId;
use sequencer_service_rpc::RpcClient as _;
use wallet::WalletCore;
use wlez_core::{
    get_wlez_definition_id, get_wlez_init_holding_id, get_wlez_vault_id, Instruction,
};

use crate::{
    program_id_from_bytes, read_id, write_id, LDEX_AMM_ERR_ACCOUNT, LDEX_AMM_ERR_KEY,
    LDEX_AMM_ERR_NULL, LDEX_AMM_ERR_SUBMIT, LDEX_AMM_ERR_UTF8, LDEX_AMM_ERR_WALLET, LDEX_AMM_OK,
};

unsafe fn c_str(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(str::to_owned)
}

// Shared multi-thread tokio runtime. See submit.rs::runtime for why this
// must be shared rather than built per call (mini-app QtRO timeout).
fn runtime() -> Result<&'static tokio::runtime::Runtime, i32> {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    Ok(RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime build failed (LDEX FFI init)")
    }))
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

// ── Pure PDA derivations ────────────────────────────────────────────

/// Deterministic WLEZ token-definition id for a deployed WLEZ program.
/// Pure - no chain call. Used by bootstrap and the mini-app to know
/// where to expect the WLEZ definition account.
///
/// # Safety
/// `wlez_program_id` and `out` must be non-null and point to 32 bytes
/// (`out` writable).
#[no_mangle]
pub unsafe extern "C" fn ldex_wlez_definition_id(
    wlez_program_id: *const u8,
    out: *mut u8,
) -> i32 {
    let Some(pid_b) = read_id(wlez_program_id) else {
        return LDEX_AMM_ERR_NULL;
    };
    let pid = program_id_from_bytes(pid_b);
    write_id(out, &get_wlez_definition_id(&pid))
}

/// Deterministic WLEZ vault account id for a deployed WLEZ program.
/// Pure - no chain call.
///
/// # Safety
/// `wlez_program_id` and `out` must be non-null and point to 32 bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_wlez_vault_id(
    wlez_program_id: *const u8,
    out: *mut u8,
) -> i32 {
    let Some(pid_b) = read_id(wlez_program_id) else {
        return LDEX_AMM_ERR_NULL;
    };
    let pid = program_id_from_bytes(pid_b);
    write_id(out, &get_wlez_vault_id(&pid))
}

// ── Submit ops ──────────────────────────────────────────────────────

/// Submit a `wlez::Initialize` instruction. One-shot per deployment;
/// the instruction itself is idempotent so re-running this call is safe.
/// `payer_holding` is any account the wallet has the signing key for -
/// the framework requires at least one signer for fee payment, and
/// Initialize itself doesn't read it. Pass any keypair-derived account
/// (e.g. the wallet's owner account).
///
/// `init_holding` is the program-derived account that token_program will
/// claim during NewFungibleDefinition; its id is derived from the WLEZ
/// program id (same `for_public_pda` trick the vault/definition use).
///
/// # Safety
/// All pointers non-null with 32 readable bytes (`out_tx_hash` writable).
#[no_mangle]
pub unsafe extern "C" fn ldex_wlez_initialize(
    config_path: *const c_char,
    storage_path: *const c_char,
    wlez_program_id: *const u8,
    reference_token_def: *const u8,
    payer_holding: *const u8,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(ref_b), Some(payer_b)) = (
        read_id(wlez_program_id),
        read_id(reference_token_def),
        read_id(payer_holding),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let wlez_pid = program_id_from_bytes(pid_b);
    let vault_id = get_wlez_vault_id(&wlez_pid);
    let def_id = get_wlez_definition_id(&wlez_pid);
    let init_holding_id = get_wlez_init_holding_id(&wlez_pid);
    let ref_id = AccountId::new(ref_b);
    let payer_id = AccountId::new(payer_b);

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
        let signers = [payer_id];
        let nonces = wallet
            .get_accounts_nonces(signers.to_vec())
            .await
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        let key = wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(payer_id)
            .ok_or(LDEX_AMM_ERR_KEY)?;
        // Account order: PDAs first (vault, definition, init_holding,
        // reference_token_def), then signer (payer). The first 4 are
        // non-signers; the framework treats `accounts[accounts.len() -
        // nonces.len() ..]` as signers, so payer at the end with
        // nonces.len()==1 makes payer the sole signer.
        let message = nssa::public_transaction::Message::try_new(
            wlez_pid,
            vec![vault_id, def_id, init_holding_id, ref_id, payer_id],
            nonces,
            Instruction::Initialize {
                // Pin the canonical token + native programs so a malicious
                // reference/native account can't redirect WLEZ accounting or
                // mint unbacked WLEZ (Wrap checks user_native against the
                // native id stored in the vault at Initialize).
                token_program_id: nssa::program::Program::token().id(),
                native_program_id: nssa::program::Program::authenticated_transfer_program().id(),
            },
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
        // Block until inclusion so the FFI surfaces sequencer rejection
        // (e.g. wlez wrap's wire-format issue) as ERR_SUBMIT rather than
        // returning rc=0 for a tx that never lands.
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

/// Lock `amount` native LEZ from `user_native_account` into the WLEZ
/// vault and mint `amount` WLEZ into `user_wlez_holding`. Caller is the
/// signer for `user_native_account` (wallet must hold its key).
///
/// `user_wlez_holding` must already be initialised for the WLEZ
/// definition (use `ldex_amm_init_token_holding` against the WLEZ def
/// id beforehand if it isn't).
///
/// # Safety
/// All pointers non-null; ids point to 32 readable bytes; `out_tx_hash`
/// writable.
#[no_mangle]
pub unsafe extern "C" fn ldex_wlez_wrap(
    config_path: *const c_char,
    storage_path: *const c_char,
    wlez_program_id: *const u8,
    user_native_account: *const u8,
    user_wlez_holding: *const u8,
    amount: u128,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(un_b), Some(uh_b)) = (
        read_id(wlez_program_id),
        read_id(user_native_account),
        read_id(user_wlez_holding),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let wlez_pid = program_id_from_bytes(pid_b);
    let user_id = AccountId::new(un_b);
    let user_holding_id = AccountId::new(uh_b);
    let vault_id = get_wlez_vault_id(&wlez_pid);
    let def_id = get_wlez_definition_id(&wlez_pid);

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
        // Only signer is the user's native account.
        let signers = [user_id];
        let nonces = wallet
            .get_accounts_nonces(signers.to_vec())
            .await
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        let key = wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(user_id)
            .ok_or(LDEX_AMM_ERR_KEY)?;
        // Account order matches the guest's wrap dispatcher signature.
        let message = nssa::public_transaction::Message::try_new(
            wlez_pid,
            vec![user_id, vault_id, def_id, user_holding_id],
            nonces,
            Instruction::Wrap { amount },
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
        // Block until inclusion so the FFI surfaces sequencer rejection
        // (e.g. wlez wrap's wire-format issue) as ERR_SUBMIT rather than
        // returning rc=0 for a tx that never lands.
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

/// Burn `amount` WLEZ from `user_wlez_holding` and release `amount`
/// native LEZ back to `user_native_account`. Caller is the signer for
/// `user_wlez_holding` (wallet must hold its key).
///
/// # Safety
/// All pointers non-null; ids point to 32 readable bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_wlez_unwrap(
    config_path: *const c_char,
    storage_path: *const c_char,
    wlez_program_id: *const u8,
    user_wlez_holding: *const u8,
    user_native_account: *const u8,
    amount: u128,
    out_tx_hash: *mut u8,
) -> i32 {
    let (Some(cfg), Some(store)) = (c_str(config_path), c_str(storage_path)) else {
        return LDEX_AMM_ERR_UTF8;
    };
    let (Some(pid_b), Some(uh_b), Some(un_b)) = (
        read_id(wlez_program_id),
        read_id(user_wlez_holding),
        read_id(user_native_account),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    if out_tx_hash.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let wlez_pid = program_id_from_bytes(pid_b);
    let user_id = AccountId::new(un_b);
    let user_holding_id = AccountId::new(uh_b);
    let vault_id = get_wlez_vault_id(&wlez_pid);
    let def_id = get_wlez_definition_id(&wlez_pid);

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
        // Only signer is the user's WLEZ holding.
        let signers = [user_holding_id];
        let nonces = wallet
            .get_accounts_nonces(signers.to_vec())
            .await
            .map_err(|_| LDEX_AMM_ERR_ACCOUNT)?;
        let key = wallet
            .storage()
            .user_data
            .get_pub_account_signing_key(user_holding_id)
            .ok_or(LDEX_AMM_ERR_KEY)?;
        // Account order matches the guest's unwrap dispatcher signature.
        let message = nssa::public_transaction::Message::try_new(
            wlez_pid,
            vec![user_holding_id, def_id, vault_id, user_id],
            nonces,
            Instruction::Unwrap { amount },
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
        // Block until inclusion so the FFI surfaces sequencer rejection
        // (e.g. wlez wrap's wire-format issue) as ERR_SUBMIT rather than
        // returning rc=0 for a tx that never lands.
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
