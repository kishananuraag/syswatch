//! Shared history for `/api/graph`, `/api/history` and `/api/analytics`.
//!
//! Three things live here, in the order a request touches them:
//!
//! 1. [`Ring`] — the agent's own last two hours of 1 s samples, pushed by the
//!    sampler thread. The dashboard's Python server keeps the same kind of
//!    in-memory ring; without one, a Rust `/api/graph?range=1h` would depend
//!    entirely on the collector having been alive, and would answer "no data"
//!    on a machine whose collector is stopped or freshly installed.
//! 2. [`CollectorHistory`] — the collector's JSONL tiers on disk
//!    (`raw/`, `tier60s/`, `tier300s/`, `tier1800s/`, …), parsed once per
//!    (file, mtime, size) and cached. This is the only durable history there
//!    is: the Python server's ring dies with the process, these files do not.
//! 3. [`Point`] — one row of history, with the same field names the Python
//!    server's ring uses (`cpu`, `ram`, `down_bps`, `up_bps`, `temps`, …), so
//!    every chart built from it expects what it already expects.
//!
//! # Honesty rules
//!
//! * A field the source did not measure stays `None`. Nothing is filled with
//!   zero, and empty buckets are dropped, never interpolated.
//! * Tier choice is disclosed in the payload (`sources.tier`), and the bytes
//!   actually read are capped: a 1 s dashboard poll must not re-read a
//!   30-day log.
//! * The collector log holds no battery or GPU-utilisation series (verified:
//!   `raw/*.jsonl` has `cpu`, `memory`, `disks`, `networks`, `processes`,
//!   `energy`, `temperature`, `process_count`). Endpoints that need those say
//!   so instead of drawing a flat line.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::timefmt;

/// Chart target, carried over from the Python server (`MAX_POINTS_PER_RANGE`).
pub const MAX_POINTS: usize = 500;
/// Ring capacity: 1 s samples for two hours.
pub const RING_CAP: usize = 7_200;
/// Files opened per request. Only files that overlap the window are considered,
/// so for a 1 h raw window this is ~5 of them; the cap is a backstop against a
/// pathological directory.
pub const MAX_FILES: usize = 32;
/// Bytes read per request (the Python server's `COLLECTOR_HISTORY_MAX_BYTES`
/// idea): a 1 s dashboard poll must not re-read a multi-megabyte log.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Parsed-file cache, keyed on (path, mtime, size).
const CACHE_TTL: Duration = Duration::from_secs(10);
const CACHE_MAX_ENTRIES: usize = 16;
/// Points kept per request. A 1 y window over the 30 min tier is 17 520 rows;
/// anything past this is a sign the tier choice is wrong, so stop honestly.
const MAX_POINTS_LOADED: usize = 40_000;

/// The named ranges `/api/history` and `/api/analytics` accept. Same names,
/// same widths as `RANGES` in dashboard/server.py.
pub const RANGES: &[(&str, f64)] = &[
    ("1m", 60.0),
    ("5m", 300.0),
    ("15m", 900.0),
    ("1h", 3_600.0),
    ("6h", 21_600.0),
    ("1d", 86_400.0),
    ("7d", 604_800.0),
    ("30d", 2_592_000.0),
    ("1y", 31_536_000.0),
];

pub fn range_secs(name: &str) -> Option<f64> {
    RANGES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| *s)
}

/// Smallest named range that covers `secs` — `_range_from_secs` in the Python
/// server, including its 1 h fallback for an empty series.
pub fn range_for_secs(secs: f64) -> &'static str {
    if secs <= 0.0 {
        return "1h";
    }
    for (name, span) in RANGES {
        if *span >= secs {
            return name;
        }
    }
    "1y"
}

/// `90 s` / `15 min` / `1.5 h` / `3.0 d` — the Python `fmt_span` ladder.
pub fn fmt_span(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0 s".to_string();
    }
    if secs < 120.0 {
        format!("{secs:.0} s")
    } else if secs < 7_200.0 {
        format!("{:.0} min", secs / 60.0)
    } else if secs < 172_800.0 {
        format!("{:.1} h", secs / 3_600.0)
    } else {
        format!("{:.1} d", secs / 86_400.0)
    }
}

