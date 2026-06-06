//! ldex — command-line client for the LDEX privacy DEX on Logos
//!
//! Mirrors every mini-app feature behind the same FFI the QML uses.
//! Source `scripts/bootstrap.env` (or pass `--env-file`) and run any
//! subcommand. Token names accept the bootstrap shortcuts (A, B, …, LEZ)
//! or any raw `Public/<b58>` | `Private/<b58>` | 64-hex id.
//!
//! Commands:
//!   status                  chain + wallet snapshot
//!   sync                    force wallet sync to chain head
//!   balance [--all]         per-token HOLD / ATA / PRIV totals
//!   accounts                list all wallet-owned accounts
//!   pools                   all (pair, fee) pool rows
//!   pool <A> <B> [-f BPS]   one pool's state
//!   quote <A> <B> <AMT> [-f BPS]
//!   wrap <AMT>              native LEZ → WLEZ (HOLD_W)
//!   unwrap <AMT>            WLEZ (HOLD_W) → native LEZ
//!   shield   <T> <AMT>      HOLD_<T> → PRIV_<T>  (real STARK)
//!   deshield <T> <AMT>      PRIV_<T> → HOLD_<T>  (real STARK)
//!   swap <PAY> <GET> <AMT>  mode-0 public ATA swap
//!     [--mode public|private|disposable] [--fee BPS] [--slip PCT]
//!   pool-create <A> <B> [-f BPS] [--amount-a N --amount-b N]
//!   liq add <A> <B> <AMT_A> <AMT_B> [-f BPS] [--mode public|private]
//!   liq remove <A> <B> <LP_AMT>     [-f BPS] [--mode public|private]
//!   env                     dump the resolved environment
//!
//! All chain-mutating commands print the resulting tx hash on success.
//! Private/Disposable swaps + shield/deshield take 3–25 min CPU.

use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

// ─────────────────────────────────────────────────────────── FFI bindings

// All FFI signatures verified against ffi/ldex-amm-ffi/src/submit.rs.
// Most ATA-flavoured functions auto-derive the AMM/TOKEN/ATA program ids
// from process env vars (LDEX_AMM_V2_PROGRAM_ID, LDEX_TOKEN_PROGRAM_ID,
// LDEX_ATA_PROGRAM_ID) — see `ata_env_ctx` in submit.rs. The CLI exports
// every bootstrap.env key to the process env before calling any FFI.
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
    fn ldex_amm_v2_new_pool_ata(
        cfg: *const i8, store: *const i8,
        amm_v2_program_id: *const u8,
        owner: *const u8, user_holding_a: *const u8, user_holding_b: *const u8,
        amount_a: u128, amount_b: u128, fees: u128,
        deadline: u64, out_tx: *mut u8) -> i32;
    fn ldex_amm_v2_swap_exact_in_ata(
        cfg: *const i8, store: *const i8,
        amm_v2_program_id: *const u8,
        owner: *const u8,
        token_def_a: *const u8, token_def_b: *const u8,
        token_definition_in: *const u8,
        swap_amount_in: u128, min_amount_out: u128, fees: u128,
        deadline: u64, out_tx: *mut u8) -> i32;
    fn ldex_amm_v2_add_liquidity_ata(
        cfg: *const i8, store: *const i8,
        amm_v2_program_id: *const u8,
        owner: *const u8,
        token_def_a: *const u8, token_def_b: *const u8,
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128, max_amount_to_add_token_b: u128,
        fees: u128, deadline: u64, out_tx: *mut u8) -> i32;
    fn ldex_amm_v2_remove_liquidity_ata(
        cfg: *const i8, store: *const i8,
        amm_v2_program_id: *const u8,
        owner: *const u8,
        token_def_a: *const u8, token_def_b: *const u8,
        remove_liquidity_amount: u128,
        min_amount_to_remove_token_a: u128, min_amount_to_remove_token_b: u128,
        fees: u128, deadline: u64, out_tx: *mut u8) -> i32;
    fn ldex_amm_v2_private_swap_exact_in(
        cfg: *const i8, store: *const i8,
        amm_v2_program_id: *const u8,
        user_holding_a: *const u8, user_holding_b: *const u8,
        token_def_a: *const u8, token_def_b: *const u8,
        token_definition_in: *const u8,
        swap_amount_in: u128, min_amount_out: u128, fees: u128,
        deadline: u64, out_tx: *mut u8) -> i32;
    fn ldex_amm_v2_disposable_swap(
        cfg: *const i8, store: *const i8,
        amm_v2_program_id: *const u8,
        user_holding_a: *const u8, user_holding_b: *const u8,
        a_holding_a: *const u8, a_holding_b: *const u8,
        token_def_a: *const u8, token_def_b: *const u8,
        token_definition_in: *const u8,
        swap_amount_in: u128, min_amount_out: u128, fees: u128,
        deadline: u64, out_tx: *mut u8) -> i32;
    fn ldex_amm_v2_private_add_liquidity(
        cfg: *const i8, store: *const i8,
        amm_v2_program_id: *const u8,
        user_holding_a: *const u8, user_holding_b: *const u8, user_holding_lp: *const u8,
        token_def_a: *const u8, token_def_b: *const u8,
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128, max_amount_to_add_token_b: u128,
        fees: u128, deadline: u64, out_tx: *mut u8) -> i32;
    fn ldex_amm_v2_private_remove_liquidity(
        cfg: *const i8, store: *const i8,
        amm_v2_program_id: *const u8,
        user_holding_a: *const u8, user_holding_b: *const u8, user_holding_lp: *const u8,
        token_def_a: *const u8, token_def_b: *const u8,
        remove_liquidity_amount: u128,
        min_amount_to_remove_token_a: u128, min_amount_to_remove_token_b: u128,
        fees: u128, deadline: u64, out_tx: *mut u8) -> i32;
    fn ldex_token_shield(
        cfg: *const i8, store: *const i8,
        sender: *const u8, recipient: *const u8,
        amount: u128, out_tx: *mut u8) -> i32;
    fn ldex_token_deshield(
        cfg: *const i8, store: *const i8,
        sender: *const u8, recipient: *const u8,
        amount: u128, out_tx: *mut u8) -> i32;
    fn ldex_wlez_wrap(
        cfg: *const i8, store: *const i8, wlez_pid: *const u8,
        from: *const u8, to: *const u8, amount: u128, out_tx: *mut u8) -> i32;
    fn ldex_wlez_unwrap(
        cfg: *const i8, store: *const i8, wlez_pid: *const u8,
        from: *const u8, to: *const u8, amount: u128, out_tx: *mut u8) -> i32;
}

// FfiAccount layout (matches wallet_ffi.h). FfiProgramId is 32 bytes (8 × u32),
// not 8 — getting that wrong shifts the data pointer.
#[repr(C)]
struct WalletHandle { _o: [u8; 0] }
#[repr(C)]
struct FfiBytes32 { data: [u8; 32] }
#[repr(C)]
struct FfiProgramId { _data: [u32; 8] }
#[repr(C)]
struct FfiU128 { _bytes: [u8; 16] }
#[repr(C)]
struct FfiAccount {
    _program_owner: FfiProgramId,
    _balance: FfiU128,
    data: *const u8,
    data_len: usize,
    _nonce: FfiU128,
}
#[repr(C)]
struct FfiAccountListEntry {
    account_id: FfiBytes32,
    is_public: bool,
}
#[repr(C)]
struct FfiAccountList {
    entries: *const FfiAccountListEntry,
    count: usize,
}

