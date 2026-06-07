//! Prometheus-text-format metrics for `/v0/metrics`. Tiny by design - one
//! `AtomicU64` per counter. Mirrors what the README §"Reputation" section
//! says belongs on-chain in Phase-1, but exposed locally for now.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct Metrics {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    pub attestations_served: AtomicU64,
    pub jobs_accepted: AtomicU64,
    pub jobs_started: AtomicU64,
    pub jobs_completed: AtomicU64,
    pub jobs_failed: AtomicU64,
    pub jobs_rejected: AtomicU64,
    pub elf_uploads: AtomicU64,
    pub elf_lookups_hit: AtomicU64,
    pub elf_lookups_miss: AtomicU64,
    pub total_wall_clock_ms: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attestation(&self) {
        self.inner.attestations_served.fetch_add(1, Ordering::Relaxed);
    }
    pub fn accepted(&self) {
        self.inner.jobs_accepted.fetch_add(1, Ordering::Relaxed);
    }
    pub fn started(&self) {
        self.inner.jobs_started.fetch_add(1, Ordering::Relaxed);
    }
    pub fn completed(&self, wall_ms: u64) {
        self.inner.jobs_completed.fetch_add(1, Ordering::Relaxed);
        self.inner
            .total_wall_clock_ms
            .fetch_add(wall_ms, Ordering::Relaxed);
    }
    pub fn failed(&self) {
        self.inner.jobs_failed.fetch_add(1, Ordering::Relaxed);
    }
    pub fn rejected(&self) {
        self.inner.jobs_rejected.fetch_add(1, Ordering::Relaxed);
    }
    pub fn elf_uploaded(&self) {
        self.inner.elf_uploads.fetch_add(1, Ordering::Relaxed);
    }
    pub fn elf_hit(&self) {
        self.inner.elf_lookups_hit.fetch_add(1, Ordering::Relaxed);
    }
    pub fn elf_miss(&self) {
        self.inner.elf_lookups_miss.fetch_add(1, Ordering::Relaxed);
    }

    pub fn avg_completed_wall_clock_ms(&self) -> u64 {
        let n = self.inner.jobs_completed.load(Ordering::Relaxed);
        if n == 0 {
            return 0;
        }
        self.inner.total_wall_clock_ms.load(Ordering::Relaxed) / n
    }

    pub fn render(&self) -> String {
        let i = &self.inner;
        let mut s = String::with_capacity(512);
        for (name, help, val) in [
            ("psychopomp_attestations_served_total", "Attestation docs served.", &i.attestations_served),
            ("psychopomp_jobs_accepted_total", "Jobs accepted into queue.", &i.jobs_accepted),
            ("psychopomp_jobs_started_total", "Jobs that began proving.", &i.jobs_started),
            ("psychopomp_jobs_completed_total", "Jobs that returned a verified receipt.", &i.jobs_completed),
            ("psychopomp_jobs_failed_total", "Jobs that errored during proving.", &i.jobs_failed),
            ("psychopomp_jobs_rejected_total", "Jobs rejected at submission (policy / schema / deadline).", &i.jobs_rejected),
            ("psychopomp_elf_uploads_total", "ELFs uploaded to cache.", &i.elf_uploads),
            ("psychopomp_elf_lookups_hit_total", "ELF cache hits at job time.", &i.elf_lookups_hit),
            ("psychopomp_elf_lookups_miss_total", "ELF cache misses at job time.", &i.elf_lookups_miss),
            ("psychopomp_total_wall_clock_ms", "Cumulative prove wall-clock (ms).", &i.total_wall_clock_ms),
        ] {
            s.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n{name} {}\n", val.load(Ordering::Relaxed)));
        }
        s
    }
}
