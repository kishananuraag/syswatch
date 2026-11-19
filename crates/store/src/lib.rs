//! Storage trait and cascade implementation.
//!
//! The `Store` trait hides the storage format so it can change without
//! touching anything else. The cascade implementation keeps 1 s → 15 s →
//! 60 s → 300 s → 1800 s → archive rollups.

use syswatch_core::{MetricDef, SensorError};
use serde::{Deserialize, Serialize};

// ── Rollup resolution ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    Raw1s,
    Tier15s,
    Tier60s,
    Tier300s,
    Tier1800s,
    Archive,
}

impl Resolution {
    pub fn interval_ms(&self) -> u64 {
        match self {
            Self::Raw1s => 1_000,
            Self::Tier15s => 15_000,
            Self::Tier60s => 60_000,
            Self::Tier300s => 300_000,
            Self::Tier1800s => 1_800_000,
            Self::Archive => 86_400_000,
        }
    }

    pub fn retention_hours(&self) -> u64 {
        match self {
            Self::Raw1s => 1,
            Self::Tier15s => 24,
            Self::Tier60s => 24 * 7,
            Self::Tier300s => 24 * 30,
            Self::Tier1800s => 24 * 365,
            Self::Archive => u64::MAX,
        }
    }
}

// ── Rollup bucket ────────────────────────────────────────────────────────────

/// One rollup bucket. Stores min/max/avg/count — never avg alone.
/// A missing bucket (count = 0) is a gap, never interpolated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupBucket {
    pub metric_id: String,
    pub resolution: Resolution,
    pub bucket_ts: u64, // unix ms, aligned to resolution boundary
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub count: u32,
}

impl RollupBucket {
    pub fn avg(&self) -> Option<f64> {
        if self.count == 0 {
            None
        } else {
            Some(self.sum / self.count as f64)
        }
    }

    pub fn is_gap(&self) -> bool {
        self.count == 0
    }
}

// ── Store trait ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// The Store trait hides the storage format.
/// Implementations: CascadeStore (JSONL + tiers), SqliteStore, etc.
pub trait Store: Send + Sync {
    /// Insert a raw sample for a metric.
    fn insert_raw(&self, metric_id: &str, ts: u64, value: f64) -> StoreResult<()>;

    /// Query a series for a metric over a time range.
    /// Returns rollup buckets at the appropriate resolution.
    /// Gaps (count=0) are included as explicit gaps.
    fn query_series(
        &self,
        metric_id: &str,
        from: u64,
        to: u64,
        max_points: usize,
    ) -> StoreResult<Vec<RollupBucket>>;

    /// Run compaction: demote old buckets to lower resolutions.
    fn compact(&self) -> StoreResult<CompactionStats>;

    /// Total disk usage in bytes.
    fn disk_usage(&self) -> StoreResult<u64>;

    /// Register a metric in the store's registry.
    fn register_metric(&self, def: &MetricDef) -> StoreResult<()>;
}

#[derive(Debug, Default)]
pub struct CompactionStats {
    pub buckets_demoted: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

// ── Cascade store (skeleton) ──────────────────────────────────────────────────

/// Cascade store: 1 s → 15 s → 60 s → 300 s → 1800 s → archive.
/// This is the skeleton — the actual file I/O will be implemented in T23.
pub struct CascadeStore {
    base_dir: std::path::PathBuf,
}

impl CascadeStore {
    pub fn new(base_dir: std::path::PathBuf) -> Self {
        Self { base_dir }
    }
}

impl Store for CascadeStore {
    fn insert_raw(&self, _metric_id: &str, _ts: u64, _value: f64) -> StoreResult<()> {
        // TODO: implement file I/O
        Ok(())
    }

    fn query_series(
        &self,
        _metric_id: &str,
        _from: u64,
        _to: u64,
        _max_points: usize,
    ) -> StoreResult<Vec<RollupBucket>> {
        // TODO: implement query
        Ok(vec![])
    }

    fn compact(&self) -> StoreResult<CompactionStats> {
        // TODO: implement compaction
        Ok(CompactionStats::default())
    }

    fn disk_usage(&self) -> StoreResult<u64> {
        // TODO: implement
        Ok(0)
    }

    fn register_metric(&self, _def: &MetricDef) -> StoreResult<()> {
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_bucket_avg() {
        let b = RollupBucket {
            metric_id: "cpu.total".into(),
            resolution: Resolution::Raw1s,
            bucket_ts: 1000,
            min: 10.0,
            max: 20.0,
            sum: 30.0,
            count: 2,
        };
        assert_eq!(b.avg(), Some(15.0));
        assert!(!b.is_gap());
    }

    #[test]
    fn rollup_bucket_gap() {
        let b = RollupBucket {
            metric_id: "cpu.total".into(),
            resolution: Resolution::Raw1s,
            bucket_ts: 1000,
            min: 0.0,
            max: 0.0,
            sum: 0.0,
            count: 0,
        };
        assert_eq!(b.avg(), None);
        assert!(b.is_gap());
    }

    #[test]
    fn resolution_intervals() {
        assert_eq!(Resolution::Raw1s.interval_ms(), 1_000);
        assert_eq!(Resolution::Tier15s.interval_ms(), 15_000);
        assert_eq!(Resolution::Tier1800s.interval_ms(), 1_800_000);
    }
}