/// Which tier answers a window, coarsest first that still fits the chart.
///
/// The first tier in the list that has any file overlapping the window wins
/// (see [`CollectorHistory::load`]), so a machine that only ever wrote `raw/`
/// still answers a 7 d question — with 1 s data and a capped file count, and a
/// `tier` field that says so.
pub fn tiers_for(range_secs: f64) -> &'static [&'static str] {
    if range_secs <= 3_600.0 {
        &["raw"]
    } else if range_secs <= 21_600.0 {
        &["tier60s", "raw"]
    } else if range_secs <= 86_400.0 {
        &["tier300s", "tier60s", "raw"]
    } else if range_secs <= 604_800.0 {
        &["tier1800s", "tier300s", "raw"]
    } else {
        &["tier1800s", "tier300s", "raw"]
    }
}

fn tier_cadence(tier: &str) -> f64 {
    match tier {
        "raw" => 1.0,
        "tier15s" => 15.0,
        "tier60s" => 60.0,
        "tier300s" => 300.0,
        "tier1800s" => 1_800.0,
        _ => 0.0,
    }
}

// ── one row of history ──────────────────────────────────────────────────────

/// One history row. Fields mirror the Python server's ring points so the same
/// chart code reads either implementation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Point {
    pub ts: f64,
    pub cpu: Option<f64>,
    pub ram: Option<f64>,
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
    pub load1: Option<f64>,
    pub proc_count: Option<f64>,
    pub temp_c: Option<f64>,
    pub attributed_w: Option<f64>,
    pub battery_pct: Option<f64>,
    pub time_remaining_s: Option<f64>,
    /// Per-sensor temperatures, in the order the source listed them.
    pub temps: Vec<f64>,
}

fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    Some(cur)
}

fn num(v: &Value, path: &[&str]) -> Option<f64> {
    at(v, path).and_then(Value::as_f64)
}

/// Timestamp of a JSONL record: `timestamp` string or numeric `ts`, with the
/// millisecond form recognised (no unix second count is 1e12 yet).
pub fn record_ts(rec: &Value) -> Option<f64> {
    match rec.get("timestamp").or_else(|| rec.get("ts"))? {
        Value::Number(n) => {
            let v = n.as_f64()?;
            Some(if v > 1e12 { v / 1000.0 } else { v })
        }
        Value::String(s) => timefmt::parse_rfc3339(s),
        _ => None,
    }
}

/// Sum of the real interfaces' rx/tx rates — the same pseudo-interface rule the
/// sampler and `/api/current` use, so a chart and a hero number agree.
fn net_total(rec: &Value) -> (Option<f64>, Option<f64>) {
    let Some(rows) = rec.get("networks").and_then(Value::as_array) else {
        return (None, None);
    };
    let mut rx = 0.0;
    let mut tx = 0.0;
    let mut seen = false;
    for row in rows {
        let name = row.get("name").and_then(Value::as_str).unwrap_or("");
        if crate::sampler::is_pseudo_iface(name) {
            continue;
        }
        if let Some(v) = row.get("rx_bps").and_then(Value::as_f64) {
            rx += v;
            seen = true;
        }
        if let Some(v) = row.get("tx_bps").and_then(Value::as_f64) {
            tx += v;
            seen = true;
        }
    }
    if seen { (Some(rx), Some(tx)) } else { (None, None) }
}

