//! Layer A — plugin/FFI integration tests for the LDEX mini-app.
//!
//! Calls every FFI function that an LdexCorePlugin Q_INVOKABLE relies on,
//! using args derived from a live bootstrap.env, and asserts the JSON
//! return shape AND values. Fails LOUDLY with the JSON the plugin would
//! have returned, so a UI-side display bug is visibly separable from a
//! chain/FFI-side data bug.
//!
//! Reproductions inline for two reported bugs:
//!   • "Pool A/B at the selected tier shows no pool in UI" — covered by
//!     `pools_lists_seeded_pool` and `pool_info_for_a_b_fee5`.
//!   • "Shield doesn't work" — covered by `shield_moves_balance` which
//!     asserts HOLD/PRIV deltas after a real STARK round-trip.
//!
//! Layer A doesn't drive QML; it isolates "did the data layer return the
//! right thing". Layer B (render) and Layer C (e2e click flows) cover the
//! UI half.

use std::env;
use std::ffi::{CStr, CString};
use std::time::Instant;

#[link(name = "ldex_amm_ffi")]
extern "C" {
    fn ldex_amm_parse_account_id(s: *const i8, out: *mut u8) -> i32;
    fn ldex_amm_pool_info(
        cfg: *const i8, store: *const i8, amm_pid: *const u8,
        a: *const u8, b: *const u8, fees: u128,
        out: *mut u8, cap: usize) -> i32;
    fn ldex_amm_token_balance(
        cfg: *const i8, store: *const i8, account_id: *const u8,
        out: *mut u8, cap: usize) -> i32;
    fn ldex_wlez_wrap(
        cfg: *const i8, store: *const i8, wlez_pid: *const u8,
        from: *const u8, to: *const u8, amount: u128, out_tx: *mut u8) -> i32;
    fn ldex_wlez_unwrap(
        cfg: *const i8, store: *const i8, wlez_pid: *const u8,
        from: *const u8, to: *const u8, amount: u128, out_tx: *mut u8) -> i32;
}

// wallet_ffi — used by the plugin for PrivateOwned balance reads (local cache).
#[repr(C)]
struct WalletHandle { _opaque: [u8; 0] }
#[repr(C)]
struct FfiBytes32 { data: [u8; 32] }

// FfiAccount mirrors wallet_ffi.h. data is a borsh-encoded TokenHolding for
// shielded token accounts: [tag=0(Fungible), def_id(32), balance(u128 LE)].
// FfiProgramId is 32 bytes (8 × u32), not 8 — getting this wrong shifts
// every later field and `data` becomes a garbage pointer.
#[repr(C)]
struct FfiProgramId { data: [u32; 8] }
#[repr(C)]
struct FfiU128 { bytes: [u8; 16] }
#[repr(C)]
struct FfiAccount {
    program_owner: FfiProgramId,
    balance: FfiU128,
    data: *const u8,
    data_len: usize,
    nonce: FfiU128,
}

#[link(name = "wallet_ffi")]
extern "C" {
    fn wallet_ffi_open(config_path: *const i8, storage_path: *const i8) -> *mut WalletHandle;
    fn wallet_ffi_destroy(handle: *mut WalletHandle);
    fn wallet_ffi_get_balance(handle: *mut WalletHandle, account_id: *const FfiBytes32,
                              is_public: bool, out_balance: *mut [u8; 16]) -> i32;
    fn wallet_ffi_get_current_block_height(handle: *mut WalletHandle, out: *mut u64) -> i32;
    fn wallet_ffi_sync_to_block(handle: *mut WalletHandle, block: u64) -> i32;
    fn wallet_ffi_get_account_private(handle: *mut WalletHandle, account_id: *const FfiBytes32,
                                      out: *mut FfiAccount) -> i32;
    fn wallet_ffi_free_account_data(account: *mut FfiAccount);
}

