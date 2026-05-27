//! Operator daemon state: long-term attestation root, ephemeral session key,
//! in-memory job table (persisted to JSONL), ELF cache, metrics, policy.

use psychopomp_attest::{Attestor, StubAttestor};
use psychopomp_types::{
    AttestationDoc, HwClass, JobPrecommit, JobStatus, TrustedRoot, TrustedRoots,
    SCHEMA_VERSION,
};
use rand::rngs::OsRng;
use rand::RngCore;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::elf_cache::ElfCache;
use crate::metrics::Metrics;
use crate::persistence::JobPersistence;
use crate::policy::Policy;
use crate::rate_limit::RateLimiter;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub mrenclave: [u8; 32],
    pub attestor: StubAttestor,
    pub session: Mutex<Session>,
    pub attestation_valid_secs: u64,
    pub jobs: Mutex<HashMap<Uuid, JobStatus>>,
    /// Precommitted jobs awaiting ciphertext upload (commit-reveal flow).
    /// `accepted_at` lets us GC stale precommits.
    pub pending_cipher: Mutex<HashMap<Uuid, (JobPrecommit, u64)>>,
    pub job_slots: Arc<Semaphore>,
    pub elf_cache: ElfCache,
    pub metrics: Metrics,
    pub policy: Policy,
    pub rate_limiter: RateLimiter,
    pub persist: Option<JobPersistence>,
}

pub struct Session {
    pub sk: StaticSecret,
    pub pk: PublicKey,
    pub nonce: [u8; 32],
    pub doc: AttestationDoc,
}

pub struct AppStateConfig {
    pub mrenclave: [u8; 32],
    pub max_concurrent: usize,
    pub attestation_valid_secs: u64,
    pub state_dir: PathBuf,
    pub policy: Policy,
    pub hw_class: HwClass,
}

impl AppState {
    pub async fn new(cfg: AppStateConfig) -> anyhow::Result<Self> {
        let attestor = StubAttestor::with_hw_class(cfg.mrenclave, cfg.hw_class);
        let session = Session::new(&attestor, cfg.attestation_valid_secs);
        let elf_cache = ElfCache::new(cfg.state_dir.join("elf")).await?;
        let (persist, jobs) = JobPersistence::open(&cfg.state_dir).await?;
        let rate_limiter = RateLimiter::new(cfg.policy.max_jobs_per_minute_per_client);
        Ok(Self {
            inner: Arc::new(Inner {
                mrenclave: cfg.mrenclave,
                attestor,
                session: Mutex::new(session),
                attestation_valid_secs: cfg.attestation_valid_secs,
                jobs: Mutex::new(jobs),
                pending_cipher: Mutex::new(HashMap::new()),
                job_slots: Arc::new(Semaphore::new(cfg.max_concurrent)),
                elf_cache,
                metrics: Metrics::new(),
                policy: cfg.policy,
                rate_limiter,
                persist: Some(persist),
            }),
        })
    }

    pub fn root_pubkey(&self) -> [u8; 32] {
        self.inner.attestor.verifying_key().to_bytes()
    }

    pub fn trusted_roots(&self) -> TrustedRoots {
        TrustedRoots {
            schema_version: SCHEMA_VERSION,
            roots: vec![TrustedRoot {
                hw_class: self.inner.attestor.hw_class(),
                label: "stub-self".into(),
                der_cert: self.root_pubkey().to_vec(),
            }],
        }
    }
}

impl Session {
    pub fn new(attestor: &StubAttestor, valid_secs: u64) -> Self {
        let mut sk_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut sk_bytes);
        let sk = StaticSecret::from(sk_bytes);
        let pk = PublicKey::from(&sk);
        let mut nonce = [0u8; 32];
        OsRng.fill_bytes(&mut nonce);
        let doc = attestor
            .produce(pk.to_bytes(), nonce, valid_secs)
            .expect("stub attestor never fails");
        Self { sk, pk, nonce, doc }
    }

    pub fn refresh_doc(&mut self, attestor: &StubAttestor, valid_secs: u64) {
        self.doc = attestor
            .produce(self.pk.to_bytes(), self.nonce, valid_secs)
            .expect("stub attestor never fails");
    }
}