impl Point {
    /// One collector JSONL record (`raw/…` or a rollup tier) → a point.
    ///
    /// Returns `None` only when the record carries no usable timestamp: a row
    /// with no time cannot be placed on a chart.
    pub fn from_record(rec: &Value) -> Option<Point> {
        let ts = record_ts(rec)?;
        let (down_bps, up_bps) = net_total(rec);
        let temps = match rec.get("temperatures").and_then(Value::as_array) {
            Some(rows) => rows.iter().filter_map(Value::as_f64).collect(),
            None => Vec::new(),
        };
        Some(Point {
            ts,
            cpu: num(rec, &["cpu", "total_pct"]),
            ram: num(rec, &["memory", "pct"]),
            down_bps,
            up_bps,
            load1: num(rec, &["cpu", "load1"]),
            proc_count: num(rec, &["process_count"]),
            temp_c: num(rec, &["temperature"]),
            attributed_w: num(rec, &["energy", "attributed_w"]),
            battery_pct: num(rec, &["battery", "pct"]),
            time_remaining_s: num(rec, &["battery", "time_remaining_s"]),
            temps,
        })
    }

    /// One `/api/current` snapshot → a point. This is what fills the ring.
    pub fn from_snapshot(snap: &Value) -> Option<Point> {
        let ts = match snap.get("ts") {
            Some(Value::String(s)) => timefmt::parse_rfc3339(s)?,
            Some(Value::Number(n)) => n.as_f64()?,
            _ => return None,
        };
        let temps: Vec<f64> = snap
            .get("temps_c")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.get("c").and_then(Value::as_f64))
                    .collect()
            })
            .unwrap_or_default();
        // `temperature` is the hottest die reading the sampler can see; the
        // ring keeps it so a temperature chart has a scalar to plot.
        let temp_c = snap
            .get("power")
            .and_then(|p| p.get("cpu_temp_c"))
            .and_then(Value::as_f64)
            .or_else(|| temps.iter().copied().reduce(f64::max));
        Some(Point {
            ts,
            cpu: num(snap, &["cpu", "total_pct"]),
            ram: num(snap, &["ram", "pct"]),
            down_bps: num(snap, &["net", "down_bps"]),
            up_bps: num(snap, &["net", "up_bps"]),
            load1: num(snap, &["load", "one"]),
            proc_count: num(snap, &["proc_count"]),
            temp_c,
            attributed_w: num(snap, &["power", "package_w"]),
            battery_pct: num(snap, &["battery", "pct"]),
            time_remaining_s: num(snap, &["battery", "time_remaining_s"]),
            temps,
        })
    }

    /// Merge `other`'s gaps into `self` (used when a ring point and a collector
    /// record describe the same instant: the ring has battery, the record has
    /// energy, and neither should win outright).
    fn fill_from(&mut self, other: &Point) {
        let pairs: [(&mut Option<f64>, Option<f64>); 9] = [
            (&mut self.cpu, other.cpu),
            (&mut self.ram, other.ram),
            (&mut self.down_bps, other.down_bps),
            (&mut self.up_bps, other.up_bps),
            (&mut self.load1, other.load1),
            (&mut self.proc_count, other.proc_count),
            (&mut self.temp_c, other.temp_c),
            (&mut self.attributed_w, other.attributed_w),
            (&mut self.battery_pct, other.battery_pct),
        ];
        for (slot, value) in pairs {
            if slot.is_none() {
                *slot = value;
            }
        }
        if self.time_remaining_s.is_none() {
            self.time_remaining_s = other.time_remaining_s;
        }
        if self.temps.is_empty() {
            self.temps = other.temps.clone();
        }
    }
}

// ── the live ring ───────────────────────────────────────────────────────────

