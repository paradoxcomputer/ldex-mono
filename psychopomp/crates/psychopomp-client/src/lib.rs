//! Wallet-side SDK.
//!
//! Public surface:
//! - [`prove`] — outsource one job to one operator. Verifies attestation,
//!   ECDH-encrypts the witness, posts, polls, verifies the returned Receipt.
//! - [`prove_multi`] — fan out to N operators in parallel; first valid response
//!   wins (README §"Censorship — Multi-route by default").
//! - [`ensure_elf_cached`] — probe the operator's `/v0/elf` cache and upload
//!   on miss, so the JobRequest can use `GuestElfRef::Cached` and skip the
//!   inline-ELF round trip on every call (big win for LDEX's ~2 MB privacy
//!   circuit).

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use psychopomp_attest::{StubVerifier, Verifier_};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use psychopomp_types::{
    image_id_hex, AttestationDoc, GuestElfRef, JobAccepted, JobAward, JobPrecommit, JobRequest,
    JobStatus, TimelockPuzzle, TrustedRoots, WitnessPayload, SCHEMA_VERSION,
};
use sha2::Digest;
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::Client as Http;
use risc0_zkvm::Receipt;
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};
use x25519_dalek::{EphemeralSecret, PublicKey};

pub use psychopomp_types as types;
pub mod discovery;
pub mod reputation;