#[link(name = "wallet_ffi")]
extern "C" {
    fn wallet_ffi_open(cfg: *const i8, store: *const i8) -> *mut WalletHandle;
    fn wallet_ffi_destroy(h: *mut WalletHandle);
    fn wallet_ffi_get_current_block_height(h: *mut WalletHandle, out: *mut u64) -> i32;
    fn wallet_ffi_sync_to_block(h: *mut WalletHandle, block: u64) -> i32;
    fn wallet_ffi_get_last_synced_block(h: *mut WalletHandle, out: *mut u64) -> i32;
    fn wallet_ffi_get_balance(h: *mut WalletHandle, id: *const FfiBytes32,
                              is_public: bool, out_balance: *mut [u8; 16]) -> i32;
    fn wallet_ffi_get_account_private(h: *mut WalletHandle, id: *const FfiBytes32,
                                      out: *mut FfiAccount) -> i32;
    fn wallet_ffi_free_account_data(account: *mut FfiAccount);
    fn wallet_ffi_list_accounts(h: *mut WalletHandle, out: *mut FfiAccountList) -> i32;
    fn wallet_ffi_free_account_list(list: *mut FfiAccountList);
}

// ─────────────────────────────────────────────────────────── env loading

#[derive(Debug)]
struct Env {
    map: HashMap<String, String>,
    cfg: CString,
    store: CString,
}

impl Env {
    fn load(path: &PathBuf) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let mut map = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            let s = line.strip_prefix("export ").unwrap_or(line);
            if let Some(eq) = s.find('=') {
                let k = s[..eq].trim().to_string();
                let mut v = s[eq+1..].trim().to_string();
                if v.starts_with('"') && v.ends_with('"') && v.len() >= 2 {
                    v = v[1..v.len()-1].to_string();
                }
                map.insert(k, v);
            }
        }
        // Export every key to the process env — the FFI reads
        // LDEX_AMM_V2_PROGRAM_ID / LDEX_TOKEN_PROGRAM_ID / LDEX_ATA_PROGRAM_ID /
        // LDEX_ROUTER_PROGRAM_ID / etc. from std::env::var inside ata_env_ctx
        // and similar helpers. Without this, every chain-mutating ATA call
        // returns rc=3 (LDEX_AMM_ERR_ACCOUNT).
        for (k, v) in &map {
            std::env::set_var(k, v);
        }
        let cfg = CString::new(map.get("LDEX_WALLET_CONFIG").cloned()
            .ok_or("LDEX_WALLET_CONFIG missing")?).map_err(|e| e.to_string())?;
        let store = CString::new(map.get("LDEX_WALLET_STORAGE").cloned()
            .ok_or("LDEX_WALLET_STORAGE missing")?).map_err(|e| e.to_string())?;
        Ok(Env { map, cfg, store })
    }

    fn get(&self, k: &str) -> Result<&str, String> {
        self.map.get(k).map(|s| s.as_str())
            .ok_or_else(|| format!("env var {k} missing from bootstrap.env"))
    }
    fn opt(&self, k: &str) -> Option<&str> { self.map.get(k).map(|s| s.as_str()) }
    fn hex_id(&self, k: &str) -> Result<[u8; 32], String> {
        let h = self.get(k)?;
        parse_hex(h).ok_or_else(|| format!("{k} = {h:?} is not a 64-char hex id"))
    }
    fn acct_id(&self, k: &str) -> Result<[u8; 32], String> {
        parse_account(self.get(k)?)
    }
    fn opt_acct_id(&self, k: &str) -> Option<[u8; 32]> {
        self.opt(k).and_then(parse_account_str_silently)
    }
}

// ─────────────────────────────────────────────────────────── id helpers

