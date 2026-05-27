//! End-to-end harness.
//!
//! Modes (selected by --test):
//!  - `hello` (default): one remote prove of the hello guest, verify locally.
//!  - `cached`: upload hello ELF via /v0/elf, then prove with GuestElfRef::Cached.
//!  - `composed`: remote-prove hello, then remote-prove composed which consumes
//!    the hello receipt as an assumption.
//!  - `multi`: fan out to two endpoints in parallel via prove_multi (pass
//!    --endpoint twice).
//!
//! Each mode prints a PASS banner if the returned Receipt verifies locally.

use anyhow::Context;
use clap::Parser;
use composed_methods::{COMPOSED_ELF, COMPOSED_ID};
use heavy_methods::{HEAVY_ELF, HEAVY_ID};
use hello_methods::{HELLO_ELF, HELLO_ID};
use psychopomp_client::reputation::ReputationLedger;
use psychopomp_client::{
    discovery, ensure_elf_cached, prove, prove_commit_reveal, prove_diverse, prove_multi,
    prove_multi_ranked, prove_with_timelock, ClientConfig,
};
use psychopomp_types::{
    image_id_hex, AttestationDoc, GuestElfRef, TimelockPuzzle, TrustedRoots, WitnessPayload,
    SCHEMA_VERSION,
};
use risc0_zkvm::serde::to_vec;
use std::time::{Duration, Instant};
use tracing::info;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum TestKind {
    Hello,
    Cached,
    Composed,
    Multi,
    Timelock,
    CommitReveal,
    Diverse,
    Discover,
    Ranked,
    /// Heavy synthetic workload (N rounds of sha2::Sha256). Tune --rounds to
    /// hit your target wall-clock; CPU baseline is roughly ~24K cycles/round.
    Heavy,
}

#[derive(Parser, Debug)]
struct Args {
    /// URL of the running prover(s). Pass multiple times for --test multi.
    #[arg(long, num_args = 1.., default_value = "http://127.0.0.1:8088")]
    endpoint: Vec<String>,
    #[arg(long, value_enum, default_value_t = TestKind::Hello)]
    test: TestKind,
    #[arg(long, default_value_t = 3)]
    a: u32,
    #[arg(long, default_value_t = 5)]
    b: u32,
    #[arg(long, default_value_t = 3600)]
    deadline_secs: u64,

    /// `--test timelock`: number of sequential SHA-256 iterations.
    #[arg(long, default_value_t = 100_000)]
    timelock_iters: u64,

    /// `--test discover`: path to a registry.json file.
    #[arg(long)]
    registry: Option<std::path::PathBuf>,

    /// `--test heavy`: number of SHA-256 rounds the guest executes. Tune to
    /// hit a target wall-clock — e.g. ~1000 ≈ a few seconds CPU baseline,
    /// ~100_000 ≈ minutes on CPU but seconds on a real GPU prover.
    #[arg(long, default_value_t = 1000)]
    rounds: u32,

    /// `--test heavy`: seed for the SHA-256 chain. Only changes the digest
    /// committed in the journal; doesn't affect proving time.
    #[arg(long, default_value_t = 1)]
    seed: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,psychopomp_client=debug,psychopomp_e2e=debug".into()),
        )
        .init();
    let args = Args::parse();
    let primary = args
        .endpoint
        .first()
        .ok_or_else(|| anyhow::anyhow!("at least one --endpoint required"))?
        .clone();

    let cfg0 = discover(&primary).await?;
    match args.test {
        TestKind::Hello => run_hello(&cfg0, &args).await,
        TestKind::Cached => run_cached(&cfg0, &args).await,
        TestKind::Composed => run_composed(&cfg0, &args).await,
        TestKind::Multi => {
            anyhow::ensure!(
                args.endpoint.len() >= 2,
                "--test multi requires at least 2 --endpoint values"
            );
            let mut cfgs = vec![cfg0];
            for ep in &args.endpoint[1..] {
                cfgs.push(discover(ep).await?);
            }
            run_multi(&cfgs, &args).await
        }
        TestKind::Timelock => run_timelock(&cfg0, &args).await,
        TestKind::CommitReveal => run_commit_reveal(&cfg0, &args).await,
        TestKind::Diverse => {
            anyhow::ensure!(args.endpoint.len() >= 2, "--test diverse needs >= 2 endpoints");
            let mut cfgs = vec![cfg0];
            for ep in &args.endpoint[1..] {
                cfgs.push(discover(ep).await?);
            }
            run_diverse(&cfgs, &args).await
        }
        TestKind::Discover => run_discover(&args).await,
        TestKind::Ranked => {
            anyhow::ensure!(args.endpoint.len() >= 2, "--test ranked needs >= 2 endpoints");
            let mut cfgs = vec![cfg0];
            for ep in &args.endpoint[1..] {
                cfgs.push(discover(ep).await?);
            }
            run_ranked(&cfgs, &args).await
        }
        TestKind::Heavy => run_heavy(&cfg0, &args).await,
    }
}

