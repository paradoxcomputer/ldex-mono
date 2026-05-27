//! HTTP surface: attestation handshake + ELF cache + job submit + status +
//! metrics. The prove call itself goes through `spawn_blocking` since
//! `risc0_zkvm::default_prover().prove()` is CPU/GPU-bound and blocks.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use psychopomp_attest::Attestor;
use psychopomp_types::{
    AttestationDoc, GuestElfRef, JobAccepted, JobAward, JobPrecommit, JobRequest, JobResult,
    JobStatus, TrustedRoots, WitnessPayload, SCHEMA_VERSION,
};
use sha2::Digest;
use risc0_zkvm::{default_prover, AssumptionReceipt, ExecutorEnv};
use sha2::Sha256;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, info, instrument};
use uuid::Uuid;

use crate::state::AppState;

const MAX_ELF_UPLOAD_BYTES: usize = 16 * 1024 * 1024;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v0/health", get(health))
        .route("/v0/metrics", get(metrics))
        .route("/v0/attestation", get(get_attestation))
        .route("/v0/attestation/roots", get(get_roots))
        .route("/v0/elf/:image_id_hex", axum::routing::head(head_elf))
        .route("/v0/elf/:image_id_hex", post(post_elf))
        .route("/v0/jobs", get(list_jobs).post(post_job))
        .route("/v0/jobs/precommit", post(post_precommit))
        .route("/v0/jobs/:job_id/ciphertext", post(post_ciphertext))
        .route("/v0/jobs/:job_id", get(get_job))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn metrics(State(state): State<AppState>) -> Response {
    let body = state.inner.metrics.render();
    ([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response()
}

async fn get_attestation(State(state): State<AppState>) -> Response {
    state.inner.metrics.attestation();
    let queue_depth = state.inner.jobs.lock().await.values().filter(|s| matches!(s, JobStatus::Pending | JobStatus::Running { .. })).count() as u32;
    let avg_ms = state.inner.metrics.avg_completed_wall_clock_ms();
    let eta_ms = avg_ms.saturating_mul(queue_depth as u64);
    let mut sess = state.inner.session.lock().await;
    sess.refresh_doc(&state.inner.attestor, state.inner.attestation_valid_secs);
    let mut doc: AttestationDoc = sess.doc.clone();
    doc.queue_depth = queue_depth;
    doc.estimated_wall_clock_ms = eta_ms;
    Json(doc).into_response()
}

#[derive(serde::Serialize)]
struct JobListEntry {
    job_id: Uuid,
    state: &'static str,
}

#[derive(serde::Serialize)]
struct JobList {
    schema_version: u16,
    total: usize,
    jobs: Vec<JobListEntry>,
}

async fn list_jobs(State(state): State<AppState>) -> Response {
    let jobs = state.inner.jobs.lock().await;
    let mut entries: Vec<JobListEntry> = jobs
        .iter()
        .map(|(id, s)| JobListEntry {
            job_id: *id,
            state: match s {
                JobStatus::Pending => "pending",
                JobStatus::Running { .. } => "running",
                JobStatus::Done { .. } => "done",
                JobStatus::Failed { .. } => "failed",
            },
        })
        .collect();
    entries.sort_by_key(|e| e.job_id);
    Json(JobList {
        schema_version: SCHEMA_VERSION,
        total: entries.len(),
        jobs: entries,
    })
    .into_response()
}

async fn get_roots(State(state): State<AppState>) -> Response {
    let r: TrustedRoots = state.trusted_roots();
    Json(r).into_response()
}

async fn head_elf(State(state): State<AppState>, Path(image_id_hex): Path<String>) -> Response {
    if state.inner.elf_cache.contains(&image_id_hex).await {
        state.inner.metrics.elf_hit();
        StatusCode::OK.into_response()
    } else {
        state.inner.metrics.elf_miss();
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn post_elf(
    State(state): State<AppState>,
    Path(image_id_hex): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(e) = state.inner.policy.check_upload_auth(auth) {
        return error_response(StatusCode::UNAUTHORIZED, e);
    }
    if body.len() > MAX_ELF_UPLOAD_BYTES {
        return error_response(StatusCode::PAYLOAD_TOO_LARGE, format!("ELF {} > 16MB", body.len()));
    }
    let expected = match psychopomp_types::image_id_from_hex(&image_id_hex) {
        Ok(id) => id,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, format!("bad image_id_hex: {e}")),
    };
    match state.inner.elf_cache.insert_verified(&expected, body.to_vec()).await {
        Ok(()) => {
            state.inner.metrics.elf_uploaded();
            StatusCode::CREATED.into_response()
        }
        Err(e) => error_response(StatusCode::BAD_REQUEST, e),
    }
}

#[instrument(skip_all, fields(image_id = ?req.image_id))]
async fn post_job(State(state): State<AppState>, Json(req): Json<JobRequest>) -> Response {
    if req.schema_version != SCHEMA_VERSION {
        state.inner.metrics.rejected();
        return error_response(StatusCode::BAD_REQUEST, format!("schema {}", req.schema_version));
    }
    if req.bound_mrenclave != state.inner.mrenclave {
        state.inner.metrics.rejected();
        return error_response(
            StatusCode::BAD_REQUEST,
            format!(
                "bound_mrenclave {} != ours {}",
                hex::encode(req.bound_mrenclave),
                hex::encode(state.inner.mrenclave)
            ),
        );
    }
    if now_unix() > req.deadline_unix {
        state.inner.metrics.rejected();
        return error_response(StatusCode::BAD_REQUEST, "deadline already past".into());
    }
    if let Err(e) = state.inner.policy.check(&req) {
        state.inner.metrics.rejected();
        return error_response(StatusCode::FORBIDDEN, e);
    }
    if let Err(retry_after) = state.inner.rate_limiter.check(req.client_x25519_pub).await {
        state.inner.metrics.rejected();
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, format!("{}", retry_after.ceil() as u64))],
            Json(serde_json::json!({ "error": "rate limited", "retry_after_secs": retry_after })),
        )
            .into_response();
    }
    // If the client says Cached, confirm we have it before accepting.
    if matches!(req.guest_elf, GuestElfRef::Cached) {
        let hex = psychopomp_types::image_id_hex(&req.image_id);
        if !state.inner.elf_cache.contains(&hex).await {
            state.inner.metrics.rejected();
            state.inner.metrics.elf_miss();
            return error_response(
                StatusCode::PRECONDITION_FAILED,
                format!("ELF {hex} not cached; upload via POST /v0/elf/{hex}"),
            );
        }
        state.inner.metrics.elf_hit();
    }

    let job_id = Uuid::new_v4();
    {
        let mut jobs = state.inner.jobs.lock().await;
        jobs.insert(job_id, JobStatus::Pending);
    }
    state.inner.metrics.accepted();
    if let Some(p) = &state.inner.persist {
        let _ = p.record(job_id, &JobStatus::Pending).await;
    }

    let st = state.clone();
    tokio::spawn(async move {
        if let Err(e) = run_job(st.clone(), job_id, req).await {
            error!(%job_id, error = %e, "job failed");
            st.inner.metrics.failed();
            let status = JobStatus::Failed { message: e.to_string() };
            st.inner.jobs.lock().await.insert(job_id, status.clone());
            if let Some(p) = &st.inner.persist {
                let _ = p.record(job_id, &status).await;
            }
        }
    });

    Json(JobAccepted {
        job_id,
        accepted_at: now_unix(),
    })
    .into_response()
}

