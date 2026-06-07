//! C-ABI shim exposing the LDEX (fee-tier) AMM to the native `ldex_core`
//! Basecamp module.
//!
//! Step #15.1 - foundational slice: pure, fee-tier-aware PDA derivations
//! using *our* forked `amm_core` (`programs/amm`), proving the dependency
//! graph (our `amm_core` + rc3 `nssa` types) links as a `cdylib` over the C
//! ABI. Signed-submit ops (new_pool / swap / add / remove liquidity) build
//! on this and land next (they add `nssa`/`wallet`/sequencer-client).
//!
//! All ids are 32-byte big-endian-agnostic raw account/program id bytes,
//! exactly as the rest of the system passes them. `ProgramId = [u32; 8]`
//! and is hashed as its native-endian bytes inside `for_public_pda`, so we
//! reconstruct it the same way (`from_ne_bytes` per 4-byte lane).

use amm_core::{
    compute_liquidity_token_pda, compute_lp_lock_holding_pda, compute_pool_pda, compute_vault_pda,
};
use nssa_core::{account::AccountId, program::ProgramId};

mod submit;
mod wlez;
pub use submit::{
    ldex_amm_add_liquidity, ldex_amm_disposable_swap_exact_in,
    ldex_amm_new_pool, ldex_amm_new_pool_ata,
    ldex_amm_init_token_holding, ldex_amm_onchain_price_history, ldex_amm_pool_info,
    ldex_amm_price_history,
    ldex_amm_private_add_liquidity, ldex_amm_private_remove_liquidity,
    ldex_amm_add_liquidity_ata, ldex_amm_private_swap_exact_in, ldex_amm_remove_liquidity,
    ldex_amm_v2_add_liquidity, ldex_amm_v2_add_liquidity_ata,
    ldex_amm_v2_disposable_swap, ldex_amm_v2_disposable_swap_inproof,
    ldex_amm_v2_disposable_swap_native_in, ldex_amm_v2_disposable_swap_native_out,
    ldex_amm_v2_new_pool, ldex_amm_v2_new_pool_ata,
    ldex_amm_v2_private_swap_exact_in,
    ldex_amm_v2_remove_liquidity, ldex_amm_v2_remove_liquidity_ata,
    ldex_amm_v2_swap_exact_in,
    ldex_amm_v2_swap_exact_in_ata, ldex_amm_v2_swap_exact_out_ata,
    ldex_amm_remove_liquidity_ata, ldex_amm_swap_exact_in, ldex_amm_swap_exact_in_ata,
    ldex_amm_swap_exact_out_ata,
    ldex_amm_swap_exact_out, ldex_amm_token_balance,
    ldex_amm_volume_estimate, ldex_amm_v2_private_add_liquidity,
    ldex_amm_v2_private_remove_liquidity,
    ldex_ata_create, ldex_ata_transfer,
    ldex_token_deshield, ldex_token_shield,
};
pub use wlez::{
    ldex_wlez_definition_id, ldex_wlez_initialize, ldex_wlez_unwrap, ldex_wlez_vault_id,
    ldex_wlez_wrap,
};

/// Return codes for every `extern "C"` function.
pub const LDEX_AMM_OK: i32 = 0;
pub const LDEX_AMM_ERR_NULL: i32 = 1;
/// Wallet could not be opened (bad config/storage path or storage).
pub const LDEX_AMM_ERR_WALLET: i32 = 2;
/// On-chain read / account-data decode failed.
pub const LDEX_AMM_ERR_ACCOUNT: i32 = 3;
/// Signing key for a required account not found in the wallet.
pub const LDEX_AMM_ERR_KEY: i32 = 4;
/// Transaction build or sequencer submission failed.
pub const LDEX_AMM_ERR_SUBMIT: i32 = 5;
/// Invalid UTF-8 in a string argument.
pub const LDEX_AMM_ERR_UTF8: i32 = 6;

/// SAFETY: caller guarantees `ptr` points to at least 32 readable bytes.
pub(crate) unsafe fn read_id(ptr: *const u8) -> Option<[u8; 32]> {
    if ptr.is_null() {
        return None;
    }
    let mut out = [0u8; 32];
    std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), 32);
    Some(out)
}

/// SAFETY: caller guarantees `out` points to at least 32 writable bytes.
pub(crate) unsafe fn write_id(out: *mut u8, id: &AccountId) -> i32 {
    if out.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    std::ptr::copy_nonoverlapping(id.value().as_ptr(), out, 32);
    LDEX_AMM_OK
}

/// 32 program-id bytes -> `[u32; 8]`, matching the `bytemuck` reinterpret
/// used by `AccountId::for_public_pda` (native-endian lanes).
pub(crate) fn program_id_from_bytes(b: [u8; 32]) -> ProgramId {
    let mut pid = [0u32; 8];
    for (i, lane) in pid.iter_mut().enumerate() {
        let mut w = [0u8; 4];
        w.copy_from_slice(&b[i * 4..i * 4 + 4]);
        *lane = u32::from_ne_bytes(w);
    }
    pid
}

