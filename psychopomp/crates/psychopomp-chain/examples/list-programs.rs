//! `cargo run -p psychopomp-chain --example list-programs -- http://127.0.0.1:3050/`
//!
//! Prints the sequencer's registered program IDs. Used by deploy-onchain.sh
//! to confirm psychopomp-registry + psychopomp-escrow landed.

use psychopomp_chain::{program_id_hex, PsychopompChain};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:3050/".to_string());
    let chain = PsychopompChain::connect(&endpoint)?;
    let ids = chain.list_program_ids().await?;
    println!("Programs at {endpoint}:");
    for (name, id) in &ids {
        println!("  {name:32}  {}", program_id_hex(id));
    }
    Ok(())
}