fn parse_hex(h: &str) -> Option<[u8; 32]> {
    if h.len() != 64 || !h.bytes().all(|c| c.is_ascii_hexdigit()) { return None; }
    let mut o = [0u8; 32];
    for i in 0..32 {
        o[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(o)
}

fn parse_account(s: &str) -> Result<[u8; 32], String> {
    let cs = CString::new(s).map_err(|_| format!("invalid CString {s:?}"))?;
    let mut o = [0u8; 32];
    let rc = unsafe { ldex_amm_parse_account_id(cs.as_ptr(), o.as_mut_ptr()) };
    if rc != 0 { return Err(format!("could not parse account id {s:?} (rc={rc})")); }
    Ok(o)
}

fn parse_account_str_silently(s: &str) -> Option<[u8; 32]> {
    parse_account(s).ok()
}

fn hx(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b { s.push_str(&format!("{x:02x}")); }
    s
}

// Resolve a token reference (A | LEZ | Public/<b58> | 64hex) → (letter or LEZ, def_id).
// Letters are used to find the matching HOLD_/ATA_/PRIV_ ids in env.
fn resolve_token(env: &Env, name: &str) -> Result<TokenRef, String> {
    let upper = name.to_uppercase();
    if upper == "LEZ" || upper == "WLEZ" {
        return Ok(TokenRef { letter: Some("LEZ".into()),
            def_id: env.hex_id("LDEX_WLEZ_DEF")?, is_native: true });
    }
    // Single-letter alias (A..Z): look up LDEX_DEF_<L>
    if upper.len() == 1 && upper.chars().all(|c| c.is_ascii_uppercase()) {
        let k = format!("LDEX_DEF_{upper}");
        if env.opt(&k).is_some() {
            return Ok(TokenRef { letter: Some(upper.clone()),
                def_id: env.acct_id(&k)?, is_native: false });
        }
    }
    // Otherwise parse as a raw account id.
    let id = parse_account(name)?;
    Ok(TokenRef { letter: None, def_id: id, is_native: false })
}

struct TokenRef {
    letter: Option<String>,
    def_id: [u8; 32],
    #[allow(dead_code)]
    is_native: bool,
}

// ─────────────────────────────────────────────────────────── output

fn ok(msg: impl AsRef<str>) { println!("✓ {}", msg.as_ref()); }
fn info(msg: impl AsRef<str>) { println!("  {}", msg.as_ref()); }
fn warn(msg: impl AsRef<str>) { eprintln!("! {}", msg.as_ref()); }
fn fail(msg: impl AsRef<str>) -> ! { eprintln!("✗ {}", msg.as_ref()); std::process::exit(1); }

// ─────────────────────────────────────────────────────────── chain helpers

fn balance_public_token(env: &Env, acct: &[u8; 32]) -> u128 {
    let mut buf = [0u8; 256];
    let rc = unsafe {
        ldex_amm_token_balance(env.cfg.as_ptr(), env.store.as_ptr(),
            acct.as_ptr(), buf.as_mut_ptr(), buf.len())
    };
    if rc != 0 { return 0; }
    let s = unsafe { CStr::from_ptr(buf.as_ptr() as *const i8).to_string_lossy().to_string() };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap_or(serde_json::Value::Null);
    v.get("balance").and_then(|x| x.as_str()).and_then(|x| x.parse().ok()).unwrap_or(0)
}

fn open_wallet(env: &Env) -> *mut WalletHandle {
    let h = unsafe { wallet_ffi_open(env.cfg.as_ptr(), env.store.as_ptr()) };
    if h.is_null() { fail("wallet_ffi_open failed"); }
    h
}

fn balance_private_token(env: &Env, acct: &[u8; 32]) -> u128 {
    let h = open_wallet(env);
    let fid = FfiBytes32 { data: *acct };
    let mut acc = FfiAccount {
        _program_owner: FfiProgramId { _data: [0; 8] },
        _balance: FfiU128 { _bytes: [0; 16] },
        data: std::ptr::null(),
        data_len: 0,
        _nonce: FfiU128 { _bytes: [0; 16] },
    };
    let rc = unsafe { wallet_ffi_get_account_private(h, &fid, &mut acc) };
    if rc != 0 { unsafe { wallet_ffi_destroy(h); } return 0; }
    // borsh layout for TokenHolding::Fungible: [tag=0, def(32), balance_le(16)] = 49 bytes
    let v = if !acc.data.is_null() && acc.data_len >= 49 {
        let slice = unsafe { std::slice::from_raw_parts(acc.data, acc.data_len) };
        if slice[0] == 0 {
            let mut b = [0u8; 16];
            b.copy_from_slice(&slice[33..49]);
            u128::from_le_bytes(b)
        } else { 0 }
    } else { 0 };
    unsafe {
        wallet_ffi_free_account_data(&mut acc);
        wallet_ffi_destroy(h);
    }
    v
}

fn balance_native_lez(env: &Env, acct: &[u8; 32]) -> u128 {
    let h = open_wallet(env);
    let fid = FfiBytes32 { data: *acct };
    let mut out = [0u8; 16];
    let rc = unsafe { wallet_ffi_get_balance(h, &fid, /*is_public*/true, &mut out) };
    unsafe { wallet_ffi_destroy(h); }
    if rc != 0 { 0 } else { u128::from_le_bytes(out) }
}

fn sync_to_head(env: &Env) -> Result<(u64, u64), String> {
    let h = open_wallet(env);
    let mut head = 0u64;
    let rc = unsafe { wallet_ffi_get_current_block_height(h, &mut head) };
    if rc != 0 {
        unsafe { wallet_ffi_destroy(h); }
        return Err(format!("get_current_block_height rc={rc}"));
    }
    let mut last = 0u64;
    let _ = unsafe { wallet_ffi_get_last_synced_block(h, &mut last) };
    let rc = unsafe { wallet_ffi_sync_to_block(h, head) };
    unsafe { wallet_ffi_destroy(h); }
    if rc != 0 { return Err(format!("sync_to_block({head}) rc={rc}")); }
    Ok((last, head))
}

fn pool_info_json(env: &Env, a: &[u8; 32], b: &[u8; 32], fee: u128) -> serde_json::Value {
    let amm = env.hex_id("LDEX_AMM_V2_PROGRAM_ID").unwrap_or_default();
    let mut buf = [0u8; 1024];
    let rc = unsafe {
        ldex_amm_pool_info(env.cfg.as_ptr(), env.store.as_ptr(),
            amm.as_ptr(), a.as_ptr(), b.as_ptr(), fee,
            buf.as_mut_ptr(), buf.len())
    };
    if rc != 0 { return serde_json::Value::Null; }
    let s = unsafe { CStr::from_ptr(buf.as_ptr() as *const i8).to_string_lossy().to_string() };
    serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)
}

// ─────────────────────────────────────────────────────────── CLI shape

// Default env-file path: discover scripts/bootstrap.env relative to the
// repo. Order:
//   1. $LDEX_ENV_FILE if set,
//   2. <repo>/scripts/bootstrap.env where <repo> is the CARGO_MANIFEST_DIR
//      at build time + "/.." (i.e. the parent of cli/),
//   3. fallback to ./scripts/bootstrap.env so a user running from the
//      repo root finds it anyway.
fn default_env_file() -> PathBuf {
    if let Ok(p) = std::env::var("LDEX_ENV_FILE") {
        return PathBuf::from(p);
    }
    let baked: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/bootstrap.env");
    let baked_pb = PathBuf::from(baked);
    if baked_pb.exists() { return baked_pb; }
    PathBuf::from("./scripts/bootstrap.env")
}

#[derive(Parser, Debug)]
#[command(name = "ldex", version, about = "Command-line client for the LDEX privacy DEX")]
struct Cli {
    /// Path to scripts/bootstrap.env. Default: $LDEX_ENV_FILE, or
    /// <repo>/scripts/bootstrap.env discovered at build time.
    #[arg(long, global = true, default_value_os_t = default_env_file())]
    env_file: PathBuf,

    /// Auto-sync the wallet to chain head before commands that read private state.
    #[arg(long, global = true)]
    no_sync: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Chain + wallet snapshot
    Status,
    /// Force wallet sync to chain head
    Sync,
    /// Show per-token balances (HOLD + ATA + PRIV)
    Balance {
        /// Show every token, not just A/B/LEZ
        #[arg(long)]
        all: bool,
    },
    /// List wallet-owned accounts
    Accounts,
    /// List all (pair, fee) pool rows
    Pools,
    /// Show one pool's state
    Pool {
        a: String,
        b: String,
        #[arg(short = 'f', long = "fee", default_value_t = 5)]
        fee_bps: u128,
    },
    /// Show a swap quote without submitting
    Quote {
        pay: String,
        get: String,
        amount: u128,
        #[arg(short = 'f', long = "fee", default_value_t = 5)]
        fee_bps: u128,
    },
    /// Wrap native LEZ → WLEZ (HOLD_W)
    Wrap { amount: u128 },
    /// Unwrap WLEZ → native LEZ
    Unwrap { amount: u128 },
    /// Move HOLD_<T> → PRIV_<T> (real STARK, several minutes)
    Shield { token: String, amount: u128 },
    /// Move PRIV_<T> → HOLD_<T> (real STARK, several minutes)
    Deshield { token: String, amount: u128 },
    /// Swap PAY → GET. Modes: public (mode-0 ATA, ~15 s), private (mode-1
    /// PrivateOwned, ~12 min STARK), disposable (mode-2 fresh A, ~15 min STARK)
    Swap {
        pay: String,
        get: String,
        amount: u128,
        #[arg(short, long, default_value = "public")]
        mode: String,
        #[arg(short = 'f', long = "fee", default_value_t = 5)]
        fee_bps: u128,
        #[arg(short = 's', long = "slip", default_value_t = 1.0)]
        slip_pct: f64,
    },
    /// Create a new (TOKENA, TOKENB, fee) pool, seeding it with initial liquidity
    PoolCreate {
        a: String,
        b: String,
        #[arg(short = 'f', long = "fee", default_value_t = 5)]
        fee_bps: u128,
        #[arg(long = "amount-a", default_value_t = 100_000)]
        amount_a: u128,
        #[arg(long = "amount-b", default_value_t = 100_000)]
        amount_b: u128,
    },
    /// Liquidity operations
    #[command(subcommand)]
    Liq(LiqCmd),
    /// Top up native LEZ + each token's ATA from existing supply.
    /// Useful for re-priming the wallet between test runs without
    /// re-running the full bootstrap. By default funds A/B; pass
    /// --tokens "A B C D" to widen.
    Fund {
        /// Native LEZ to send from the genesis funder to USER_OWNER.
        #[arg(long = "lez", default_value_t = 1000)]
        lez_amount: u128,
        /// Per-token amount to send from HOLD_<L> to ATA_<L>.
        #[arg(long = "token", default_value_t = 10_000)]
        token_amount: u128,
        /// Space-separated token letters to top up.
        #[arg(long, default_value = "A B")]
        tokens: String,
        /// Skip the LEZ auth-transfer step.
        #[arg(long)]
        skip_lez: bool,
        /// Skip the token ATA top-up step.
        #[arg(long)]
        skip_tokens: bool,
    },
    /// Dump the resolved bootstrap environment
    Env,
}

#[derive(Subcommand, Debug)]
enum LiqCmd {
    /// Add liquidity to a pool
    Add {
        a: String,
        b: String,
        amount_a: u128,
        amount_b: u128,
        #[arg(short = 'f', long = "fee", default_value_t = 5)]
        fee_bps: u128,
        #[arg(short, long, default_value = "public")]
        mode: String,
    },
    /// Remove liquidity from a pool
    Remove {
        a: String,
        b: String,
        lp_amount: u128,
        #[arg(short = 'f', long = "fee", default_value_t = 5)]
        fee_bps: u128,
        #[arg(short, long, default_value = "public")]
        mode: String,
        #[arg(long = "min-a", default_value_t = 0)]
        min_a: u128,
        #[arg(long = "min-b", default_value_t = 0)]
        min_b: u128,
    },
}

// ─────────────────────────────────────────────────────────── command impls

fn cmd_status(env: &Env) -> Result<(), String> {
    println!("LDEX status");
    println!("  bootstrap.env:    {}", env.cfg.to_string_lossy());
    println!("  sequencer:        {}", env.opt("LDEX_SEQUENCER_ADDR").unwrap_or("?"));
    println!("  amm_v2:           {}", env.opt("LDEX_AMM_V2_PROGRAM_ID").unwrap_or("?"));
    println!("  wlez:             {}", env.opt("LDEX_WLEZ_PROGRAM_ID").unwrap_or("?"));
    println!("  user_owner:       {}", env.opt("LDEX_USER_OWNER").unwrap_or("?"));

    let h = open_wallet(env);
    let mut head = 0u64;
    let mut last = 0u64;
    unsafe {
        let _ = wallet_ffi_get_current_block_height(h, &mut head);
        let _ = wallet_ffi_get_last_synced_block(h, &mut last);
        wallet_ffi_destroy(h);
    }
    println!("  chain height:     {head}");
    println!("  wallet synced to: {last}  (delta {})", head.saturating_sub(last));

    if let Ok(owner) = env.acct_id("LDEX_USER_OWNER") {
        let lez = balance_native_lez(env, &owner);
        println!("  native LEZ:       {lez}");
    }
    Ok(())
}

fn cmd_sync(env: &Env) -> Result<(), String> {
    let t0 = Instant::now();
    let (last_before, head) = sync_to_head(env)?;
    ok(format!("synced {} → {} in {:?}", last_before, head, t0.elapsed()));
    Ok(())
}

fn cmd_balance(env: &Env, all: bool, auto_sync: bool) -> Result<(), String> {
    if auto_sync { let _ = sync_to_head(env); }
    let letters: Vec<String> = if all {
        env.opt("LDEX_TOKENS").unwrap_or("A B").split_whitespace().map(String::from).collect()
    } else {
        vec!["A".into(), "B".into()]
    };
    println!("{:8}  {:>14}  {:>14}  {:>14}  {:>14}",
             "TOKEN", "HOLD", "ATA", "PRIV", "TOTAL");
    println!("{}", "─".repeat(74));
    // Native LEZ + WLEZ
    if let Ok(owner) = env.acct_id("LDEX_USER_OWNER") {
        let lez = balance_native_lez(env, &owner);
        let hold_w = env.opt_acct_id("LDEX_HOLD_W").map(|a| balance_public_token(env, &a)).unwrap_or(0);
        let ata_w = env.opt_acct_id("LDEX_ATA_W").map(|a| balance_public_token(env, &a)).unwrap_or(0);
        println!("{:8}  {:>14}  {:>14}  {:>14}  {:>14}",
                 "LEZ/WLEZ", lez, format!("{}+{}", hold_w, ata_w), "—", lez + hold_w + ata_w);
    }
    for letter in &letters {
        let hold = env.opt_acct_id(&format!("LDEX_HOLD_{letter}")).map(|a| balance_public_token(env, &a)).unwrap_or(0);
        let ata  = env.opt_acct_id(&format!("LDEX_ATA_{letter}")).map(|a| balance_public_token(env, &a)).unwrap_or(0);
        let prv  = env.opt_acct_id(&format!("LDEX_PRIV_{letter}")).map(|a| balance_private_token(env, &a)).unwrap_or(0);
        let tot = hold + ata + prv;
        println!("{:8}  {:>14}  {:>14}  {:>14}  {:>14}",
                 format!("TOKEN{letter}"), hold, ata, prv, tot);
    }
    Ok(())
}

fn cmd_accounts(env: &Env) -> Result<(), String> {
    let h = open_wallet(env);
    let mut list = FfiAccountList { entries: std::ptr::null(), count: 0 };
    let rc = unsafe { wallet_ffi_list_accounts(h, &mut list) };
    if rc != 0 {
        unsafe { wallet_ffi_destroy(h); }
        return Err(format!("wallet_ffi_list_accounts rc={rc}"));
    }
    println!("{:5} {:>4}  account_id", "kind", "#");
    println!("{}", "─".repeat(80));
    for i in 0..list.count {
        let e = unsafe { &*list.entries.add(i) };
        let kind = if e.is_public { "pub" } else { "priv" };
        let id = hx(&e.account_id.data);
        println!("{:5} {:>4}  {}", kind, i, id);
    }
    unsafe {
        wallet_ffi_free_account_list(&mut list);
        wallet_ffi_destroy(h);
    }
    Ok(())
}

fn cmd_pools(env: &Env) -> Result<(), String> {
    let letters: Vec<String> = env.opt("LDEX_TOKENS").unwrap_or("A B").split_whitespace().map(String::from).collect();
    let mut toks: Vec<(String, [u8; 32])> = vec![];
    for letter in &letters {
        if let Some(a) = env.opt_acct_id(&format!("LDEX_DEF_{letter}")) {
            toks.push((format!("TOKEN{letter}"), a));
        }
    }
    if let Some(w) = env.opt_acct_id("LDEX_WLEZ_DEF") {
        toks.push(("LEZ".into(), w));
    }
    let tiers: [u128; 4] = [1, 5, 30, 100];
    println!("{:14}  {:>4}  {:>14}  {:>14}  {:>14}",
             "pair", "fee", "reserve_a", "reserve_b", "lp_supply");
    println!("{}", "─".repeat(74));
    let mut found = 0usize;
    for i in 0..toks.len() {
        for j in i + 1..toks.len() {
            for f in tiers {
                let p = pool_info_json(env, &toks[i].1, &toks[j].1, f);
                if p.get("exists").and_then(|v| v.as_bool()).unwrap_or(false) {
                    let r_a = p.get("reserve_a").and_then(|v| v.as_str()).unwrap_or("?");
                    let r_b = p.get("reserve_b").and_then(|v| v.as_str()).unwrap_or("?");
                    let lp  = p.get("lp_supply").and_then(|v| v.as_str()).unwrap_or("?");
                    let bps = format!("{}.{:02}%", f / 100, f % 100);
                    println!("{:14}  {:>4}  {:>14}  {:>14}  {:>14}",
                             format!("{}/{}", toks[i].0, toks[j].0), bps, r_a, r_b, lp);
                    found += 1;
                }
            }
        }
    }
    if found == 0 { warn("no pools found"); }
    Ok(())
}

fn cmd_pool(env: &Env, a: &str, b: &str, fee: u128) -> Result<(), String> {
    let ta = resolve_token(env, a)?;
    let tb = resolve_token(env, b)?;
    let p = pool_info_json(env, &ta.def_id, &tb.def_id, fee);
    if !p.get("exists").and_then(|v| v.as_bool()).unwrap_or(false) {
        let pr = pool_info_json(env, &tb.def_id, &ta.def_id, fee);
        if pr.get("exists").and_then(|v| v.as_bool()).unwrap_or(false) {
            return cmd_pool(env, b, a, fee);  // print with B/A ordering
        }
        warn(format!("no pool at fee={fee} for {a}/{b} (tiers: 1, 5, 30, 100)"));
        return Ok(());
    }
    println!("{}/{} @ {:.2}%", a, b, fee as f64 / 100.0);
    for k in ["reserve_a","reserve_b","lp_supply","cum_volume_a","cum_volume_b","cum_fees_a","cum_fees_b"] {
        if let Some(v) = p.get(k).and_then(|v| v.as_str()) {
            println!("  {k:18} {v}");
        }
    }
    Ok(())
}

// Constant-product quote at 30-bps-of-input fee (replicates amm_core math).
fn cmd_quote(env: &Env, pay: &str, get: &str, amt: u128, fee: u128) -> Result<(), String> {
    let tp = resolve_token(env, pay)?;
    let tg = resolve_token(env, get)?;
    let mut p = pool_info_json(env, &tp.def_id, &tg.def_id, fee);
    let mut pay_is_a = true;
    if !p.get("exists").and_then(|v| v.as_bool()).unwrap_or(false) {
        p = pool_info_json(env, &tg.def_id, &tp.def_id, fee);
        pay_is_a = false;
        if !p.get("exists").and_then(|v| v.as_bool()).unwrap_or(false) {
            return Err(format!("no pool for {pay}/{get} at fee={fee}"));
        }
    }
    let ra: u128 = p["reserve_a"].as_str().unwrap_or("0").parse().unwrap_or(0);
    let rb: u128 = p["reserve_b"].as_str().unwrap_or("0").parse().unwrap_or(0);
    let (r_in, r_out) = if pay_is_a { (ra, rb) } else { (rb, ra) };
    if amt == 0 || r_in == 0 || r_out == 0 {
        return Err("empty pool or zero amount: cannot quote".into());
    }
    // effective_in = floor(amt * (10000-fee) / 10000)
    let eff_in = amt.saturating_mul(10_000u128.saturating_sub(fee)) / 10_000;
    let out = r_out.saturating_mul(eff_in) / r_in.saturating_add(eff_in);
    let price_now = if pay_is_a { rb as f64 / ra as f64 } else { ra as f64 / rb as f64 };
    let price_eff = out as f64 / amt as f64;
    let impact = (1.0 - price_eff / price_now) * 100.0;
    println!("quote {pay} → {get} @ {:.2}% fee", fee as f64 / 100.0);
    println!("  pay:         {amt}");
    println!("  receive:     {out}");
    println!("  price (now): {price_now:.6}");
    println!("  price (eff): {price_eff:.6}");
    println!("  impact:      {impact:.4} %");
    println!("  fee paid:    {}  (deducted from input pre-pricing)", amt - eff_in);
    Ok(())
}

fn cmd_wrap(env: &Env, amount: u128) -> Result<(), String> {
    let wlez = env.hex_id("LDEX_WLEZ_PROGRAM_ID")?;
    let owner = env.acct_id("LDEX_USER_OWNER")?;
    let hold_w = env.acct_id("LDEX_HOLD_W")?;
    let mut tx = [0u8; 32];
    let t0 = Instant::now();
    let rc = unsafe {
        ldex_wlez_wrap(env.cfg.as_ptr(), env.store.as_ptr(),
            wlez.as_ptr(), owner.as_ptr(), hold_w.as_ptr(), amount, tx.as_mut_ptr())
    };
    if rc != 0 { return Err(format!("wrap rc={rc}")); }
    ok(format!("wrap {amount} LEZ → WLEZ (HOLD_W)  tx={}  ({:?})", hx(&tx), t0.elapsed()));
    Ok(())
}

fn cmd_unwrap(env: &Env, amount: u128) -> Result<(), String> {
    let wlez = env.hex_id("LDEX_WLEZ_PROGRAM_ID")?;
    let owner = env.acct_id("LDEX_USER_OWNER")?;
    let hold_w = env.acct_id("LDEX_HOLD_W")?;
    let mut tx = [0u8; 32];
    let t0 = Instant::now();
    let rc = unsafe {
        ldex_wlez_unwrap(env.cfg.as_ptr(), env.store.as_ptr(),
            wlez.as_ptr(), hold_w.as_ptr(), owner.as_ptr(), amount, tx.as_mut_ptr())
    };
    if rc != 0 { return Err(format!("unwrap rc={rc}")); }
    ok(format!("unwrap {amount} WLEZ → LEZ  tx={}  ({:?})", hx(&tx), t0.elapsed()));
    Ok(())
}

fn cmd_shield(env: &Env, token: &str, amount: u128) -> Result<(), String> {
    let t = resolve_token(env, token)?;
    let letter = t.letter.as_deref()
        .ok_or_else(|| format!("shield needs a TOKEN letter or LEZ, got {token}"))?;
    let hold = env.acct_id(&format!("LDEX_HOLD_{letter}"))?;
    let prv  = env.acct_id(&format!("LDEX_PRIV_{letter}"))?;
    println!("shielding {amount} TOKEN{letter} (real STARK — typically 3–5 min)...");
    let mut tx = [0u8; 32];
    let t0 = Instant::now();
    let rc = unsafe {
        ldex_token_shield(env.cfg.as_ptr(), env.store.as_ptr(),
            hold.as_ptr(), prv.as_ptr(), amount, tx.as_mut_ptr())
    };
    if rc != 0 { return Err(format!("shield rc={rc} after {:?}", t0.elapsed())); }
    ok(format!("shielded {amount} TOKEN{letter}  tx={}  ({:?})", hx(&tx), t0.elapsed()));
    Ok(())
}

fn cmd_deshield(env: &Env, token: &str, amount: u128) -> Result<(), String> {
    let t = resolve_token(env, token)?;
    let letter = t.letter.as_deref()
        .ok_or_else(|| format!("deshield needs a TOKEN letter, got {token}"))?;
    let prv = env.acct_id(&format!("LDEX_PRIV_{letter}"))?;
    let hold = env.acct_id(&format!("LDEX_HOLD_{letter}"))?;
    println!("deshielding {amount} TOKEN{letter} (real STARK — typically 3–5 min)...");
    let mut tx = [0u8; 32];
    let t0 = Instant::now();
    let rc = unsafe {
        ldex_token_deshield(env.cfg.as_ptr(), env.store.as_ptr(),
            prv.as_ptr(), hold.as_ptr(), amount, tx.as_mut_ptr())
    };
    if rc != 0 { return Err(format!("deshield rc={rc} after {:?}", t0.elapsed())); }
    ok(format!("deshielded {amount} TOKEN{letter}  tx={}  ({:?})", hx(&tx), t0.elapsed()));
    Ok(())
}

fn cmd_swap(env: &Env, pay: &str, get: &str, amount: u128,
            mode: &str, fee: u128, slip_pct: f64) -> Result<(), String> {
    let tp = resolve_token(env, pay)?;
    let tg = resolve_token(env, get)?;
    // Derive a min-out from quote * (1 - slippage). The pool PDA is
    // order-independent (the seed sorts the two def ids), so a single
    // probe returns the canonical pool regardless of (pay, get) order;
    // `pool_info` reports the pool's *stored* token_a via `token_a_def`.
    // `pay_is_a` must reflect that stored leg order — NOT the probe arg
    // order — so reserve_in/out (and thus min_out) and the (def_a, def_b)
    // we pass to the swap FFI all line up with the pool's canonical token A.
    let q = pool_info_json(env, &tp.def_id, &tg.def_id, fee);
    if !q.get("exists").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(format!("no pool for {pay}/{get} at fee={fee}"));
    }
    let pay_is_a = q.get("token_a_def").and_then(|v| v.as_str()) == Some(hx(&tp.def_id).as_str());
    let ra: u128 = q["reserve_a"].as_str().unwrap_or("0").parse().unwrap_or(0);
    let rb: u128 = q["reserve_b"].as_str().unwrap_or("0").parse().unwrap_or(0);
    let (r_in, r_out) = if pay_is_a { (ra, rb) } else { (rb, ra) };
    // MED: a slippage >= 100% (or negative) collapses min_out to 0 — silently
    // disabling slippage protection. Reject it.
    if !(0.0..100.0).contains(&slip_pct) {
        return Err(format!("slippage percent must be in [0, 100), got {slip_pct}"));
    }
    let eff_in = amount.saturating_mul(10_000u128.saturating_sub(fee)) / 10_000;
    // LOW: saturating + zero-guarded so a large quote can't overflow/panic.
    let denom = r_in.saturating_add(eff_in);
    let q_out = if denom == 0 { 0 } else { r_out.saturating_mul(eff_in) / denom };
    let min_out = ((q_out as f64) * (1.0 - slip_pct / 100.0)).floor().max(0.0) as u128;
    let deadline = u64::MAX;
    let amm = env.hex_id("LDEX_AMM_V2_PROGRAM_ID")?;
    let owner = env.acct_id("LDEX_USER_OWNER")?;
    // Pool PDA seeds use a canonical (defA, defB) ordering, but the
    // swap FFIs take the *pool's* token_def_a / token_def_b in that
    // canonical order plus a token_definition_in flag. We probed the
    // pool above; pay_is_a tells us whether `pay` is defA or defB.
    let (def_a, def_b) = if pay_is_a {
        (tp.def_id, tg.def_id)
    } else {
        (tg.def_id, tp.def_id)
    };
    let def_in = tp.def_id;

    let mut tx = [0u8; 32];
    let t0 = Instant::now();
    let rc = match mode {
        "public" | "pub" | "mode0" | "0" => {
            println!("public swap (mode-0 ATA, ~15 s)...");
            unsafe {
                ldex_amm_v2_swap_exact_in_ata(env.cfg.as_ptr(), env.store.as_ptr(),
                    amm.as_ptr(), owner.as_ptr(),
                    def_a.as_ptr(), def_b.as_ptr(), def_in.as_ptr(),
                    amount, min_out, fee, deadline, tx.as_mut_ptr())
            }
        }
        "private" | "priv" | "mode1" | "1" => {
            let lp = tp.letter.as_deref().ok_or("private swap needs token letters")?;
            let lg = tg.letter.as_deref().ok_or("private swap needs token letters")?;
            // `user_holding_a` is the PRIV holding matching pool side A.
            let priv_a_letter = if pay_is_a { lp } else { lg };
            let priv_b_letter = if pay_is_a { lg } else { lp };
            let priv_a = env.acct_id(&format!("LDEX_PRIV_{priv_a_letter}"))?;
            let priv_b = env.acct_id(&format!("LDEX_PRIV_{priv_b_letter}"))?;
            println!("private swap (mode-1 PrivateOwned — STARK ~10–15 min)...");
            unsafe {
                ldex_amm_v2_private_swap_exact_in(env.cfg.as_ptr(), env.store.as_ptr(),
                    amm.as_ptr(),
                    priv_a.as_ptr(), priv_b.as_ptr(),
                    def_a.as_ptr(), def_b.as_ptr(), def_in.as_ptr(),
                    amount, min_out, fee, deadline, tx.as_mut_ptr())
            }
        }
        "disposable" | "disp" | "mode2" | "2" => {
            let lp = tp.letter.as_deref().ok_or("disposable swap needs token letters")?;
            let lg = tg.letter.as_deref().ok_or("disposable swap needs token letters")?;
            // user_holding_a/b are the user's PRIV holdings (source of funds).
            let priv_a_letter = if pay_is_a { lp } else { lg };
            let priv_b_letter = if pay_is_a { lg } else { lp };
            let priv_a = env.acct_id(&format!("LDEX_PRIV_{priv_a_letter}"))?;
            let priv_b = env.acct_id(&format!("LDEX_PRIV_{priv_b_letter}"))?;
            // a_holding_a/b are the FRESH single-use account-A holdings the
            // disposable saga deshields into and re-shields from — typed to the
            // pool-canonical def_a / def_b. The FFI does NOT create them: it
            // reads them as real account ids. They must be freshly allocated per
            // swap (e.g. via `w account new-public` + ATA init for def_a/def_b)
            // and exported as LDEX_A_A / LDEX_A_B. Passing zero ids here (the old
            // behaviour) would deshield the user's funds into the null account.
            let a_a = env.acct_id("LDEX_A_A").map_err(|_| {
                "disposable swap needs fresh account-A holdings: set LDEX_A_A (for def_a) and \
                 LDEX_A_B (for def_b) to freshly-allocated, ATA-initialised public holdings".to_string()
            })?;
            let a_b = env.acct_id("LDEX_A_B").map_err(|_| {
                "disposable swap needs fresh account-A holdings: set LDEX_A_A (for def_a) and \
                 LDEX_A_B (for def_b) to freshly-allocated, ATA-initialised public holdings".to_string()
            })?;
            println!("disposable swap (mode-2 fresh A — STARK ~15–25 min)...");
            unsafe {
                ldex_amm_v2_disposable_swap(env.cfg.as_ptr(), env.store.as_ptr(),
                    amm.as_ptr(),
                    priv_a.as_ptr(), priv_b.as_ptr(),
                    a_a.as_ptr(), a_b.as_ptr(),
                    def_a.as_ptr(), def_b.as_ptr(), def_in.as_ptr(),
                    amount, min_out, fee, deadline, tx.as_mut_ptr())
            }
        }
        other => return Err(format!("unknown mode {other:?} (use public | private | disposable)")),
    };
    if rc != 0 { return Err(format!("swap rc={rc} after {:?}", t0.elapsed())); }
    ok(format!("swap {pay}→{get} {amount} (mode={mode}, min_out={min_out})  tx={}  ({:?})",
               hx(&tx), t0.elapsed()));
    Ok(())
}