/// The agent's own recent samples. Written only by the sampler thread; read by
/// request handlers through a mutex, so a request never waits on a sample.
pub struct Ring {
    cap: usize,
    points: VecDeque<Point>,
    pushed: u64,
    skipped: u64,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            points: VecDeque::new(),
            pushed: 0,
            skipped: 0,
        }
    }

    pub fn push_snapshot(&mut self, snap: &Value) -> bool {
        let Some(p) = Point::from_snapshot(snap) else {
            self.skipped += 1;
            return false;
        };
        if let Some(last) = self.points.back()
            && p.ts <= last.ts
        {
            // The clock stepped back (NTP), or two samples landed in the same
            // second: keep the ring monotonic instead of appending a point that
            // sorts before its neighbour.
            self.skipped += 1;
            return false;
        }
        self.points.push_back(p);
        while self.points.len() > self.cap {
            self.points.pop_front();
        }
        self.pushed += 1;
        true
    }

    /// Diagnostics accessors: used by the tests and available to a future
    /// `/api/health` detail block. The ring's own bookkeeping is not otherwise
    /// read at request time.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    #[allow(dead_code)]
    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    #[allow(dead_code)]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    pub fn span_s(&self) -> Option<f64> {
        let first = self.points.front()?;
        let last = self.points.back()?;
        Some(last.ts - first.ts)
    }

    /// Points inside `[from, to]`, oldest first.
    pub fn between(&self, from: f64, to: f64) -> Vec<Point> {
        self.points
            .iter()
            .filter(|p| p.ts >= from && p.ts <= to)
            .cloned()
            .collect()
    }
}

// ── the collector log on disk ───────────────────────────────────────────────

struct CacheEntry {
    path: PathBuf,
    mtime: Option<SystemTime>,
    size: u64,
    at: Instant,
    points: Arc<Vec<Point>>,
}

/// What a query actually read. Every payload carries this so a surprising chart
/// can be traced to files instead of guessed at.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub tier: String,
    pub dir: String,
    pub files: Vec<String>,
    pub records: usize,
    pub ring_points: usize,
    pub ring_span_s: Option<f64>,
    pub collector_points: usize,
    pub oldest_ts: Option<f64>,
    pub newest_ts: Option<f64>,
    pub range_s: f64,
    /// Whole-tier history on this machine, when it could be measured cheaply.
    pub reason: Option<String>,
}

impl Sources {
    pub fn to_json(&self) -> Value {
        json!({
            "tier": self.tier,
            "dir": self.dir,
            "files": self.files,
            "records": self.records,
            "range_s": self.range_s,
            "ring": {
                "points": self.ring_points,
                "span_s": self.ring_span_s,
                "source": "in-process 1 s sampler ring (agent memory, last 2 h)",
            },
            "collector": {
                "points": self.collector_points,
                "oldest_ts": self.oldest_ts,
                "newest_ts": self.newest_ts,
                "source": "collector JSONL tiers on disk",
            },
            "reason": self.reason,
        })
    }
}

/// Reader for the collector's JSONL tiers, with a parsed-file cache.
pub struct CollectorHistory {
    logs_dir: PathBuf,
    cache: Mutex<Vec<CacheEntry>>,
}

