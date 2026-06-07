//! LDEX price-indexer microservice - design.md §5.11 layer ②.
//!
//! Solves the chart's "historical depth" gap: instead of an in-memory
//! ≤15-min session buffer that resets every view, this daemon tails the
//! chain and **persists** the pool's on-chain reserves per new block to a
//! durable CSV, giving a real, unbounded, restart-surviving price
//! history. Data is 100% on-chain (the pool PDA's `PoolDefinition`
//! reserves read from the live sequencer) - no oracle, no third party.
//!
//!   price_indexer <cfg> <store> <amm> <defA> <defB> <fee> [interval_s]
//!
//! Output: ${LDEX_PRICE_DIR:-$HOME/.ldex/price}/<amm8>_<a8>_<b8>_<fee>.csv
//! columns: block_id,unix_ms,reserve_a,reserve_b,lp_supply
//!
//! block_id is the on-chain anchor (monotonic, truthful). unix_ms is the
//! sample wall-clock for axis labels; swapping in the block header's
//! on-chain `timestamp` (common::block::BlockHeader.timestamp) is the
//! §5.11 refinement for fully on-chain time.
//!
//! Forward-indexes from start (tip-only RPC ⇒ no pre-start backfill;
//! that needs §5.11 layer ① `getAccountAtBlock`). Idempotent: resumes
//! from the last recorded block_id.

use std::ffi::CString;
use std::fs::{create_dir_all, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use amm_core::{compute_pool_pda, PoolDefinition};
use ldex_amm_ffi::{ldex_amm_parse_account_id, LDEX_AMM_OK};
use nssa_core::account::AccountId;
use nssa_core::program::ProgramId;
use sequencer_service_rpc::RpcClient as _;
use wallet::WalletCore;

fn id32(s: &str) -> [u8; 32] {
    let c = CString::new(s).unwrap();
    let mut out = [0u8; 32];
    let rc = unsafe { ldex_amm_parse_account_id(c.as_ptr(), out.as_mut_ptr()) };
    assert_eq!(rc, LDEX_AMM_OK, "bad id {s} (rc={rc})");
    out
}

fn pid_from(b: [u8; 32]) -> ProgramId {
    let mut p = [0u32; 8];
    for (i, l) in p.iter_mut().enumerate() {
        let mut w = [0u8; 4];
        w.copy_from_slice(&b[i * 4..i * 4 + 4]);
        *l = u32::from_ne_bytes(w);
    }
    p
}

fn last_block_in(path: &PathBuf) -> u64 {
    let Ok(f) = std::fs::File::open(path) else {
        return 0;
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| l.split(',').next().and_then(|s| s.parse::<u64>().ok()))
        .max()
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 7 {
        eprintln!("usage: price_indexer <cfg> <store> <amm> <defA> <defB> <fee> [interval_s]");
        std::process::exit(2);
    }
    let (cfg, store) = (a[1].clone(), a[2].clone());
    let amm = pid_from(id32(&a[3]));
    let def_a = AccountId::new(id32(&a[4]));
    let def_b = AccountId::new(id32(&a[5]));
    let fees: u128 = a[6].parse().expect("fee");
    let interval = a.get(7).and_then(|s| s.parse().ok()).unwrap_or(10u64);

    let dir = std::env::var("LDEX_PRICE_DIR").unwrap_or_else(|_| {
        format!("{}/.ldex/price", std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
    });
    create_dir_all(&dir).expect("mkdir price dir");
    let h4 = |b: &[u8; 32]| b[..4].iter().map(|x| format!("{x:02x}")).collect::<String>();
    let pool = compute_pool_pda(amm, def_a, def_b, fees);
    let fname = format!(
        "{}/{}_{}_{}_{}.csv",
        dir,
        h4(&{
            let mut t = [0u8; 32];
            for (i, l) in amm.iter().enumerate() {
                t[i * 4..i * 4 + 4].copy_from_slice(&l.to_ne_bytes());
            }
            t
        }),
        h4(def_a.value()),
        h4(def_b.value()),
        fees
    );
    let path = PathBuf::from(&fname);
    let mut last = last_block_in(&path);
    println!("price-indexer: pool={} file={} resume@block={last}", pool, fname);

    loop {
        match index_once(&cfg, &store, pool, last).await {
            Ok(Some((bid, ra, rb, lp))) => {
                let ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
                    let _ = writeln!(f, "{bid},{ms},{ra},{rb},{lp}");
                }
                last = bid;
                println!("  block {bid}: ra={ra} rb={rb} lp={lp}");
            }
            Ok(None) => {}
            Err(e) => eprintln!("  (transient: {e})"),
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}

async fn index_once(
    cfg: &str,
    store: &str,
    pool: AccountId,
    last: u64,
) -> Result<Option<(u64, u128, u128, u128)>, String> {
    let w = WalletCore::new_update_chain(PathBuf::from(cfg), PathBuf::from(store), None)
        .map_err(|e| format!("wallet: {e:?}"))?;
    let bid: u64 = w
        .sequencer_client
        .get_last_block_id()
        .await
        .map_err(|e| format!("block id: {e:?}"))?
        .to_string()
        .parse()
        .map_err(|_| "block id parse".to_string())?;
    if bid <= last {
        return Ok(None);
    }
    let acc = w
        .get_account_public(pool)
        .await
        .map_err(|e| format!("pool read: {e:?}"))?;
    match PoolDefinition::try_from(&acc.data) {
        Ok(p) if p.liquidity_pool_supply > 0 => {
            Ok(Some((bid, p.reserve_a, p.reserve_b, p.liquidity_pool_supply)))
        }
        _ => Ok(None), // pool not initialized yet at tip
    }
}