#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("borsh: {0}")]
    Borsh(#[from] std::io::Error),
    #[error("attest: {0}")]
    Attest(#[from] psychopomp_attest::AttestError),
    #[error("aead: {0}")]
    Aead(String),
    #[error("schema mismatch: got {0}")]
    SchemaMismatch(u16),
    #[error("server error: {0}")]
    Server(String),
    #[error("job failed: {0}")]
    JobFailed(String),
    #[error("deadline exceeded after {0:?}")]
    Deadline(Duration),
    #[error("receipt verification failed: {0}")]
    ReceiptVerify(String),
    #[error("policy: {0}")]
    Policy(String),
    #[error("all routes failed: {0:?}")]
    AllRoutesFailed(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub endpoint: String,
    pub expected_mrenclave: [u8; 32],
    pub trusted_roots: Vec<[u8; 32]>,
    pub deadline: Duration,
    pub poll_interval: Duration,
    /// Bearer token for POST /v0/elf (operator-side ELF upload auth).
    /// Empty = no Authorization header sent.
    pub upload_bearer: Option<String>,
    /// Trust the operator's self-signed TLS cert without verifying its chain.
    /// Use only in tests / local-dev where the operator and client share an
    /// out-of-band trust path (the MRENCLAVE pin already provides binding).
    pub accept_invalid_tls: bool,
}

impl ClientConfig {
    pub fn local(endpoint: impl Into<String>, expected_mrenclave: [u8; 32], root: [u8; 32]) -> Self {
        Self {
            endpoint: endpoint.into(),
            expected_mrenclave,
            trusted_roots: vec![root],
            deadline: Duration::from_secs(60 * 60),
            poll_interval: Duration::from_millis(750),
            upload_bearer: None,
            accept_invalid_tls: false,
        }
    }
}

fn http_client(cfg: &ClientConfig, timeout: Duration) -> Result<Http, ClientError> {
    Ok(Http::builder()
        .timeout(timeout)
        .danger_accept_invalid_certs(cfg.accept_invalid_tls)
        .build()?)
}

/// Ensure the operator has cached the given ELF. Returns `true` if the ELF
/// was already present, `false` if it was uploaded. Use before
/// `prove(..., GuestElfRef::Cached)`.
pub async fn ensure_elf_cached(
    cfg: &ClientConfig,
    image_id: &[u32; 8],
    elf: &[u8],
) -> Result<bool, ClientError> {
    let http = http_client(cfg, Duration::from_secs(30))?;
    let hex = image_id_hex(image_id);
    let head = http
        .head(format!("{}/v0/elf/{}", cfg.endpoint, hex))
        .send()
        .await?;
    if head.status().is_success() {
        debug!(image_id = %hex, "ELF already cached on operator");
        return Ok(true);
    }
    info!(image_id = %hex, bytes = elf.len(), "uploading ELF to operator");
    let mut req = http
        .post(format!("{}/v0/elf/{}", cfg.endpoint, hex))
        .header("content-type", "application/octet-stream");
    if let Some(tok) = &cfg.upload_bearer {
        req = req.header("authorization", format!("Bearer {tok}"));
    }
    let resp = req.body(elf.to_vec()).send().await?;
    if !resp.status().is_success() {
        let st = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(ClientError::Server(format!("POST /v0/elf {st}: {body}")));
    }
    Ok(false)
}

/// Outsource a single proof to one Psychopomp operator. `guest_elf` carries
/// either the inline ELF (for small guests) or a `Cached` reference (caller
/// must have uploaded via [`ensure_elf_cached`] first).
pub async fn prove(
    cfg: &ClientConfig,
    witness: WitnessPayload,
    guest_elf: GuestElfRef,
    image_id: [u32; 8],
) -> Result<Receipt, ClientError> {
    prove_with_timelock(cfg, witness, guest_elf, image_id, None).await
}

/// Same as `prove`, but wraps the AEAD key in a sequential-work puzzle. The
/// operator must compute `puzzle.solve()` before they can decrypt the
/// witness. README §"MEV / front-running" — time-locked encryption for the
/// highest-sensitivity jobs.
pub async fn prove_with_timelock(
    cfg: &ClientConfig,
    witness: WitnessPayload,
    guest_elf: GuestElfRef,
    image_id: [u32; 8],
    timelock: Option<TimelockPuzzle>,
) -> Result<Receipt, ClientError> {
    let http = http_client(cfg, Duration::from_secs(45))?;

    // 1. Attestation -----------------------------------------------------------
    let roots: TrustedRoots = http
        .get(format!("{}/v0/attestation/roots", cfg.endpoint))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let server_roots: Vec<[u8; 32]> = roots
        .roots
        .iter()
        .filter_map(|r| {
            let v = &r.der_cert;
            if v.len() == 32 {
                let mut a = [0u8; 32];
                a.copy_from_slice(v);
                Some(a)
            } else {
                None
            }
        })
        .collect();
    if !cfg.trusted_roots.iter().any(|r| server_roots.contains(r)) {
        return Err(ClientError::Policy(
            "operator's published roots do not overlap our trust set".into(),
        ));
    }

    let doc: AttestationDoc = http
        .get(format!("{}/v0/attestation", cfg.endpoint))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let verifier = StubVerifier::new(cfg.trusted_roots.clone());
    verifier.verify_attestation(&doc, &cfg.expected_mrenclave)?;
    debug!("attestation verified; enclave epk = {}", hex::encode(doc.ephemeral_x25519_pub));

    // 2. ECDH + AEAD over the witness -----------------------------------------
    let client_sk = EphemeralSecret::random_from_rng(OsRng);
    let client_pk = PublicKey::from(&client_sk);
    let enclave_pk = PublicKey::from(doc.ephemeral_x25519_pub);
    let shared = client_sk.diffie_hellman(&enclave_pk);
    let base_key = derive_aead_key(shared.as_bytes(), &doc.nonce, &cfg.expected_mrenclave);
    // If a timelock puzzle is attached, XOR the AEAD key with the puzzle
    // output the operator MUST compute. Operator side does the same XOR
    // after solving — symmetric.
    let aead_key = match &timelock {
        Some(p) => {
            let mut k = base_key;
            let mask = p.solve();
            for (a, b) in k.iter_mut().zip(mask.iter()) {
                *a ^= *b;
            }
            k
        }
        None => base_key,
    };
    let aead = XChaCha20Poly1305::new(&aead_key.into());

    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let plaintext = borsh::to_vec(&witness)?;
    let aad = build_aad(&cfg.expected_mrenclave, &image_id);
    let ct = aead
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: &plaintext, aad: &aad })
        .map_err(|e| ClientError::Aead(e.to_string()))?;

    // 3. Post job --------------------------------------------------------------
    let deadline_unix = now_unix() + cfg.deadline.as_secs();
    let req = JobRequest {
        schema_version: SCHEMA_VERSION,
        image_id,
        guest_elf,
        witness_ct: ct,
        witness_nonce: nonce,
        client_x25519_pub: client_pk.to_bytes(),
        bound_mrenclave: cfg.expected_mrenclave,
        deadline_unix,
        timelock,
    };
    let post = http
        .post(format!("{}/v0/jobs", cfg.endpoint))
        .json(&req)
        .send()
        .await?;
    if !post.status().is_success() {
        let st = post.status();
        let body = post.text().await.unwrap_or_default();
        return Err(ClientError::Server(format!("POST /v0/jobs {st}: {body}")));
    }
    let accepted: JobAccepted = post.json().await?;
    info!(job_id = %accepted.job_id, "job accepted");

    // 4. Poll until terminal status -------------------------------------------
    let started = SystemTime::now();
    loop {
        if started.elapsed().unwrap_or_default() > cfg.deadline {
            return Err(ClientError::Deadline(cfg.deadline));
        }
        let status: JobStatus = http
            .get(format!("{}/v0/jobs/{}", cfg.endpoint, accepted.job_id))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match status {
            JobStatus::Pending | JobStatus::Running { .. } => {
                tokio::time::sleep(cfg.poll_interval).await;
                continue;
            }
            JobStatus::Failed { message } => return Err(ClientError::JobFailed(message)),
            JobStatus::Done { result } => {
                let receipt: Receipt = borsh::from_slice(&result.receipt)?;
                receipt
                    .verify(image_id)
                    .map_err(|e| ClientError::ReceiptVerify(e.to_string()))?;
                verifier.verify_job_binding(
                    &result.attestation,
                    &result.job_binding,
                    &req,
                    receipt.journal.bytes.as_slice(),
                )?;
                info!(wall_clock_ms = result.wall_clock_ms, "remote proof verified locally");
                return Ok(receipt);
            }
        }
    }
}