fn cmd_pool_create(env: &Env, a: &str, b: &str, fee: u128,
                   amount_a: u128, amount_b: u128) -> Result<(), String> {
    let ta = resolve_token(env, a)?;
    let tb = resolve_token(env, b)?;
    let owner = env.acct_id("LDEX_USER_OWNER")?;
    // Pool create takes user_holding_a / user_holding_b — the public
    // holdings funding the initial liquidity. Use the letter-derived
    // HOLD_<L>; bootstrap creates them.
    let la = ta.letter.as_deref().ok_or("pool-create needs token letters")?;
    let lb = tb.letter.as_deref().ok_or("pool-create needs token letters")?;
    let hold_a = env.acct_id(&format!("LDEX_HOLD_{la}"))?;
    let hold_b = env.acct_id(&format!("LDEX_HOLD_{lb}"))?;
    let amm = env.hex_id("LDEX_AMM_V2_PROGRAM_ID")?;
    let mut tx = [0u8; 32];
    let t0 = Instant::now();
    let rc = unsafe {
        ldex_amm_v2_new_pool_ata(env.cfg.as_ptr(), env.store.as_ptr(),
            amm.as_ptr(), owner.as_ptr(),
            hold_a.as_ptr(), hold_b.as_ptr(),
            amount_a, amount_b, fee, u64::MAX, tx.as_mut_ptr())
    };
    if rc != 0 { return Err(format!("pool-create rc={rc}")); }
    ok(format!("created pool {a}/{b} @ {:.2}% ({amount_a}/{amount_b})  tx={}  ({:?})",
               fee as f64 / 100.0, hx(&tx), t0.elapsed()));
    Ok(())
}