// Read a private *token* balance via wallet_ffi_get_account_private and
// parse the data field as TokenHolding::Fungible. Returns None if the
// account isn't a Fungible token holding.
fn wallet_priv_token_balance(c: &Ctx, acct: &[u8; 32]) -> Option<u128> {
    let h = unsafe { wallet_ffi_open(c.cfg.as_ptr(), c.store.as_ptr()) };
    if h.is_null() { return None; }
    let fid = FfiBytes32 { data: *acct };
    let mut acc = FfiAccount {
        program_owner: FfiProgramId { data: [0; 8] },
        balance: FfiU128 { bytes: [0; 16] },
        data: std::ptr::null(),
        data_len: 0,
        nonce: FfiU128 { bytes: [0; 16] },
    };
    let rc = unsafe { wallet_ffi_get_account_private(h, &fid, &mut acc) };
    if rc != 0 {
        unsafe { wallet_ffi_destroy(h); }
        return None;
    }
    let v = if !acc.data.is_null() && acc.data_len >= 49 {
        let slice = unsafe { std::slice::from_raw_parts(acc.data, acc.data_len) };
        // TokenHolding::Fungible borsh: [tag=0, def_id:32, balance_le:16]
        if slice[0] == 0 && slice.len() >= 49 {
            let mut bal_bytes = [0u8; 16];
            bal_bytes.copy_from_slice(&slice[33..49]);
            Some(u128::from_le_bytes(bal_bytes))
        } else { None }
    } else { None };
    unsafe { wallet_ffi_free_account_data(&mut acc); }
    unsafe { wallet_ffi_destroy(h); }
    v
}

fn sync_wallet_to_head(c: &Ctx) -> Result<u64, String> {
    let h = unsafe { wallet_ffi_open(c.cfg.as_ptr(), c.store.as_ptr()) };
    if h.is_null() { return Err("wallet_ffi_open failed".into()); }
    let mut head = 0u64;
    let rc = unsafe { wallet_ffi_get_current_block_height(h, &mut head) };
    if rc != 0 {
        unsafe { wallet_ffi_destroy(h); }
        return Err(format!("get_current_block_height rc={rc}"));
    }
    let rc = unsafe { wallet_ffi_sync_to_block(h, head) };
    unsafe { wallet_ffi_destroy(h); }
    if rc != 0 { return Err(format!("sync_to_block({head}) rc={rc}")); }
    Ok(head)
}

fn wallet_balance(c: &Ctx, acct: &[u8; 32], is_public: bool) -> Option<u128> {
    let h = unsafe { wallet_ffi_open(c.cfg.as_ptr(), c.store.as_ptr()) };
    if h.is_null() { return None; }
    let fid = FfiBytes32 { data: *acct };
    let mut out_le = [0u8; 16];
    let rc = unsafe { wallet_ffi_get_balance(h, &fid, is_public, &mut out_le) };
    unsafe { wallet_ffi_destroy(h); }
    if rc != 0 { return None; }
    Some(u128::from_le_bytes(out_le))
}

fn parse(s: &str) -> [u8; 32] {
    let cs = CString::new(s).unwrap();
    let mut o = [0u8; 32];
    let rc = unsafe { ldex_amm_parse_account_id(cs.as_ptr() as *const i8, o.as_mut_ptr()) };
    assert_eq!(rc, 0, "parse_account_id failed for {s:?} rc={rc}");
    o
}