/// Two-phase commit-reveal flow (README §"MEV / front-running >
/// Commit-reveal cipher delivery").
///
/// 1. POST `/v0/jobs/precommit` with `sha256(ciphertext)` (not the ciphertext).
/// 2. Operator returns an ed25519 award signature by the attestation root,
///    committing to this exact `(job_id, ciphertext_hash)` BEFORE seeing the
///    plaintext.
/// 3. Client verifies the award signature against the trusted root.
/// 4. POST `/v0/jobs/{id}/ciphertext` with the actual ciphertext bytes.
/// 5. Operator verifies sha256(ct), kicks off proving.
/// 6. Poll as usual.
///
/// Protects against the "operator peeks then forks a re-bid with their own
/// info" attack that vanilla one-shot delivery is vulnerable to.
pub async fn prove_commit_reveal(
    cfg: &ClientConfig,
    witness: WitnessPayload,
    guest_elf: GuestElfRef,
    image_id: [u32; 8],
    timelock: Option<TimelockPuzzle>,
) -> Result<Receipt, ClientError> {
    let http = http_client(cfg, Duration::from_secs(45))?;

    // Attestation handshake (same as `prove`).
    let doc: AttestationDoc = http
        .get(format!("{}/v0/attestation", cfg.endpoint))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let verifier = StubVerifier::new(cfg.trusted_roots.clone());
    verifier.verify_attestation(&doc, &cfg.expected_mrenclave)?;

    // ECDH + AEAD (same key derivation as one-shot path).
    let client_sk = EphemeralSecret::random_from_rng(OsRng);
    let client_pk = PublicKey::from(&client_sk);
    let enclave_pk = PublicKey::from(doc.ephemeral_x25519_pub);
    let shared = client_sk.diffie_hellman(&enclave_pk);
    let base_key = derive_aead_key(shared.as_bytes(), &doc.nonce, &cfg.expected_mrenclave);
    let aead_key = match &timelock {
        Some(p) => {
            let mut k = base_key;
            for (a, b) in k.iter_mut().zip(p.solve().iter()) {
                *a ^= *b;
            }
            k
        }
        None => base_key,
    };
    let aead = XChaCha20Poly1305::new(&aead_key.into());

    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let plaintext = borsh::to_vec(&witness)?;
    let aad = build_aad(&cfg.expected_mrenclave, &image_id);
    let ct = aead
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: &plaintext, aad: &aad })
        .map_err(|e| ClientError::Aead(e.to_string()))?;

    // 1. Precommit ----------------------------------------------------------
    let mut hasher = sha2::Sha256::new();
    hasher.update(&ct);
    let ct_hash: [u8; 32] = hasher.finalize().into();
    let pre = JobPrecommit {
        schema_version: SCHEMA_VERSION,
        image_id,
        guest_elf,
        ciphertext_hash: ct_hash,
        witness_nonce: nonce,
        client_x25519_pub: client_pk.to_bytes(),
        bound_mrenclave: cfg.expected_mrenclave,
        deadline_unix: now_unix() + cfg.deadline.as_secs(),
        timelock,
    };
    let award: JobAward = http
        .post(format!("{}/v0/jobs/precommit", cfg.endpoint))
        .json(&pre)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // 2. Verify the operator's award commitment against the trusted root.
    let root_bytes: [u8; 32] = doc.vendor_chain.first()
        .ok_or_else(|| ClientError::Policy("empty vendor_chain".into()))?
        .clone()
        .try_into()
        .map_err(|_| ClientError::Policy("Phase-0 stub root must be 32 bytes".into()))?;
    if !cfg.trusted_roots.contains(&root_bytes) {
        return Err(ClientError::Policy("award root not trusted".into()));
    }
    let vk = VerifyingKey::from_bytes(&root_bytes).map_err(|e| ClientError::Policy(e.to_string()))?;
    let sig_bytes: [u8; 64] = award.award_signature.clone().try_into()
        .map_err(|_| ClientError::Policy("award signature not 64 bytes".into()))?;
    let sig = Signature::from_bytes(&sig_bytes);
    vk.verify(&pre.signing_bytes(award.job_id, award.accepted_at), &sig)
        .map_err(|e| ClientError::Policy(format!("award sig verify: {e}")))?;
    info!(job_id = %award.job_id, "award commitment verified — revealing ciphertext");

    // 3. Reveal the ciphertext.
    let reveal = serde_json::json!({ "ciphertext": hex::encode(&ct) });
    let post = http
        .post(format!("{}/v0/jobs/{}/ciphertext", cfg.endpoint, award.job_id))
        .json(&reveal)
        .send()
        .await?;
    if !post.status().is_success() {
        let st = post.status();
        let body = post.text().await.unwrap_or_default();
        return Err(ClientError::Server(format!("ciphertext reveal {st}: {body}")));
    }

    // 4. Poll. Same loop as the one-shot path.
    let started = SystemTime::now();
    loop {
        if started.elapsed().unwrap_or_default() > cfg.deadline {
            return Err(ClientError::Deadline(cfg.deadline));
        }
        let status: JobStatus = http
            .get(format!("{}/v0/jobs/{}", cfg.endpoint, award.job_id))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match status {
            JobStatus::Pending | JobStatus::Running { .. } => {
                tokio::time::sleep(cfg.poll_interval).await;
                continue;
            }
            JobStatus::Failed { message } => return Err(ClientError::JobFailed(message)),
            JobStatus::Done { result } => {
                let receipt: Receipt = borsh::from_slice(&result.receipt)?;
                receipt
                    .verify(image_id)
                    .map_err(|e| ClientError::ReceiptVerify(e.to_string()))?;
                // Reassemble the JobRequest the server reconstructed, so the
                // binding signature verifies under the same canonical bytes.
                let req = JobRequest {
                    schema_version: pre.schema_version,
                    image_id: pre.image_id,
                    guest_elf: pre.guest_elf.clone(),
                    witness_ct: ct,
                    witness_nonce: pre.witness_nonce,
                    client_x25519_pub: pre.client_x25519_pub,
                    bound_mrenclave: pre.bound_mrenclave,
                    deadline_unix: pre.deadline_unix,
                    timelock: pre.timelock.clone(),
                };
                verifier.verify_job_binding(
                    &result.attestation,
                    &result.job_binding,
                    &req,
                    receipt.journal.bytes.as_slice(),
                )?;
                return Ok(receipt);
            }
        }
    }
}