async fn get_job(State(state): State<AppState>, Path(job_id): Path<Uuid>) -> Response {
    match state.inner.jobs.lock().await.get(&job_id) {
        Some(s) => Json(s.clone()).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "job not found".into()),
    }
}

#[instrument(skip_all, fields(image_id = ?pre.image_id))]
async fn post_precommit(
    State(state): State<AppState>,
    Json(pre): Json<JobPrecommit>,
) -> Response {
    if pre.schema_version != SCHEMA_VERSION {
        state.inner.metrics.rejected();
        return error_response(StatusCode::BAD_REQUEST, format!("schema {}", pre.schema_version));
    }
    if pre.bound_mrenclave != state.inner.mrenclave {
        state.inner.metrics.rejected();
        return error_response(StatusCode::BAD_REQUEST, "bound_mrenclave mismatch".into());
    }
    if now_unix() > pre.deadline_unix {
        state.inner.metrics.rejected();
        return error_response(StatusCode::BAD_REQUEST, "deadline already past".into());
    }
    if matches!(pre.guest_elf, GuestElfRef::Cached) {
        let hex = psychopomp_types::image_id_hex(&pre.image_id);
        if !state.inner.elf_cache.contains(&hex).await {
            state.inner.metrics.rejected();
            return error_response(
                StatusCode::PRECONDITION_FAILED,
                format!("ELF {hex} not cached"),
            );
        }
    }
    if let Err(retry_after) = state.inner.rate_limiter.check(pre.client_x25519_pub).await {
        state.inner.metrics.rejected();
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, format!("{}", retry_after.ceil() as u64))],
            Json(serde_json::json!({ "error": "rate limited", "retry_after_secs": retry_after })),
        )
            .into_response();
    }
    let job_id = Uuid::new_v4();
    let accepted_at = now_unix();
    // Sign award commitment with the attestation root.
    let bytes = pre.signing_bytes(job_id, accepted_at);
    let signature = state
        .inner
        .attestor
        .root_sign(&bytes);
    {
        let mut pc = state.inner.pending_cipher.lock().await;
        pc.insert(job_id, (pre.clone(), accepted_at));
    }
    {
        let mut jobs = state.inner.jobs.lock().await;
        jobs.insert(job_id, JobStatus::Pending);
    }
    state.inner.metrics.accepted();
    if let Some(p) = &state.inner.persist {
        let _ = p.record(job_id, &JobStatus::Pending).await;
    }
    Json(JobAward {
        job_id,
        accepted_at,
        award_signature: signature,
    })
    .into_response()
}

