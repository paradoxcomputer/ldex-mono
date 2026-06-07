//! Live test of the fault path: client posts a job with a SHORT deadline,
//! operator accepts but never delivers, anyone (a keeper) submits
//! `Fault::Liveness` after the deadline → JobState = Refunded + operator's
//! `reputation.liveness_faults` increments via chained `record_settlement`.
//!
//!   cargo run --release -p psychopomp-chain --example live-fault

use psychopomp_chain::{
    job_pda, operator_pda, parse_account_id, submit_accept, submit_fault, submit_post,
    submit_register, PsychopompChain,
};
use psychopomp_escrow_core::{JobFilter, Status};
use psychopomp_hwclass::HwClass;
use psychopomp_registry_core::{FaultKind, OperatorStatus};
use sha2::{Digest, Sha256};
use std::time::Duration;

const ENDPOINT: &str = "http://127.0.0.1:3050/";
const REGISTRY_IMAGE_ID_HEX: &str =
    "d4b2e372688a35c8e26f107f16c1522cffa7d2dbbb16cfebd0675503c0f655ae";
const ESCROW_IMAGE_ID_HEX: &str =
    "e73209b5c83abdd77304abde6ec5805913d8345c7ab734d71737da93851c49f0";

// A separate operator pubkey from live-lifecycle so the rep counters start clean.
const OPERATOR_PK: [u8; 32] = [0x11; 32];
const ATTEST_ROOT: [u8; 32] = [0x22; 32];
const CLIENT_PK: [u8; 32] = [0x33; 32];
const OPERATOR_MRENCLAVE: [u8; 32] = [0x44; 32];

fn hex_to_id(s: &str) -> anyhow::Result<[u32; 8]> {
    let bytes = hex::decode(s)?;
    let mut out = [0u32; 8];
    for (i, w) in out.iter_mut().enumerate() {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&bytes[i * 4..(i + 1) * 4]);
        *w = u32::from_le_bytes(buf);
    }
    Ok(out)
}

async fn poll<T, F, Fut>(label: &str, mut f: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Option<T>>>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(t) = f().await? {
            return Ok(t);
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("{label}: not observable within 120s");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry_id = hex_to_id(REGISTRY_IMAGE_ID_HEX)?;
    let escrow_id = hex_to_id(ESCROW_IMAGE_ID_HEX)?;
    let cfg_path = std::env::var("PSYCHOPOMP_WALLET_CFG")
        .unwrap_or_else(|_| "sequencer-state/wallet/wallet_config.json".into());
    let store_path = std::env::var("PSYCHOPOMP_WALLET_STORAGE")
        .unwrap_or_else(|_| "sequencer-state/wallet/storage.json".into());
    let cfg = std::path::Path::new(&cfg_path);
    let store = std::path::Path::new(&store_path);
    let client_acc = parse_account_id("6iArKUXxhUJqS7kCaPNhwMWt3ro71PDyBj7jwAyE2VQV")?;
    let operator_acc = parse_account_id("7wHg9sbJwc6h3NP1S9bekfAzB8CHifEcxKswCKUt3YQo")?;
    let chain = PsychopompChain::connect(ENDPOINT)?;

    // [1] Register operator (idempotent - skip if already present).
    let op_pda = operator_pda(&registry_id, &OPERATOR_PK);
    if chain.get_operator_state(op_pda).await?.is_none() {
        println!("[1] registering operator at {op_pda:?}");
        let (h, _) = submit_register(
            cfg, store, registry_id, operator_acc,
            OPERATOR_PK, ATTEST_ROOT, vec![OPERATOR_MRENCLAVE],
            HwClass::H100CC, 1_000_000_000_000_000_000u128,
        ).await?;
        println!("    register tx: {h:?}");
        poll("operator state", || async {
            Ok(chain.get_operator_state(op_pda).await?.filter(|s| s.status == OperatorStatus::Active))
        }).await?;
    } else {
        println!("[1] operator already exists at {op_pda:?}");
    }
    let pre = chain.get_operator_state(op_pda).await?.unwrap();
    let pre_faults = pre.reputation.liveness_faults;
    println!("    pre-test reputation.liveness_faults = {pre_faults}");

    // [2] Post a job with a SHORT (1 epoch) deadline.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let mut h = Sha256::new();
    h.update(b"psychopomp-fault-job/");
    h.update(now.to_le_bytes());
    let job_id: [u8; 32] = h.finalize().into();
    let mut h = Sha256::new();
    h.update(b"placeholder-ciphertext-for-fault-test");
    let ct_hash: [u8; 32] = h.finalize().into();
    let job_slot = job_pda(&escrow_id, &job_id);
    println!("[2] posting job {} (deadline = now+5s)", hex::encode(job_id));
    let filter = JobFilter {
        accepted_hw_classes: vec![HwClass::H100CC],
        accepted_mrenclaves: vec![OPERATOR_MRENCLAVE],
    };
    let (h, _) = submit_post(
        cfg, store, escrow_id, client_acc,
        job_id, CLIENT_PK, ct_hash, filter,
        500u128, now + 5,
    ).await?;
    println!("    post tx: {h:?}");
    poll("Open job", || async {
        Ok(chain.get_job_state(job_slot).await?.filter(|s| s.status == Status::Open))
    }).await?;

    // [3] Operator accepts.
    println!("[3] operator accepts (then disappears)");
    let (h, _) = submit_accept(
        cfg, store, escrow_id, operator_acc,
        job_id, OPERATOR_PK, HwClass::H100CC, OPERATOR_MRENCLAVE,
    ).await?;
    println!("    accept tx: {h:?}");
    poll("Awarded", || async {
        Ok(chain.get_job_state(job_slot).await?.filter(|s| matches!(s.status, Status::Awarded { .. })))
    }).await?;

    // [4] Wait for deadline.
    println!("[4] sleeping past deadline (+10s)");
    tokio::time::sleep(Duration::from_secs(10)).await;

    // [5] Anyone calls Fault::Liveness.
    println!("[5] submitting Fault::Liveness (caller = client_acc, but anyone could)");
    let (h, _) = submit_fault(
        cfg, store, escrow_id, client_acc, op_pda,
        job_id, FaultKind::Liveness,
        now + 100, // claimed_epoch_now well past deadline (now+5)
    ).await?;
    println!("    fault tx: {h:?}");
    let st = poll("Refunded", || async {
        Ok(chain.get_job_state(job_slot).await?.filter(|s| matches!(s.status, Status::Refunded { .. })))
    }).await?;
    if let Status::Refunded { reason } = &st.status {
        println!("    PASS  JobState = Refunded (reason = {reason:?})");
    }

    // [6] Verify reputation.liveness_faults bumped.
    let post = chain.get_operator_state(op_pda).await?.unwrap();
    println!("[6] verifying reputation.liveness_faults++");
    println!("    pre  = {pre_faults}");
    println!("    post = {}", post.reputation.liveness_faults);
    if post.reputation.liveness_faults != pre_faults + 1 {
        anyhow::bail!("expected liveness_faults to bump by 1");
    }

    println!();
    println!("================================================================");
    println!("PASS  Fault path verified LIVE on chain:");
    println!("  Post (deadline=now+5s) -> Accept -> wait 10s -> Fault::Liveness");
    println!("  → JobState = Refunded {{ reason: Liveness }}");
    println!("  → OperatorState.reputation.liveness_faults incremented");
    println!("  → Reputation chain-callback works for the FAULT path too");
    Ok(())
}
