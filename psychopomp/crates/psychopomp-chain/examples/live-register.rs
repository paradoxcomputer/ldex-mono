//! End-to-end live test: send a Register tx to the deployed
//! psychopomp-registry program on the local sequencer, await inclusion,
//! and read the operator's `OperatorState` back via `get_operator_state`.
//!
//! Usage:
//!   cargo run --release -p psychopomp-chain --example live-register
//!
//! Requires:
//!   - psychopomp sequencer running on :3050 (run-psychopomp-sequencer.sh)
//!   - wallet initialized at $NSSA_WALLET_HOME_DIR
//!   - psychopomp-registry deployed (deployment-config.toml has its image_id)

use psychopomp_chain::{operator_pda, parse_account_id, submit_register, PsychopompChain};
use psychopomp_hwclass::HwClass;
use psychopomp_registry_core::OperatorStatus;
use std::time::Duration;

const ENDPOINT: &str = "http://127.0.0.1:3050/";
const REGISTRY_IMAGE_ID_HEX: &str =
    "b934f143c4cd591a83d896d8d7afced317d7a466240deeec02a7cad8c6ecde1c";
// Operator pubkey to register (just an example - any 32 bytes).
const OPERATOR_PK: [u8; 32] = [0xaa; 32];
const ATTEST_ROOT: [u8; 32] = [0xbb; 32];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Convert hex registry program-id to [u32; 8].
    let id_bytes = hex::decode(REGISTRY_IMAGE_ID_HEX)?;
    let mut registry_id = [0u32; 8];
    for (i, w) in registry_id.iter_mut().enumerate() {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&id_bytes[i * 4..(i + 1) * 4]);
        *w = u32::from_le_bytes(buf);
    }

    // Wallet config: load from environment. Pin the funder account.
    // Defaults are repo-relative - run `cargo run -p psychopomp-chain
    // --example live-register` from the repo root.
    let cfg_path = std::env::var("PSYCHOPOMP_WALLET_CFG")
        .unwrap_or_else(|_| "sequencer-state/wallet/wallet_config.json".into());
    let store_path = std::env::var("PSYCHOPOMP_WALLET_STORAGE")
        .unwrap_or_else(|_| "sequencer-state/wallet/storage.json".into());
    let funder_str = std::env::var("PSYCHOPOMP_FUNDER")
        .unwrap_or_else(|_| "6iArKUXxhUJqS7kCaPNhwMWt3ro71PDyBj7jwAyE2VQV".into());
    // Strip optional "Public/" or "Private/" prefix - AccountId::parse expects raw base58.
    let raw = funder_str.split('/').next_back().unwrap_or(&funder_str);
    let funder = parse_account_id(raw)?;

    let pda = operator_pda(&registry_id, &OPERATOR_PK);
    println!("registry program: {REGISTRY_IMAGE_ID_HEX}");
    println!("operator_pk:      {}", hex::encode(OPERATOR_PK));
    println!("operator slot:    {funder_str:?} → PDA {pda:?}");
    println!();

    let (tx_hash, pda_back) = submit_register(
        std::path::Path::new(&cfg_path),
        std::path::Path::new(&store_path),
        registry_id,
        funder,
        OPERATOR_PK,
        ATTEST_ROOT,
        vec![[0xcc; 32]],
        HwClass::H100CC,
        1_000_000_000_000_000_000u128,
    )
    .await?;
    assert_eq!(pda, pda_back);
    println!("submitted Register tx: {:?}", tx_hash);

    let chain = PsychopompChain::connect(ENDPOINT)?;
    println!("polling chain for OperatorState at {pda:?}...");

    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(st) = chain.get_operator_state(pda).await? {
            println!();
            println!("PASS  operator state landed on chain:");
            println!("  operator_pk      = {}", hex::encode(st.operator_pk));
            println!("  attestation_root = {}", hex::encode(st.attestation_root));
            println!("  hw_class         = {:?}", st.hw_class);
            println!("  stake            = {}", st.stake);
            println!("  status           = {:?}", st.status);
            assert_eq!(st.operator_pk, OPERATOR_PK);
            assert_eq!(st.status, OperatorStatus::Active);
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("Register tx did not produce a state account within 120s");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