/// Pool PDA for `(token_a, token_b, fee_tier)` - distinct per fee tier so
/// pools for the same pair coexist (RFP-004 Func #6).
///
/// # Safety
/// All pointers must be non-null and point to 32 bytes (`out` writable).
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_pool_id(
    amm_program_id: *const u8,
    token_a_def: *const u8,
    token_b_def: *const u8,
    fees: u128,
    out: *mut u8,
) -> i32 {
    let (Some(pid), Some(a), Some(b)) = (
        read_id(amm_program_id),
        read_id(token_a_def),
        read_id(token_b_def),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let pool = compute_pool_pda(
        program_id_from_bytes(pid),
        AccountId::new(a),
        AccountId::new(b),
        fees,
    );
    write_id(out, &pool)
}

/// Vault PDA for a token within a pool.
///
/// # Safety
/// All pointers must be non-null and point to 32 bytes (`out` writable).
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_vault_id(
    amm_program_id: *const u8,
    pool_id: *const u8,
    token_def: *const u8,
    out: *mut u8,
) -> i32 {
    let (Some(pid), Some(pool), Some(tok)) = (
        read_id(amm_program_id),
        read_id(pool_id),
        read_id(token_def),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let vault = compute_vault_pda(
        program_id_from_bytes(pid),
        AccountId::new(pool),
        AccountId::new(tok),
    );
    write_id(out, &vault)
}

/// Liquidity-token-definition PDA for a pool.
///
/// # Safety
/// All pointers must be non-null and point to 32 bytes (`out` writable).
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_lp_definition_id(
    amm_program_id: *const u8,
    pool_id: *const u8,
    out: *mut u8,
) -> i32 {
    let (Some(pid), Some(pool)) = (read_id(amm_program_id), read_id(pool_id)) else {
        return LDEX_AMM_ERR_NULL;
    };
    let lp = compute_liquidity_token_pda(program_id_from_bytes(pid), AccountId::new(pool));
    write_id(out, &lp)
}

/// LP-lock holding PDA for a pool.
///
/// # Safety
/// All pointers must be non-null and point to 32 bytes (`out` writable).
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_lp_lock_id(
    amm_program_id: *const u8,
    pool_id: *const u8,
    out: *mut u8,
) -> i32 {
    let (Some(pid), Some(pool)) = (read_id(amm_program_id), read_id(pool_id)) else {
        return LDEX_AMM_ERR_NULL;
    };
    let lock = compute_lp_lock_holding_pda(program_id_from_bytes(pid), AccountId::new(pool));
    write_id(out, &lock)
}

/// Deterministic Associated Token Account id for `(owner, mint)`
/// (RFP-004 Func #8). Pure - `for_public_pda(ata_pid,
/// sha256(owner ‖ mint))`. Any integrator can recompute a user's token
/// holding address from just the owner and the token definition.
///
/// # Safety
/// All pointers non-null, 32 bytes (`out` writable).
#[no_mangle]
pub unsafe extern "C" fn ldex_ata_id(
    ata_program_id: *const u8,
    owner: *const u8,
    token_def: *const u8,
    out: *mut u8,
) -> i32 {
    let (Some(pid), Some(o), Some(d)) = (
        read_id(ata_program_id),
        read_id(owner),
        read_id(token_def),
    ) else {
        return LDEX_AMM_ERR_NULL;
    };
    let seed = ata_core::compute_ata_seed(AccountId::new(o), AccountId::new(d));
    let ata = ata_core::get_associated_token_account_id(&program_id_from_bytes(pid), &seed);
    write_id(out, &ata)
}

/// Parse an account/program id string into 32 bytes. Accepts the forms ids
/// actually appear in: `Public/<base58>`, `Private/<base58>`, bare
/// `<base58>`, or 64-char hex. Lets the UI pass exactly what's in
/// `bootstrap.env` / wallet output - no manual hex conversion.
///
/// # Safety
/// `s` is a NUL-terminated UTF-8 string; `out` points to 32 writable bytes.
#[no_mangle]
pub unsafe extern "C" fn ldex_amm_parse_account_id(
    s: *const core::ffi::c_char,
    out: *mut u8,
) -> i32 {
    if s.is_null() || out.is_null() {
        return LDEX_AMM_ERR_NULL;
    }
    let Ok(raw) = core::ffi::CStr::from_ptr(s).to_str() else {
        return 6; // LDEX_AMM_ERR_UTF8
    };
    let raw = raw.trim();
    let bare = raw.rsplit('/').next().unwrap_or(raw); // strip Public/ Private/
    let mut bytes = [0u8; 32];
    if bare.len() == 64 && bare.bytes().all(|c| c.is_ascii_hexdigit()) {
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&bare[i * 2..i * 2 + 2], 16).unwrap();
        }
    } else {
        match bare.parse::<AccountId>() {
            Ok(id) => bytes = *id.value(),
            Err(_) => return 3, // LDEX_AMM_ERR_ACCOUNT (unparseable id)
        }
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 32);
    LDEX_AMM_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_id_distinct_per_fee_tier() {
        let pid = [7u8; 32];
        let a = [42u8; 32];
        let b = [43u8; 32];
        let mut p5 = [0u8; 32];
        let mut p30 = [0u8; 32];
        unsafe {
            assert_eq!(
                ldex_amm_pool_id(pid.as_ptr(), a.as_ptr(), b.as_ptr(), 5, p5.as_mut_ptr()),
                LDEX_AMM_OK
            );
            assert_eq!(
                ldex_amm_pool_id(pid.as_ptr(), a.as_ptr(), b.as_ptr(), 30, p30.as_mut_ptr()),
                LDEX_AMM_OK
            );
        }
        assert_ne!(p5, p30, "fee tiers must yield distinct pool ids");
        assert_ne!(p5, [0u8; 32]);
    }

    #[test]
    fn null_pointers_rejected() {
        let mut out = [0u8; 32];
        let ok = [0u8; 32];
        unsafe {
            assert_eq!(
                ldex_amm_pool_id(
                    std::ptr::null(),
                    ok.as_ptr(),
                    ok.as_ptr(),
                    30,
                    out.as_mut_ptr()
                ),
                LDEX_AMM_ERR_NULL
            );
        }
    }
}
