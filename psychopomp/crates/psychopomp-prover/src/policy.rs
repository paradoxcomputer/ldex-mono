//! Operator-side admission policy. Loaded from TOML at startup; defaults are
//! "allow everything" (Phase-0 dev posture). Real operators will tighten:
//! restrict image_ids to ones they've inspected, cap session_limit so a
//! pathological guest can't tie up the GPU forever, cap inline ELF size to
//! force big guests through the upload-then-Cached path.

use psychopomp_types::JobRequest;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Policy {
    /// If non-empty, only these image_ids are accepted (hex 32-byte strings).
    #[serde(default)]
    pub allowed_image_ids: HashSet<String>,
    /// Reject JobRequests whose `session_limit` exceeds this. None = no cap.
    #[serde(default)]
    pub max_session_limit: Option<u64>,
    /// Reject JobRequests whose inline ELF exceeds this many bytes. Forces
    /// big guests to go through the cache. None = no cap.
    #[serde(default)]
    pub max_inline_elf_bytes: Option<usize>,
    /// Reject witness ciphertexts larger than this. None = no cap.
    #[serde(default)]
    pub max_witness_ct_bytes: Option<usize>,
    /// If non-empty, POST /v0/elf requires `Authorization: Bearer <token>`
    /// matching one of these. Empty = unauthenticated uploads allowed
    /// (Phase-0 dev posture).
    #[serde(default)]
    pub upload_bearer_tokens: HashSet<String>,
    /// Per-client job-submission rate cap, keyed by the client's X25519
    /// pubkey. 0 = unlimited.
    #[serde(default)]
    pub max_jobs_per_minute_per_client: u32,
}

impl Policy {
    /// Check whether an inbound POST /v0/elf is authorized. The `header_value`
    /// is the raw `Authorization` header value (e.g. `"Bearer abc123"`).
    pub fn check_upload_auth(&self, header_value: Option<&str>) -> Result<(), String> {
        if self.upload_bearer_tokens.is_empty() {
            return Ok(());
        }
        let h = header_value.ok_or_else(|| "missing Authorization header".to_string())?;
        let tok = h
            .strip_prefix("Bearer ")
            .ok_or_else(|| "Authorization must be 'Bearer <token>'".to_string())?;
        if self.upload_bearer_tokens.contains(tok) {
            Ok(())
        } else {
            Err("token not in upload allowlist".to_string())
        }
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let s = std::fs::read_to_string(path)?;
        toml::from_str(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }

    pub fn check(&self, req: &JobRequest) -> Result<(), String> {
        if !self.allowed_image_ids.is_empty() {
            let hex = psychopomp_types::image_id_hex(&req.image_id);
            if !self.allowed_image_ids.contains(&hex) {
                return Err(format!("image_id {hex} not in operator allowlist"));
            }
        }
        if let Some(max) = self.max_witness_ct_bytes {
            if req.witness_ct.len() > max {
                return Err(format!(
                    "witness_ct {} bytes > max {max}",
                    req.witness_ct.len()
                ));
            }
        }
        if let psychopomp_types::GuestElfRef::InlineBytes(b) = &req.guest_elf {
            if let Some(max) = self.max_inline_elf_bytes {
                if b.len() > max {
                    return Err(format!("inline ELF {} bytes > max {max}", b.len()));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psychopomp_types::{GuestElfRef, SCHEMA_VERSION};

    fn fake_req() -> JobRequest {
        JobRequest {
            schema_version: SCHEMA_VERSION,
            image_id: [0u32; 8],
            guest_elf: GuestElfRef::InlineBytes(vec![0u8; 10]),
            witness_ct: vec![0u8; 100],
            witness_nonce: [0u8; 24],
            client_x25519_pub: [0u8; 32],
            bound_mrenclave: [0u8; 32],
            deadline_unix: 0,
            timelock: None,
        }
    }

    #[test]
    fn default_allows_all() {
        Policy::default().check(&fake_req()).unwrap();
    }

    #[test]
    fn rejects_unlisted_image_id() {
        let mut p = Policy::default();
        p.allowed_image_ids.insert(
            "1111111111111111111111111111111111111111111111111111111111111111".into(),
        );
        assert!(p.check(&fake_req()).is_err());
    }

    #[test]
    fn rejects_oversized_witness() {
        let p = Policy { max_witness_ct_bytes: Some(10), ..Default::default() };
        assert!(p.check(&fake_req()).is_err());
    }

    #[test]
    fn rejects_oversized_inline_elf() {
        let p = Policy { max_inline_elf_bytes: Some(5), ..Default::default() };
        assert!(p.check(&fake_req()).is_err());
    }

    #[test]
    fn upload_auth_disabled_when_empty() {
        let p = Policy::default();
        assert!(p.check_upload_auth(None).is_ok());
    }

    #[test]
    fn upload_auth_requires_bearer_when_configured() {
        let mut tokens = HashSet::new();
        tokens.insert("s3cr3t".to_string());
        let p = Policy { upload_bearer_tokens: tokens, ..Default::default() };
        assert!(p.check_upload_auth(None).is_err());
        assert!(p.check_upload_auth(Some("Basic abc")).is_err());
        assert!(p.check_upload_auth(Some("Bearer wrong")).is_err());
        assert!(p.check_upload_auth(Some("Bearer s3cr3t")).is_ok());
    }
}
