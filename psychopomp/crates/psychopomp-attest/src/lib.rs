//! Attestation: stub TEE for Phase-0, traits sized to swap in NVIDIA NRAS /
//! AMD SEV-SNP later. The on-the-wire `AttestationDoc` shape is identical for
//! both — only the chain validator changes.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use psychopomp_types::{AttestationDoc, HwClass, JobBinding, JobRequest, SCHEMA_VERSION};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(thiserror::Error, Debug)]
pub enum AttestError {
    #[error("mrenclave mismatch")]
    MrenclaveMismatch { expected: [u8; 32], got: [u8; 32] },
    #[error("attestation expired: now={now} not_after={not_after}")]
    Expired { now: u64, not_after: u64 },
    #[error("attestation not yet valid: now={now} not_before={not_before}")]
    NotYetValid { now: u64, not_before: u64 },
    #[error("vendor_chain empty")]
    EmptyChain,
    #[error("leaf cert not in trusted roots")]
    UntrustedRoot,
    #[error("signature verification failed: {0}")]
    SigVerify(String),
    #[error("schema mismatch: got {0}")]
    SchemaMismatch(u16),
    #[error("ed25519: {0}")]
    Ed25519(#[from] ed25519_dalek::SignatureError),
}

pub trait Attestor: Send + Sync {
    fn mrenclave(&self) -> [u8; 32];
    fn hw_class(&self) -> HwClass;
    fn produce(
        &self,
        ephemeral_pk: [u8; 32],
        nonce: [u8; 32],
        valid_seconds: u64,
    ) -> Result<AttestationDoc, AttestError>;
    fn sign_job(&self, req: &JobRequest, journal: &[u8]) -> Result<JobBinding, AttestError>;
    /// DER-encoded "cert" for this attestor's long-term root signing key.
    /// Phase-0: SubjectPublicKeyInfo only; Phase-1: full vendor cert.
    fn root_cert_der(&self) -> Vec<u8>;
}

pub trait Verifier_: Send + Sync {
    fn verify_attestation(
        &self,
        doc: &AttestationDoc,
        expected_mrenclave: &[u8; 32],
    ) -> Result<(), AttestError>;
    fn verify_job_binding(
        &self,
        doc: &AttestationDoc,
        binding: &JobBinding,
        req: &JobRequest,
        journal: &[u8],
    ) -> Result<(), AttestError>;
}

// ---------- Phase-0 stub implementation ----------

/// Stub attestor. The "vendor chain" is just one ed25519 SPKI DER. The
/// "signature" is a real ed25519 over the same bytes a real TEE doc would
/// cover. Result: byte-identical wire shape to a future Phase-1 NRAS doc.
pub struct StubAttestor {
    root_sk: SigningKey,
    mrenclave: [u8; 32],
    hw_class: HwClass,
}

impl StubAttestor {
    pub fn new(mrenclave: [u8; 32]) -> Self {
        Self::with_hw_class(mrenclave, HwClass::Stub)
    }
    pub fn with_hw_class(mrenclave: [u8; 32], hw_class: HwClass) -> Self {
        let root_sk = SigningKey::generate(&mut OsRng);
        Self {
            root_sk,
            mrenclave,
            hw_class,
        }
    }