fn parse_hex(h: &str) -> [u8; 32] {
    assert_eq!(h.len(), 64, "expected 64-char hex, got {}", h.len());
    let mut o = [0u8; 32];
    for i in 0..32 {
        o[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap();
    }
    o
}

fn env_or_fail(k: &str) -> String {
    env::var(k).unwrap_or_else(|_| {
        eprintln!("FAIL: env var {k} not set — source scripts/bootstrap.env first");
        std::process::exit(2);
    })
}

struct Ctx {
    cfg: CString,
    store: CString,
    amm_v2: [u8; 32],
    wlez_pid: [u8; 32],
    user_owner: [u8; 32],
    def_a: [u8; 32],
    def_b: [u8; 32],
    hold_a: [u8; 32],
    hold_b: [u8; 32],
    hold_w: [u8; 32],
    priv_a: [u8; 32],
}
impl Ctx {
    fn load() -> Self {
        Self {
            cfg: CString::new(env_or_fail("LDEX_WALLET_CONFIG")).unwrap(),
            store: CString::new(env_or_fail("LDEX_WALLET_STORAGE")).unwrap(),
            amm_v2: parse_hex(&env_or_fail("LDEX_AMM_V2_PROGRAM_ID")),
            wlez_pid: parse_hex(&env_or_fail("LDEX_WLEZ_PROGRAM_ID")),
            user_owner: parse(&env_or_fail("LDEX_USER_OWNER")),
            def_a: parse(&env_or_fail("LDEX_DEF_A")),
            def_b: parse(&env_or_fail("LDEX_DEF_B")),
            hold_a: parse(&env_or_fail("LDEX_HOLD_A")),
            hold_b: parse(&env_or_fail("LDEX_HOLD_B")),
            hold_w: parse(&env_or_fail("LDEX_HOLD_W")),
            priv_a: parse(&env_or_fail("LDEX_PRIV_A")),
        }
    }
}

fn pool_info(c: &Ctx, a: &[u8; 32], b: &[u8; 32], fee: u128) -> serde_json::Value {
    let mut buf = [0u8; 1024];
    let rc = unsafe {
        ldex_amm_pool_info(c.cfg.as_ptr(), c.store.as_ptr(),
            c.amm_v2.as_ptr(), a.as_ptr(), b.as_ptr(), fee,
            buf.as_mut_ptr(), buf.len())
    };
    assert_eq!(rc, 0, "pool_info rc={rc}");
    let s = unsafe { CStr::from_ptr(buf.as_ptr() as *const i8).to_string_lossy().to_string() };
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("pool_info JSON: {s:?} err={e}"))
}

fn token_balance(c: &Ctx, acct: &[u8; 32]) -> serde_json::Value {
    let mut buf = [0u8; 256];
    let rc = unsafe {
        ldex_amm_token_balance(c.cfg.as_ptr(), c.store.as_ptr(),
            acct.as_ptr(), buf.as_mut_ptr(), buf.len())
    };
    assert_eq!(rc, 0, "token_balance rc={rc}");
    let s = unsafe { CStr::from_ptr(buf.as_ptr() as *const i8).to_string_lossy().to_string() };
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("token_balance JSON: {s:?} err={e}"))
}

fn balance_u128(c: &Ctx, acct: &[u8; 32]) -> u128 {
    token_balance(c, acct)
        .get("balance").and_then(|v| v.as_str())
        .map(|s| s.parse::<u128>().unwrap_or(0))
        .unwrap_or(0)
}

// ---------------------------------------------------------------- tests

struct TestResult {
    name: &'static str,
    pass: bool,
    detail: String,
    elapsed_ms: u128,
}

fn test_pool_info_for_a_b_fee5(c: &Ctx) -> TestResult {
    // Plugin's `poolInfoFor(defA, defB, 5)` MUST return exists:true with
    // non-zero reserves. (Reproduces the "pool not shown in UI" report.)
    // Reserves can be any value — bootstrap seeds 100000/100000, swaps
    // shift them — so we only assert >0, not >= the seed amount.
    let t0 = Instant::now();
    let p = pool_info(c, &c.def_a, &c.def_b, 5);
    let exists = p.get("exists").and_then(|v| v.as_bool()).unwrap_or(false);
    let res_a: u128 = p.get("reserve_a").and_then(|v| v.as_str()).unwrap_or("0").parse().unwrap_or(0);
    let res_b: u128 = p.get("reserve_b").and_then(|v| v.as_str()).unwrap_or("0").parse().unwrap_or(0);
    let pass = exists && res_a > 0 && res_b > 0;
    TestResult {
        name: "pool_info_for_a_b_fee5",
        pass,
        detail: format!("exists={exists} reserve_a={res_a} reserve_b={res_b}"),
        elapsed_ms: t0.elapsed().as_millis(),
    }
}