fn cmd_liq_add(env: &Env, a: &str, b: &str, amt_a: u128, amt_b: u128,
               fee: u128, mode: &str) -> Result<(), String> {
    let ta = resolve_token(env, a)?;
    let tb = resolve_token(env, b)?;
    let owner = env.acct_id("LDEX_USER_OWNER")?;
    let amm = env.hex_id("LDEX_AMM_V2_PROGRAM_ID")?;
    // The pool exists in canonical (defA, defB) ordering. The user's
    // (amt_a, amt_b) is in the order they typed; if the user typed B/A
    // we swap so the FFI sees pool-canonical order.
    let mut tx = [0u8; 32];
    let t0 = Instant::now();
    let rc = match mode {
        "public" | "pub" => {
            unsafe {
                ldex_amm_v2_add_liquidity_ata(env.cfg.as_ptr(), env.store.as_ptr(),
                    amm.as_ptr(), owner.as_ptr(),
                    ta.def_id.as_ptr(), tb.def_id.as_ptr(),
                    0 /* min_amount_liquidity */, amt_a, amt_b,
                    fee, u64::MAX, tx.as_mut_ptr())
            }
        }
        "private" | "priv" => {
            let la = ta.letter.as_deref().ok_or("private liq needs token letters")?;
            let lb = tb.letter.as_deref().ok_or("private liq needs token letters")?;
            let priv_a = env.acct_id(&format!("LDEX_PRIV_{la}"))?;
            let priv_b = env.acct_id(&format!("LDEX_PRIV_{lb}"))?;
            let priv_lp = env.opt_acct_id("LDEX_PRIV_LP")
                .ok_or("LDEX_PRIV_LP missing — bootstrap may not have created the priv-LP holding")?;
            println!("private add-liq (STARK ~20+ min)...");
            unsafe {
                ldex_amm_v2_private_add_liquidity(env.cfg.as_ptr(), env.store.as_ptr(),
                    amm.as_ptr(),
                    priv_a.as_ptr(), priv_b.as_ptr(), priv_lp.as_ptr(),
                    ta.def_id.as_ptr(), tb.def_id.as_ptr(),
                    0 /* min_amount_liquidity */, amt_a, amt_b,
                    fee, u64::MAX, tx.as_mut_ptr())
            }
        }
        other => return Err(format!("unknown mode {other:?} (use public | private)")),
    };
    if rc != 0 { return Err(format!("liq-add rc={rc}")); }
    ok(format!("liq-add {a}/{b} ({amt_a}/{amt_b}, mode={mode})  tx={}  ({:?})",
               hx(&tx), t0.elapsed()));
    Ok(())
}

