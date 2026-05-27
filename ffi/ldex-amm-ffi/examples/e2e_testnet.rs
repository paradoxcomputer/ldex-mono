//! #24 e2e harness — drives the shim FFI directly against the live
//! testnet. Two ops:
//!
//!   pool  <cfg> <store> <amm> <holdA> <holdB> <lp> <amount> <fees>
//!   swap1 <cfg> <store> <amm> <PA> <PB> <defA> <defB> <tokIn> <amt> <min> <fees>
//!
//! `pool` creates the (TOKENA,TOKENB,fee) pool via OUR fee-tier
//! `ldex_amm_new_pool` (so the PDA matches what the private path
//! derives). `swap1` runs the corrected mode-1 PrivateOwned swap. Ids
//! accept `Public/<b58>` | `Private/<b58>` | `<b58>` | `<64hex>`.

use std::ffi::CString;

use ldex_amm_ffi::{
    ldex_amm_disposable_swap_exact_in, ldex_amm_init_token_holding, ldex_amm_new_pool,
    ldex_amm_onchain_price_history, ldex_amm_parse_account_id, ldex_amm_private_add_liquidity,
    ldex_amm_private_swap_exact_in, ldex_amm_swap_exact_in,
    ldex_amm_v2_disposable_swap, ldex_amm_v2_new_pool, ldex_amm_v2_new_pool_ata,
    ldex_amm_v2_private_swap_exact_in, ldex_amm_v2_remove_liquidity_ata,
    ldex_amm_v2_swap_exact_in, ldex_amm_v2_swap_exact_in_ata,
    ldex_amm_swap_exact_in_ata, ldex_amm_swap_exact_out_ata,
    ldex_amm_add_liquidity_ata, ldex_amm_remove_liquidity_ata,
    ldex_ata_create, ldex_ata_id, ldex_amm_v2_private_add_liquidity,
    ldex_amm_v2_private_remove_liquidity,
    ldex_token_deshield, ldex_token_shield, LDEX_AMM_OK,
};

fn hx(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b { s.push_str(&format!("{x:02x}")); }
    s
}

