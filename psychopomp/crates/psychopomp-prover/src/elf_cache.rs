//! On-disk ELF cache, keyed by RISC0 image_id (32-byte hex). The operator
//! stores ELFs under `<state_dir>/elf/<hex>.elf`; clients probe via
//! `HEAD /v0/elf/{hex}` and upload via `POST /v0/elf/{hex}`.
//!
//! Verification: on upload we recompute the image_id from the ELF and reject
//! mismatches - the operator never trusts the client's claim that "this is
//! image_id X."

use risc0_zkvm::compute_image_id;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

#[derive(Clone)]
pub struct ElfCache {
    inner: Arc<Inner>,
}

struct Inner {
    dir: PathBuf,
    /// Coarse-grained lock for compute_image_id during upload. The actual
    /// on-disk reads/writes are atomic per-file.
    write_lock: RwLock<()>,
}

impl ElfCache {
    pub async fn new(dir: PathBuf) -> std::io::Result<Self> {
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                dir,
                write_lock: RwLock::new(()),
            }),
        })
    }

    pub fn path_for(&self, image_id_hex: &str) -> PathBuf {
        self.inner.dir.join(format!("{image_id_hex}.elf"))
    }

    pub async fn contains(&self, image_id_hex: &str) -> bool {
        tokio::fs::metadata(self.path_for(image_id_hex)).await.is_ok()
    }

    pub async fn get(&self, image_id: &[u32; 8]) -> std::io::Result<Vec<u8>> {
        let hex = psychopomp_types::image_id_hex(image_id);
        tokio::fs::read(self.path_for(&hex)).await
    }

    /// Inserts `bytes` if and only if its computed image_id equals
    /// `expected_image_id`. Returns `Err` on mismatch.
    pub async fn insert_verified(
        &self,
        expected_image_id: &[u32; 8],
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        let computed = tokio::task::spawn_blocking(move || {
            compute_image_id(&bytes).map(|d| (d, bytes))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
        .map_err(|e| format!("compute_image_id: {e}"))?;
        let (digest, bytes) = computed;
        let words = digest_to_words(&digest);
        if words != *expected_image_id {
            return Err(format!(
                "image_id mismatch: expected {}, computed {}",
                psychopomp_types::image_id_hex(expected_image_id),
                psychopomp_types::image_id_hex(&words),
            ));
        }
        let hex = psychopomp_types::image_id_hex(expected_image_id);
        let path = self.path_for(&hex);
        let _g = self.inner.write_lock.write().await;
        // Write to a tempfile then rename for atomicity.
        let tmp = path.with_extension("elf.tmp");
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(|e| format!("write tmp: {e}"))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| format!("rename: {e}"))?;
        info!(image_id = %hex, bytes = bytes.len(), "cached ELF");
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }
}

fn digest_to_words(d: &risc0_zkvm::sha::Digest) -> [u32; 8] {
    let bytes = d.as_bytes();
    let mut out = [0u32; 8];
    for (i, w) in out.iter_mut().enumerate() {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&bytes[i * 4..(i + 1) * 4]);
        *w = u32::from_le_bytes(buf);
    }
    out
}
