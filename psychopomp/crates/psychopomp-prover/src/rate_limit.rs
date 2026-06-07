//! Per-client token-bucket rate limiter.
//!
//! Keyed by the client's X25519 pubkey (which is bound to the AEAD they
//! encrypted the witness under - operators see it on every job submission).
//! For the precommit/reveal flow we key on the same pubkey from
//! `JobPrecommit::client_x25519_pub`. The cap is policy-configurable.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct Bucket {
    pub tokens: f64,
    pub last_refill: Instant,
}

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<[u8; 32], Bucket>>>,
    pub max_tokens: f64,
    pub refill_per_sec: f64,
}

impl RateLimiter {
    /// `max_per_minute = 0` disables limiting.
    pub fn new(max_per_minute: u32) -> Self {
        let cap = max_per_minute as f64;
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_tokens: cap,
            refill_per_sec: if max_per_minute == 0 { 0.0 } else { cap / 60.0 },
        }
    }

    pub fn disabled(&self) -> bool {
        self.refill_per_sec == 0.0
    }

    /// Try to take one token. Returns Err(retry_after_seconds) on cap.
    pub async fn check(&self, key: [u8; 32]) -> Result<(), f64> {
        if self.disabled() {
            return Ok(());
        }
        let now = Instant::now();
        let mut map = self.inner.lock().await;
        let b = map.entry(key).or_insert(Bucket {
            tokens: self.max_tokens,
            last_refill: now,
        });
        let elapsed = now.duration_since(b.last_refill).as_secs_f64();
        b.tokens = (b.tokens + elapsed * self.refill_per_sec).min(self.max_tokens);
        b.last_refill = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            let need = 1.0 - b.tokens;
            Err(need / self.refill_per_sec)
        }
    }

    /// GC: drop buckets that haven't been touched in `idle`.
    #[allow(dead_code)]
    pub async fn evict_idle(&self, idle: Duration) {
        let now = Instant::now();
        let mut map = self.inner.lock().await;
        map.retain(|_, b| now.duration_since(b.last_refill) < idle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_allows_all() {
        let r = RateLimiter::new(0);
        for _ in 0..1000 {
            r.check([0u8; 32]).await.unwrap();
        }
    }

    #[tokio::test]
    async fn caps_at_max() {
        let r = RateLimiter::new(6); // 6/min => 0.1/sec refill, 6-token burst
        for _ in 0..6 {
            r.check([1u8; 32]).await.unwrap();
        }
        assert!(r.check([1u8; 32]).await.is_err());
    }

    #[tokio::test]
    async fn keys_are_independent() {
        let r = RateLimiter::new(3);
        for _ in 0..3 { r.check([1u8; 32]).await.unwrap(); }
        for _ in 0..3 { r.check([2u8; 32]).await.unwrap(); }
        assert!(r.check([1u8; 32]).await.is_err());
        assert!(r.check([2u8; 32]).await.is_err());
    }
}