fn id32(s: &str) -> [u8; 32] {
    let c = CString::new(s).unwrap();
    let mut out = [0u8; 32];
    let rc = unsafe { ldex_amm_parse_account_id(c.as_ptr(), out.as_mut_ptr()) };
    assert_eq!(rc, LDEX_AMM_OK, "could not parse id {s} (rc={rc})");
    out
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let op = a.get(1).map(String::as_str).unwrap_or("");
    let cfg = CString::new(a[2].clone()).unwrap();
    let store = CString::new(a[3].clone()).unwrap();
    let amm = id32(&a[4]);
    let mut tx = [0u8; 32];

    match op {
        "pool" => {
            let (hold_a, hold_b, lp) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let amount: u128 = a[8].parse().unwrap();
            let fees: u128 = a[9].parse().unwrap();
            let rc = unsafe {
                ldex_amm_new_pool(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    hold_a.as_ptr(),
                    hold_b.as_ptr(),
                    lp.as_ptr(),
                    amount,
                    amount,
                    fees,
                    u64::MAX,
                    tx.as_mut_ptr(),
                )
            };
            println!("new_pool rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 pool creation (no Clock account; amm_v2 deliberately
        // skips the on-chain oracle so privacy proofs stay drift-free).
        // v2pool <cfg> <store> <amm_v2_pid> <holdA> <holdB> <lp> <amount> <fees>
        "v2pool" => {
            let (hold_a, hold_b, lp) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let amount: u128 = a[8].parse().unwrap();
            let fees: u128 = a[9].parse().unwrap();
            let rc = unsafe {
                ldex_amm_v2_new_pool(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    hold_a.as_ptr(),
                    hold_b.as_ptr(),
                    lp.as_ptr(),
                    amount,
                    amount,
                    fees,
                    u64::MAX,
                    tx.as_mut_ptr(),
                )
            };
            println!("amm_v2_new_pool rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 pool creation with LP minted into ATA(owner, lp_def).
        // Token deposits use the user's KEYPAIR holdings (token::Transfer
        // with PDA-claim, mirroring canonical new_definition).
        // v2pool_ata <cfg> <store> <amm_v2_pid> <ata_pid> <owner> <holdA> <holdB> <amount> <fees>
        "v2pool_ata" => {
            let (owner, hold_a, hold_b) =
                (id32(&a[6]), id32(&a[7]), id32(&a[8]));
            let amount: u128 = a[9].parse().unwrap();
            let fees: u128 = a[10].parse().unwrap();
            std::env::set_var("LDEX_ATA_PROGRAM_ID", &a[5]);
            let rc = unsafe {
                ldex_amm_v2_new_pool_ata(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    owner.as_ptr(), hold_a.as_ptr(), hold_b.as_ptr(),
                    amount, amount, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            println!("amm_v2_new_pool_ata rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 mode-0 ATA-only swap. v2pubswap_ata <cfg> <store> <amm_v2_pid> <ata_pid> <owner> <defA> <defB> <tokIn> <amt> <min> <fees>
        "v2pubswap_ata" => {
            let (owner, def_a, def_b, tin) =
                (id32(&a[6]), id32(&a[7]), id32(&a[8]), id32(&a[9]));
            let amt: u128 = a[10].parse().unwrap();
            let min: u128 = a[11].parse().unwrap();
            let fees: u128 = a[12].parse().unwrap();
            std::env::set_var("LDEX_ATA_PROGRAM_ID", &a[5]);
            let rc = unsafe {
                ldex_amm_v2_swap_exact_in_ata(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    owner.as_ptr(), def_a.as_ptr(), def_b.as_ptr(), tin.as_ptr(),
                    amt, min, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            println!("amm_v2_swap_exact_in_ata rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 add-liquidity ATA. v2add_ata <cfg> <store> <amm_v2_pid> <ata_pid> <owner> <defA> <defB> <minLp> <maxA> <maxB> <fees>
        "v2add_ata" => {
            let (owner, def_a, def_b) = (id32(&a[6]), id32(&a[7]), id32(&a[8]));
            let mlp: u128 = a[9].parse().unwrap();
            let mxa: u128 = a[10].parse().unwrap();
            let mxb: u128 = a[11].parse().unwrap();
            let fees: u128 = a[12].parse().unwrap();
            std::env::set_var("LDEX_ATA_PROGRAM_ID", &a[5]);
            let rc = unsafe {
                ldex_amm_ffi::ldex_amm_v2_add_liquidity_ata(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    owner.as_ptr(), def_a.as_ptr(), def_b.as_ptr(),
                    mlp, mxa, mxb, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            println!("amm_v2_add_liquidity_ata rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 remove-liquidity ATA. v2remove_ata <cfg> <store> <amm_v2_pid> <ata_pid> <owner> <defA> <defB> <lpAmount> <minA> <minB> <fees>
        "v2remove_ata" => {
            let (owner, def_a, def_b) = (id32(&a[6]), id32(&a[7]), id32(&a[8]));
            let lp: u128 = a[9].parse().unwrap();
            let mna: u128 = a[10].parse().unwrap();
            let mnb: u128 = a[11].parse().unwrap();
            let fees: u128 = a[12].parse().unwrap();
            std::env::set_var("LDEX_ATA_PROGRAM_ID", &a[5]);
            let rc = unsafe {
                ldex_amm_v2_remove_liquidity_ata(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    owner.as_ptr(), def_a.as_ptr(), def_b.as_ptr(),
                    lp, mna, mnb, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            println!("amm_v2_remove_liquidity_ata rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 mode-0 public swap. v2pubswap <cfg> <store> <amm_v2_pid> <holdA> <holdB> <tokIn> <amt> <min> <fees>
        "v2pubswap" => {
            let (ha, hb, tin) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let amt: u128 = a[8].parse().unwrap();
            let min: u128 = a[9].parse().unwrap();
            let fees: u128 = a[10].parse().unwrap();
            let rc = unsafe {
                ldex_amm_v2_swap_exact_in(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    ha.as_ptr(), hb.as_ptr(), tin.as_ptr(),
                    amt, min, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            println!("amm_v2_swap_exact_in rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 mode-1 private swap. v2swap1 <cfg> <store> <amm_v2_pid> <PA> <PB> <defA> <defB> <tokIn> <amt> <min> <fees>
        "v2swap1" => {
            let (pa, pb) = (id32(&a[5]), id32(&a[6]));
            let (def_a, def_b, tok_in) = (id32(&a[7]), id32(&a[8]), id32(&a[9]));
            let amount: u128 = a[10].parse().unwrap();
            let min_out: u128 = a[11].parse().unwrap();
            let fees: u128 = a[12].parse().unwrap();
            let t0 = std::time::Instant::now();
            let rc = unsafe {
                ldex_amm_v2_private_swap_exact_in(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    pa.as_ptr(), pb.as_ptr(),
                    def_a.as_ptr(), def_b.as_ptr(), tok_in.as_ptr(),
                    amount, min_out, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            let dt = t0.elapsed();
            println!("amm_v2_private_swap_exact_in rc={rc} elapsed={}s tx={}", dt.as_secs(), hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        "swap1" => {
            let (pa, pb) = (id32(&a[5]), id32(&a[6]));
            let (def_a, def_b, tok_in) = (id32(&a[7]), id32(&a[8]), id32(&a[9]));
            let amount: u128 = a[10].parse().unwrap();
            let min_out: u128 = a[11].parse().unwrap();
            let fees: u128 = a[12].parse().unwrap();
            let rc = unsafe {
                ldex_amm_private_swap_exact_in(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    pa.as_ptr(),
                    pb.as_ptr(),
                    def_a.as_ptr(),
                    def_b.as_ptr(),
                    tok_in.as_ptr(),
                    amount,
                    min_out,
                    fees,
                    u64::MAX,
                    tx.as_mut_ptr(),
                )
            };
            println!("private_swap_exact_in rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // Public swap (fast, no proof) — drives an on-chain AMM tx that
        // accumulates the oracle + pushes an observation (§5.11③).
        // pubswap <cfg> <store> <amm> <holdA> <holdB> <tokIn> <amt> <min> <fees>
        "pubswap" => {
            let (ha, hb, tin) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let amt: u128 = a[8].parse().unwrap();
            let min: u128 = a[9].parse().unwrap();
            let fees: u128 = a[10].parse().unwrap();
            let rc = unsafe {
                ldex_amm_swap_exact_in(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    ha.as_ptr(),
                    hb.as_ptr(),
                    tin.as_ptr(),
                    amt,
                    min,
                    fees,
                    u64::MAX,
                    tx.as_mut_ptr(),
                )
            };
            println!("swap_exact_in rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // RFP Func #8 — ATA-based swap.
        // swap_ata <cfg> <store> <amm> <owner> <defA> <defB> <token_in> <amount> <min_out> <fees>
        "swap_ata" => {
            let (owner, da, db, tin) =
                (id32(&a[5]), id32(&a[6]), id32(&a[7]), id32(&a[8]));
            let amt: u128 = a[9].parse().unwrap();
            let min: u128 = a[10].parse().unwrap();
            let fees: u128 = a[11].parse().unwrap();
            let rc = unsafe {
                ldex_amm_swap_exact_in_ata(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    owner.as_ptr(),
                    da.as_ptr(),
                    db.as_ptr(),
                    tin.as_ptr(),
                    amt,
                    min,
                    fees,
                    u64::MAX,
                    tx.as_mut_ptr(),
                )
            };
            println!("swap_exact_in_ata rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // swap_out_ata <cfg> <store> <amm> <owner> <defA> <defB> <token_in> <exact_out> <max_in> <fees>
        "swap_out_ata" => {
            let (owner, da, db, tin) = (id32(&a[5]), id32(&a[6]), id32(&a[7]), id32(&a[8]));
            let exact_out: u128 = a[9].parse().unwrap();
            let max_in: u128 = a[10].parse().unwrap();
            let fees: u128 = a[11].parse().unwrap();
            let rc = unsafe {
                ldex_amm_swap_exact_out_ata(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    owner.as_ptr(), da.as_ptr(), db.as_ptr(), tin.as_ptr(),
                    exact_out, max_in, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            println!("swap_exact_out_ata rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // add_ata <cfg> <store> <amm> <owner> <defA> <defB> <min_lp> <max_a> <max_b> <fees>
        "add_ata" => {
            let (owner, da, db) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let min_lp: u128 = a[8].parse().unwrap();
            let max_a: u128 = a[9].parse().unwrap();
            let max_b: u128 = a[10].parse().unwrap();
            let fees: u128 = a[11].parse().unwrap();
            let rc = unsafe {
                ldex_amm_add_liquidity_ata(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    owner.as_ptr(), da.as_ptr(), db.as_ptr(),
                    min_lp, max_a, max_b, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            println!("add_liquidity_ata rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // rem_ata <cfg> <store> <amm> <owner> <defA> <defB> <lp_amt> <min_a> <min_b> <fees>
        "rem_ata" => {
            let (owner, da, db) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let lp_amt: u128 = a[8].parse().unwrap();
            let min_a: u128 = a[9].parse().unwrap();
            let min_b: u128 = a[10].parse().unwrap();
            let fees: u128 = a[11].parse().unwrap();
            let rc = unsafe {
                ldex_amm_remove_liquidity_ata(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    owner.as_ptr(), da.as_ptr(), db.as_ptr(),
                    lp_amt, min_a, min_b, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            println!("remove_liquidity_ata rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // Read the ON-CHAIN observation ring (gapless price history).
        // ohist <cfg> <store> <amm> <defA> <defB> <fees>
        "ohist" => {
            let (da, db) = (id32(&a[5]), id32(&a[6]));
            let fees: u128 = a[7].parse().unwrap();
            let mut buf = vec![0u8; 262144];
            let rc = unsafe {
                ldex_amm_onchain_price_history(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    da.as_ptr(),
                    db.as_ptr(),
                    fees,
                    buf.as_mut_ptr(),
                    buf.len(),
                )
            };
            let s = std::ffi::CStr::from_bytes_until_nul(&buf)
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default();
            println!("onchain_price_history rc={rc} {s}");
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // Mode-2 router/disposable private swap (RFP-literal account-A).
        // disp <cfg> <store> <amm> <router> <userA> <userB> <aA> <aB> <defA> <defB> <tokIn> <amt> <min> <fees>
        "disp" => {
            let router = id32(&a[5]);
            let (ua, ub) = (id32(&a[6]), id32(&a[7]));
            let (aa, ab) = (id32(&a[8]), id32(&a[9]));
            let (da, db, tin) = (id32(&a[10]), id32(&a[11]), id32(&a[12]));
            let amt: u128 = a[13].parse().unwrap();
            let min: u128 = a[14].parse().unwrap();
            let fees: u128 = a[15].parse().unwrap();
            let rc = unsafe {
                ldex_amm_disposable_swap_exact_in(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    router.as_ptr(),
                    ua.as_ptr(),
                    ub.as_ptr(),
                    aa.as_ptr(),
                    ab.as_ptr(),
                    da.as_ptr(),
                    db.as_ptr(),
                    tin.as_ptr(),
                    amt,
                    min,
                    fees,
                    u64::MAX,
                    tx.as_mut_ptr(),
                )
            };
            println!("disposable_swap_exact_in rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 combined disposable swap (mode-2, testnet-compat).
        // Same arg shape as `disp` minus the router id (amm_v2 replaces
        // both router AND amm), with the FIRST positional `amm` arg
        // re-interpreted as the amm_v2 program id.
        // v2disp <cfg> <store> <amm_v2_pid> <userA> <userB> <aA> <aB> <defA> <defB> <tokIn> <amt> <min> <fees>
        "v2disp" => {
            let (ua, ub) = (id32(&a[5]), id32(&a[6]));
            let (aa, ab) = (id32(&a[7]), id32(&a[8]));
            let (da, db, tin) = (id32(&a[9]), id32(&a[10]), id32(&a[11]));
            let amt: u128 = a[12].parse().unwrap();
            let min: u128 = a[13].parse().unwrap();
            let fees: u128 = a[14].parse().unwrap();
            let t0 = std::time::Instant::now();
            let rc = unsafe {
                ldex_amm_v2_disposable_swap(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    ua.as_ptr(),
                    ub.as_ptr(),
                    aa.as_ptr(),
                    ab.as_ptr(),
                    da.as_ptr(),
                    db.as_ptr(),
                    tin.as_ptr(),
                    amt,
                    min,
                    fees,
                    u64::MAX,
                    tx.as_mut_ptr(),
                )
            };
            let dt = t0.elapsed();
            println!(
                "amm_v2_disposable_swap rc={rc} elapsed={}s tx={}",
                dt.as_secs(),
                hx(&tx)
            );
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 mode-2 disposable with native LEZ as input (LEZ → token).
        // Chains WLEZ::Wrap (USER native → a_wlez) + AMM swap (a_wlez →
        // a_out) + reshield token::Transfer (a_out → user_priv_out).
        // v2dispnativein <cfg> <store> <amm_v2_pid> <wlez_pid> <userNative> <wlezVault> <wlezDef> <aWlez> <aOut> <defOut> <userPrivOut> <amt> <min> <fees>
        "v2dispnativein" => {
            let (wlez_pid, user_native) = (id32(&a[5]), id32(&a[6]));
            let (wlez_vault, wlez_def) = (id32(&a[7]), id32(&a[8]));
            let (a_wlez, a_out) = (id32(&a[9]), id32(&a[10]));
            let (def_out, user_priv_out) = (id32(&a[11]), id32(&a[12]));
            let amt: u128 = a[13].parse().unwrap();
            let min: u128 = a[14].parse().unwrap();
            let fees: u128 = a[15].parse().unwrap();
            let t0 = std::time::Instant::now();
            let rc = unsafe {
                ldex_amm_ffi::ldex_amm_v2_disposable_swap_native_in(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    wlez_pid.as_ptr(), user_native.as_ptr(),
                    wlez_vault.as_ptr(), wlez_def.as_ptr(),
                    a_wlez.as_ptr(), a_out.as_ptr(),
                    def_out.as_ptr(), user_priv_out.as_ptr(),
                    amt, min, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            let dt = t0.elapsed();
            println!(
                "amm_v2_disposable_swap_native_in rc={rc} elapsed={}s tx={}",
                dt.as_secs(), hx(&tx)
            );
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // amm_v2 mode-2 disposable with native LEZ as output (token → LEZ).
        // Chains token::Transfer (deshield, user_priv → a_holding_in) +
        // AMM swap (a_holding_in → a_wlez) + WLEZ::Unwrap (a_wlez → user_native).
        // v2dispnativeout <cfg> <store> <amm_v2_pid> <wlez_pid> <userPrivIn> <aHoldingIn> <aWlez> <wlezDef> <wlezVault> <userNative> <defIn> <amt> <min> <fees>
        "v2dispnativeout" => {
            let (wlez_pid, user_priv_in, a_holding_in) =
                (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let (a_wlez, wlez_def, wlez_vault) =
                (id32(&a[8]), id32(&a[9]), id32(&a[10]));
            let (user_native, def_in) = (id32(&a[11]), id32(&a[12]));
            let amt: u128 = a[13].parse().unwrap();
            let min: u128 = a[14].parse().unwrap();
            let fees: u128 = a[15].parse().unwrap();
            let t0 = std::time::Instant::now();
            let rc = unsafe {
                ldex_amm_ffi::ldex_amm_v2_disposable_swap_native_out(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    wlez_pid.as_ptr(), user_priv_in.as_ptr(),
                    a_holding_in.as_ptr(), a_wlez.as_ptr(),
                    wlez_def.as_ptr(), wlez_vault.as_ptr(),
                    user_native.as_ptr(), def_in.as_ptr(),
                    amt, min, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            let dt = t0.elapsed();
            println!(
                "amm_v2_disposable_swap_native_out rc={rc} elapsed={}s tx={}",
                dt.as_secs(), hx(&tx)
            );
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // Initialize a fresh public account as a token holding.
        // init <cfg> <store> <amm(unused)> <tokenDef> <holding>
        "init" => {
            let (tdef, hold) = (id32(&a[5]), id32(&a[6]));
            let rc = unsafe {
                ldex_amm_init_token_holding(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    tdef.as_ptr(),
                    hold.as_ptr(),
                    tx.as_mut_ptr(),
                )
            };
            println!("init_token_holding rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // Private add-liquidity.
        // padd <cfg> <store> <amm> <PA> <PB> <PLP> <defA> <defB> <minLp> <maxA> <maxB> <fees>
        "padd" => {
            let (pa, pb, plp) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let (da, db) = (id32(&a[8]), id32(&a[9]));
            let mlp: u128 = a[10].parse().unwrap();
            let mxa: u128 = a[11].parse().unwrap();
            let mxb: u128 = a[12].parse().unwrap();
            let fees: u128 = a[13].parse().unwrap();
            let rc = unsafe {
                ldex_amm_private_add_liquidity(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    amm.as_ptr(),
                    pa.as_ptr(),
                    pb.as_ptr(),
                    plp.as_ptr(),
                    da.as_ptr(),
                    db.as_ptr(),
                    mlp,
                    mxa,
                    mxb,
                    fees,
                    u64::MAX,
                    tx.as_mut_ptr(),
                )
            };
            println!("private_add_liquidity rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // Derive the amm_v2 LP-def PDA for (defA, defB, fees). Pure —
        // matches what amm_v2 deploys when the pool is created. Lets the
        // bootstrap / test harness pre-create the user's LP ATA before
        // calling the ATA-only pool-create path.
        // v2lp <cfg(unused)> <store(unused)> <amm_v2_pid> <defA> <defB> <fees>
        "v2lp" => {
            let (def_a, def_b) = (id32(&a[5]), id32(&a[6]));
            let fees: u128 = a[7].parse().unwrap();
            let mut pool = [0u8; 32];
            let mut lp = [0u8; 32];
            let rc1 = unsafe {
                ldex_amm_ffi::ldex_amm_pool_id(
                    amm.as_ptr(), def_a.as_ptr(), def_b.as_ptr(),
                    fees, pool.as_mut_ptr(),
                )
            };
            let rc2 = unsafe {
                ldex_amm_ffi::ldex_amm_lp_definition_id(
                    amm.as_ptr(), pool.as_ptr(), lp.as_mut_ptr(),
                )
            };
            let id = nssa_core::account::AccountId::new(lp);
            println!("Public/{id}");
            eprintln!("hex={}", hx(&lp));
            std::process::exit(if rc1 == LDEX_AMM_OK && rc2 == LDEX_AMM_OK { 0 } else { 1 });
        }
        // RFP Func #8: deterministic ATA derivation for (owner, mint).
        // ataid <cfg(unused)> <store(unused)> <ataPid> <owner> <tokenDef>
        "ataid" => {
            let (ap, ow, td) = (amm, id32(&a[5]), id32(&a[6]));
            let mut o = [0u8; 32];
            let rc = unsafe {
                ldex_ata_id(ap.as_ptr(), ow.as_ptr(), td.as_ptr(), o.as_mut_ptr())
            };
            // Canonical "Public/<base58>" (what the wallet CLI + bootstrap
            // env expect); also echo hex for debugging.
            let id = nssa_core::account::AccountId::new(o);
            println!("Public/{id}");
            eprintln!("hex={}", hx(&o));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // v2padd <cfg> <store> <amm_v2_pid> <PA> <PB> <PLP> <DA> <DB> <mlp> <mxa> <mxb> <fees>
        "v2padd" => {
            let (pa, pb, plp) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let (da, db) = (id32(&a[8]), id32(&a[9]));
            let mlp: u128 = a[10].parse().unwrap();
            let mxa: u128 = a[11].parse().unwrap();
            let mxb: u128 = a[12].parse().unwrap();
            let fees: u128 = a[13].parse().unwrap();
            let t0 = std::time::Instant::now();
            let rc = unsafe {
                ldex_amm_v2_private_add_liquidity(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    pa.as_ptr(), pb.as_ptr(), plp.as_ptr(),
                    da.as_ptr(), db.as_ptr(),
                    mlp, mxa, mxb, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            let dt = t0.elapsed();
            println!("amm_v2_private_add_liquidity rc={rc} elapsed={}s tx={}", dt.as_secs(), hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // v2prem <cfg> <store> <amm_v2_pid> <PA> <PB> <PLP> <DA> <DB> <lpAmt> <minA> <minB> <fees>
        "v2prem" => {
            let (pa, pb, plp) = (id32(&a[5]), id32(&a[6]), id32(&a[7]));
            let (da, db) = (id32(&a[8]), id32(&a[9]));
            let lp_amt: u128 = a[10].parse().unwrap();
            let mna: u128 = a[11].parse().unwrap();
            let mnb: u128 = a[12].parse().unwrap();
            let fees: u128 = a[13].parse().unwrap();
            let t0 = std::time::Instant::now();
            let rc = unsafe {
                ldex_amm_v2_private_remove_liquidity(
                    cfg.as_ptr(), store.as_ptr(), amm.as_ptr(),
                    pa.as_ptr(), pb.as_ptr(), plp.as_ptr(),
                    da.as_ptr(), db.as_ptr(),
                    lp_amt, mna, mnb, fees, u64::MAX, tx.as_mut_ptr(),
                )
            };
            let dt = t0.elapsed();
            println!("amm_v2_private_remove_liquidity rc={rc} elapsed={}s tx={}", dt.as_secs(), hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // shield <cfg> <store> <from_public> <to_private> <amount>
        "shield" => {
            let (from, to) = (id32(&a[4]), id32(&a[5]));
            let amount: u128 = a[6].parse().unwrap();
            // For shield, args[4] = cfg path was clobbered by our generic
            // arg layout (we put `amm` at args[4]); shift correctly here:
            // shield expects cfg=a[2], store=a[3], from=a[4], to=a[5], amount=a[6].
            let cfg = CString::new(a[2].clone()).unwrap();
            let store = CString::new(a[3].clone()).unwrap();
            let rc = unsafe {
                ldex_token_shield(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    from.as_ptr(),
                    to.as_ptr(),
                    amount,
                    tx.as_mut_ptr(),
                )
            };
            println!("shield rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // deshield <cfg> <store> <from_private> <to_public> <amount>
        "deshield" => {
            let (from, to) = (id32(&a[4]), id32(&a[5]));
            let amount: u128 = a[6].parse().unwrap();
            let cfg = CString::new(a[2].clone()).unwrap();
            let store = CString::new(a[3].clone()).unwrap();
            let rc = unsafe {
                ldex_token_deshield(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    from.as_ptr(),
                    to.as_ptr(),
                    amount,
                    tx.as_mut_ptr(),
                )
            };
            println!("deshield rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        // atacreate <cfg> <store> <ataPid> <owner> <tokenDef>
        "atacreate" => {
            let (ap, ow, td) = (amm, id32(&a[5]), id32(&a[6]));
            let rc = unsafe {
                ldex_ata_create(
                    cfg.as_ptr(),
                    store.as_ptr(),
                    ap.as_ptr(),
                    ow.as_ptr(),
                    td.as_ptr(),
                    tx.as_mut_ptr(),
                )
            };
            println!("ata_create rc={rc} tx={}", hx(&tx));
            std::process::exit(if rc == LDEX_AMM_OK { 0 } else { 1 });
        }
        _ => {
            eprintln!("usage: e2e_testnet pool|swap1|mono|mono2|pubswap|ohist|disp|init|padd|ataid|atacreate ...");
            std::process::exit(2);
        }
    }
}