/// Fan out the same proof to N operators. Returns the first verified Receipt.
/// All in-flight requests are cancelled once we have a winner. Different
/// operators get independent ECDH-encrypted ciphertexts (no shared key).
///
/// If `ledger` is provided, the call:
///   1. ranks `cfgs` by reputation score (best-first) before dispatching, and
///   2. records each route's outcome (success+latency, or failure).
pub async fn prove_multi(
    cfgs: &[ClientConfig],
    witness: WitnessPayload,
    guest_elf: GuestElfRef,
    image_id: [u32; 8],
) -> Result<Receipt, ClientError> {
    prove_multi_ranked(cfgs, witness, guest_elf, image_id, None).await
}

pub async fn prove_multi_ranked(
    cfgs: &[ClientConfig],
    witness: WitnessPayload,
    guest_elf: GuestElfRef,
    image_id: [u32; 8],
    ledger: Option<&reputation::ReputationLedger>,
) -> Result<Receipt, ClientError> {
    if cfgs.is_empty() {
        return Err(ClientError::Policy("no routes".into()));
    }
    use futures::stream::{FuturesUnordered, StreamExt};

    // Optional ranking pass: sort cfgs best-first by reputation score.
    let order: Vec<usize> = match ledger {
        Some(l) => l.rank(cfgs, |c| c.endpoint.as_str()).await,
        None => (0..cfgs.len()).collect(),
    };

    let mut tasks: FuturesUnordered<_> = order
        .iter()
        .map(|&i| {
            let witness = witness.clone();
            let elf = guest_elf.clone();
            let cfg = cfgs[i].clone();
            async move {
                let started = std::time::Instant::now();
                let res = prove(&cfg, witness, elf, image_id).await;
                (i, cfg.endpoint, started.elapsed(), res)
            }
        })
        .collect();

    let mut errors = Vec::new();
    let mut winner: Option<(String, Receipt)> = None;
    while let Some((i, endpoint, elapsed, res)) = tasks.next().await {
        match res {
            Ok(r) if winner.is_none() => {
                info!(winner_endpoint = %endpoint, ms = elapsed.as_millis() as u64, "multi-route winner");
                if let Some(l) = ledger {
                    l.record_success(&endpoint, elapsed.as_millis() as u64).await;
                }
                winner = Some((endpoint, r));
                // Cancel the rest by dropping the futures stream.
                drop(tasks);
                break;
            }
            Ok(_) => {} // already have a winner; ignore (cancellation race)
            Err(e) => {
                warn!(route = i, endpoint = %endpoint, error = %e, "route failed");
                if let Some(l) = ledger {
                    l.record_failure(&endpoint).await;
                }
                errors.push(format!("[{i}] {endpoint}: {e}"));
            }
        }
    }
    if let Some((_, r)) = winner {
        return Ok(r);
    }
    Err(ClientError::AllRoutesFailed(errors))
}

