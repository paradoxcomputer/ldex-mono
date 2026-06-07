//! Local reputation ledger. Persisted to disk as JSON; updated on every
//! `prove*` outcome. Used by `prove_multi` to rank routes by score.
//!
//! Score = α·success_rate − β·log10(avg_latency_ms+1) − γ·fail_rate
//! defaults α=10, β=2, γ=5 - tuned so a route with 0 history (score = 0)
//! beats a route with a 100% fail rate but loses to one with successes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EndpointStats {
    pub successes: u64,
    pub failures: u64,
    pub total_latency_ms: u128,
    /// Unix seconds of the most recent counter update. Used for exponential
    /// decay so stale stats drift toward neutral (a bad week doesn't haunt
    /// an operator forever).
    #[serde(default)]
    pub last_updated_unix: u64,
}

impl EndpointStats {
    pub fn n(&self) -> u64 { self.successes + self.failures }
    pub fn success_rate(&self) -> f64 {
        let n = self.n();
        if n == 0 { 0.0 } else { self.successes as f64 / n as f64 }
    }
    pub fn fail_rate(&self) -> f64 { 1.0 - self.success_rate() }
    pub fn avg_latency_ms(&self) -> f64 {
        if self.successes == 0 { 0.0 } else { self.total_latency_ms as f64 / self.successes as f64 }
    }

    pub fn score(&self) -> f64 {
        self.score_decayed(0, f64::INFINITY)
    }

    /// Score with optional exponential decay: each counter is multiplied by
    /// `exp(-Δt / half_life_secs * ln(2))` where Δt is now − last_updated.
    /// Pass `now_unix = 0` or `half_life_secs = f64::INFINITY` to skip decay.
    pub fn score_decayed(&self, now_unix: u64, half_life_secs: f64) -> f64 {
        const A: f64 = 10.0;
        const B: f64 = 2.0;
        const G: f64 = 5.0;
        let n = self.n();
        if n == 0 { return 0.0; }
        let decay = if now_unix == 0 || !half_life_secs.is_finite() || self.last_updated_unix == 0 {
            1.0
        } else {
            let dt = now_unix.saturating_sub(self.last_updated_unix) as f64;
            (-dt / half_life_secs * std::f64::consts::LN_2).exp()
        };
        let s = self.success_rate();
        let f = self.fail_rate();
        let l = (self.avg_latency_ms() + 1.0).log10();
        decay * (A * s - B * l - G * f)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LedgerFile {
    pub schema_version: u16,
    pub stats: HashMap<String, EndpointStats>,
}

#[derive(Clone)]
pub struct ReputationLedger {
    path: Option<PathBuf>,
    inner: Arc<Mutex<LedgerFile>>,
    /// Half-life in seconds for exponential score decay. `f64::INFINITY` to
    /// disable (default for `ephemeral`). 7 days for `open()` default.
    pub half_life_secs: f64,
}

impl ReputationLedger {
    /// In-memory ledger; never persisted.
    pub fn ephemeral() -> Self {
        Self {
            path: None,
            inner: Arc::new(Mutex::new(LedgerFile {
                schema_version: psychopomp_types::SCHEMA_VERSION,
                stats: HashMap::new(),
            })),
            half_life_secs: f64::INFINITY,
        }
    }

    pub async fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = match tokio::fs::read(&path).await {
            Ok(b) => serde_json::from_slice::<LedgerFile>(&b)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => LedgerFile {
                schema_version: psychopomp_types::SCHEMA_VERSION,
                stats: HashMap::new(),
            },
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: Some(path),
            inner: Arc::new(Mutex::new(file)),
            half_life_secs: 7.0 * 24.0 * 3600.0, // 7 days
        })
    }

    pub async fn record_success(&self, endpoint: &str, latency_ms: u64) {
        let mut g = self.inner.lock().await;
        let s = g.stats.entry(endpoint.to_string()).or_default();
        s.successes = s.successes.saturating_add(1);
        s.total_latency_ms = s.total_latency_ms.saturating_add(latency_ms as u128);
        s.last_updated_unix = now_unix();
        drop(g);
        let _ = self.flush().await;
    }

    pub async fn record_failure(&self, endpoint: &str) {
        let mut g = self.inner.lock().await;
        let s = g.stats.entry(endpoint.to_string()).or_default();
        s.failures = s.failures.saturating_add(1);
        s.last_updated_unix = now_unix();
        drop(g);
        let _ = self.flush().await;
    }

    pub async fn score(&self, endpoint: &str) -> f64 {
        let now = now_unix();
        self.inner
            .lock()
            .await
            .stats
            .get(endpoint)
            .map(|s| s.score_decayed(now, self.half_life_secs))
            .unwrap_or(0.0)
    }

    pub async fn rank<T>(&self, items: &[T], key: impl Fn(&T) -> &str) -> Vec<usize> {
        let g = self.inner.lock().await;
        let now = now_unix();
        let mut idx: Vec<usize> = (0..items.len()).collect();
        idx.sort_by(|a, b| {
            let sa = g.stats.get(key(&items[*a])).map(|s| s.score_decayed(now, self.half_life_secs)).unwrap_or(0.0);
            let sb = g.stats.get(key(&items[*b])).map(|s| s.score_decayed(now, self.half_life_secs)).unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        idx
    }

    async fn flush(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()); };
        let g = self.inner.lock().await;
        let bytes = serde_json::to_vec_pretty(&*g)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    pub async fn snapshot(&self) -> LedgerFile {
        self.inner.lock().await.clone()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn higher_success_outranks() {
        let l = ReputationLedger::ephemeral();
        l.record_success("a", 100).await;
        l.record_success("a", 100).await;
        l.record_failure("b").await;
        l.record_failure("b").await;
        let endpoints = vec!["a".to_string(), "b".to_string()];
        let order = l.rank(&endpoints, |s| s.as_str()).await;
        assert_eq!(order[0], 0, "a (2/2 success) must outrank b (0/2)");
    }

    #[tokio::test]
    async fn persists_across_open() {
        let dir = tempdir::TempDir::new("psy-rep").unwrap();
        let path = dir.path().join("rep.json");
        {
            let l = ReputationLedger::open(path.clone()).await.unwrap();
            l.record_success("a", 500).await;
        }
        let l = ReputationLedger::open(path).await.unwrap();
        let snap = l.snapshot().await;
        let s = snap.stats.get("a").unwrap();
        assert_eq!(s.successes, 1);
        assert_eq!(s.total_latency_ms, 500);
    }

    #[test]
    fn zero_history_score_is_zero() {
        assert_eq!(EndpointStats::default().score(), 0.0);
    }

    #[test]
    fn all_failure_score_is_negative() {
        let s = EndpointStats { successes: 0, failures: 5, total_latency_ms: 0, last_updated_unix: 0 };
        assert!(s.score() < 0.0);
    }

    #[test]
    fn decay_attenuates_old_stats() {
        let s = EndpointStats {
            successes: 10,
            failures: 0,
            total_latency_ms: 1000,
            last_updated_unix: 100,
        };
        let fresh = s.score_decayed(100, 3600.0);
        // 5 half-lives later
        let stale = s.score_decayed(100 + 5 * 3600, 3600.0);
        assert!(fresh > stale, "fresh ({fresh}) must outrank stale ({stale})");
        // After 5 half-lives, attenuation factor is 2^-5 = 0.03125
        assert!((stale / fresh - 0.03125).abs() < 0.001);
    }
}