fn cmd_liq_remove(env: &Env, a: &str, b: &str, lp_amt: u128,
                  fee: u128, mode: &str, min_a: u128, min_b: u128) -> Result<(), String> {
    let ta = resolve_token(env, a)?;
    let tb = resolve_token(env, b)?;
    let owner = env.acct_id("LDEX_USER_OWNER")?;
    let amm = env.hex_id("LDEX_AMM_V2_PROGRAM_ID")?;
    let mut tx = [0u8; 32];
    let t0 = Instant::now();
    let rc = match mode {
        "public" | "pub" => {
            unsafe {
                ldex_amm_v2_remove_liquidity_ata(env.cfg.as_ptr(), env.store.as_ptr(),
                    amm.as_ptr(), owner.as_ptr(),
                    ta.def_id.as_ptr(), tb.def_id.as_ptr(),
                    lp_amt, min_a, min_b, fee, u64::MAX, tx.as_mut_ptr())
            }
        }
        "private" | "priv" => {
            let la = ta.letter.as_deref().ok_or("private liq needs token letters")?;
            let lb = tb.letter.as_deref().ok_or("private liq needs token letters")?;
            let priv_a = env.acct_id(&format!("LDEX_PRIV_{la}"))?;
            let priv_b = env.acct_id(&format!("LDEX_PRIV_{lb}"))?;
            let priv_lp = env.opt_acct_id("LDEX_PRIV_LP")
                .ok_or("LDEX_PRIV_LP missing")?;
            println!("private remove-liq (STARK ~25+ min)...");
            unsafe {
                ldex_amm_v2_private_remove_liquidity(env.cfg.as_ptr(), env.store.as_ptr(),
                    amm.as_ptr(),
                    priv_a.as_ptr(), priv_b.as_ptr(), priv_lp.as_ptr(),
                    ta.def_id.as_ptr(), tb.def_id.as_ptr(),
                    lp_amt, min_a, min_b, fee, u64::MAX, tx.as_mut_ptr())
            }
        }
        other => return Err(format!("unknown mode {other:?} (use public | private)")),
    };
    if rc != 0 { return Err(format!("liq-remove rc={rc}")); }
    ok(format!("liq-remove {a}/{b} {lp_amt} LP (mode={mode})  tx={}  ({:?})",
               hx(&tx), t0.elapsed()));
    Ok(())
}

