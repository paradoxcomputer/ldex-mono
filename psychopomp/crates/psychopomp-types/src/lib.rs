//! Wire protocol + shared types for psychopomp.
//!
//! Everything here is pure: serde + borsh, no I/O, no crypto execution. The
//! `client` and `prover` crates import these to talk over HTTP/JSON wrapping
//! Borsh-encoded inner payloads.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub mod hex_bytes;

pub const SCHEMA_VERSION: u16 = 0;

/// Self-declared hardware class of an operator's prover. Defined in the
/// `psychopomp-hwclass` crate so the LEZ guests can import it without
/// cascading the full `psychopomp-types` dep graph.
pub use psychopomp_hwclass::HwClass;

/// Attestation document published by an operator. In Phase-0 the chain is one
/// self-signed ed25519 cert; in Phase-1 it's the vendor's DER chain. Shape is
/// identical so verification code lives in `psychopomp-attest`.
#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct AttestationDoc {
    pub schema_version: u16,
    pub hw_class: HwClass,
    #[serde(with = "hex_bytes::array32")]
    pub mrenclave: [u8; 32],
    #[serde(with = "hex_bytes::array32")]
    pub ephemeral_x25519_pub: [u8; 32],
    #[serde(with = "hex_bytes::array32")]
    pub nonce: [u8; 32],
    pub not_before: u64,
    pub not_after: u64,
    /// DER-encoded cert chain, leaf-first. Phase-0 stub: single self-signed
    /// ed25519 cert whose subject pubkey signed `signature`.
    #[serde(with = "hex_bytes::vec_vec")]
    pub vendor_chain: Vec<Vec<u8>>,
    /// Operator's current load advertisement. Clients ranking by latency can
    /// use this as a freshness signal beyond their own reputation ledger.
    /// Fields default to zero — not covered by the signature for backward
    /// compatibility with older readers.
    #[serde(default)]
    pub queue_depth: u32,
    /// Operator's published estimated wall-clock for a fresh job posted right
    /// now (ms). 0 = unknown / no history yet.
    #[serde(default)]
    pub estimated_wall_clock_ms: u64,
    /// ed25519 signature over canonical-borsh of `Self` with `signature` set to
    /// empty.
    #[serde(with = "hex_bytes::vec")]
    pub signature: Vec<u8>,
}

impl AttestationDoc {
    /// Bytes that `signature` covers. Computed by borsh-encoding `Self` with
    /// `signature` swapped for an empty `Vec<u8>`.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut copy = self.clone();
        copy.signature = Vec::new();
        borsh::to_vec(&copy).expect("borsh AttestationDoc")
    }
}

/// Reference to the guest ELF to prove against.
///
/// - `InlineBytes` — full ELF in the request. Fine for small (<200 KB) guests.
/// - `Cached` — operator looks up the ELF in its `/v0/elf` cache, keyed by
///   `JobRequest::image_id`. Client should probe `HEAD /v0/elf/{image_id_hex}`
///   first and upload via `POST /v0/elf` on a miss.
#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum GuestElfRef {
    InlineBytes(#[serde(with = "hex_bytes::vec")] Vec<u8>),
    Cached,
}

/// Render an `image_id` as a 64-char hex string. Stable + path-safe.
pub fn image_id_hex(id: &[u32; 8]) -> String {
    let mut bytes = [0u8; 32];
    for (i, w) in id.iter().enumerate() {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    hex::encode(bytes)
}

pub fn image_id_from_hex(s: &str) -> Result<[u32; 8], String> {
    let bytes = hex::decode(s).map_err(|e| e.to_string())?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut id = [0u32; 8];
    for (i, w) in id.iter_mut().enumerate() {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&bytes[i * 4..(i + 1) * 4]);
        *w = u32::from_le_bytes(buf);
    }
    Ok(id)
}

/// The plaintext we encrypt and send across the wire. The prover reconstructs
/// an `ExecutorEnv` from these fields.
///
/// - `stdin` is appended to the guest's input via `write_slice` (no length
///   prefix). This is what `ExecutorEnvBuilder::write(&T)` produces under the
///   hood, so a host that does `builder.write(&MyInput { ... })` can compute
///   `stdin = bytemuck::cast_slice(&risc0_zkvm::serde::to_vec(&input))` and
///   the guest sees identical bytes.
/// - `stdin_frames` map 1:1 to `write_frame` calls (length-prefixed; for
///   guests that explicitly use `env::read_frame`).
/// - `assumptions[i]` is a bincode-encoded `risc0_zkvm::AssumptionReceipt`
///   (so both `Proven(InnerReceipt)` and `Unresolved(Assumption)` round-trip).
///   The simplest producer is
///   `bincode::serialize(&AssumptionReceipt::from(receipt))`.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct WitnessPayload {
    pub schema_version: u16,
    pub stdin: Vec<u8>,
    pub stdin_frames: Vec<Vec<u8>>,
    pub assumptions: Vec<Vec<u8>>,
    pub env_vars: Vec<(String, String)>,
    pub session_limit: Option<u64>,
    pub segment_limit_po2: Option<u32>,
}

