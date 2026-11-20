//! Storage trait and the disk-side down-sampling cascade.
//!
//! The `Store` trait hides the storage format so it can change without
//! touching anything else. [`CascadeStore`] keeps the collector's cascade on
//! disk, one directory per resolution:
//!
//! ```text
//!   <base>/raw/YYYY-MM-DD.jsonl       1 s buckets, kept 6 h   (rolling buffer)
//!   <base>/tier15s/YYYY-MM-DD.jsonl  15 s buckets, kept 6 h -> 1 d
//!   <base>/tier60s/YYYY-MM-DD.jsonl  60 s buckets, kept 1 d -> 7 d
//!   <base>/tier300s/YYYY-MM-DD.jsonl 300 s buckets, kept 7 d -> 30 d
//!   <base>/tier1800s/YYYY-MM-DD.jsonl 1800 s buckets, kept 30 d -> 1 y
//!   <base>/archive/YYYY-MM-DD.jsonl  1 bucket/day, kept forever
//! ```
//!
//! The windows live in ONE table ([`CASCADE`]), so a tier cannot be added,
//! renamed or re-windowed in one place and forgotten in another: the directory
//! list, `Resolution::interval_ms`, the retention check and the compaction walk
//! all read the same rows.
//!
//! Every bucket is `{min, max, mean, n}` — never a mean alone — so a single
//! 1 s spike is still readable at every coarser resolution. A bucket with
//! `count == 0` is a gap; gaps are never interpolated.
//!
//! Writes are atomic (`tmp` + `fsync` + `rename` + directory `fsync`). A source
//! file is only removed *after* the coarser tier's write returned, so a crash
//! can at worst duplicate one bucket, and the next read dedups it by
//! `(metric_id, resolution, bucket_ts)`.
//!
//! ```
//! use syswatch_store::{CascadeStore, Resolution, Store};
//!
//! let dir = std::env::temp_dir().join(format!("syswatch-store-doc-{}", std::process::id()));
//! let store = CascadeStore::new(dir.clone());
//! let ts = 1_700_000_000_000; // unix ms
//! store.insert_raw("cpu.total_pct", ts, 42.0).unwrap();
//!
//! let buckets = store.query_series("cpu.total_pct", ts, ts + 1_000, 100).unwrap();
//! assert_eq!(buckets.len(), 1);
//! assert_eq!(buckets[0].resolution, Resolution::Raw1s);
//! assert_eq!(buckets[0].max, 42.0);
//!
//! // Age the bucket past the 6 h raw window: one pass walks every tier.
//! let later = ts + 400 * 86_400_000;
//! let stats = store.compact_at(later).unwrap();
//! assert_eq!(stats.buckets_demoted, 5);
//! # std::fs::remove_dir_all(&dir).ok();
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;
use syswatch_core::MetricDef;

pub const HOUR_MS: u64 = 3_600_000;
pub const DAY_MS: u64 = 86_400_000;

/// Compaction cadence: once at startup, then hourly (T18 §3).
pub const COMPACT_INTERVAL_MS: u64 = HOUR_MS;

// ── Rollup resolution ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Resolution {
    Raw1s,
    Tier15s,
    Tier60s,
    Tier300s,
    Tier1800s,
    Archive,
}

impl Resolution {
    /// Bucket width in milliseconds.
    pub fn interval_ms(self) -> u64 {
        tier_spec(self).bucket_ms
    }

    /// How long this tier keeps a bucket, in hours (`u64::MAX` = forever).
    pub fn retention_hours(self) -> u64 {
        match tier_spec(self).keep_ms {
            u64::MAX => u64::MAX,
            keep => keep / HOUR_MS,
        }
    }

    /// Directory name under the store base dir.
    pub fn dir_name(self) -> &'static str {
        tier_spec(self).dir
    }
}

/// One row of the cascade table. `keep_ms` is the age at which a bucket leaves
/// this tier; `u64::MAX` means the tier is terminal (the archive).
#[derive(Debug, Clone, Copy)]
pub struct TierSpec {
    pub resolution: Resolution,
    pub dir: &'static str,
    pub bucket_ms: u64,
    pub keep_ms: u64,
}