fn cmd_env(env: &Env) -> Result<(), String> {
    let mut keys: Vec<&String> = env.map.keys().collect();
    keys.sort();
    for k in keys {
        println!("{k}={}", env.map[k]);
    }
    Ok(())
}

// Resolve the LEZ wallet binary. Lookup order:
//   1. $LDEX_WALLET_BIN env var (explicit override)
//   2. $LDEX_LEZ_DIR/target/release/wallet (LEZ source tree env var)
//   3. ~/ldex-spike/lez/target/release/wallet (the convention used by
//      run-sequencer.sh; standard if you followed the SETUP guide)
//   4. `which wallet` on $PATH (in case the user installed it system-wide)
fn resolve_wallet_bin() -> Result<String, String> {
    if let Ok(p) = std::env::var("LDEX_WALLET_BIN") {
        if std::path::Path::new(&p).is_file() { return Ok(p); }
        return Err(format!("LDEX_WALLET_BIN={p} not a file"));
    }
    if let Ok(d) = std::env::var("LDEX_LEZ_DIR") {
        let p = format!("{d}/target/release/wallet");
        if std::path::Path::new(&p).is_file() { return Ok(p); }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = format!("{home}/ldex-spike/lez/target/release/wallet");
        if std::path::Path::new(&p).is_file() { return Ok(p); }
    }
    if let Ok(out) = std::process::Command::new("which").arg("wallet").output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() && std::path::Path::new(&p).is_file() { return Ok(p); }
        }
    }
    Err("LEZ wallet binary not found. Set $LDEX_WALLET_BIN or $LDEX_LEZ_DIR, \
         or clone+build LEZ at ~/ldex-spike/lez (see SETUP.md).".into())
}