impl Default for WitnessPayload {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            stdin: Vec::new(),
            stdin_frames: Vec::new(),
            assumptions: Vec::new(),
            env_vars: Vec::new(),
            session_limit: None,
            segment_limit_po2: None,
        }
    }
}

/// Iterated-SHA-256 time-lock puzzle parameters. Sequential work: the
/// operator must compute `sha256^iterations(seed)` (each iteration depends on
/// the prior, so parallelism doesn't help on commodity hardware). The result
/// is XOR'd with the AEAD key to bind decryption to the puzzle solution.
///
/// This is a deliberate Phase-1 stand-in for a proper VDF. Strong against
/// "operator peeks then races a rebid"; weak against an adversary with
/// dedicated SHA-256 ASICs. Pick `iterations` so that one round on
/// commodity CPU is at least `desired_min_delay`.
#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct TimelockPuzzle {
    pub schema_version: u16,
    pub iterations: u64,
    #[serde(with = "hex_bytes::array32")]
    pub seed: [u8; 32],
}

impl TimelockPuzzle {
    pub fn new(iterations: u64, seed: [u8; 32]) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            iterations,
            seed,
        }
    }

    /// Compute `sha256^iterations(seed)`. CPU-bound; caller should run on a
    /// blocking thread. Deterministic.
    pub fn solve(&self) -> [u8; 32] {
        let mut state = self.seed;
        for _ in 0..self.iterations {
            let mut h = Sha256::new();
            h.update(state);
            state = h.finalize().into();
        }
        state
    }

    /// Benchmark this machine's SHA-256 throughput and return the number of
    /// iterations expected to take roughly `target` on a single CPU core. Uses
    /// a fixed-budget calibration (50 ms) and extrapolates linearly.
    ///
    /// Use this so callers don't have to magic-number iteration counts.
    /// Note the operator may have faster or slower hardware than the client;
    /// the puzzle's wall-clock cost on the prover side will differ.
    pub fn calibrate_for(target: std::time::Duration) -> u64 {
        let probe_iters: u64 = 200_000;
        let started = std::time::Instant::now();
        let p = Self::new(probe_iters, [0xa5; 32]);
        let _ = p.solve();
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return probe_iters;
        }
        let iters_per_sec = (probe_iters as f64) / elapsed;
        let want = (iters_per_sec * target.as_secs_f64()).max(1.0).round() as u64;
        want.max(1)
    }
}

/// Posted to `POST /v0/jobs`. The witness ciphertext is bound under AEAD AAD
/// to `bound_mrenclave` and the IMAGE_ID, so an operator running a different
/// binary or a substituted guest cannot decrypt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobRequest {
    pub schema_version: u16,
    pub image_id: [u32; 8],
    pub guest_elf: GuestElfRef,
    #[serde(with = "hex_bytes::vec")]
    pub witness_ct: Vec<u8>,
    #[serde(with = "hex_bytes::array24")]
    pub witness_nonce: [u8; 24],
    #[serde(with = "hex_bytes::array32")]
    pub client_x25519_pub: [u8; 32],
    #[serde(with = "hex_bytes::array32")]
    pub bound_mrenclave: [u8; 32],
    pub deadline_unix: u64,
    /// If `Some`, the operator must compute the puzzle's `solve()` to derive
    /// the inner AEAD key (XOR'd against the puzzle output). Default `None`
    /// → operator decrypts immediately on receipt.
    #[serde(default)]
    pub timelock: Option<TimelockPuzzle>,
}

impl JobRequest {
    /// Canonical bytes used in `JobBinding::job_id_hash` (so the server's
    /// receipt is provably tied to the exact request).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialize JobRequest")
    }

    pub fn job_id_hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(self.canonical_bytes());
        h.finalize().into()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobAccepted {
    pub job_id: Uuid,
    pub accepted_at: u64,
}

/// First half of commit-reveal: client posts everything except the witness
/// ciphertext (it sends only `ciphertext_hash`). Operator awards the job to
/// itself before seeing the ciphertext — see README §"MEV / front-running >
/// Commit-reveal cipher delivery".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobPrecommit {
    pub schema_version: u16,
    pub image_id: [u32; 8],
    pub guest_elf: GuestElfRef,
    #[serde(with = "hex_bytes::array32")]
    pub ciphertext_hash: [u8; 32],
    #[serde(with = "hex_bytes::array24")]
    pub witness_nonce: [u8; 24],
    #[serde(with = "hex_bytes::array32")]
    pub client_x25519_pub: [u8; 32],
    #[serde(with = "hex_bytes::array32")]
    pub bound_mrenclave: [u8; 32],
    pub deadline_unix: u64,
    #[serde(default)]
    pub timelock: Option<TimelockPuzzle>,
}