/// Diverse-attestation co-proving (README §"Hardware diversity"). Same job
/// fanned out to operators on DIFFERENT hardware classes. Returns once
/// `min_diverse` operators on `min_diverse` distinct HwClass values have
/// returned matching-journal receipts. The returned Receipt is the first
/// one to verify; the others are dropped (their attestations are logged).
pub async fn prove_diverse(
    cfgs: &[ClientConfig],
    witness: WitnessPayload,
    guest_elf: GuestElfRef,
    image_id: [u32; 8],
    min_diverse: usize,
) -> Result<Receipt, ClientError> {
    use futures::stream::{FuturesUnordered, StreamExt};
    if cfgs.len() < min_diverse {
        return Err(ClientError::Policy(format!(
            "need >={min_diverse} routes, got {}",
            cfgs.len()
        )));
    }
    let mut tasks: FuturesUnordered<_> = cfgs
        .iter()
        .map(|cfg| {
            let witness = witness.clone();
            let elf = guest_elf.clone();
            let cfg = cfg.clone();
            async move {
                let res = prove(&cfg, witness, elf, image_id).await;
                let http = http_client(&cfg, Duration::from_secs(15));
                let doc = match http {
                    Ok(h) => h
                        .get(format!("{}/v0/attestation", cfg.endpoint))
                        .send()
                        .await
                        .ok()
                        .and_then(|r| r.error_for_status().ok())
                        .map(|r| async move { r.json::<AttestationDoc>().await.ok() }),
                    Err(_) => None,
                };
                let hw = match doc {
                    Some(f) => f.await.map(|d| d.hw_class),
                    None => None,
                };
                (cfg.endpoint, hw, res)
            }
        })
        .collect();

    let mut seen_hw: std::collections::HashSet<psychopomp_types::HwClass> =
        std::collections::HashSet::new();
    let mut first_receipt: Option<Receipt> = None;
    let mut journals: std::collections::HashMap<Vec<u8>, usize> = Default::default();
    let mut errors = Vec::new();

    while let Some((endpoint, hw, res)) = tasks.next().await {
        match res {
            Ok(r) => {
                let journal = r.journal.bytes.clone();
                *journals.entry(journal).or_insert(0) += 1;
                if let Some(h) = hw {
                    seen_hw.insert(h);
                }
                if first_receipt.is_none() {
                    first_receipt = Some(r);
                }
                info!(endpoint = %endpoint, hw_class = ?hw, distinct_hw = seen_hw.len(), "diverse route returned");
                if seen_hw.len() >= min_diverse && journals.len() == 1 {
                    return Ok(first_receipt.unwrap());
                }
            }
            Err(e) => {
                warn!(endpoint = %endpoint, error = %e, "diverse route failed");
                errors.push(format!("{endpoint}: {e}"));
            }
        }
    }
    if journals.len() > 1 {
        return Err(ClientError::Policy(format!(
            "diverse routes returned DIFFERENT journals (potential operator divergence): {journals:?}"
        )));
    }
    Err(ClientError::AllRoutesFailed(errors))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn derive_aead_key(shared: &[u8; 32], nonce: &[u8; 32], mrenclave: &[u8; 32]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(nonce), shared);
    let mut info = Vec::with_capacity(64);
    info.extend_from_slice(b"psychopomp/v0/witness");
    info.extend_from_slice(mrenclave);
    let mut out = [0u8; 32];
    hk.expand(&info, &mut out).expect("hkdf expand 32 bytes");
    out
}

pub fn build_aad(mrenclave: &[u8; 32], image_id: &[u32; 8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(32 + 32);
    aad.extend_from_slice(mrenclave);
    for w in image_id {
        aad.extend_from_slice(&w.to_le_bytes());
    }
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aad_is_deterministic() {
        let me = [9u8; 32];
        let id = [1u32, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(build_aad(&me, &id), build_aad(&me, &id));
        assert_ne!(build_aad(&me, &id), build_aad(&[0u8; 32], &id));
    }

    #[test]
    fn hkdf_distinct_per_mrenclave() {
        let shared = [42u8; 32];
        let nonce = [7u8; 32];
        let k1 = derive_aead_key(&shared, &nonce, &[1u8; 32]);
        let k2 = derive_aead_key(&shared, &nonce, &[2u8; 32]);
        assert_ne!(k1, k2);
    }
}