// Shell out to the LEZ wallet CLI. The wallet binary lives in the
// LEZ source tree (clone separately — see SETUP.md). Resolved via
// `resolve_wallet_bin` from env vars or a conventional path.
fn wallet_run(env: &Env, args: &[&str]) -> Result<String, String> {
    let pw = env.get("LDEX_WALLET_PW").unwrap_or("ldexdev");
    let home = std::path::Path::new(env.get("LDEX_WALLET_CONFIG")?)
        .parent().map(|p| p.to_owned())
        .ok_or("LDEX_WALLET_CONFIG has no parent dir")?;
    let bin = resolve_wallet_bin()?;
    let mut child = Command::new(&bin)
        .args(args)
        .env("NSSA_WALLET_HOME_DIR", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn wallet: {e}"))?;
    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{pw}");
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        return Err(format!("wallet {args:?} failed: stdout={stdout} stderr={stderr}"));
    }
    Ok(stdout + &stderr)
}

// Genesis-funded account on the dev sequencer (20000 LEZ at init). The
// bootstrap uses this same account because it's the only one with both
// a public key in the wallet's signable set + a non-zero native LEZ
// balance under the auth-transfer program.
const GENESIS_FUNDER: &str = "Public/2RHZhw9h534Zr3eq2RGhQete2Hh667foECzXPmSkGni2";

// Block until `acct`'s balance reaches `expected`, or 60 s elapses.
// The wallet CLI's `token send` submits + returns; the sequencer takes
// up to one block_create_timeout (15 s on dev) to include. We poll the
// on-chain state instead of trusting the wallet's local cache.
fn wait_for_token_balance(env: &Env, acct_id: &str, expected: u128) -> bool {
    let Ok(acct) = parse_account(acct_id) else { return false; };
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let cur = balance_public_token(env, &acct);
        if cur >= expected { return true; }
        if Instant::now() > deadline { return false; }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn cmd_fund(env: &Env, lez: u128, tok: u128, tokens: &str,
            skip_lez: bool, skip_tokens: bool) -> Result<(), String> {
    let user = env.get("LDEX_USER_OWNER")?;

    if !skip_lez {
        println!("→ auth-transfer {lez} LEZ from genesis funder → USER_OWNER...");
        let t0 = Instant::now();
        let out = wallet_run(env, &[
            "auth-transfer", "send",
            "--from", GENESIS_FUNDER,
            "--to", user,
            "--amount", &lez.to_string(),
        ])?;
        let tx = extract_tx_hash(&out);
        ok(format!("native LEZ +{lez}  tx={}  ({:?})",
                   tx.as_deref().unwrap_or("?"), t0.elapsed()));
    }

    if !skip_tokens {
        for letter in tokens.split_whitespace() {
            let letter = letter.trim().to_uppercase();
            if letter.is_empty() { continue; }
            let hold_key = format!("LDEX_HOLD_{letter}");
            let ata_key  = format!("LDEX_ATA_{letter}");
            let hold = match env.opt(&hold_key) { Some(v) => v, None => {
                warn(format!("skip TOKEN{letter}: {hold_key} not in env"));
                continue;
            }}.to_string();
            let ata = match env.opt(&ata_key) { Some(v) => v, None => {
                warn(format!("skip TOKEN{letter}: {ata_key} not in env (run pool-create's ATA setup first)"));
                continue;
            }}.to_string();
            // Snapshot before submission, then wait for the on-chain
            // delta to materialise. The wallet binary submits the tx
            // and returns in ~10 ms without polling; the sequencer
            // takes up to one block_create_timeout (15 s on dev) to
            // include. Reporting "tx=?" or "+5000" before inclusion
            // would let a tx-rejected case look like success.
            let ata_bytes = match parse_account(&ata) {
                Ok(b) => b,
                Err(e) => { warn(format!("TOKEN{letter}: bad ATA id: {e}")); continue; }
            };
            let pre = balance_public_token(env, &ata_bytes);
            print!("→ token send {tok} TOKEN{letter} from HOLD → ATA... ");
            use std::io::Write; let _ = std::io::stdout().flush();
            let t0 = Instant::now();
            match wallet_run(env, &[
                "token", "send",
                "--from", &hold,
                "--to", &ata,
                "--amount", &tok.to_string(),
            ]) {
                Ok(_) => {
                    let target = pre.saturating_add(tok);
                    if wait_for_token_balance(env, &ata, target) {
                        println!("✓");
                        info(format!("TOKEN{letter} ATA {pre} → {target}  ({:?})", t0.elapsed()));
                    } else {
                        println!("⚠ timeout");
                        let post = balance_public_token(env, &ata_bytes);
                        warn(format!("TOKEN{letter}: expected {target}, still at {post} after 60 s"));
                    }
                }
                Err(e) => { println!(); warn(format!("TOKEN{letter}: {e}")); }
            }
        }
    }
    Ok(())
}

fn extract_tx_hash(s: &str) -> Option<String> {
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("Transaction hash is ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

// ─────────────────────────────────────────────────────────── main

fn main() {
    let cli = Cli::parse();
    let env = Env::load(&cli.env_file).unwrap_or_else(|e| fail(format!("env load: {e}")));

    // Reads of private state are stale unless the wallet is synced.
    let auto_sync = !cli.no_sync;
    let needs_sync = matches!(cli.cmd, Cmd::Balance { .. } | Cmd::Status);

    if auto_sync && needs_sync {
        if let Err(e) = sync_to_head(&env) { warn(format!("sync skipped: {e}")); }
    }

    let res = match &cli.cmd {
        Cmd::Status                  => cmd_status(&env),
        Cmd::Sync                    => cmd_sync(&env),
        Cmd::Balance { all }         => cmd_balance(&env, *all, auto_sync),
        Cmd::Accounts                => cmd_accounts(&env),
        Cmd::Pools                   => cmd_pools(&env),
        Cmd::Pool { a, b, fee_bps }  => cmd_pool(&env, a, b, *fee_bps),
        Cmd::Quote { pay, get, amount, fee_bps } =>
            cmd_quote(&env, pay, get, *amount, *fee_bps),
        Cmd::Wrap   { amount }       => cmd_wrap(&env, *amount),
        Cmd::Unwrap { amount }       => cmd_unwrap(&env, *amount),
        Cmd::Shield { token, amount }=> cmd_shield(&env, token, *amount),
        Cmd::Deshield { token, amount } => cmd_deshield(&env, token, *amount),
        Cmd::Swap { pay, get, amount, mode, fee_bps, slip_pct } =>
            cmd_swap(&env, pay, get, *amount, mode, *fee_bps, *slip_pct),
        Cmd::PoolCreate { a, b, fee_bps, amount_a, amount_b } =>
            cmd_pool_create(&env, a, b, *fee_bps, *amount_a, *amount_b),
        Cmd::Liq(LiqCmd::Add { a, b, amount_a, amount_b, fee_bps, mode }) =>
            cmd_liq_add(&env, a, b, *amount_a, *amount_b, *fee_bps, mode),
        Cmd::Liq(LiqCmd::Remove { a, b, lp_amount, fee_bps, mode, min_a, min_b }) =>
            cmd_liq_remove(&env, a, b, *lp_amount, *fee_bps, mode, *min_a, *min_b),
        Cmd::Fund { lez_amount, token_amount, tokens, skip_lez, skip_tokens } =>
            cmd_fund(&env, *lez_amount, *token_amount, tokens, *skip_lez, *skip_tokens),
        Cmd::Env                     => cmd_env(&env),
    };
    if let Err(e) = res { fail(e); }
}