impl JobPrecommit {
    pub fn signing_bytes(&self, job_id: Uuid, accepted_at: u64) -> Vec<u8> {
        let mut b = Vec::with_capacity(256);
        b.extend_from_slice(b"psychopomp/v0/award");
        b.extend_from_slice(&self.schema_version.to_le_bytes());
        for w in &self.image_id {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.extend_from_slice(&self.ciphertext_hash);
        b.extend_from_slice(&self.witness_nonce);
        b.extend_from_slice(&self.client_x25519_pub);
        b.extend_from_slice(&self.bound_mrenclave);
        b.extend_from_slice(&self.deadline_unix.to_le_bytes());
        b.extend_from_slice(job_id.as_bytes());
        b.extend_from_slice(&accepted_at.to_le_bytes());
        b
    }
}

/// Operator's response to `JobPrecommit`. Contains an ed25519 signature by
/// the attestation root over `(precommit fields, job_id, accepted_at)` — the
/// operator commits to running this exact job with this exact ciphertext_hash
/// BEFORE they see the ciphertext, so they can't peek-then-rebid.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobAward {
    pub job_id: Uuid,
    pub accepted_at: u64,
    #[serde(with = "hex_bytes::vec")]
    pub award_signature: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running { since_unix: u64 },
    Done { result: Box<JobResult> },
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobResult {
    pub schema_version: u16,
    #[serde(with = "hex_bytes::vec")]
    pub receipt: Vec<u8>,
    pub attestation: AttestationDoc,
    pub job_binding: JobBinding,
    pub wall_clock_ms: u64,
}

/// Ties an attestation document to a specific completed job. The operator's
/// long-term root key signs over the request hash + the journal hash; tampering
/// with either invalidates the binding.
#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct JobBinding {
    #[serde(with = "hex_bytes::array32")]
    pub job_id_hash: [u8; 32],
    #[serde(with = "hex_bytes::array32")]
    pub journal_hash: [u8; 32],
    #[serde(with = "hex_bytes::vec")]
    pub signature: Vec<u8>,
}

impl JobBinding {
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut copy = self.clone();
        copy.signature = Vec::new();
        borsh::to_vec(&copy).expect("borsh JobBinding")
    }
}

/// Returned by `GET /v0/attestation/roots`. List of DER-encoded ed25519 cert
/// SPKIs (or, in Phase-1, vendor root certs) the client should trust.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustedRoots {
    pub schema_version: u16,
    pub roots: Vec<TrustedRoot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustedRoot {
    pub hw_class: HwClass,
    pub label: String,
    #[serde(with = "hex_bytes::vec")]
    pub der_cert: Vec<u8>,
}

#[derive(thiserror::Error, Debug)]
pub enum WireError {
    #[error("schema mismatch: got {got}, want {want}")]
    SchemaMismatch { got: u16, want: u16 },
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("borsh: {0}")]
    Borsh(#[from] std::io::Error),
}

/// Compute the sha256 of the running binary at the given path. Phase-0
/// "MRENCLAVE": same property as a hardware measurement (this exact binary).
pub fn measure_binary(path: &std::path::Path) -> std::io::Result<[u8; 32]> {
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(h.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timelock_solve_is_deterministic() {
        let p = TimelockPuzzle::new(100, [7u8; 32]);
        assert_eq!(p.solve(), p.solve());
    }

    #[test]
    fn timelock_changes_with_iterations() {
        let a = TimelockPuzzle::new(10, [7u8; 32]).solve();
        let b = TimelockPuzzle::new(11, [7u8; 32]).solve();
        assert_ne!(a, b);
    }

    #[test]
    fn timelock_changes_with_seed() {
        let a = TimelockPuzzle::new(10, [7u8; 32]).solve();
        let b = TimelockPuzzle::new(10, [8u8; 32]).solve();
        assert_ne!(a, b);
    }

    #[test]
    fn timelock_calibrate_picks_nonzero() {
        let n = TimelockPuzzle::calibrate_for(std::time::Duration::from_millis(20));
        assert!(n > 0);
        let p = TimelockPuzzle::new(n, [0u8; 32]);
        let started = std::time::Instant::now();
        let _ = p.solve();
        let ms = started.elapsed().as_millis() as i64;
        // Allow large slack: target 20ms, accept 1..400ms
        assert!((1..=400).contains(&ms), "solve took {ms} ms");
    }

    #[test]
    fn image_id_hex_roundtrip() {
        let id = [1u32, 2, 3, 4, 5, 6, 7, 8];
        let s = image_id_hex(&id);
        let id2 = image_id_from_hex(&s).unwrap();
        assert_eq!(id, id2);
    }
}