/// The cascade, finest first. This is the single source of truth for the tier
/// windows named in the brief: 6 h, 1 d, 7 d, 30 d, 1 y, forever.
pub const CASCADE: [TierSpec; 6] = [
    TierSpec {
        resolution: Resolution::Raw1s,
        dir: "raw",
        bucket_ms: 1_000,
        keep_ms: 6 * HOUR_MS,
    },
    TierSpec {
        resolution: Resolution::Tier15s,
        dir: "tier15s",
        bucket_ms: 15_000,
        keep_ms: DAY_MS,
    },
    TierSpec {
        resolution: Resolution::Tier60s,
        dir: "tier60s",
        bucket_ms: 60_000,
        keep_ms: 7 * DAY_MS,
    },
    TierSpec {
        resolution: Resolution::Tier300s,
        dir: "tier300s",
        bucket_ms: 300_000,
        keep_ms: 30 * DAY_MS,
    },
    TierSpec {
        resolution: Resolution::Tier1800s,
        dir: "tier1800s",
        bucket_ms: 1_800_000,
        keep_ms: 365 * DAY_MS,
    },
    TierSpec {
        resolution: Resolution::Archive,
        dir: "archive",
        bucket_ms: DAY_MS,
        keep_ms: u64::MAX,
    },
];

/// Row for one resolution. A lookup, not a copy: the table stays authoritative.
pub fn tier_spec(r: Resolution) -> &'static TierSpec {
    let idx = match r {
        Resolution::Raw1s => 0,
        Resolution::Tier15s => 1,
        Resolution::Tier60s => 2,
        Resolution::Tier300s => 3,
        Resolution::Tier1800s => 4,
        Resolution::Archive => 5,
    };
    &CASCADE[idx]
}

/// The next coarser tier, or `None` for the archive (terminal).
pub fn next_tier(r: Resolution) -> Option<Resolution> {
    CASCADE
        .iter()
        .skip_while(|t| t.resolution != r)
        .nth(1)
        .map(|t| t.resolution)
}

// ── Rollup bucket ────────────────────────────────────────────────────────────