impl CollectorHistory {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self {
            logs_dir,
            cache: Mutex::new(Vec::new()),
        }
    }

    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    /// Points for a window from the first tier that has files in it.
    ///
    /// Capped at [`MAX_FILES`] files and [`MAX_POINTS_LOADED`] rows; the
    /// disclosure travels back in [`Sources`].
    pub fn load(&self, from: f64, to: f64, range_s: f64, tiers: &[&str]) -> (Vec<Point>, Sources) {
        let mut sources = Sources {
            range_s,
            ..Sources::default()
        };
        for tier in tiers {
            let dir = if *tier == "raw" {
                self.logs_dir.join("raw")
            } else if tier.starts_with("tier") {
                self.logs_dir.join(tier)
            } else {
                self.logs_dir.clone()
            };
            let files = crate::series::jsonl_files(&dir);
            let overlapping: Vec<&(PathBuf, f64, f64)> = files
                .iter()
                .filter(|(_, start, window)| start + window >= from && *start <= to)
                .collect();
            if overlapping.is_empty() {
                continue;
            }
            sources.tier = (*tier).to_string();
            sources.dir = dir.display().to_string();
            sources.reason = Some(format!(
                "cadence {} s — {} tier",
                tier_cadence(tier) as u64,
                tier
            ));

            let mut points: Vec<Point> = Vec::new();
            let mut seen: Vec<String> = Vec::new();
            let mut budget = MAX_BYTES;
            for (path, _start, _window) in overlapping {
                if seen.len() >= MAX_FILES || points.len() >= MAX_POINTS_LOADED || budget == 0 {
                    break;
                }
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if size > budget {
                    break;
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Some(rows) = self.points_for(&path) {
                    budget = budget.saturating_sub(size);
                    points.extend(
                        rows.iter()
                            .filter(|p| p.ts >= from && p.ts <= to)
                            .cloned(),
                    );
                    seen.push(name);
                }
            }
            sources.files = seen;
            points.sort_by(|a, b| a.ts.total_cmp(&b.ts));
            points.truncate(MAX_POINTS_LOADED);
            sources.records = points.len();
            sources.collector_points = points.len();
            sources.oldest_ts = points.first().map(|p| p.ts);
            sources.newest_ts = points.last().map(|p| p.ts);
            return (points, sources);
        }
        sources.tier = "none".to_string();
        sources.reason = Some(format!(
            "no {} tier on disk under {}",
            tiers.join(" / "),
            self.logs_dir.display()
        ));
        (Vec::new(), sources)
    }

    /// The oldest window start any of `tiers` has on disk, without parsing a
    /// single line — a directory listing is enough to answer "data starts …".
    pub fn oldest_available_ts(&self, tiers: &[&str]) -> Option<f64> {
        let mut oldest: Option<f64> = None;
        for tier in tiers {
            let dir = if *tier == "raw" {
                self.logs_dir.join("raw")
            } else if tier.starts_with("tier") {
                self.logs_dir.join(tier)
            } else {
                self.logs_dir.clone()
            };
            let files = crate::series::jsonl_files(&dir);
            if let Some((_, start, _)) = files.last() {
                oldest = Some(match oldest {
                    Some(v) => v.min(*start),
                    None => *start,
                });
            }
        }
        oldest
    }

    /// Whole-file points, cached on (path, mtime, size).
    fn points_for(&self, path: &Path) -> Option<Arc<Vec<Point>>> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok();
        let size = meta.len();

        if let Ok(mut guard) = self.cache.lock() {
            guard.retain(|e| e.at.elapsed() < CACHE_TTL);
            if let Some(e) = guard
                .iter()
                .find(|e| e.path == path && e.mtime == mtime && e.size == size)
            {
                return Some(Arc::clone(&e.points));
            }
        }

        let text = std::fs::read_to_string(path).ok()?;
        let mut points = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue; // a line the collector was still writing
            }
            let Ok(rec) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(p) = Point::from_record(&rec) {
                points.push(p);
            }
        }
        let points = Arc::new(points);
        if let Ok(mut guard) = self.cache.lock() {
            guard.retain(|e| e.at.elapsed() < CACHE_TTL);
            if guard.len() >= CACHE_MAX_ENTRIES {
                guard.remove(0);
            }
            guard.push(CacheEntry {
                path: path.to_path_buf(),
                mtime,
                size,
                at: Instant::now(),
                points: Arc::clone(&points),
            });
        }
        Some(points)
    }
}

/// Ring + collector, merged. One instance per process, shared by the handlers.
pub struct History {
    pub ring: Arc<Mutex<Ring>>,
    pub collector: CollectorHistory,
}