    pub fn from_sk(root_sk: SigningKey, mrenclave: [u8; 32]) -> Self {
        Self {
            root_sk,
            mrenclave,
            hw_class: HwClass::Stub,
        }
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.root_sk.verifying_key()
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl StubAttestor {
    /// Sign arbitrary bytes with the attestor's long-term root key. Used by
    /// the operator daemon for award-signatures in the commit-reveal flow.
    pub fn root_sign(&self, msg: &[u8]) -> Vec<u8> {
        self.root_sk.sign(msg).to_bytes().to_vec()
    }
}

impl Attestor for StubAttestor {
    fn mrenclave(&self) -> [u8; 32] {
        self.mrenclave
    }

    fn hw_class(&self) -> HwClass {
        self.hw_class
    }

    fn produce(
        &self,
        ephemeral_pk: [u8; 32],
        nonce: [u8; 32],
        valid_seconds: u64,
    ) -> Result<AttestationDoc, AttestError> {
        let now = now_unix();
        let leaf_der = self.root_cert_der();
        let mut doc = AttestationDoc {
            schema_version: SCHEMA_VERSION,
            hw_class: self.hw_class,
            mrenclave: self.mrenclave,
            ephemeral_x25519_pub: ephemeral_pk,
            nonce,
            not_before: now,
            not_after: now + valid_seconds,
            vendor_chain: vec![leaf_der],
            queue_depth: 0,
            estimated_wall_clock_ms: 0,
            signature: Vec::new(),
        };
        let sig = self.root_sk.sign(&doc.signing_bytes());
        doc.signature = sig.to_bytes().to_vec();
        Ok(doc)
    }

    fn sign_job(&self, req: &JobRequest, journal: &[u8]) -> Result<JobBinding, AttestError> {
        let job_id_hash = req.job_id_hash();
        let mut h = Sha256::new();
        h.update(journal);
        let journal_hash: [u8; 32] = h.finalize().into();
        let mut binding = JobBinding {
            job_id_hash,
            journal_hash,
            signature: Vec::new(),
        };
        let sig = self.root_sk.sign(&binding.signing_bytes());
        binding.signature = sig.to_bytes().to_vec();
        Ok(binding)
    }

    fn root_cert_der(&self) -> Vec<u8> {
        // Phase-0 "cert" = raw ed25519 public key bytes (32). A real Phase-1
        // attestor would emit a full DER cert chain to the vendor root.
        self.root_sk.verifying_key().to_bytes().to_vec()
    }
}

/// Stub verifier. Accepts any doc whose leaf "cert" (raw ed25519 pubkey bytes)
/// matches one of the configured trusted roots.
pub struct StubVerifier {
    trusted_roots: Vec<[u8; 32]>,
}

impl StubVerifier {
    pub fn new(trusted_roots: Vec<[u8; 32]>) -> Self {
        Self { trusted_roots }
    }
}

impl Verifier_ for StubVerifier {
    fn verify_attestation(
        &self,
        doc: &AttestationDoc,
        expected_mrenclave: &[u8; 32],
    ) -> Result<(), AttestError> {
        if doc.schema_version != SCHEMA_VERSION {
            return Err(AttestError::SchemaMismatch(doc.schema_version));
        }
        if doc.mrenclave != *expected_mrenclave {
            return Err(AttestError::MrenclaveMismatch {
                expected: *expected_mrenclave,
                got: doc.mrenclave,
            });
        }
        let now = now_unix();
        if now < doc.not_before {
            return Err(AttestError::NotYetValid {
                now,
                not_before: doc.not_before,
            });
        }
        if now > doc.not_after {
            return Err(AttestError::Expired {
                now,
                not_after: doc.not_after,
            });
        }
        let leaf = doc.vendor_chain.first().ok_or(AttestError::EmptyChain)?;
        if leaf.len() != 32 {
            return Err(AttestError::SigVerify("leaf pubkey not 32 bytes".into()));
        }
        let mut leaf_arr = [0u8; 32];
        leaf_arr.copy_from_slice(leaf);
        if !self.trusted_roots.contains(&leaf_arr) {
            return Err(AttestError::UntrustedRoot);
        }
        let vk = VerifyingKey::from_bytes(&leaf_arr)?;
        let sig_bytes: [u8; 64] = doc
            .signature
            .clone()
            .try_into()
            .map_err(|_| AttestError::SigVerify("signature not 64 bytes".into()))?;
        let sig = Signature::from_bytes(&sig_bytes);
        vk.verify(&doc.signing_bytes(), &sig)
            .map_err(|e| AttestError::SigVerify(e.to_string()))?;
        Ok(())
    }

    fn verify_job_binding(
        &self,
        doc: &AttestationDoc,
        binding: &JobBinding,
        req: &JobRequest,
        journal: &[u8],
    ) -> Result<(), AttestError> {
        if binding.job_id_hash != req.job_id_hash() {
            return Err(AttestError::SigVerify("job_id_hash mismatch".into()));
        }
        let mut h = Sha256::new();
        h.update(journal);
        let expected_jh: [u8; 32] = h.finalize().into();
        if binding.journal_hash != expected_jh {
            return Err(AttestError::SigVerify("journal_hash mismatch".into()));
        }
        let leaf = doc.vendor_chain.first().ok_or(AttestError::EmptyChain)?;
        let mut leaf_arr = [0u8; 32];
        leaf_arr.copy_from_slice(leaf);
        let vk = VerifyingKey::from_bytes(&leaf_arr)?;
        let sig_bytes: [u8; 64] = binding
            .signature
            .clone()
            .try_into()
            .map_err(|_| AttestError::SigVerify("binding signature not 64 bytes".into()))?;
        let sig = Signature::from_bytes(&sig_bytes);
        vk.verify(&binding.signing_bytes(), &sig)
            .map_err(|e| AttestError::SigVerify(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psychopomp_types::GuestElfRef;

    fn fake_req() -> JobRequest {
        JobRequest {
            schema_version: SCHEMA_VERSION,
            image_id: [0u32; 8],
            guest_elf: GuestElfRef::InlineBytes(vec![1, 2, 3]),
            witness_ct: vec![9; 10],
            witness_nonce: [7u8; 24],
            client_x25519_pub: [4u8; 32],
            bound_mrenclave: [0u8; 32],
            deadline_unix: 0,
            timelock: None,
        }
    }

    #[test]
    fn attestation_roundtrip() {
        let me = [42u8; 32];
        let attestor = StubAttestor::new(me);
        let pk_root: [u8; 32] = attestor.verifying_key().to_bytes();
        let verifier = StubVerifier::new(vec![pk_root]);
        let doc = attestor.produce([1u8; 32], [2u8; 32], 600).unwrap();
        verifier.verify_attestation(&doc, &me).unwrap();
    }

    #[test]
    fn rejects_wrong_mrenclave() {
        let me = [42u8; 32];
        let attestor = StubAttestor::new(me);
        let pk_root: [u8; 32] = attestor.verifying_key().to_bytes();
        let verifier = StubVerifier::new(vec![pk_root]);
        let doc = attestor.produce([1u8; 32], [2u8; 32], 600).unwrap();
        let res = verifier.verify_attestation(&doc, &[0u8; 32]);
        assert!(matches!(res, Err(AttestError::MrenclaveMismatch { .. })));
    }

    #[test]
    fn rejects_untrusted_root() {
        let me = [42u8; 32];
        let attestor = StubAttestor::new(me);
        let verifier = StubVerifier::new(vec![[7u8; 32]]); // unrelated root
        let doc = attestor.produce([1u8; 32], [2u8; 32], 600).unwrap();
        assert!(matches!(
            verifier.verify_attestation(&doc, &me),
            Err(AttestError::UntrustedRoot)
        ));
    }

    #[test]
    fn job_binding_roundtrip() {
        let me = [42u8; 32];
        let attestor = StubAttestor::new(me);
        let pk_root: [u8; 32] = attestor.verifying_key().to_bytes();
        let verifier = StubVerifier::new(vec![pk_root]);
        let doc = attestor.produce([1u8; 32], [2u8; 32], 600).unwrap();
        let req = fake_req();
        let journal = vec![0x42u8; 64];
        let binding = attestor.sign_job(&req, &journal).unwrap();
        verifier
            .verify_job_binding(&doc, &binding, &req, &journal)
            .unwrap();
    }

    #[test]
    fn rejects_tampered_journal() {
        let me = [42u8; 32];
        let attestor = StubAttestor::new(me);
        let pk_root: [u8; 32] = attestor.verifying_key().to_bytes();
        let verifier = StubVerifier::new(vec![pk_root]);
        let doc = attestor.produce([1u8; 32], [2u8; 32], 600).unwrap();
        let req = fake_req();
        let binding = attestor.sign_job(&req, &[1u8; 32]).unwrap();
        assert!(verifier
            .verify_job_binding(&doc, &binding, &req, &[2u8; 32])
            .is_err());
    }
}
