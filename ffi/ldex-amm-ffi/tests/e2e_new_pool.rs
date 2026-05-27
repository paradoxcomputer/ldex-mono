//! E2E: drive the shim's `ldex_amm_new_pool` against a *running* standalone
//! sequencer using the wallet/tokens created by `scripts/bootstrap.sh`.
//!
//! Gated: only runs when `LDEX_E2E=1` and the `LDEX_*` env vars from
//! `scripts/bootstrap.env` are present, so a plain `cargo test` never tries
//! to hit a sequencer. Run:
//!
//!   source scripts/bootstrap.env && LDEX_E2E=1 \
//!     cargo test -p ldex-amm-ffi --release --test e2e_new_pool -- --nocapture
//!
//! Proves the full path: open the bootstrapped wallet → read on-chain token
//! holdings → derive fee-tier PDAs → sign with wallet keys → submit to the
//! sequencer → get a tx hash.

use std::ffi::CString;

fn b58_to_32(s: &str) -> [u8; 32] {
    // env ids look like "Public/<base58>"; AccountId::from_str wants the
    // bare base58 (32 bytes, no prefix).
    let bare = s.split('/').next_back().unwrap();
    let id: nssa_core::account::AccountId = bare.parse().expect("valid base58 account id");
    *id.value()
}

fn hex_to_32(s: &str) -> [u8; 32] {
    assert_eq!(s.len(), 64, "program id must be 64 hex chars");
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
    }
    out
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn e2e_new_pool_against_running_sequencer() {
    if env("LDEX_E2E").as_deref() != Some("1") {
        eprintln!("skipping e2e (set LDEX_E2E=1 + source scripts/bootstrap.env)");
        return;
    }
    let cfg = CString::new(env("LDEX_WALLET_CONFIG").expect("LDEX_WALLET_CONFIG")).unwrap();
    let store = CString::new(env("LDEX_WALLET_STORAGE").expect("LDEX_WALLET_STORAGE")).unwrap();
    let amm = hex_to_32(&env("LDEX_AMM_PROGRAM_ID").expect("LDEX_AMM_PROGRAM_ID"));
    let uha = b58_to_32(&env("LDEX_USER_HOLDING_A").expect("LDEX_USER_HOLDING_A"));
    let uhb = b58_to_32(&env("LDEX_USER_HOLDING_B").expect("LDEX_USER_HOLDING_B"));
    let uhlp = b58_to_32(&env("LDEX_USER_HOLDING_LP").expect("LDEX_USER_HOLDING_LP"));

    // FEE_TIER_BPS_30 (0.3%); amounts well under the minted 1_000_000.
    let fees: u128 = 30;
    let (amount_a, amount_b): (u128, u128) = (100_000, 200_000);
    let deadline: u64 = u64::MAX;

    // Bootstrap's token-mint txs may still be confirming; retry on
    // ERR_ACCOUNT (3) which means the holding isn't a funded token holding
    // on-chain yet.
    let mut last = -1;
    for attempt in 0..12 {
        let mut tx_hash = [0u8; 32];
        let rc = unsafe {
            ldex_amm_ffi::ldex_amm_new_pool(
                cfg.as_ptr(),
                store.as_ptr(),
                amm.as_ptr(),
                uha.as_ptr(),
                uhb.as_ptr(),
                uhlp.as_ptr(),
                amount_a,
                amount_b,
                fees,
                deadline,
                tx_hash.as_mut_ptr(),
            )
        };
        last = rc;
        eprintln!("attempt {attempt}: ldex_amm_new_pool rc={rc}");
        if rc == ldex_amm_ffi::LDEX_AMM_OK {
            assert_ne!(tx_hash, [0u8; 32], "expected a non-zero tx hash");
            eprintln!(
                "OK — pool-create tx hash = {}",
                tx_hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            return;
        }
        if rc != ldex_amm_ffi::LDEX_AMM_ERR_ACCOUNT {
            break; // a non-timing error — fail fast with the code
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
    panic!("ldex_amm_new_pool did not succeed; last rc={last}");
}