impl History {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self {
            ring: Arc::new(Mutex::new(Ring::new(RING_CAP))),
            collector: CollectorHistory::new(logs_dir),
        }
    }

    /// Push one snapshot into the ring (called from the sampler thread).
    pub fn push(&self, snap: &Value) -> bool {
        match self.ring.lock() {
            Ok(mut r) => r.push_snapshot(snap),
            Err(_) => false,
        }
    }

    /// Points in `[from, to]` from every source that has them, oldest first.
    ///
    /// A ring point and a collector record describing the same second are
    /// merged field-by-field rather than one silently replacing the other.
    pub fn points(
        &self,
        from: f64,
        to: f64,
        range_s: f64,
        tiers: &[&str],
    ) -> (Vec<Point>, Sources) {
        let (collector_points, mut sources) = self.collector.load(from, to, range_s, tiers);
        let (mut ring_points, ring_span) = match self.ring.lock() {
            Ok(r) => (r.between(from, to), r.span_s()),
            Err(_) => (Vec::new(), None),
        };
        sources.ring_points = ring_points.len();
        sources.ring_span_s = ring_span.map(round1);

        ring_points.sort_by(|a, b| a.ts.total_cmp(&b.ts));
        let mut merged: Vec<Point> = Vec::with_capacity(ring_points.len() + collector_points.len());
        let (mut i, mut j) = (0usize, 0usize);
        while i < ring_points.len() || j < collector_points.len() {
            let take_ring = match (ring_points.get(i), collector_points.get(j)) {
                (Some(r), Some(c)) => {
                    if (r.ts - c.ts).abs() < 0.5 {
                        // Same instant from both sources: merge, then advance.
                        let mut p = r.clone();
                        p.fill_from(c);
                        i += 1;
                        j += 1;
                        merged.push(p);
                        continue;
                    }
                    r.ts <= c.ts
                }
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_ring {
                merged.push(ring_points[i].clone());
                i += 1;
            } else {
                merged.push(collector_points[j].clone());
                j += 1;
            }
        }
        (merged, sources)
    }
}

