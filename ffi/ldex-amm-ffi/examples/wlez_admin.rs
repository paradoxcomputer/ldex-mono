//! WLEZ shell admin - drives the WLEZ FFI directly so bootstrap.sh can
//! deploy + initialise + pre-wrap WLEZ without learning Rust.
//!
//!   cargo run -q --release --example wlez_admin -- defid   <wlez_program_id>
//!   cargo run -q --release --example wlez_admin -- vaultid <wlez_program_id>
//!   cargo run -q --release --example wlez_admin -- initialize <cfg> <store> <wlez_program_id> <ref_token_def> <payer_holding>
//!   cargo run -q --release --example wlez_admin -- wrap        <cfg> <store> <wlez_program_id> <user_native> <user_wlez_holding> <amount>
//!   cargo run -q --release --example wlez_admin -- unwrap      <cfg> <store> <wlez_program_id> <user_wlez_holding> <user_native> <amount>
//!
//! All ids accept `Public/<b58>`, `Private/<b58>`, `<b58>`, or 64-hex.
//! For program ids you can also pass the 64-hex form printed by
//! `amm_program_id`. Pure ops (`defid`, `vaultid`) print 64-hex to
//! stdout; submit ops print `rc=<n> tx=<64hex>` (rc=0 on success).

use std::ffi::CString;

use ldex_amm_ffi::{
    ldex_amm_parse_account_id, ldex_wlez_definition_id, ldex_wlez_initialize, ldex_wlez_unwrap,
    ldex_wlez_vault_id, ldex_wlez_wrap, LDEX_AMM_OK,
};

fn hx(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// Accept a Public/Private base58 id, bare b58, or 64-hex.
fn id32(s: &str) -> [u8; 32] {
    // First try hex.
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
        }
        return out;
    }
    // Otherwise go through the FFI parser (handles Public/<b58> etc).
    let c = CString::new(s).unwrap();
    let mut out = [0u8; 32];
    let rc = unsafe { ldex_amm_parse_account_id(c.as_ptr(), out.as_mut_ptr()) };
    assert_eq!(rc, LDEX_AMM_OK, "could not parse id {s} (rc={rc})");
    out
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let op = a.get(1).map(String::as_str).unwrap_or("");

    match op {
        "defid" | "def-id" => {
            let pid = id32(&a[2]);
            let mut out = [0u8; 32];
            let rc = unsafe { ldex_wlez_definition_id(pid.as_ptr(), out.as_mut_ptr()) };
            assert_eq!(rc, LDEX_AMM_OK, "defid failed (rc={rc})");
            println!("{}", hx(&out));
        }
        "vaultid" | "vault-id" => {
            let pid = id32(&a[2]);
            let mut out = [0u8; 32];
            let rc = unsafe { ldex_wlez_vault_id(pid.as_ptr(), out.as_mut_ptr()) };
            assert_eq!(rc, LDEX_AMM_OK, "vaultid failed (rc={rc})");
            println!("{}", hx(&out));
        }
        "initialize" | "init" => {
            let cfg = CString::new(a[2].clone()).unwrap();
            let store = CString::new(a[3].clone()).unwrap();
            let pid = id32(&a[4]);
            let ref_def = id32(&a[5]);
            let payer = id32(&a[6]);
            let mut tx = [0u8; 32];
            let rc = unsafe {
                ldex_wlez_initialize(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    pid.as_ptr(),
                    ref_def.as_ptr(),
                    payer.as_ptr(),
                    tx.as_mut_ptr(),
                )
            };
            println!("initialize rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        "wrap" => {
            let cfg = CString::new(a[2].clone()).unwrap();
            let store = CString::new(a[3].clone()).unwrap();
            let pid = id32(&a[4]);
            let user_native = id32(&a[5]);
            let user_holding = id32(&a[6]);
            let amount: u128 = a[7].parse().expect("amount must be a u128");
            let mut tx = [0u8; 32];
            let rc = unsafe {
                ldex_wlez_wrap(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    pid.as_ptr(),
                    user_native.as_ptr(),
                    user_holding.as_ptr(),
                    amount,
                    tx.as_mut_ptr(),
                )
            };
            println!("wrap rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        "unwrap" => {
            let cfg = CString::new(a[2].clone()).unwrap();
            let store = CString::new(a[3].clone()).unwrap();
            let pid = id32(&a[4]);
            let user_holding = id32(&a[5]);
            let user_native = id32(&a[6]);
            let amount: u128 = a[7].parse().expect("amount must be a u128");
            let mut tx = [0u8; 32];
            let rc = unsafe {
                ldex_wlez_unwrap(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    pid.as_ptr(),
                    user_holding.as_ptr(),
                    user_native.as_ptr(),
                    amount,
                    tx.as_mut_ptr(),
                )
            };
            println!("unwrap rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        _ => {
            eprintln!("usage: wlez_admin defid|vaultid|initialize|wrap|unwrap ...");
            std::process::exit(2);
        }
    }
}
