use anyhow::Context;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::info;

mod elf_cache;
mod metrics;
mod persistence;
mod policy;
mod rate_limit;
mod server;
mod state;

#[derive(Parser, Debug)]
#[command(name = "psychopomp-prover", about = "TEE-attested outsourced RISC Zero prover")]
struct Args {
    /// Address to bind the HTTP server to.
    #[arg(long, default_value = "127.0.0.1:8088")]
    bind: SocketAddr,

    /// Override path used to compute MRENCLAVE. Defaults to /proc/self/exe.
    #[arg(long)]
    measure_path: Option<PathBuf>,

    /// Number of in-flight proof jobs allowed at once.
    #[arg(long, default_value_t = 2)]
    max_concurrent: usize,

    /// Attestation validity window in seconds.
    #[arg(long, default_value_t = 300)]
    attestation_valid_secs: u64,

    /// Directory for job log + ELF cache. Created if missing.
    #[arg(long, default_value = "./psychopomp-state")]
    state_dir: PathBuf,

    /// Optional TOML policy file (allowed image_ids, max limits).
    #[arg(long)]
    policy: Option<PathBuf>,

    /// PEM-encoded TLS cert chain. If set together with --tls-key, serve HTTPS.
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    /// PEM-encoded TLS private key.
    #[arg(long)]
    tls_key: Option<PathBuf>,
    /// If set with --tls-cert + --tls-key both absent, generate an in-memory
    /// self-signed cert for the bound address and serve HTTPS. The cert is
    /// printed to stderr as a PEM-encoded SubjectPublicKeyInfo so clients can
    /// pin it out-of-band. Phase-0 dev shortcut.
    #[arg(long)]
    tls_dev: bool,

    /// Hardware class to advertise in the attestation doc. Default: stub.
    #[arg(long, default_value = "stub")]
    hw_class: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,psychopomp_prover=debug,risc0_zkvm=info".into()),
        )
        .init();

    let args = Args::parse();
    let measure_path = args
        .measure_path
        .clone()
        .unwrap_or_else(|| std::env::current_exe().expect("read /proc/self/exe"));
    info!(measure_path = %measure_path.display(), "measuring binary");
    let mrenclave = psychopomp_types::measure_binary(&measure_path)
        .with_context(|| format!("read {}", measure_path.display()))?;
    info!(mrenclave = hex::encode(mrenclave), "MRENCLAVE");

    let policy = match &args.policy {
        Some(p) => {
            let pol = policy::Policy::load(p).with_context(|| format!("load {}", p.display()))?;
            info!(
                file = %p.display(),
                allowed_image_ids = pol.allowed_image_ids.len(),
                "loaded policy"
            );
            pol
        }
        None => {
            info!("no policy file; allow-all (dev posture)");
            policy::Policy::default()
        }
    };

    let hw_class: psychopomp_types::HwClass = args
        .hw_class
        .parse()
        .map_err(|e: String| anyhow::anyhow!("--hw-class: {e}"))?;
    let cfg = state::AppStateConfig {
        mrenclave,
        max_concurrent: args.max_concurrent,
        attestation_valid_secs: args.attestation_valid_secs,
        state_dir: args.state_dir.clone(),
        policy,
        hw_class,
    };
    let state = state::AppState::new(cfg).await?;
    info!(state_dir = %state.inner.elf_cache.dir().display(), "state directory ready");

    // Periodic compaction: every 5 min, if jobs.jsonl > 4 MiB, snapshot live state.
    {
        let st = state.clone();
        tokio::spawn(async move {
            const COMPACT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);
            const COMPACT_THRESHOLD_BYTES: u64 = 4 * 1024 * 1024;
            loop {
                tokio::time::sleep(COMPACT_INTERVAL).await;
                let Some(p) = &st.inner.persist else { continue };
                match p.size().await {
                    Ok(sz) if sz > COMPACT_THRESHOLD_BYTES => {
                        let live = st.inner.jobs.lock().await.clone();
                        if let Err(e) = p.compact(&live).await {
                            tracing::warn!(error = %e, "compaction failed");
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    let app = server::router(state);

    // rustls 0.23+ requires an explicit CryptoProvider. Install once at
    // startup so subsequent TLS calls in this process pick it up.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = build_tls_config(&args).await?;
    match tls {
        Some(rustls_cfg) => {
            info!(bind = %args.bind, "psychopomp-prover starting (TLS)");
            axum_server::bind_rustls(args.bind, rustls_cfg)
                .serve(app.into_make_service())
                .await?;
        }
        None => {
            info!(bind = %args.bind, "psychopomp-prover starting (plain HTTP)");
            let listener = tokio::net::TcpListener::bind(args.bind).await?;
            axum::serve(listener, app).await?;
        }
    }
    Ok(())
}

async fn build_tls_config(args: &Args) -> anyhow::Result<Option<axum_server::tls_rustls::RustlsConfig>> {
    use axum_server::tls_rustls::RustlsConfig;
    match (&args.tls_cert, &args.tls_key, args.tls_dev) {
        (Some(c), Some(k), _) => {
            info!(cert = %c.display(), "loading TLS cert+key from disk");
            let cfg = RustlsConfig::from_pem_file(c, k).await?;
            Ok(Some(cfg))
        }
        (None, None, true) => {
            info!("generating self-signed dev TLS cert");
            let mut params = rcgen::CertificateParams::new(vec![
                "localhost".to_string(),
                args.bind.ip().to_string(),
            ])?;
            params.distinguished_name = rcgen::DistinguishedName::new();
            params.distinguished_name.push(rcgen::DnType::CommonName, "psychopomp-dev");
            let key_pair = rcgen::KeyPair::generate()?;
            let cert = params.self_signed(&key_pair)?;
            let cert_pem = cert.pem();
            let key_pem = key_pair.serialize_pem();
            eprintln!("---BEGIN psychopomp-prover dev TLS cert---\n{cert_pem}---END---");
            let cfg = RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes()).await?;
            Ok(Some(cfg))
        }
        (None, None, false) => Ok(None),
        _ => anyhow::bail!("--tls-cert and --tls-key must be set together"),
    }
}
