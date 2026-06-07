//! Operator discovery. Phase-1-off-chain: read a JSON registry file. The
//! same API will back the on-chain `psychopomp-registry` query in Phase-1.
//!
//! Use it like:
//! ```ignore
//! let ops = psychopomp_client::discovery::discover(
//!     &psychopomp_client::discovery::Source::File("registry.json".into()),
//! ).await?;
//! let cfg = ops[0].to_client_config(Duration::from_secs(60));
//! ```

use psychopomp_types::HwClass;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

use crate::ClientConfig;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperatorRecord {
    pub endpoint: String,
    #[serde(with = "psychopomp_types::hex_bytes::array32")]
    pub mrenclave: [u8; 32],
    #[serde(with = "psychopomp_types::hex_bytes::array32")]
    pub attestation_root: [u8; 32],
    pub hw_class: HwClass,
    /// Optional public-facing label used in `prove_multi` logs.
    #[serde(default)]
    pub label: Option<String>,
}

impl OperatorRecord {
    pub fn to_client_config(&self, deadline: Duration) -> ClientConfig {
        ClientConfig {
            endpoint: self.endpoint.clone(),
            expected_mrenclave: self.mrenclave,
            trusted_roots: vec![self.attestation_root],
            deadline,
            poll_interval: Duration::from_millis(750),
            upload_bearer: None,
            accept_invalid_tls: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryFile {
    pub schema_version: u16,
    pub operators: Vec<OperatorRecord>,
}

#[derive(Clone, Debug)]
pub enum Source {
    /// Local JSON file. Standard for Phase-1-off-chain.
    File(PathBuf),
    /// On-chain registry query. Not yet implemented - Phase-1 will hook it
    /// up via an LEZ RPC query against the deployed registry program.
    Chain { rpc_endpoint: String, program_id: [u32; 8] },
}

#[derive(thiserror::Error, Debug)]
pub enum DiscoveryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("schema mismatch: got {0}")]
    Schema(u16),
    #[error("chain source not yet implemented")]
    ChainNotImplemented,
}

pub async fn discover(source: &Source) -> Result<Vec<OperatorRecord>, DiscoveryError> {
    match source {
        Source::File(path) => {
            let bytes = tokio::fs::read(path).await?;
            let reg: RegistryFile = serde_json::from_slice(&bytes)?;
            if reg.schema_version != psychopomp_types::SCHEMA_VERSION {
                return Err(DiscoveryError::Schema(reg.schema_version));
            }
            Ok(reg.operators)
        }
        Source::Chain { .. } => Err(DiscoveryError::ChainNotImplemented),
    }
}

/// Discover, then keep only operators whose `hw_class` is in `accepted` and
/// whose `mrenclave` is in `allowed_mrenclaves` (empty = no filter).
pub fn filter(
    operators: Vec<OperatorRecord>,
    accepted_hw: &[HwClass],
    allowed_mrenclaves: &[[u8; 32]],
) -> Vec<OperatorRecord> {
    operators
        .into_iter()
        .filter(|o| accepted_hw.is_empty() || accepted_hw.contains(&o.hw_class))
        .filter(|o| allowed_mrenclaves.is_empty() || allowed_mrenclaves.contains(&o.mrenclave))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(label: &str, hw: HwClass) -> OperatorRecord {
        OperatorRecord {
            endpoint: format!("http://{label}:8088"),
            mrenclave: [label.as_bytes()[0]; 32],
            attestation_root: [label.as_bytes()[0]; 32],
            hw_class: hw,
            label: Some(label.into()),
        }
    }

    #[tokio::test]
    async fn discover_from_file() {
        let dir = tempdir::TempDir::new("psy-disc").unwrap();
        let path = dir.path().join("registry.json");
        let reg = RegistryFile {
            schema_version: psychopomp_types::SCHEMA_VERSION,
            operators: vec![rec("a", HwClass::H100CC), rec("b", HwClass::MI300SEV)],
        };
        tokio::fs::write(&path, serde_json::to_vec(&reg).unwrap()).await.unwrap();
        let got = discover(&Source::File(path)).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].endpoint, "http://a:8088");
    }

    #[test]
    fn filter_by_hw_class() {
        let ops = vec![rec("a", HwClass::H100CC), rec("b", HwClass::MI300SEV), rec("c", HwClass::H100CC)];
        let only_h100 = filter(ops, &[HwClass::H100CC], &[]);
        assert_eq!(only_h100.len(), 2);
    }

    #[test]
    fn filter_by_mrenclave() {
        let a = rec("a", HwClass::H100CC);
        let want = a.mrenclave;
        let ops = vec![a, rec("b", HwClass::MI300SEV)];
        let filtered = filter(ops, &[], &[want]);
        assert_eq!(filtered.len(), 1);
    }
}