/// One rollup bucket. Stores min/max/sum/count — never a mean alone.
/// A missing bucket (count = 0) is a gap, never interpolated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupBucket {
    pub metric_id: String,
    pub resolution: Resolution,
    pub bucket_ts: u64, // unix ms, aligned to the resolution boundary
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
    /// A window with no samples yields no bucket — the caller draws a gap; no
    /// point is ever invented.
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompactionStats {
    pub buckets_demoted: u64,
    pub files_rewritten: u64,
    pub files_removed: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

// ── Cascade store ────────────────────────────────────────────────────────────

pub struct CascadeStore {
    base_dir: PathBuf,
    mu: Mutex<()>,
    /// Unix ms of the last compaction pass this process ran. 0 = never, which
    /// is what makes the startup pass happen on the first `compact_if_due`.
    last_compact_ms: AtomicU64,
}

impl CascadeStore {
    pub fn new(base_dir: PathBuf) -> Self {
        let s = Self {
            base_dir,
            mu: Mutex::new(()),
            last_compact_ms: AtomicU64::new(0),
        };
        let _ = fs::create_dir_all(&s.base_dir);
        for spec in CASCADE.iter() {
            let _ = fs::create_dir_all(s.base_dir.join(spec.dir));
        }
        s
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// A poisoned mutex means another thread panicked mid-write. The on-disk
    /// files are still whole (every write is tmp+rename), so running on is
    /// safer than panicking the sampler thread.
    fn lock(&self) -> MutexGuard<'_, ()> {
        self.mu.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tier_dir(&self, r: Resolution) -> PathBuf {
        self.base_dir.join(r.dir_name())
    }

    fn bucket_line(b: &RollupBucket) -> String {
        let mut v = serde_json::to_value(b).unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("mean".into(), json!(b.avg().unwrap_or(0.0)));
            obj.insert("n".into(), json!(b.count));
        }
        v.to_string()
    }

    fn read_buckets(path: &Path) -> Vec<RollupBucket> {
        let Ok(text) = fs::read_to_string(path) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue;
            }
            // A truncated last line (power loss mid-append) is skipped, not
            // fatal: the rest of the file still answers queries.
            if let Ok(b) = serde_json::from_str::<RollupBucket>(line) {
                out.push(b);
            }
        }
        dedup_merge(out)
    }

    fn write_buckets(path: &Path, buckets: &[RollupBucket]) -> StoreResult<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir)?;
        let mut text = String::new();
        for b in buckets {
            text.push_str(&Self::bucket_line(b));
            text.push('\n');
        }
        atomic_write(path, &text)
    }

    /// Raw file path for one day of the finest tier.
    fn raw_path(&self, ts_ms: u64) -> PathBuf {
        self.tier_dir(Resolution::Raw1s)
            .join(format!("{}.jsonl", utc_date(ts_ms)))
    }

    /// Compaction with an explicit "now". Deterministic, so the tests exercise
    /// 3 h / 2 d / 20 d / 100 d / 400 d of age without touching the clock.
    pub fn compact_at(&self, now_ms: u64) -> StoreResult<CompactionStats> {
        let _g = self.lock();
        let mut stats = CompactionStats {
            bytes_before: walk_dir(&self.base_dir),
            ..Default::default()
        };

        // windows(2): each source tier paired with the next coarser one.
        for pair in CASCADE.windows(2) {
            let (src, tgt) = (pair[0], pair[1]);
            if src.keep_ms == u64::MAX {
                continue; // terminal tier: nothing demotes out of it
            }
            let cutoff = now_ms.saturating_sub(src.keep_ms);
            let src_dir = self.base_dir.join(src.dir);

            let mut kept_by_file: Vec<(PathBuf, Vec<RollupBucket>)> = Vec::new();
            let mut target_map: BTreeMap<PathBuf, Vec<RollupBucket>> = BTreeMap::new();
            let mut demoted = 0u64;

            for path in list_jsonl(&src_dir) {
                let buckets = Self::read_buckets(&path);
                let mut kept: Vec<RollupBucket> = Vec::with_capacity(buckets.len());
                for b in buckets {
                    if b.bucket_ts >= cutoff {
                        kept.push(b);
                        continue;
                    }
                    demoted += 1;
                    let nb = RollupBucket {
                        resolution: tgt.resolution,
                        bucket_ts: align_ts(b.bucket_ts, tgt.bucket_ms),
                        ..b
                    };
                    let dst = self
                        .base_dir
                        .join(tgt.dir)
                        .join(format!("{}.jsonl", utc_date(nb.bucket_ts)));
                    target_map.entry(dst).or_default().push(nb);
                }
                kept_by_file.push((path, kept));
            }

            if demoted == 0 {
                continue; // nothing aged out: a re-run is a no-op
            }

            // Phase 1 — every coarser bucket is written and fsync'd first.
            // If any write fails we return before touching a source file.
            for (path, newbs) in &target_map {
                merge_into_file(path, newbs)?;
            }

            // Phase 2 — only now may a source file shrink or disappear.
            for (path, kept) in kept_by_file {
                if kept.is_empty() {
                    match fs::remove_file(&path) {
                        Ok(()) => stats.files_removed += 1,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                } else {
                    Self::write_buckets(&path, &kept)?;
                    stats.files_rewritten += 1;
                }
            }

            stats.buckets_demoted += demoted;
        }

        stats.bytes_after = walk_dir(&self.base_dir);
        Ok(stats)
    }

    /// Startup + hourly entry point (§3). `Some` when a pass ran, `None` when
    /// the hourly interval has not elapsed — the caller can call this on every
    /// tick for free.
    pub fn compact_if_due_at(&self, now_ms: u64) -> StoreResult<Option<CompactionStats>> {
        let last = self.last_compact_ms.load(Ordering::Acquire);
        if last != 0 && now_ms.saturating_sub(last) < COMPACT_INTERVAL_MS {
            return Ok(None);
        }
        let stats = self.compact_at(now_ms)?;
        self.last_compact_ms.store(now_ms, Ordering::Release);
        Ok(Some(stats))
    }

    /// Wall-clock form of [`Self::compact_if_due_at`].
    pub fn compact_if_due(&self) -> StoreResult<Option<CompactionStats>> {
        self.compact_if_due_at(now_ms())
    }

    /// Milliseconds since this process last ran a compaction pass (0 = never).
    pub fn last_compact_age_ms(&self, now_ms: u64) -> u64 {
        let last = self.last_compact_ms.load(Ordering::Acquire);
        if last == 0 {
            u64::MAX
        } else {
            now_ms.saturating_sub(last)
        }
    }
}