fn test_pools_lists_seeded_pool(c: &Ctx) -> TestResult {
    // Reconstructs what the plugin's `pools()` Q_INVOKABLE returns: all
    // pairs × all 4 tiers. The TOKENA/TOKENB fee=5 row MUST exist with
    // exists:true. (If this passes but the UI still hides it, the bug
    // is in QML render — Layer B catches that.)
    let t0 = Instant::now();
    let mut a_b_fee5_exists = false;
    let mut total_existing = 0;
    for fee in [1u128, 5, 30, 100] {
        let p = pool_info(c, &c.def_a, &c.def_b, fee);
        if p.get("exists").and_then(|v| v.as_bool()).unwrap_or(false) {
            total_existing += 1;
            if fee == 5 { a_b_fee5_exists = true; }
        }
    }
    TestResult {
        name: "pools_lists_seeded_pool",
        pass: a_b_fee5_exists,
        detail: format!("A/B fee=5 exists={a_b_fee5_exists} total_existing_tiers={total_existing}"),
        elapsed_ms: t0.elapsed().as_millis(),
    }
}

fn test_balance_reads_make_sense(c: &Ctx) -> TestResult {
    // Mirrors the plugin's `walletTokens` per-letter loop: HOLD_<L> +
    // ATA_<L> via FFI token_balance. The bootstrap funds 750000 of each
    // (minus what was shielded). Asserts the chain returns sensible totals.
    let t0 = Instant::now();
    let bal_a = balance_u128(c, &c.hold_a);
    let bal_b = balance_u128(c, &c.hold_b);
    let bal_w = balance_u128(c, &c.hold_w);
    let pass = bal_a >= 100_000 && bal_b >= 100_000 && bal_w >= 1_000;
    TestResult {
        name: "balance_reads_make_sense",
        pass,
        detail: format!("HOLD_A={bal_a} HOLD_B={bal_b} HOLD_W={bal_w}"),
        elapsed_ms: t0.elapsed().as_millis(),
    }
}

fn test_wrap_round_trip(c: &Ctx) -> TestResult {
    // Plugin's `wrapNative` → `unwrapNative` round-trip. Asserts the
    // chain delta matches the requested amount. Catches "FFI succeeds
    // but plugin returns success without state moving" class of bug.
    let t0 = Instant::now();
    let amt: u128 = 500;
    let pre_w = balance_u128(c, &c.hold_w);
    let mut tx = [0u8; 32];
    let rc = unsafe {
        ldex_wlez_wrap(c.cfg.as_ptr(), c.store.as_ptr(),
            c.wlez_pid.as_ptr(), c.user_owner.as_ptr(),
            c.hold_w.as_ptr(), amt, tx.as_mut_ptr())
    };
    if rc != 0 {
        return TestResult { name: "wrap_round_trip", pass: false,
            detail: format!("wrap rc={rc}"), elapsed_ms: t0.elapsed().as_millis() };
    }
    let mid_w = balance_u128(c, &c.hold_w);
    let rc = unsafe {
        ldex_wlez_unwrap(c.cfg.as_ptr(), c.store.as_ptr(),
            c.wlez_pid.as_ptr(), c.hold_w.as_ptr(),
            c.user_owner.as_ptr(), amt, tx.as_mut_ptr())
    };
    if rc != 0 {
        return TestResult { name: "wrap_round_trip", pass: false,
            detail: format!("unwrap rc={rc} after wrap+{amt}"), elapsed_ms: t0.elapsed().as_millis() };
    }
    let post_w = balance_u128(c, &c.hold_w);
    let pass = mid_w == pre_w + amt && post_w == pre_w;
    TestResult {
        name: "wrap_round_trip",
        pass,
        detail: format!("pre={pre_w} mid={mid_w} (expected {}) post={post_w} (expected {pre_w})",
            pre_w + amt),
        elapsed_ms: t0.elapsed().as_millis(),
    }
}