async fn discover(endpoint: &str) -> anyhow::Result<ClientConfig> {
    let http = reqwest::Client::new();
    let doc: AttestationDoc = http
        .get(format!("{endpoint}/v0/attestation"))
        .send()
        .await
        .context("GET /v0/attestation")?
        .error_for_status()?
        .json()
        .await?;
    let roots: TrustedRoots = http
        .get(format!("{endpoint}/v0/attestation/roots"))
        .send()
        .await
        .context("GET /v0/attestation/roots")?
        .error_for_status()?
        .json()
        .await?;
    if doc.schema_version != SCHEMA_VERSION {
        anyhow::bail!("attestation schema mismatch: {}", doc.schema_version);
    }
    let root: [u8; 32] = roots
        .roots
        .first()
        .context("no trusted roots published")?
        .der_cert
        .clone()
        .try_into()
        .map_err(|_| anyhow::anyhow!("Phase-0 stub root must be 32 bytes"))?;
    info!(endpoint, mrenclave = hex::encode(doc.mrenclave), root = hex::encode(root), "discovered prover");
    Ok(ClientConfig {
        endpoint: endpoint.to_string(),
        expected_mrenclave: doc.mrenclave,
        trusted_roots: vec![root],
        deadline: Duration::from_secs(3600),
        poll_interval: Duration::from_millis(750),
        upload_bearer: None,
        accept_invalid_tls: false,
    })
}

fn hello_payload(a: u32, b: u32) -> anyhow::Result<WitnessPayload> {
    let mut payload = WitnessPayload::default();
    let a_words = to_vec(&a)?;
    let b_words = to_vec(&b)?;
    payload.stdin.extend(words_to_bytes(&a_words));
    payload.stdin.extend(words_to_bytes(&b_words));
    Ok(payload)
}

async fn run_heavy(cfg: &ClientConfig, args: &Args) -> anyhow::Result<()> {
    let mut payload = WitnessPayload::default();
    let rounds_words = to_vec(&args.rounds)?;
    let seed_words = to_vec(&args.seed)?;
    payload.stdin.extend(words_to_bytes(&rounds_words));
    payload.stdin.extend(words_to_bytes(&seed_words));

    info!(rounds = args.rounds, seed = args.seed, "remote-proving heavy guest");
    let started = Instant::now();
    let receipt = prove(cfg, payload, GuestElfRef::InlineBytes(HEAVY_ELF.to_vec()), HEAVY_ID).await?;
    let elapsed = started.elapsed();
    receipt.verify(HEAVY_ID).context("verify heavy receipt")?;
    let (rounds_back, digest): (u32, [u8; 32]) = receipt.journal.decode()?;
    anyhow::ensure!(rounds_back == args.rounds, "heavy journal rounds mismatch");
    print_pass(
        &format!("heavy ({} sha256 rounds, seed={})", args.rounds, args.seed),
        &cfg.endpoint,
        HEAVY_ID,
        &format!("digest={}", hex::encode(digest)),
        elapsed,
    );
    Ok(())
}

async fn run_hello(cfg: &ClientConfig, args: &Args) -> anyhow::Result<()> {
    let payload = hello_payload(args.a, args.b)?;
    let started = Instant::now();
    let receipt = prove(cfg, payload, GuestElfRef::InlineBytes(HELLO_ELF.to_vec()), HELLO_ID).await?;
    let elapsed = started.elapsed();
    receipt.verify(HELLO_ID).context("verify hello receipt")?;
    let (sum, prod): (u32, u32) = receipt.journal.decode()?;
    let ex_sum = args.a.wrapping_add(args.b);
    let ex_prod = args.a.wrapping_mul(args.b);
    anyhow::ensure!(sum == ex_sum && prod == ex_prod, "journal mismatch");
    print_pass("hello", &cfg.endpoint, HELLO_ID, &format!("({ex_sum}, {ex_prod})"), elapsed);
    Ok(())
}

