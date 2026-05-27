//! Job-table persistence. Append-only JSONL of (job_id, JobStatus) events;
//! replayed on startup to reconstruct in-memory state. One file write per
//! status transition — fine at Phase-0 throughput (single-digit jobs/min).

use psychopomp_types::JobStatus;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Event {
    job_id: Uuid,
    status: JobStatus,
}

#[derive(Clone)]
pub struct JobPersistence {
    #[allow(dead_code)]
    path: PathBuf,
    writer: std::sync::Arc<Mutex<tokio::fs::File>>,
}

impl JobPersistence {
    pub async fn open(dir: &Path) -> std::io::Result<(Self, HashMap<Uuid, JobStatus>)> {
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join("jobs.jsonl");
        // Replay
        let mut state: HashMap<Uuid, JobStatus> = HashMap::new();
        match tokio::fs::read_to_string(&path).await {
            Ok(contents) => {
                for (lineno, line) in contents.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Event>(line) {
                        Ok(ev) => {
                            state.insert(ev.job_id, ev.status);
                        }
                        Err(e) => {
                            warn!(line = lineno + 1, error = %e, "skipping malformed persistence line");
                        }
                    }
                }
                info!(jobs = state.len(), "replayed jobs.jsonl");
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Open for append
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok((
            Self {
                path,
                writer: std::sync::Arc::new(Mutex::new(file)),
            },
            state,
        ))
    }

    pub async fn record(&self, job_id: Uuid, status: &JobStatus) -> std::io::Result<()> {
        let ev = Event {
            job_id,
            status: status.clone(),
        };
        let mut line = serde_json::to_vec(&ev)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push(b'\n');
        let mut w = self.writer.lock().await;
        w.write_all(&line).await?;
        w.flush().await?;
        Ok(())
    }

    /// Rewrite `jobs.jsonl` to contain exactly one line per live job,
    /// dropping all historical events. Called periodically by the operator
    /// when the file exceeds a size cap.
    pub async fn compact(&self, live: &HashMap<Uuid, JobStatus>) -> std::io::Result<()> {
        let mut new_path = self.path.clone();
        new_path.set_extension("jsonl.tmp");
        let mut buf = Vec::with_capacity(live.len() * 256);
        for (job_id, status) in live {
            let ev = Event { job_id: *job_id, status: status.clone() };
            let bytes = serde_json::to_vec(&ev)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            buf.extend_from_slice(&bytes);
            buf.push(b'\n');
        }
        tokio::fs::write(&new_path, &buf).await?;
        // Swap the writer to a new file handle pointing at the rewritten log.
        let mut w = self.writer.lock().await;
        tokio::fs::rename(&new_path, &self.path).await?;
        *w = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        info!(jobs = live.len(), "compacted jobs.jsonl");
        Ok(())
    }

    /// Returns the on-disk size of the log in bytes.
    pub async fn size(&self) -> std::io::Result<u64> {
        Ok(tokio::fs::metadata(&self.path).await?.len())
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