/// Body: { "ciphertext": "<hex>" }. The client reveals the witness ciphertext
/// after seeing the award_signature; server checks sha256(ct) ==
/// ciphertext_hash from the precommit, then kicks off proving.
#[derive(serde::Deserialize)]
struct CiphertextReveal {
    #[serde(with = "psychopomp_types::hex_bytes::vec")]
    ciphertext: Vec<u8>,
}

async fn post_ciphertext(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    Json(body): Json<CiphertextReveal>,
) -> Response {
    let (pre, _accepted_at) = match state.inner.pending_cipher.lock().await.remove(&job_id) {
        Some(p) => p,
        None => return error_response(StatusCode::NOT_FOUND, "no pending precommit".into()),
    };
    let mut h = sha2::Sha256::new();
    h.update(&body.ciphertext);
    let got: [u8; 32] = h.finalize().into();
    if got != pre.ciphertext_hash {
        state.inner.metrics.rejected();
        return error_response(
            StatusCode::BAD_REQUEST,
            "sha256(ct) != precommit.ciphertext_hash".into(),
        );
    }
    // Re-assemble a JobRequest and dispatch to the standard worker.
    let req = JobRequest {
        schema_version: pre.schema_version,
        image_id: pre.image_id,
        guest_elf: pre.guest_elf,
        witness_ct: body.ciphertext,
        witness_nonce: pre.witness_nonce,
        client_x25519_pub: pre.client_x25519_pub,
        bound_mrenclave: pre.bound_mrenclave,
        deadline_unix: pre.deadline_unix,
        timelock: pre.timelock,
    };
    let st = state.clone();
    let id = job_id;
    tokio::spawn(async move {
        if let Err(e) = run_job(st.clone(), id, req).await {
            error!(%id, error = %e, "job failed");
            st.inner.metrics.failed();
            let status = JobStatus::Failed { message: e.to_string() };
            st.inner.jobs.lock().await.insert(id, status.clone());
            if let Some(p) = &st.inner.persist {
                let _ = p.record(id, &status).await;
            }
        }
    });
    StatusCode::ACCEPTED.into_response()
}