async fn run_cached(cfg: &ClientConfig, args: &Args) -> anyhow::Result<()> {
    info!("ensuring hello ELF is in operator cache");
    let was_cached = ensure_elf_cached(cfg, &HELLO_ID, HELLO_ELF).await?;
    info!(was_cached, "ELF cache state");
    let payload = hello_payload(args.a, args.b)?;
    let started = Instant::now();
    let receipt = prove(cfg, payload, GuestElfRef::Cached, HELLO_ID).await?;
    let elapsed = started.elapsed();
    receipt.verify(HELLO_ID).context("verify cached receipt")?;
    let (sum, prod): (u32, u32) = receipt.journal.decode()?;
    anyhow::ensure!(
        sum == args.a.wrapping_add(args.b) && prod == args.a.wrapping_mul(args.b),
        "journal mismatch"
    );
    print_pass(
        if was_cached { "cached (already present)" } else { "cached (just uploaded)" },
        &cfg.endpoint,
        HELLO_ID,
        &format!("({sum}, {prod})"),
        elapsed,
    );
    Ok(())
}

async fn run_composed(cfg: &ClientConfig, args: &Args) -> anyhow::Result<()> {
    // 1. Outsource the inner (hello) proof.
    info!("[composed] step 1: remote-prove inner hello");
    let payload = hello_payload(args.a, args.b)?;
    let inner = prove(cfg, payload, GuestElfRef::InlineBytes(HELLO_ELF.to_vec()), HELLO_ID).await?;
    inner.verify(HELLO_ID).context("verify inner")?;
    let inner_journal = inner.journal.bytes.clone();

    // 2. Build outer witness: HELLO_ID + expected_journal (= inner_journal),
    //    pass the inner Receipt as an assumption.
    info!("[composed] step 2: remote-prove outer, with inner as assumption");
    let mut outer_payload = WitnessPayload::default();
    let id_words = to_vec(&HELLO_ID)?;
    outer_payload.stdin.extend(words_to_bytes(&id_words));
    let journal_words = to_vec(&inner_journal)?;
    outer_payload.stdin.extend(words_to_bytes(&journal_words));
    outer_payload
        .assumptions
        .push(bincode::serialize(&risc0_zkvm::AssumptionReceipt::from(inner.clone()))?);

    let started = Instant::now();
    let outer = prove(
        cfg,
        outer_payload,
        GuestElfRef::InlineBytes(COMPOSED_ELF.to_vec()),
        COMPOSED_ID,
    )
    .await?;
    let elapsed = started.elapsed();
    outer.verify(COMPOSED_ID).context("verify outer composed receipt")?;
    let (id_back, journal_back): ([u32; 8], Vec<u8>) = outer.journal.decode()?;
    anyhow::ensure!(id_back == HELLO_ID, "outer journal HELLO_ID mismatch");
    anyhow::ensure!(journal_back == inner_journal, "outer journal inner-journal mismatch");
    print_pass("composed (assumption pass-through)", &cfg.endpoint, COMPOSED_ID, "inner-id + inner-journal", elapsed);
    Ok(())
}

async fn run_multi(cfgs: &[ClientConfig], args: &Args) -> anyhow::Result<()> {
    let payload = hello_payload(args.a, args.b)?;
    let started = Instant::now();
    let receipt = prove_multi(
        cfgs,
        payload,
        GuestElfRef::InlineBytes(HELLO_ELF.to_vec()),
        HELLO_ID,
    )
    .await?;
    let elapsed = started.elapsed();
    receipt.verify(HELLO_ID).context("verify multi receipt")?;
    let (sum, prod): (u32, u32) = receipt.journal.decode()?;
    print_pass(
        &format!("multi-route (k={})", cfgs.len()),
        &cfgs.iter().map(|c| c.endpoint.clone()).collect::<Vec<_>>().join(", "),
        HELLO_ID,
        &format!("({sum}, {prod})"),
        elapsed,
    );
    Ok(())
}

async fn run_timelock(cfg: &ClientConfig, args: &Args) -> anyhow::Result<()> {
    let mut payload = WitnessPayload::default();
    let a_words = to_vec(&args.a)?;
    let b_words = to_vec(&args.b)?;
    payload.stdin.extend(words_to_bytes(&a_words));
    payload.stdin.extend(words_to_bytes(&b_words));

    let mut seed = [0u8; 32];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let puzzle = TimelockPuzzle::new(args.timelock_iters, seed);
    info!(iterations = args.timelock_iters, "encrypting under timelock puzzle");

    let started = Instant::now();
    let receipt = prove_with_timelock(
        cfg,
        payload,
        GuestElfRef::InlineBytes(HELLO_ELF.to_vec()),
        HELLO_ID,
        Some(puzzle),
    )
    .await?;
    let elapsed = started.elapsed();
    receipt.verify(HELLO_ID)?;
    print_pass(
        &format!("timelock ({} iters)", args.timelock_iters),
        &cfg.endpoint,
        HELLO_ID,
        "",
        elapsed,
    );
    Ok(())
}