impl Store for CascadeStore {
    fn insert_raw(&self, metric_id: &str, ts: u64, value: f64) -> StoreResult<()> {
        let _g = self.lock();
        let r = Resolution::Raw1s;
        let bucket_ts = align_ts(ts, r.interval_ms());
        let b = RollupBucket {
            metric_id: metric_id.to_string(),
            resolution: r,
            bucket_ts,
            min: value,
            max: value,
            sum: value,
            count: 1,
        };
        let path = self.raw_path(bucket_ts);
        if !path.exists() {
            // Only the first sample of a new day pays for the directory check.
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
        }
        // Append, never rewrite: a 1 Hz writer must not re-serialise a growing
        // day file on every tick. Two samples inside one bucket land on two
        // lines and are merged by (metric, resolution, bucket_ts) on read.
        let mut line = Self::bucket_line(&b);
        line.push('\n');
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())?;
        f.sync_data()?;
        Ok(())
    }

    fn query_series(
        &self,
        metric_id: &str,
        from: u64,
        to: u64,
        max_points: usize,
    ) -> StoreResult<Vec<RollupBucket>> {
        let _g = self.lock();
        // A timestamp can exist in two tiers right after a crash (target written,
        // source not yet rewritten). The finest bucket wins; the duplicate is
        // not double counted.
        let mut by_ts: BTreeMap<u64, RollupBucket> = BTreeMap::new();
        for spec in CASCADE.iter() {
            let dir = self.base_dir.join(spec.dir);
            for path in list_jsonl(&dir) {
                for b in Self::read_buckets(&path) {
                    if b.metric_id != metric_id {
                        continue;
                    }
                    let iv = b.resolution.interval_ms();
                    if b.bucket_ts + iv <= from || b.bucket_ts > to {
                        continue;
                    }
                    match by_ts.get(&b.bucket_ts) {
                        Some(existing)
                            if existing.resolution.interval_ms() <= b.resolution.interval_ms() => {}
                        _ => {
                            by_ts.insert(b.bucket_ts, b);
                        }
                    }
                }
            }
        }
        let mut out: Vec<RollupBucket> = by_ts.into_values().collect();
        out.sort_by_key(|b| b.bucket_ts);
        if max_points > 0 && out.len() > max_points {
            out = downsample_buckets(&out, max_points);
        }
        Ok(out)
    }

    fn compact(&self) -> StoreResult<CompactionStats> {
        self.compact_at(now_ms())
    }

    fn disk_usage(&self) -> StoreResult<u64> {
        let _g = self.lock();
        Ok(walk_dir(&self.base_dir))
    }

    fn register_metric(&self, _def: &MetricDef) -> StoreResult<()> {
        Ok(())
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn align_ts(ts: u64, interval_ms: u64) -> u64 {
    if interval_ms == 0 {
        ts
    } else {
        ts - (ts % interval_ms)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn utc_date(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let (y, m, d) = days_to_civil(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn days_to_civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn list_jsonl(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .collect();
    out.sort();
    out
}

fn walk_dir(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                total += walk_dir(&p);
            } else if let Ok(m) = fs::metadata(&p) {
                total += m.len();
            }
        }
    }
    total
}

/// Write `contents` to `path` so a reader sees either the old file or the new
/// one, never a partial line: tmp file, fsync, rename, fsync the directory.
fn atomic_write(path: &Path, contents: &str) -> StoreResult<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

fn merge_bucket(dst: &mut RollupBucket, src: &RollupBucket) {
    if src.count == 0 {
        return;
    }
    if dst.count == 0 {
        *dst = src.clone();
        return;
    }
    if src.min < dst.min {
        dst.min = src.min;
    }
    if src.max > dst.max {
        dst.max = src.max;
    }
    dst.sum += src.sum;
    dst.count = dst.count.saturating_add(src.count);
}

fn dedup_merge(buckets: Vec<RollupBucket>) -> Vec<RollupBucket> {
    let mut map: BTreeMap<(String, Resolution, u64), RollupBucket> = BTreeMap::new();
    for b in buckets {
        let key = (b.metric_id.clone(), b.resolution, b.bucket_ts);
        if let Some(existing) = map.get_mut(&key) {
            merge_bucket(existing, &b);
        } else {
            map.insert(key, b);
        }
    }
    map.into_values().collect()
}

fn merge_into_file(path: &Path, newbs: &[RollupBucket]) -> StoreResult<()> {
    let mut existing = CascadeStore::read_buckets(path);
    for nb in newbs {
        match existing.iter_mut().find(|e| {
            e.metric_id == nb.metric_id
                && e.resolution == nb.resolution
                && e.bucket_ts == nb.bucket_ts
        }) {
            Some(e) => merge_bucket(e, nb),
            None => existing.push(nb.clone()),
        }
    }
    existing.sort_by_key(|b| b.bucket_ts);
    CascadeStore::write_buckets(path, &existing)
}

fn downsample_buckets(buckets: &[RollupBucket], max_points: usize) -> Vec<RollupBucket> {
    if buckets.is_empty() || max_points == 0 {
        return Vec::new();
    }
    if buckets.len() <= max_points {
        return buckets.to_vec();
    }
    let first = buckets[0].bucket_ts;
    let last = buckets[buckets.len() - 1].bucket_ts;
    let width = ((last - first) / max_points as u64).max(1);
    let mut groups: Vec<Option<RollupBucket>> = vec![None; max_points];
    for b in buckets {
        let idx = (((b.bucket_ts - first) / width) as usize).min(max_points - 1);
        match groups[idx].as_mut() {
            None => {
                // A down-sampled bucket keeps min/max and sums counts, so the
                // peak is not lost when the chart thins the series.
                let mut nb = b.clone();
                nb.bucket_ts = first + idx as u64 * width;
                groups[idx] = Some(nb);
            }
            Some(g) => {
                if b.min < g.min {
                    g.min = b.min;
                }
                if b.max > g.max {
                    g.max = b.max;
                }
                g.sum += b.sum;
                g.count = g.count.saturating_add(b.count);
            }
        }
    }
    groups.into_iter().flatten().collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────
//
// Every test drives the public surface and asserts what a reader of the data
// can see: a spike's max, a value that must survive a tier change, an empty
// window that must stay empty, a second compaction that must do nothing.

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Fixed "now" for the whole module: 2027-01-15T08:00:00Z. Tests never
    /// read the wall clock, so an age of "400 days" means exactly that.
    const NOW: u64 = 1_800_000_000_000;

    fn tmp() -> PathBuf {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("sw-store-{}-{}", process::id(), n));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn store() -> (PathBuf, CascadeStore) {
        let dir = tmp();
        let store = CascadeStore::new(dir.clone());
        (dir, store)
    }

    /// Every bucket on disk in one tier, read straight from its files.
    fn buckets_in(dir: &Path, r: Resolution) -> Vec<RollupBucket> {
        let mut out = Vec::new();
        for path in list_jsonl(&dir.join(r.dir_name())) {
            out.extend(CascadeStore::read_buckets(&path));
        }
        out
    }

    /// (relative path, byte length, contents) for every file under base — the
    /// fingerprint a re-run must not change.
    fn tree(dir: &Path) -> Vec<(String, u64, String)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = fs::read_dir(&d) else { continue };
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(dir).unwrap().display().to_string();
                    let text = fs::read_to_string(&p).unwrap_or_default();
                    out.push((rel, len_of(&p), text));
                }
            }
        }
        out.sort();
        out
    }

    fn len_of(p: &Path) -> u64 {
        fs::metadata(p).map(|m| m.len()).unwrap_or(0)
    }

    #[test]
    fn cascade_table_names_every_tier_and_window() {
        let names: Vec<&str> = CASCADE.iter().map(|t| t.dir).collect();
        assert_eq!(
            names,
            vec!["raw", "tier15s", "tier60s", "tier300s", "tier1800s", "archive"],
            "the brief names these directories"
        );
        let widths: Vec<u64> = CASCADE.iter().map(|t| t.bucket_ms).collect();
        assert_eq!(widths, vec![1_000, 15_000, 60_000, 300_000, 1_800_000, DAY_MS]);
        let windows: Vec<u64> = CASCADE.iter().map(|t| t.keep_ms).collect();
        assert_eq!(
            windows,
            vec![6 * HOUR_MS, DAY_MS, 7 * DAY_MS, 30 * DAY_MS, 365 * DAY_MS, u64::MAX],
            "6h -> 1d -> 7d -> 30d -> 1y -> forever"
        );
        // The chain is unbroken and ends at the archive.
        assert_eq!(next_tier(Resolution::Raw1s), Some(Resolution::Tier15s));
        assert_eq!(next_tier(Resolution::Tier1800s), Some(Resolution::Archive));
        assert_eq!(next_tier(Resolution::Archive), None);
    }

    #[test]
    fn one_second_spike_survives_to_the_one_day_resolution() {
        let (dir, store) = store();
        let day = NOW - 400 * DAY_MS; // old enough to fall all the way to archive
        let base = day + 12 * HOUR_MS; // noon, inside one UTC day
        for i in 0..30u64 {
            store.insert_raw("temp.cpu.die", base + i * 1_000, 30.0).unwrap();
        }
        store.insert_raw("temp.cpu.die", base + 5_000, 99.0).unwrap();

        let stats = store.compact_at(NOW).unwrap();
        // One move per bucket per tier it crosses: 30 raw -> 2 15s -> 1 60s ->
        // 1 300s -> 1 1800s -> 1 archive.
        assert!(stats.buckets_demoted >= 31, "not every bucket moved: {stats:?}");
        for spec in CASCADE.iter().filter(|s| s.resolution != Resolution::Archive) {
            assert!(buckets_in(&dir, spec.resolution).is_empty(), "{} still holds data", spec.dir);
        }
        assert_eq!(buckets_in(&dir, Resolution::Archive).len(), 1, "one record for that day");

        let buckets = store
            .query_series("temp.cpu.die", day, day + DAY_MS, 1_000)
            .unwrap();
        let day_bucket = buckets
            .iter()
            .find(|b| b.resolution == Resolution::Archive)
            .unwrap_or_else(|| panic!("no 1 d bucket: {buckets:?}"));
        assert_eq!(day_bucket.bucket_ts, align_ts(base, DAY_MS));
        assert_eq!(day_bucket.count, 31);
        assert!((day_bucket.max - 99.0).abs() < 1e-9, "spike max lost: {day_bucket:?}");
        assert!((day_bucket.min - 30.0).abs() < 1e-9, "min lost: {day_bucket:?}");
        let mean = day_bucket.avg().unwrap();
        assert!((mean - (30.0 * 30.0 + 99.0) / 31.0).abs() < 1e-9, "mean {mean}");

        // The same day answers a "1 d" read with the spike intact.
        let one_day = store
            .query_series("temp.cpu.die", base - HOUR_MS, base + HOUR_MS, 500)
            .unwrap();
        assert!(one_day.iter().any(|b| (b.max - 99.0).abs() < 1e-9), "{one_day:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn min_and_max_survive_every_tier() {
        // age -> the tier the data must be sitting in after one pass
        let cases = [
            (3 * HOUR_MS, Resolution::Raw1s),
            (2 * DAY_MS, Resolution::Tier60s),
            (20 * DAY_MS, Resolution::Tier300s),
            (100 * DAY_MS, Resolution::Tier1800s),
            (400 * DAY_MS, Resolution::Archive),
        ];
        for (age, expected) in cases {
            let (dir, store) = store();
            let base = NOW - age;
            for (i, v) in [10.0f64, 99.0, 20.0].into_iter().enumerate() {
                store.insert_raw("m", base + i as u64 * 1_000, v).unwrap();
            }
            store.compact_at(NOW).unwrap();

            for spec in CASCADE.iter() {
                let found = buckets_in(&dir, spec.resolution);
                if spec.resolution == expected {
                    assert!(!found.is_empty(), "age {age} ms: nothing in {}", spec.dir);
                    let max = found.iter().map(|b| b.max).fold(f64::MIN, f64::max);
                    let min = found.iter().map(|b| b.min).fold(f64::MAX, f64::min);
                    assert!((max - 99.0).abs() < 1e-9, "age {age}: max {max} in {}", spec.dir);
                    assert!((min - 10.0).abs() < 1e-9, "age {age}: min {min} in {}", spec.dir);
                } else {
                    assert!(
                        found.is_empty(),
                        "age {age} ms: {} still holds {} bucket(s)",
                        spec.dir,
                        found.len()
                    );
                }
            }
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn archive_holds_one_record_per_metric_per_day() {
        let (dir, store) = store();
        let day = NOW - 400 * DAY_MS;
        store.insert_raw("cpu.total_pct", day + 1 * HOUR_MS, 10.0).unwrap();
        store.insert_raw("cpu.total_pct", day + 9 * HOUR_MS, 30.0).unwrap();
        store.insert_raw("mem.pct", day + 3 * HOUR_MS, 50.0).unwrap();
        store.compact_at(NOW).unwrap();

        let lines: Vec<String> = list_jsonl(&dir.join(Resolution::Archive.dir_name()))
            .iter()
            .flat_map(|p| fs::read_to_string(p).unwrap_or_default().lines().map(str::to_string).collect::<Vec<_>>())
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert_eq!(lines.len(), 2, "one cpu line + one mem line: {lines:?}");

        let cpu = store
            .query_series("cpu.total_pct", day, day + DAY_MS, 10)
            .unwrap();
        assert_eq!(cpu.len(), 1, "a day collapses to one bucket: {cpu:?}");
        assert_eq!(cpu[0].count, 2);
        assert!((cpu[0].min - 10.0).abs() < 1e-9 && (cpu[0].max - 30.0).abs() < 1e-9);
        assert!((cpu[0].avg().unwrap() - 20.0).abs() < 1e-9);

        // Each line carries the four fields the brief requires.
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        for key in ["min", "max", "mean", "n"] {
            assert!(v.get(key).is_some(), "archive line has no {key}: {v}");
        }
        assert!(!list_jsonl(&dir.join(Resolution::Archive.dir_name())).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gap_renders_as_a_gap_not_an_interpolation() {
        let (dir, store) = store();
        let t0 = NOW - HOUR_MS; // inside the raw window
        store.insert_raw("m", t0, 5.0).unwrap();
        store.insert_raw("m", t0 + 60_000, 7.0).unwrap();

        let buckets = store.query_series("m", t0, t0 + 120_000, 1_000).unwrap();
        assert_eq!(buckets.len(), 2, "only measured buckets: {buckets:?}");
        assert_eq!(buckets[0].bucket_ts, align_ts(t0, 1_000));
        assert_eq!(buckets[1].bucket_ts, align_ts(t0 + 60_000, 1_000));
        assert!(
            !buckets.iter().any(|b| b.bucket_ts == align_ts(t0 + 30_000, 1_000)),
            "the empty middle was invented: {buckets:?}"
        );
        assert!(buckets.iter().all(|b| b.count > 0 && !b.is_gap()));

        // An empty window answers with nothing at all, so the chart draws a gap.
        let empty = store
            .query_series("m", t0 + 10 * HOUR_MS, t0 + 11 * HOUR_MS, 1_000)
            .unwrap();
        assert!(empty.is_empty(), "empty window returned {empty:?}");

        // A zero-count bucket is a gap, and its mean is None rather than 0.
        let gap = RollupBucket {
            metric_id: "m".into(),
            resolution: Resolution::Tier15s,
            bucket_ts: 0,
            min: 0.0,
            max: 0.0,
            sum: 0.0,
            count: 0,
        };
        assert!(gap.is_gap() && gap.avg().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compaction_is_idempotent_across_two_consecutive_runs() {
        let (dir, store) = store();
        let base = NOW - 20 * DAY_MS;
        for i in 0..10u64 {
            store.insert_raw("m", base + i * 1_000, i as f64).unwrap();
        }
        let first = store.compact_at(NOW).unwrap();
        assert!(first.buckets_demoted > 0, "nothing was demoted: {first:?}");
        let after_first = tree(&dir);

        let second = store.compact_at(NOW).unwrap();
        assert_eq!(second.buckets_demoted, 0, "re-run demoted again: {second:?}");
        assert_eq!(second.files_rewritten, 0);
        assert_eq!(second.files_removed, 0);
        assert_eq!(tree(&dir), after_first, "a no-op run still rewrote files");

        // The data is still readable after both passes.
        let buckets = store.query_series("m", base, base + 60_000, 1_000).unwrap();
        assert_eq!(buckets.iter().map(|b| b.count).sum::<u32>(), 10);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_cascade_loses_no_sample() {
        let (dir, store) = store();
        let base = NOW - 400 * DAY_MS;
        let n = 300u64;
        for i in 0..n {
            // spread over ~5 minutes: 40 buckets per 15 s tier bucket
            store.insert_raw("m", base + i * 1_000, 1.0 + (i % 7) as f64).unwrap();
        }
        store.compact_at(NOW).unwrap();
        let total: u32 = CASCADE
            .iter()
            .flat_map(|s| buckets_in(&dir, s.resolution))
            .filter(|b| b.metric_id == "m")
            .map(|b| b.count)
            .sum();
        assert_eq!(total, n as u32, "samples vanished in the cascade");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_is_kept_when_the_coarser_write_fails() {
        let (dir, store) = store();
        let base = NOW - 2 * DAY_MS; // raw -> tier15s -> tier60s
        store.insert_raw("m", base, 42.0).unwrap();
        store.insert_raw("m", base + 1_000, 43.0).unwrap();

        // Make the first destination unwritable by occupying its name with a
        // directory: the rename cannot land there.
        let dst = dir
            .join(Resolution::Tier15s.dir_name())
            .join(format!("{}.jsonl", utc_date(align_ts(base, Resolution::Tier15s.interval_ms()))));
        fs::create_dir_all(&dst).unwrap();

        let err = store.compact_at(NOW);
        assert!(err.is_err(), "compaction claimed success with a blocked target");

        // The raw file was not touched, so nothing is lost.
        let raw = buckets_in(&dir, Resolution::Raw1s);
        assert_eq!(raw.iter().map(|b| b.count).sum::<u32>(), 2, "{raw:?}");
        assert!(raw.iter().any(|b| (b.max - 43.0).abs() < 1e-9));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn half_written_last_line_does_not_poison_the_file() {
        let (dir, store) = store();
        let base = NOW - HOUR_MS;
        store.insert_raw("m", base, 5.0).unwrap();

        let path = dir
            .join(Resolution::Raw1s.dir_name())
            .join(format!("{}.jsonl", utc_date(base)));
        {
            use std::io::Write as _;
            let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
            // Exactly what a power cut mid-append leaves behind.
            writeln!(
                f,
                "{{\"metric_id\":\"m\",\"resolution\":\"Raw1s\",\"bucket_ts\":{},\"min\":1",
                base + 1_000
            )
            .unwrap();
            f.flush().unwrap();
        }

        // The intact line still answers.
        let before = store.query_series("m", base - 1_000, base + 10_000, 1_000).unwrap();
        assert_eq!(before.len(), 1, "truncated tail broke the read: {before:?}");
        assert!((before[0].max - 5.0).abs() < 1e-9);

        // The writer keeps working next to the torn line.
        store.insert_raw("m", base + 2_000, 6.0).unwrap();
        let after = store.query_series("m", base - 1_000, base + 10_000, 1_000).unwrap();
        assert_eq!(after.len(), 2, "{after:?}");
        assert_eq!(after.iter().map(|b| b.count).sum::<u32>(), 2);

        // The first rewrite repairs the file: 7 h later every raw bucket has
        // aged into the 15 s tier, which leaves no half line behind.
        store.compact_at(base + 7 * HOUR_MS).unwrap();
        for spec in CASCADE.iter() {
            for p in list_jsonl(&dir.join(spec.dir)) {
                let text = fs::read_to_string(&p).unwrap();
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    serde_json::from_str::<serde_json::Value>(line)
                        .unwrap_or_else(|e| panic!("unparseable line in {}: {line} ({e})", spec.dir));
                }
            }
        }
        let demoted = buckets_in(&dir, Resolution::Tier15s);
        assert_eq!(demoted.iter().map(|b| b.count).sum::<u32>(), 2, "{demoted:?}");
        assert!(
            list_jsonl(&dir.join(Resolution::Raw1s.dir_name())).is_empty(),
            "the raw file was not retired after its buckets were written coarser"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compaction_runs_at_startup_and_then_hourly() {
        let (dir, store) = store();
        store.insert_raw("m", NOW - 2 * DAY_MS, 1.0).unwrap();

        // Process start: nothing has run yet, so the first call is due.
        assert_eq!(store.last_compact_age_ms(NOW), u64::MAX);
        let startup = store.compact_if_due_at(NOW).unwrap();
        assert!(startup.is_some(), "startup pass did not run");

        // 59 minutes later: the hourly timer has not elapsed.
        assert!(store.compact_if_due_at(NOW + 59 * 60_000).unwrap().is_none());
        // 61 minutes later: due again.
        let hourly = store.compact_if_due_at(NOW + 61 * 60_000).unwrap();
        assert!(hourly.is_some(), "hourly pass did not run");
        assert_eq!(hourly.unwrap().buckets_demoted, 0, "the second pass was not a no-op");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_do_not_lose_samples() {
        let dir = tmp();
        let store = Arc::new(CascadeStore::new(dir.clone()));
        let base = NOW - HOUR_MS;
        let per_thread = 25u64;
        std::thread::scope(|s| {
            for t in 0..4u64 {
                let st = Arc::clone(&store);
                s.spawn(move || {
                    for i in 0..per_thread {
                        let ts = base + (t * per_thread + i) * 1_000;
                        st.insert_raw("cpu.total_pct", ts, 10.0 + t as f64).unwrap();
                    }
                });
            }
        });
        let buckets = store
            .query_series("cpu.total_pct", base - 1_000, base + 10 * 60_000, 0)
            .unwrap();
        assert_eq!(buckets.iter().map(|b| b.count).sum::<u32>(), 100);
        assert_eq!(buckets.len(), 100, "one bucket per second");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_prefers_the_finest_resolution_on_a_duplicate_timestamp() {
        let (dir, store) = store();
        let ts = NOW - HOUR_MS;
        store.insert_raw("m", ts, 4.0).unwrap();
        // Simulate a crash-between-phases duplicate: the same bucket also sits
        // in the coarser tier with a different count.
        let coarse = dir.join(Resolution::Tier15s.dir_name());
        fs::create_dir_all(&coarse).unwrap();
        let b = RollupBucket {
            metric_id: "m".into(),
            resolution: Resolution::Tier15s,
            bucket_ts: align_ts(ts, Resolution::Tier15s.interval_ms()),
            min: 4.0,
            max: 4.0,
            sum: 4.0 * 9.0,
            count: 9,
        };
        CascadeStore::write_buckets(&coarse.join("dup.jsonl"), &[b]).unwrap();

        let out = store.query_series("m", ts - 1_000, ts + 1_000, 0).unwrap();
        let at_ts = out
            .iter()
            .find(|x| x.bucket_ts == align_ts(ts, 1_000))
            .expect("bucket missing");
        assert_eq!(at_ts.resolution, Resolution::Raw1s, "coarse duplicate won: {out:?}");
        assert_eq!(at_ts.count, 1, "the duplicate was double counted: {out:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn downsampling_keeps_the_peak() {
        let (dir, store) = store();
        let base = NOW - HOUR_MS;
        for i in 0..100u64 {
            store.insert_raw("m", base + i * 1_000, if i == 42 { 99.0 } else { 1.0 }).unwrap();
        }
        let out = store.query_series("m", base, base + 100_000, 10).unwrap();
        assert!(out.len() <= 10, "{} points", out.len());
        let max = out.iter().map(|b| b.max).fold(f64::MIN, f64::max);
        assert!((max - 99.0).abs() < 1e-9, "peak dropped when thinning: {out:?}");
        assert_eq!(out.iter().map(|b| b.count).sum::<u32>(), 100);
        let _ = fs::remove_dir_all(&dir);
    }
}