async fn run_job(state: AppState, job_id: Uuid, req: JobRequest) -> anyhow::Result<()> {
    let _permit = state.inner.job_slots.clone().acquire_owned().await?;
    {
        let status = JobStatus::Running { since_unix: now_unix() };
        state.inner.jobs.lock().await.insert(job_id, status.clone());
        state.inner.metrics.started();
        if let Some(p) = &state.inner.persist {
            let _ = p.record(job_id, &status).await;
        }
    }

    // 1. Reconstruct AEAD key (and solve timelock puzzle if attached).
    let base_key = {
        let sess = state.inner.session.lock().await;
        let shared = sess.sk.diffie_hellman(&x25519_dalek::PublicKey::from(req.client_x25519_pub));
        derive_aead_key(shared.as_bytes(), &sess.nonce, &state.inner.mrenclave)
    };

    let aead_key = match &req.timelock {
        Some(puzzle) => {
            info!(iterations = puzzle.iterations, "solving timelock puzzle");
            let solve_started = Instant::now();
            // CPU-bound; offload from the runtime.
            let p = puzzle.clone();
            let mask = tokio::task::spawn_blocking(move || p.solve()).await?;
            let solve_ms = solve_started.elapsed().as_millis();
            info!(solve_ms, "timelock solved");
            let mut k = base_key;
            for (a, b) in k.iter_mut().zip(mask.iter()) {
                *a ^= *b;
            }
            k
        }
        None => base_key,
    };
    let aead = XChaCha20Poly1305::new(&aead_key.into());
    let aad = build_aad(&state.inner.mrenclave, &req.image_id);
    let plaintext = aead
        .decrypt(
            XNonce::from_slice(&req.witness_nonce),
            Payload { msg: &req.witness_ct, aad: &aad },
        )
        .map_err(|e| anyhow::anyhow!("aead decrypt: {e}"))?;
    let payload: WitnessPayload = borsh::from_slice(&plaintext)?;
    if payload.schema_version != SCHEMA_VERSION {
        anyhow::bail!("witness schema {}", payload.schema_version);
    }

    // 2. Resolve guest ELF (inline or cache lookup).
    let elf = match &req.guest_elf {
        GuestElfRef::InlineBytes(b) => b.clone(),
        GuestElfRef::Cached => state
            .inner
            .elf_cache
            .get(&req.image_id)
            .await
            .map_err(|e| anyhow::anyhow!("cache miss for image_id {}: {e}", psychopomp_types::image_id_hex(&req.image_id)))?,
    };

    // 3. Run the prove on a blocking thread.
    let req_clone = req.clone();
    let started = Instant::now();
    let proved = tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
        info!(
            stdin_bytes = payload.stdin.len(),
            stdin_frames = payload.stdin_frames.len(),
            assumptions = payload.assumptions.len(),
            "starting prover"
        );
        let mut builder = ExecutorEnv::builder();
        if !payload.stdin.is_empty() {
            builder.write_slice(&payload.stdin);
        }
        for frame in &payload.stdin_frames {
            builder.write_frame(frame);
        }
        for (i, bytes) in payload.assumptions.iter().enumerate() {
            let assumption: AssumptionReceipt = bincode::deserialize(bytes)
                .map_err(|e| anyhow::anyhow!("assumption[{i}] bincode decode: {e}"))?;
            builder.add_assumption(assumption);
        }
        if !payload.assumptions.is_empty() {
            info!(count = payload.assumptions.len(), "passing assumptions to prover");
        }
        for (k, v) in &payload.env_vars {
            builder.env_var(k, v);
        }
        if let Some(po2) = payload.segment_limit_po2 {
            builder.segment_limit_po2(po2);
        }
        if let Some(lim) = payload.session_limit {
            builder.session_limit(Some(lim));
        }
        let env = builder.build()?;
        let info = default_prover().prove(env, &elf)?;
        let receipt_bytes = borsh::to_vec(&info.receipt)?;
        let journal = info.receipt.journal.bytes.clone();
        Ok((receipt_bytes, journal))
    })
    .await??;
    let elapsed = started.elapsed();
    let (receipt_bytes, journal) = proved;

    // 4. Sign the binding.
    let binding = state.inner.attestor.sign_job(&req_clone, &journal)?;
    let doc = state.inner.session.lock().await.doc.clone();

    let result = JobResult {
        schema_version: SCHEMA_VERSION,
        receipt: receipt_bytes,
        attestation: doc,
        job_binding: binding,
        wall_clock_ms: elapsed.as_millis() as u64,
    };

    let status = JobStatus::Done { result: Box::new(result) };
    state.inner.jobs.lock().await.insert(job_id, status.clone());
    state.inner.metrics.completed(elapsed.as_millis() as u64);
    if let Some(p) = &state.inner.persist {
        let _ = p.record(job_id, &status).await;
    }
    info!(%job_id, elapsed_ms = elapsed.as_millis() as u64, "job done");
    Ok(())
}

fn error_response(code: StatusCode, msg: String) -> Response {
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn derive_aead_key(shared: &[u8; 32], nonce: &[u8; 32], mrenclave: &[u8; 32]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(nonce), shared);
    let mut info = Vec::with_capacity(64);
    info.extend_from_slice(b"psychopomp/v0/witness");
    info.extend_from_slice(mrenclave);
    let mut out = [0u8; 32];
    hk.expand(&info, &mut out).expect("hkdf expand 32 bytes");
    out
}

fn build_aad(mrenclave: &[u8; 32], image_id: &[u32; 8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(32 + 32);
    aad.extend_from_slice(mrenclave);
    for w in image_id {
        aad.extend_from_slice(&w.to_le_bytes());
    }
    aad
}