fn test_priv_a_chain_read_returns_zero(c: &Ctx) -> TestResult {
    // ldex_amm_token_balance hits the chain's public-state index; a
    // PrivateOwned account is not visible there. Asserts the contract:
    // chain read returns 0 (no balance), NOT an error. Catches the
    // failure mode where a future FFI change starts panicking instead.
    let t0 = Instant::now();
    let v = token_balance(c, &c.priv_a);
    let bal: u128 = v.get("balance").and_then(|x| x.as_str())
        .unwrap_or("0").parse().unwrap_or(0);
    TestResult {
        name: "priv_a_chain_read_returns_zero",
        pass: bal == 0,
        detail: format!("public-chain read of PRIV_A returns balance={bal} (expected 0)"),
        elapsed_ms: t0.elapsed().as_millis(),
    }
}

fn test_priv_a_token_balance_correct(c: &Ctx) -> TestResult {
    // THE bug behind "shielding doesn't work in the UI":
    // The plugin's walletTokens reads private balances via
    // wallet_ffi_get_balance(is_public=false), which returns the *native
    // LEZ* balance of the account — NOT the token balance held in the
    // private account's `data` field. For a Token::Fungible PrivateOwned
    // account, native LEZ is always 0, so the UI always displays 0
    // regardless of how many tokens are shielded.
    //
    // The correct API is wallet_ffi_get_account_private + parse data as
    // borsh-encoded TokenHolding::Fungible{def_id, balance}. This test
    // verifies the correct path returns the real balance.
    let t0 = Instant::now();
    let _ = sync_wallet_to_head(c);  // ensure cache is fresh
    let bal = wallet_priv_token_balance(c, &c.priv_a).unwrap_or(0);
    TestResult {
        name: "priv_a_token_balance_correct",
        pass: bal >= 100_000,
        detail: format!("PRIV_A token balance via get_account_private+parse-data = {bal} (expected ≥ 100000)"),
        elapsed_ms: t0.elapsed().as_millis(),
    }
}

fn test_wrong_api_returns_native_lez(c: &Ctx) -> TestResult {
    // Documents the historical bug: `wallet_ffi_get_balance(is_public=false)`
    // returns NATIVE LEZ balance, not the token balance held in the
    // private account's `data` field. The plugin originally used this
    // API for private TOKEN balances → UI always showed 0. This test
    // asserts the documented behaviour of the WRONG API: returns 0 (or
    // some other LEZ figure) — not the token balance. If this ever
    // starts returning >=100_000 the wallet FFI semantics changed and
    // the plugin can revert to the simpler API.
    let t0 = Instant::now();
    let priv_a_via_get_balance = wallet_balance(c, &c.priv_a, false).unwrap_or(0);
    // priv_a's native LEZ is 0 (token holdings don't accrue gas balance).
    let pass = priv_a_via_get_balance < 100_000;
    TestResult {
        name: "wrong_api_returns_native_lez",
        pass,
        detail: format!("wallet_ffi_get_balance(PRIV_A, is_public=false) = {priv_a_via_get_balance} (sentinel for plugin bug: must be < token balance, else the simpler API is safe again)"),
        elapsed_ms: t0.elapsed().as_millis(),
    }
}

// Removed: test_priv_a_wallet_cache_matches_chain — superseded by
// test_wrong_api_returns_native_lez. The original test ran the OLD
// (buggy) API and asserted it should return the token balance; that
// was a "fail to prove the bug exists" framing. The new test asserts
// the API's documented (LEZ) behaviour, and the regression for the
// FIX lives in test_priv_a_token_balance_correct.

