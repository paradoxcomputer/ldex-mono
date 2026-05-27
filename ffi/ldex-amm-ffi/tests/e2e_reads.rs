//! E2E sanity for the chain-state reads used by the UI's balances/pools.
//! Gated by LDEX_E2E=1 + sourced scripts/bootstrap.env. Run:
//!   source scripts/bootstrap.env && LDEX_E2E=1 \
//!     cargo test -p ldex-amm-ffi --release --test e2e_reads -- --nocapture

use std::ffi::CString;

fn b58_to_32(s: &str) -> [u8; 32] {
    let bare = s.split('/').next_back().unwrap();
    let id: nssa_core::account::AccountId = bare.parse().expect("b58 id");
    *id.value()
}
fn hex_to_32(s: &str) -> [u8; 32] {
    let mut o = [0u8; 32];
    for (i, b) in o.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    o
}
fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

#[test]
fn e2e_pool_info_and_balance() {
    if env("LDEX_E2E") != "1" {
        eprintln!("skip (LDEX_E2E!=1)");
        return;
    }
    let cfg = CString::new(env("LDEX_WALLET_CONFIG")).unwrap();
    let st = CString::new(env("LDEX_WALLET_STORAGE")).unwrap();
    let amm = hex_to_32(&env("LDEX_AMM_PROGRAM_ID"));
    let da = b58_to_32(&env("LDEX_DEF_A"));
    let db = b58_to_32(&env("LDEX_DEF_B"));
    let ha = b58_to_32(&env("LDEX_USER_HOLDING_A"));

    for fee in [1u128, 5, 30, 100] {
        let mut buf = [0u8; 512];
        let rc = unsafe {
            ldex_amm_ffi::ldex_amm_pool_info(
                cfg.as_ptr(), st.as_ptr(), amm.as_ptr(), da.as_ptr(), db.as_ptr(),
                fee, buf.as_mut_ptr(), buf.len(),
            )
        };
        let s = std::str::from_utf8(&buf).unwrap().trim_end_matches('\0');
        eprintln!("pool_info fee={fee} rc={rc} -> {s}");
    }
    let mut bb = [0u8; 256];
    let rc = unsafe {
        ldex_amm_ffi::ldex_amm_token_balance(
            cfg.as_ptr(), st.as_ptr(), ha.as_ptr(), bb.as_mut_ptr(), bb.len(),
        )
    };
    let s = std::str::from_utf8(&bb).unwrap().trim_end_matches('\0');
    eprintln!("token_balance(HOLDING_A) rc={rc} -> {s}");
    assert_eq!(rc, ldex_amm_ffi::LDEX_AMM_OK);
}