/// Merge two sorted point lists (a dense recent source and a coarse older one),
/// keeping one point per instant and preferring `dense` when both describe the
/// same second.
pub fn merge(dense: &[Point], coarse: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(dense.len() + coarse.len());
    let (mut i, mut j) = (0usize, 0usize);
    while i < dense.len() || j < coarse.len() {
        let take_dense = match (dense.get(i), coarse.get(j)) {
            (Some(d), Some(c)) => {
                if (d.ts - c.ts).abs() < 0.5 {
                    let mut p = d.clone();
                    p.fill_from(c);
                    i += 1;
                    j += 1;
                    out.push(p);
                    continue;
                }
                d.ts <= c.ts
            }
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_dense {
            out.push(dense[i].clone());
            i += 1;
        } else {
            out.push(coarse[j].clone());
            j += 1;
        }
    }
    out
}

/// Bucket-average `points` into at most `cap` buckets of `bucket_s` seconds,
/// anchored at `from`. Empty buckets are skipped — a chart draws a gap.
///
/// Each bucket carries the mean of every field plus `samples`, and the bucket
/// timestamp is its centre, so a downsampled chart lines up with its labels.
pub fn bucketize(points: &[Point], from: f64, bucket_s: f64, cap: usize) -> Vec<Bucket> {
    if points.is_empty() || bucket_s <= 0.0 || cap == 0 {
        return Vec::new();
    }
    let mut out: Vec<Bucket> = Vec::new();
    for p in points {
        if p.ts < from {
            continue;
        }
        let idx = (((p.ts - from) / bucket_s).floor() as usize).min(cap - 1);
        if out.len() <= idx {
            while out.len() < idx {
                out.push(Bucket {
                    ts: from + out.len() as f64 * bucket_s,
                    ..Bucket::default()
                });
            }
            out.push(Bucket {
                ts: from + idx as f64 * bucket_s,
                ..Bucket::default()
            });
        }
        out[idx].add(p);
    }
    out.into_iter().filter(|b| b.samples > 0).collect()
}

/// One averaged bucket of history.
#[derive(Debug, Clone, Default)]
pub struct Bucket {
    pub ts: f64,
    pub samples: u64,
    cpu_sum: f64,
    cpu_n: u64,
    ram_sum: f64,
    ram_n: u64,
    down_sum: f64,
    down_n: u64,
    up_sum: f64,
    up_n: u64,
    load_sum: f64,
    load_n: u64,
    proc_sum: f64,
    proc_n: u64,
    temp_sum: f64,
    temp_n: u64,
    temps_sum: Vec<f64>,
    temps_n: Vec<u64>,
}

impl Bucket {
    fn add(&mut self, p: &Point) {
        self.samples += 1;
        macro_rules! acc {
            ($v:expr, $sum:ident, $n:ident) => {
                if let Some(v) = $v {
                    self.$sum += v;
                    self.$n += 1;
                }
            };
        }
        acc!(p.cpu, cpu_sum, cpu_n);
        acc!(p.ram, ram_sum, ram_n);
        acc!(p.down_bps, down_sum, down_n);
        acc!(p.up_bps, up_sum, up_n);
        acc!(p.load1, load_sum, load_n);
        acc!(p.proc_count, proc_sum, proc_n);
        acc!(p.temp_c, temp_sum, temp_n);
        for (i, t) in p.temps.iter().enumerate() {
            if self.temps_sum.len() <= i {
                self.temps_sum.resize(i + 1, 0.0);
                self.temps_n.resize(i + 1, 0);
            }
            self.temps_sum[i] += t;
            self.temps_n[i] += 1;
        }
    }

    fn mean(sum: f64, n: u64) -> Option<f64> {
        (n > 0).then(|| round2(sum / n as f64))
    }

    pub fn cpu(&self) -> Option<f64> {
        Self::mean(self.cpu_sum, self.cpu_n)
    }
    pub fn ram(&self) -> Option<f64> {
        Self::mean(self.ram_sum, self.ram_n)
    }
    pub fn down_bps(&self) -> Option<f64> {
        Self::mean(self.down_sum, self.down_n)
    }
    pub fn up_bps(&self) -> Option<f64> {
        Self::mean(self.up_sum, self.up_n)
    }
    pub fn load1(&self) -> Option<f64> {
        Self::mean(self.load_sum, self.load_n)
    }
    pub fn proc_count(&self) -> Option<f64> {
        Self::mean(self.proc_sum, self.proc_n)
    }
    pub fn temp_c(&self) -> Option<f64> {
        Self::mean(self.temp_sum, self.temp_n)
    }

    /// Per-sensor means, `None` for a sensor missing in this bucket.
    pub fn temps_mean(&self) -> Vec<Option<f64>> {
        self.temps_sum
            .iter()
            .zip(self.temps_n.iter())
            .map(|(sum, n)| (*n > 0).then(|| round2(sum / *n as f64)))
            .collect()
    }
}

/// Rounded to one decimal.
pub fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

pub fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Local UTC offset (seconds east of UTC) in force at `ts`.
///
/// One offset per window is deliberate: axis ticks are aligned on the wall
/// clock as it stood at the start of the window. A window that straddles a DST
/// change gets labels shifted by an hour rather than a misaligned ladder, and
/// the epoch values (what the chart actually plots) are never affected.
#[cfg(unix)]
pub fn local_offset_secs(ts: f64) -> i64 {
    unsafe {
        let t = ts as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
pub fn local_offset_secs(_ts: f64) -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(json_text: &str) -> Value {
        serde_json::from_str(json_text).unwrap()
    }

    #[test]
    fn range_names_match_the_python_table() {
        assert_eq!(range_secs("1h"), Some(3_600.0));
        assert_eq!(range_secs("7d"), Some(604_800.0));
        assert_eq!(range_secs("nope"), None);
        assert_eq!(range_for_secs(30.0), "1m");
        assert_eq!(range_for_secs(3_600.0), "1h");
        assert_eq!(range_for_secs(0.0), "1h");
        assert_eq!(range_for_secs(9e9), "1y");
    }

    #[test]
    fn record_points_carry_the_measured_fields() {
        let r = rec(
            r#"{"timestamp":"2026-11-19T05:42:04Z",
                "cpu":{"total_pct":20.3},
                "memory":{"pct":69.1},
                "temperature":51.85,
                "process_count":431,
                "energy":{"attributed_w":1.4},
                "networks":[{"name":"en0","rx_bps":5000.0,"tx_bps":700.0},
                            {"name":"lo0","rx_bps":999999.0,"tx_bps":999999.0}]}"#,
        );
        let p = Point::from_record(&r).expect("point");
        assert_eq!(p.ts, 1_795_066_924.0);
        assert_eq!(p.cpu, Some(20.3));
        assert_eq!(p.ram, Some(69.1));
        assert_eq!(p.temp_c, Some(51.85));
        assert_eq!(p.proc_count, Some(431.0));
        assert_eq!(p.attributed_w, Some(1.4));
        // loopback is excluded, exactly like /api/current's net block
        assert_eq!(p.down_bps, Some(5000.0));
        assert_eq!(p.up_bps, Some(700.0));
        assert_eq!(p.battery_pct, None);
    }

    #[test]
    fn a_record_without_a_timestamp_is_dropped() {
        assert!(Point::from_record(&rec(r#"{"cpu":{"total_pct":1.0}}"#)).is_none());
    }

    #[test]
    fn ring_is_monotonic_and_capped() {
        let mut ring = Ring::new(3);
        for (i, ts) in ["2026-11-19T05:00:00Z", "2026-11-19T05:00:01Z", "2026-11-19T05:00:02Z", "2026-11-19T05:00:03Z"]
            .iter()
            .enumerate()
        {
            let snap = json!({"ts": ts, "cpu": {"total_pct": i as f64}});
            assert!(ring.push_snapshot(&snap));
        }
        assert_eq!(ring.len(), 3);
        let pts = ring.between(0.0, f64::MAX);
        assert_eq!(pts.len(), 3);
        assert!(pts.windows(2).all(|w| w[0].ts < w[1].ts));
        // A clock that steps backwards must not append.
        assert!(!ring.push_snapshot(&json!({"ts": "2026-11-19T05:00:02Z"})));
        assert_eq!(ring.skipped(), 1);
    }

    #[test]
    fn buckets_average_and_skip_empty_ones() {
        let mut pts: Vec<Point> = Vec::new();
        for i in 0..4 {
            pts.push(Point {
                ts: 100.0 + i as f64,
                cpu: Some(10.0 * (i as f64)),
                ..Point::default()
            });
        }
        // A cluster 1000 s later: the middle must be a gap, not a line.
        pts.push(Point {
            ts: 1_100.0,
            cpu: Some(99.0),
            ..Point::default()
        });
        let out = bucketize(&pts, 100.0, 1.0, 20);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0].cpu(), Some(0.0));
        assert_eq!(out[3].cpu(), Some(30.0));
        assert_eq!(out[4].cpu(), Some(99.0));
    }

    #[test]
    fn bucket_means_ignore_missing_fields() {
        let pts = vec![
            Point {
                ts: 10.0,
                cpu: Some(10.0),
                ..Point::default()
            },
            Point {
                ts: 11.0,
                cpu: None,
                ..Point::default()
            },
        ];
        let out = bucketize(&pts, 10.0, 60.0, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cpu(), Some(10.0), "a missing sample must not pull the mean to 0");
        assert_eq!(out[0].ram(), None);
    }

    #[test]
    fn tier_choice_is_coarser_as_the_window_grows() {
        assert_eq!(tiers_for(60.0), &["raw"]);
        assert_eq!(tiers_for(3_600.0), &["raw"]);
        assert_eq!(tiers_for(21_600.0), &["tier60s", "raw"]);
        assert_eq!(tiers_for(86_400.0), &["tier300s", "tier60s", "raw"]);
        assert_eq!(tiers_for(2_592_000.0), &["tier1800s", "tier300s", "raw"]);
    }

    #[test]
    fn fmt_span_ladder() {
        assert_eq!(fmt_span(90.0), "90 s");
        assert_eq!(fmt_span(900.0), "15 min");
        assert_eq!(fmt_span(5_400.0), "90 min");
        assert_eq!(fmt_span(9_000.0), "2.5 h");
        assert_eq!(fmt_span(259_200.0), "3.0 d");
        assert_eq!(fmt_span(0.0), "0 s");
    }
}