async fn run_commit_reveal(cfg: &ClientConfig, args: &Args) -> anyhow::Result<()> {
    let payload = hello_payload(args.a, args.b)?;
    let started = Instant::now();
    let receipt = prove_commit_reveal(
        cfg,
        payload,
        GuestElfRef::InlineBytes(HELLO_ELF.to_vec()),
        HELLO_ID,
        None,
    )
    .await?;
    let elapsed = started.elapsed();
    receipt.verify(HELLO_ID)?;
    let (sum, prod): (u32, u32) = receipt.journal.decode()?;
    anyhow::ensure!(
        sum == args.a.wrapping_add(args.b) && prod == args.a.wrapping_mul(args.b),
        "commit-reveal journal mismatch"
    );
    print_pass("commit-reveal", &cfg.endpoint, HELLO_ID, &format!("({sum}, {prod})"), elapsed);
    Ok(())
}

async fn run_diverse(cfgs: &[ClientConfig], args: &Args) -> anyhow::Result<()> {
    let payload = hello_payload(args.a, args.b)?;
    let started = Instant::now();
    let receipt = prove_diverse(
        cfgs,
        payload,
        GuestElfRef::InlineBytes(HELLO_ELF.to_vec()),
        HELLO_ID,
        cfgs.len(), // require ALL distinct hw_classes
    )
    .await?;
    let elapsed = started.elapsed();
    receipt.verify(HELLO_ID)?;
    print_pass(
        &format!("diverse-attestation ({} routes)", cfgs.len()),
        &cfgs.iter().map(|c| c.endpoint.clone()).collect::<Vec<_>>().join(", "),
        HELLO_ID,
        "matched-journal",
        elapsed,
    );
    Ok(())
}

async fn run_discover(args: &Args) -> anyhow::Result<()> {
    let path = args
        .registry
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--registry <path> required for --test discover"))?;
    let ops = discovery::discover(&discovery::Source::File(path.clone())).await?;
    println!();
    println!("PASS  discovered {} operator(s) from {}", ops.len(), path.display());
    for o in &ops {
        println!(
            "  {} @ {} hw={:?} mrenclave={}",
            o.label.as_deref().unwrap_or(""),
            o.endpoint,
            o.hw_class,
            hex::encode(o.mrenclave)
        );
    }
    Ok(())
}

async fn run_ranked(cfgs: &[ClientConfig], args: &Args) -> anyhow::Result<()> {
    let ledger = ReputationLedger::ephemeral();
    // Pre-bias the ledger so the second endpoint outranks the first.
    ledger.record_failure(&cfgs[0].endpoint).await;
    ledger.record_failure(&cfgs[0].endpoint).await;
    ledger.record_success(&cfgs[1].endpoint, 50).await;
    ledger.record_success(&cfgs[1].endpoint, 60).await;
    let payload = hello_payload(args.a, args.b)?;
    let started = Instant::now();
    let receipt = prove_multi_ranked(
        cfgs,
        payload,
        GuestElfRef::InlineBytes(HELLO_ELF.to_vec()),
        HELLO_ID,
        Some(&ledger),
    )
    .await?;
    let elapsed = started.elapsed();
    receipt.verify(HELLO_ID)?;
    let snap = ledger.snapshot().await;
    print_pass(
        &format!("ranked (pre-biased: {} > {})", cfgs[1].endpoint, cfgs[0].endpoint),
        &format!("score[{}] = {:.2}, score[{}] = {:.2}",
                cfgs[0].endpoint,
                snap.stats.get(&cfgs[0].endpoint).map(|s| s.score()).unwrap_or(0.0),
                cfgs[1].endpoint,
                snap.stats.get(&cfgs[1].endpoint).map(|s| s.score()).unwrap_or(0.0)),
        HELLO_ID,
        "matched-journal",
        elapsed,
    );
    Ok(())
}

fn print_pass(label: &str, endpoint: &str, image_id: [u32; 8], journal: &str, elapsed: std::time::Duration) {
    println!();
    println!("PASS  end-to-end remote-proved receipt verified ({label})");
    println!("  endpoint        = {endpoint}");
    println!("  image_id        = {}", image_id_hex(&image_id));
    println!("  journal         = {journal}");
    println!("  wall_clock      = {elapsed:?}");
    if std::env::var("RISC0_DEV_MODE").as_deref() == Ok("1") {
        println!("  mode            = DEV (fake receipts; correctness path only)");
    } else {
        println!("  mode            = REAL STARK");
    }
}

fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}