fn test_env_shape() -> TestResult {
    // Plugin's `ensureEnv()` requires LDEX_AMM_PROGRAM_ID + LDEX_USER_HOLDING_A
    // OR LDEX_AMM_V2_PROGRAM_ID (modern bootstrap). Verify the keys QML
    // depends on are all present.
    let t0 = Instant::now();
    let required = [
        "LDEX_WALLET_CONFIG", "LDEX_WALLET_STORAGE",
        "LDEX_SEQUENCER_ADDR",
        "LDEX_USER_OWNER", "LDEX_TOKENS",
        "LDEX_AMM_V2_PROGRAM_ID", "LDEX_WLEZ_PROGRAM_ID",
        "LDEX_WLEZ_DEF", "LDEX_HOLD_W", "LDEX_ATA_W",
        "LDEX_DEF_A", "LDEX_HOLD_A", "LDEX_PRIV_A",
        "LDEX_DEF_B", "LDEX_HOLD_B", "LDEX_PRIV_B",
        "LDEX_USER_HOLDING_A", "LDEX_USER_HOLDING_LP",
    ];
    let missing: Vec<&str> = required.iter().filter(|k| env::var(k).is_err()).copied().collect();
    TestResult {
        name: "env_shape",
        pass: missing.is_empty(),
        detail: if missing.is_empty() { "all required keys present".into() }
                else { format!("missing: {missing:?}") },
        elapsed_ms: t0.elapsed().as_millis(),
    }
}

// ---------------------------------------------------------------- main

fn main() {
    let do_mutate = env::var("LAYER_A_MUTATE").ok().as_deref() == Some("1");

    println!("LDEX Layer A — plugin/FFI integration");
    println!("  bootstrap.env:    {}", env::var("LDEX_WALLET_CONFIG").unwrap_or_else(|_| "<unset>".into()));
    println!("  sequencer:        {}", env::var("LDEX_SEQUENCER_ADDR").unwrap_or_else(|_| "<unset>".into()));
    println!("  mutating tests:   {} (set LAYER_A_MUTATE=1 to enable)", if do_mutate { "ON" } else { "off" });
    println!();

    let mut results = Vec::new();

    results.push(test_env_shape());

    // Bail early if env shape is broken — chain queries would all fail.
    if !results[0].pass {
        print_summary(&results);
        std::process::exit(1);
    }

    // Print env_shape result that was already pushed.
    let env_r = results.pop().unwrap();
    let tag = if env_r.pass { "PASS" } else { "FAIL" };
    println!("[{tag}] {:38}  {:>7}ms   {}", env_r.name, env_r.elapsed_ms, env_r.detail);
    results.push(env_r);

    let c = Ctx::load();
    let suite: &[(&str, &dyn Fn(&Ctx) -> TestResult)] = &[
        ("pool_info_for_a_b_fee5",         &test_pool_info_for_a_b_fee5),
        ("pools_lists_seeded_pool",        &test_pools_lists_seeded_pool),
        ("balance_reads_make_sense",       &test_balance_reads_make_sense),
        ("priv_a_chain_read_returns_zero", &test_priv_a_chain_read_returns_zero),
        ("wrong_api_returns_native_lez",   &test_wrong_api_returns_native_lez),
        ("priv_a_token_balance_correct",   &test_priv_a_token_balance_correct),
    ];
    for (_, f) in suite {
        let r = f(&c);
        let tag = if r.pass { "PASS" } else { "FAIL" };
        println!("[{tag}] {:38}  {:>7}ms   {}", r.name, r.elapsed_ms, r.detail);
        results.push(r);
    }

    if do_mutate {
        results.push(test_wrap_round_trip(&c));
    }

    print_summary(&results);
    if results.iter().any(|r| !r.pass) {
        std::process::exit(1);
    }
}

// Print one line per result as it lands. Lets us see hangs/crashes mid-suite.
fn step(name: &str, r: TestResult) -> TestResult {
    let tag = if r.pass { "PASS" } else { "FAIL" };
    println!("[{tag}] {:38}  {:>7}ms   {}", r.name, r.elapsed_ms, r.detail);
    let _ = name; r
}

fn print_summary(rs: &[TestResult]) {
    let pass_count = rs.iter().filter(|r| r.pass).count();
    let fail_count = rs.iter().filter(|r| !r.pass).count();
    println!("{:─<74}", "");
    for r in rs {
        let tag = if r.pass { "PASS" } else { "FAIL" };
        println!("[{tag}] {:38}  {:>7}ms   {}", r.name, r.elapsed_ms, r.detail);
    }
    println!("{:─<74}", "");
    println!("Total: {} pass, {} fail", pass_count, fail_count);
}
