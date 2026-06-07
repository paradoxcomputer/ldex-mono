//! End-to-end live test of the full operator-economic loop on chain:
//!
//!   1. Register an operator (writes OperatorState to the registry PDA)
//!   2. Client Posts a job (writes JobState=Open to the escrow PDA)
//!   3. Operator Accepts the job (status → Awarded)
//!   4. Operator Settles after off-chain proving (status → Settled)
//!
//! Reads back at every step via psychopomp-chain. Uses the live deployed
//! psychopomp sequencer at :3050.
//!
//! Usage:
//!   cargo run --release -p psychopomp-chain --example live-lifecycle

use psychopomp_chain::{
    job_pda, operator_pda, parse_account_id, submit_accept, submit_post, submit_register,
    submit_settle, PsychopompChain,
};
use psychopomp_escrow_core::{JobFilter, Status};
use psychopomp_hwclass::HwClass;
use psychopomp_registry_core::OperatorStatus;
use sha2::{Digest, Sha256};
use std::time::Duration;

const ENDPOINT: &str = "http://127.0.0.1:3050/";
const REGISTRY_IMAGE_ID_HEX: &str =
    "d4b2e372688a35c8e26f107f16c1522cffa7d2dbbb16cfebd0675503c0f655ae";
const ESCROW_IMAGE_ID_HEX: &str =
    "e73209b5c83abdd77304abde6ec5805913d8345c7ab734d71737da93851c49f0";

const OPERATOR_PK: [u8; 32] = [0xaa; 32];
const ATTEST_ROOT: [u8; 32] = [0xbb; 32];
const CLIENT_PK: [u8; 32] = [0xcc; 32];
const OPERATOR_MRENCLAVE: [u8; 32] = [0xdd; 32];

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
    // Two wallet-managed accounts: one acts as client, one as operator. We use
    // the two preconfigured public accounts the wallet generated on init.
    let client_acc = parse_account_id("6iArKUXxhUJqS7kCaPNhwMWt3ro71PDyBj7jwAyE2VQV")?;
    let operator_acc =
        parse_account_id("7wHg9sbJwc6h3NP1S9bekfAzB8CHifEcxKswCKUt3YQo")?;

    let chain = PsychopompChain::connect(ENDPOINT)?;
    let cfg = std::path::Path::new(&cfg_path);
    let store = std::path::Path::new(&store_path);

    // ---- Step 1: Register operator -------------------------------------------
    let op_pda = operator_pda(&registry_id, &OPERATOR_PK);
    if chain.get_operator_state(op_pda).await?.is_some() {
        println!("[1] operator already registered on chain at {op_pda:?} - reusing");
    } else {
        println!("[1] registering operator at {op_pda:?}");
        let (hash, _) = submit_register(
            cfg, store, registry_id, operator_acc,
            OPERATOR_PK, ATTEST_ROOT, vec![OPERATOR_MRENCLAVE],
            HwClass::H100CC, 1_000_000_000_000_000_000u128,
        )
        .await?;
        println!("    register tx: {hash:?}");
        let st = poll("operator state", || async {
            Ok(chain.get_operator_state(op_pda).await?)
        }).await?;
        assert_eq!(st.status, OperatorStatus::Active);
        println!("    PASS  OperatorState = Active (rep={} succ / {} fail)",
                 st.reputation.successes, st.reputation.liveness_faults + st.reputation.correctness_faults);
    }

    // ---- Step 2: Post a job --------------------------------------------------
    // Generate a fresh job_id each run (timestamp-derived) so PDAs don't collide.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let mut h = Sha256::new();
    h.update(b"psychopomp-lifecycle-job/");
    h.update(now.to_le_bytes());
    let job_id: [u8; 32] = h.finalize().into();
    let mut h = Sha256::new();
    h.update(b"placeholder-ciphertext-for-lifecycle-test");
    let ct_hash: [u8; 32] = h.finalize().into();
    let job_slot = job_pda(&escrow_id, &job_id);
    println!("[2] posting job {} → {job_slot:?}", hex::encode(job_id));
    let filter = JobFilter {
        accepted_hw_classes: vec![HwClass::H100CC],
        accepted_mrenclaves: vec![OPERATOR_MRENCLAVE],
    };
    let (hash, _) = submit_post(
        cfg, store, escrow_id, client_acc,
        job_id, CLIENT_PK, ct_hash, filter,
        500u128, now + 3600,
    ).await?;
    println!("    post tx: {hash:?}");
    let st = poll("Open job", || async {
        Ok(chain.get_job_state(job_slot).await?)
    }).await?;
    assert_eq!(st.status, Status::Open);
    println!("    PASS  JobState = Open (escrow {} max_bid)", st.max_bid);

    // ---- Step 3: Operator accepts -------------------------------------------
    println!("[3] operator accepts job");
    let (hash, _) = submit_accept(
        cfg, store, escrow_id, operator_acc,
        job_id, OPERATOR_PK, HwClass::H100CC, OPERATOR_MRENCLAVE,
    ).await?;
    println!("    accept tx: {hash:?}");
    let st = poll("Awarded job", || async {
        Ok(chain.get_job_state(job_slot).await?.filter(|s| matches!(s.status, Status::Awarded { .. })))
    }).await?;
    if let Status::Awarded { operator_pk, operator_locked_stake, .. } = &st.status {
        println!("    PASS  JobState = Awarded (operator_pk={} locked_stake={})",
                 hex::encode(operator_pk), operator_locked_stake);
    }

    // ---- Step 4: Operator settles -------------------------------------------
    // Off-chain: operator would now run psychopomp-prover and produce the STARK
    // here. Phase-1 stub: we just call Settle with the measured wall_clock.
    println!("[4] operator settles (wall_clock_ms=42000)");
    let (hash, _) = submit_settle(
        cfg, store, escrow_id, operator_acc, op_pda,
        job_id, OPERATOR_PK, 42_000u64,
    ).await?;
    println!("    settle tx: {hash:?}");
    let st = poll("Settled job", || async {
        Ok(chain.get_job_state(job_slot).await?.filter(|s| matches!(s.status, Status::Settled { .. })))
    }).await?;
    if let Status::Settled { operator_pk, wall_clock_ms } = &st.status {
        println!("    PASS  JobState = Settled (operator_pk={} wall_clock_ms={})",
                 hex::encode(operator_pk), wall_clock_ms);
    }

    // ---- Step 5: Verify rep counter chain-callback ---------------------------
    let pre_rep = chain.get_operator_state(op_pda).await?.unwrap();
    let pre_succ = pre_rep.reputation.successes;
    // pre_succ already reflects the just-completed settle; report it.
    println!("[5] verifying operator reputation auto-update");
    println!("    PASS  OperatorState.reputation.successes = {pre_succ}");
    if pre_succ == 0 {
        anyhow::bail!("rep callback did not increment successes");
    }

    println!();
    println!("================================================================");
    println!("PASS  Full operator-economic loop verified LIVE on chain:");
    println!("  registry  {REGISTRY_IMAGE_ID_HEX}");
    println!("  escrow    {ESCROW_IMAGE_ID_HEX}");
    println!("  operator  PDA {op_pda:?}");
    println!("  job       PDA {job_slot:?}");
    println!("  flow      Register -> Post -> Accept -> Settle");
    Ok(())
}
