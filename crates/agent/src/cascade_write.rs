//! Optional disk cascade for the agent's own samples.
//!
//! The agent is read-only against the collector's log by design: the collector
//! owns `logs/YYYY-MM-DD.jsonl`, and two writers on one file is how a history
//! gets corrupted. When the agent is started with `--cascade-dir DIR` (T18), the
//! same sampler tick is *also* written through [`syswatch_store::CascadeStore`]
//! into `DIR/raw`, `DIR/tier15s`, … `DIR/archive`, and the hourly compaction
//! pass rides on the same tick. Without the flag the agent writes nothing, so a
//! plain `syswatch-agent` still touches no file.
//!
//! Metric ids are the ones `/api/series?metric=` already accepts, so a value
//! written here can be read straight back through the API.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use syswatch_store::{CascadeStore, Store};

use crate::series;
use crate::timefmt;

/// Scalar metrics recorded per tick. Every id resolves through
/// [`series::extract`], the same translator `/api/series` uses, against the
/// keys the sampler actually emits (`cpu.total_pct`, `memory.pct`,
/// `proc_count`, `load.one`).
const METRICS: &[&str] = &["cpu.total", "memory.pct", "proc_count", "load.one"];

pub struct CascadeWriter {
    store: Arc<CascadeStore>,
    written: u64,
    failed: u64,
}

impl CascadeWriter {
    pub fn new(dir: PathBuf) -> Self {
        let store = Arc::new(CascadeStore::new(dir));
        // Startup compaction pass (T18 §3). Idempotent on a tree that is
        // already compacted, so a restart costs nothing.
        let _ = store.compact_if_due();
        Self {
            store,
            written: 0,
            failed: 0,
        }
    }

    /// The store behind the writer — used by the behaviour test to read the
    /// tick back through the same query path `/api/series` uses.
    #[cfg(test)]
    pub fn store(&self) -> &Arc<CascadeStore> {
        &self.store
    }

    /// Record one snapshot. A failed write is counted, never propagated: the
    /// monitor must not die because a disk filled up.
    pub fn record(&mut self, snap: &Value) {
        let ts_ms = snap
            .get("ts")
            .and_then(Value::as_str)
            .and_then(timefmt::parse_rfc3339)
            .map(|secs| (secs * 1000.0) as u64)
            .unwrap_or_else(|| (timefmt::now_unix() as u64) * 1000);

        for metric in METRICS {
            if let Some(v) = series::extract(metric, snap) {
                self.write(metric, ts_ms, v);
            }
        }

        if let Some(cores) = snap
            .get("cpu")
            .and_then(|c| c.get("per_core_pct"))
            .and_then(Value::as_array)
        {
            for (i, v) in cores.iter().enumerate() {
                if let Some(v) = v.as_f64() {
                    self.write(&format!("cpu.core.{i}"), ts_ms, v);
                }
            }
        }

        // Hourly compaction rides on the sampler tick; the store returns early
        // on an atomic load when the interval has not elapsed.
        let _ = self.store.compact_if_due();
    }

    fn write(&mut self, metric: &str, ts_ms: u64, value: f64) {
        match self.store.insert_raw(metric, ts_ms, value) {
            Ok(()) => self.written += 1,
            Err(_) => self.failed += 1,
        }
    }

    #[cfg(test)]
    pub fn written(&self) -> u64 {
        self.written
    }

    #[cfg(test)]
    pub fn failed(&self) -> u64 {
        self.failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use syswatch_store::Resolution;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "syswatch-agent-cascade-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn one_tick_lands_in_the_raw_tier_and_reads_back() {
        let dir = tmp();
        let mut w = CascadeWriter::new(dir.clone());
        let snap = json!({
            "ts": "2027-01-15T08:00:00Z",
            "cpu": {"total_pct": 42.5, "per_core_pct": [40.0, 45.0]},
            "ram": {"pct": 71.0},
            "memory": {"pct": 71.0},
            "proc_count": 512,
            "load": {"one": 1.75, "five": 1.5, "fifteen": 1.25},
        });
        w.record(&snap);
        assert_eq!(w.failed(), 0, "a write failed");
        assert_eq!(w.written(), 6, "4 scalars + 2 cores");

        let buckets = w
            .store()
            .query_series("cpu.total", 1_800_000_000_000, 1_800_000_001_000, 10)
            .unwrap();
        assert_eq!(buckets.len(), 1, "{buckets:?}");
        assert_eq!(buckets[0].resolution, Resolution::Raw1s);
        assert!((buckets[0].max - 42.5).abs() < 1e-9);

        let core = w
            .store()
            .query_series("cpu.core.1", 1_800_000_000_000, 1_800_000_001_000, 10)
            .unwrap();
        assert_eq!(core.len(), 1, "per-core sample missing");
        assert!((core[0].max - 45.0).abs() < 1e-9);
        assert!(w.written() >= 5, "wrote {} samples", w.written());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
