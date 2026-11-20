//! `/api/series` — one metric over a time range, read from the collector's
//! JSONL history.
//!
//! The on-disk layout this reads (verified on this machine):
//!
//! ```text
//! ~/Library/Application Support/syswatch/logs/
//!   2026-11-19.jsonl             15 s tier records (daily file, no processes)
//!   raw/2026-11-19T05-30.jsonl   1 s raw records, rotated every 15 min
//!   tier15s/ tier60s/ …          rollup tiers, same record shape
//! ```
//!
//! `raw/` is preferred when it has files, because it is the finest resolution
//! and it is the only tier that carries a `processes` block (which
//! `/api/consumers` needs). Everything else falls back to the daily 15 s file.
//!
//! Rules, carried over from the Python reader:
//!
//! 1. **Bounded work.** At most [`MAX_FILES`] files are opened per request and
//!    parsing is cached for [`CACHE_TTL`]. A 1 s dashboard poll must not
//!    re-parse 6 MB of JSON every second.
//! 2. **No invented points.** Empty time buckets are dropped, never
//!    interpolated. A metric the record does not carry yields no points (and a
//!    `reason` in the payload), not zeros.
//! 3. **Seconds, not milliseconds.** `from`/`to` and every `ts` are unix
//!    seconds as floats, matching `time.time()` on the Python side.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use syswatch_sensors_macos::slug;

use crate::timefmt;

/// A raw file covers 15 minutes; a daily tier file covers a whole day.
const RAW_WINDOW_S: f64 = 900.0;
const DAILY_WINDOW_S: f64 = 86_400.0;
/// Newest-first cap. Four raw files is an hour of 1 s data — enough to fill any
/// chart without turning one request into a directory scan of 24 files.
const MAX_FILES: usize = 4;
/// Parsed-file cache. The collector rewrites a file for its whole 15 min
/// window, so a 10 s TTL still means "one parse per window per metric".
const CACHE_TTL: Duration = Duration::from_secs(10);
const CACHE_MAX_ENTRIES: usize = 12;

#[derive(Debug, Clone)]
pub struct SeriesRequest {
    pub metric: String,
    pub from: f64,
    pub to: f64,
    pub max_points: usize,
}

struct CacheEntry {
    path: PathBuf,
    mtime: Option<std::time::SystemTime>,
    size: u64,
    metric: String,
    at: Instant,
    points: Arc<Vec<(f64, f64)>>,
}

pub struct SeriesReader {
    logs_dir: PathBuf,
    cache: Mutex<Vec<CacheEntry>>,
}

impl SeriesReader {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self {
            logs_dir,
            cache: Mutex::new(Vec::new()),
        }
    }

    /// Build the payload for one query. Never fails: an unknown metric or an
    /// empty window is a 200 with `empty: true` and a reason, because "no data"
    /// is a valid answer the UI must render.
    pub fn series(&self, req: &SeriesRequest) -> Value {
        let (dir, resolution) = self.source();
        let files = jsonl_files(&dir);

        let mut used_files: Vec<String> = Vec::new();
        let mut collected: Vec<(f64, f64)> = Vec::new();

        for (path, start, window) in &files {
            // Newest-first. A file ending before `from` means every older file
            // ends before it too, so this is the stop point.
            if *start + *window < req.from {
                break;
            }
            if *start > req.to {
                continue;
            }
            if used_files.len() >= MAX_FILES {
                break;
            }
            if let Some(points) = self.points_for(path, &req.metric) {
                used_files.push(
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                );
                collected.extend(
                    points
                        .iter()
                        .copied()
                        .filter(|(ts, _)| *ts >= req.from && *ts <= req.to),
                );
            }
        }

        collected.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let raw_count = collected.len();
        let path = metric_path(&req.metric);
        let points = downsample(&collected, req.max_points);
        let actual_range = if collected.is_empty() {
            None
        } else {
            Some((collected[0].0, collected[collected.len() - 1].0))
        };

        let mut out = json!({
            "metric": req.metric,
            "path": path,
            "unit": unit_for(&req.metric),
            "from": req.from,
            "to": req.to,
            "max_points": req.max_points,
            "raw_count": raw_count,
            "count": points.len(),
            "empty": points.is_empty(),
            "resolution": "raw",
            "source": {
                "dir": dir.display().to_string(),
                "tier": resolution,
                "format": "collector JSONL (one record per line, timestamp + nested metrics)",
                "files": used_files,
                "max_files": MAX_FILES,
            },
            "points": points,
        });

        if let Some((a, b)) = actual_range {
            out["actual_from"] = json!(round3(a));
            out["actual_to"] = json!(round3(b));
        }
        if out["empty"] == json!(true) {
            out["reason"] = json!(if used_files.is_empty() {
                "no collector log file overlaps this time range on this machine"
            } else {
                "the collector logs in range do not carry this metric (see `path`)"
            });
        }
        out
    }

    /// `raw/` when it holds JSONL, else the log directory itself.
    fn source(&self) -> (PathBuf, &'static str) {
        let raw = self.logs_dir.join("raw");
        if !jsonl_files(&raw).is_empty() {
            return (raw, "raw");
        }
        (self.logs_dir.clone(), "tier")
    }

    /// Whole-file points for one metric, cached on (path, mtime, size, metric).
    fn points_for(&self, path: &Path, metric: &str) -> Option<Arc<Vec<(f64, f64)>>> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok();
        let size = meta.len();

        if let Ok(mut guard) = self.cache.lock() {
            guard.retain(|e| e.at.elapsed() < CACHE_TTL);
            if let Some(e) = guard.iter().find(|e| {
                e.path == path && e.mtime == mtime && e.size == size && e.metric == metric
            }) {
                return Some(Arc::clone(&e.points));
            }
        }

        let text = std::fs::read_to_string(path).ok()?;
        let mut points = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue; // fragment of a line the collector was writing
            }
            let Ok(rec) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(ts) = record_ts(&rec) else { continue };
            if let Some(v) = extract(metric, &rec) {
                points.push((ts, v));
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
                metric: metric.to_string(),
                at: Instant::now(),
                points: Arc::clone(&points),
            });
        }
        Some(points)
    }
}

/// `*.jsonl` files with their window start, newest first. Shared with
/// `histdata`, which walks the same tiers for graph/history/analytics.
pub(crate) fn jsonl_files(dir: &Path) -> Vec<(PathBuf, f64, f64)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, f64, f64)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let (start, window) = file_window(&name)?;
            Some((p, start, window))
        })
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Window start + width from a collector file name.
///
/// `2026-11-19T05-30.jsonl` → 05:30Z + 15 min; `2026-11-19.jsonl` → midnight +
/// 24 h. Anything else is ignored rather than guessed.
fn file_window(name: &str) -> Option<(f64, f64)> {
    let stem = name.strip_suffix(".jsonl")?;
    if let Some((date, hm)) = stem.split_once('T') {
        let (h, m) = hm.split_once('-')?;
        let ts = timefmt::parse_rfc3339(&format!("{date}T{h}:{m}:00Z"))?;
        return Some((ts, RAW_WINDOW_S));
    }
    let ts = timefmt::parse_rfc3339(&format!("{stem}T00:00:00Z"))?;
    Some((ts, DAILY_WINDOW_S))
}

fn record_ts(rec: &Value) -> Option<f64> {
    match rec.get("timestamp").or_else(|| rec.get("ts"))? {
        Value::Number(n) => {
            let v = n.as_f64()?;
            // Milliseconds are recognisable: no unix second count is 1e12 yet.
            Some(if v > 1e12 { v / 1000.0 } else { v })
        }
        Value::String(s) => timefmt::parse_rfc3339(s),
        _ => None,
    }
}

// ── metric → value ──────────────────────────────────────────────────────────

/// The JSON path this metric resolves through, for the payload's `path` field.
pub fn metric_path(metric: &str) -> Vec<String> {
    if let Some(path) = alias(metric) {
        return path.iter().map(|s| s.to_string()).collect();
    }
    if let Some(path) = array_path(metric) {
        return path;
    }
    metric.split('.').map(str::to_string).collect()
}

/// Read one metric out of one collector record, in seconds-accurate terms.
pub fn extract(metric: &str, rec: &Value) -> Option<f64> {
    let parts: Vec<&str> = metric.split('.').collect();

    // 1. The id as written may already be the exact JSON path (raw files carry
    //    `cpu.total_pct`, `memory.used_bytes`, `temperature`).
    if let Some(v) = walk(rec, &parts) {
        return Some(v);
    }
    // 2. Manifest ids are not collector field names; translate the documented
    //    ones explicitly so `?metric=cpu.total` works.
    if let Some(path) = alias(metric)
        && let Some(v) = walk(rec, path)
    {
        return Some(v);
    }
    // 3. Array-valued blocks: `net.en0.rx_bytes_sec`, `disk.root.used_pct`.
    if let Some(v) = array_value(metric, rec) {
        return Some(v);
    }
    // 4. Last resort: the same path with the unit suffix the collector uses.
    for suffix in ["_pct", "_bytes", "_bps"] {
        let mut alt: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
        if let Some(last) = alt.last_mut() {
            last.push_str(suffix);
        }
        let refs: Vec<&str> = alt.iter().map(String::as_str).collect();
        if let Some(v) = walk(rec, &refs) {
            return Some(v);
        }
    }
    None
}

fn walk(rec: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = rec;
    for key in path {
        cur = cur.get(*key)?;
    }
    match cur {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Manifest id → collector field path.
fn alias(metric: &str) -> Option<&'static [&'static str]> {
    Some(match metric {
        "cpu.total" | "cpu.pct" | "cpu.usage" => &["cpu", "total_pct"],
        "cpu.load_1m" | "cpu.load1" | "cpu.load" => &["cpu", "load1"],
        "cpu.freq_mhz" => &["cpu", "freq_mhz"],
        "memory.used" => &["memory", "used_bytes"],
        "memory.total" => &["memory", "total_bytes"],
        "memory.swap_used" => &["memory", "swap_used_bytes"],
        "memory.used_pct" | "memory.pct" => &["memory", "pct"],
        "temperature" | "temp.cpu.die" | "temp.cpu" => &["temperature"],
        "process_count" | "processes.count" => &["process_count"],
        "uptime_secs" | "uptime" => &["uptime_secs"],
        "energy.attributed_w" | "power.attributed_w" => &["energy", "attributed_w"],
        _ => return None,
    })
}

/// `net.<iface>.<field>` / `disk.<slug>.<field>` → the matching array element.
fn array_value(metric: &str, rec: &Value) -> Option<f64> {
    let path = array_path(metric)?;
    // path is [block, key, field] with the block name as written.
    let (block, key, field) = (path[0].as_str(), path[1].as_str(), path[2].as_str());

    if block == "net" || block == "networks" {
        let field = match field {
            "rx_bytes_sec" | "rx_bps" => "rx_bps",
            "tx_bytes_sec" | "tx_bps" => "tx_bps",
            other => other,
        };
        return array_field(rec.get("networks")?, "name", key, field);
    }
    if block == "disk" || block == "disks" {
        let want = slug(key);
        for row in rec.get("disks")?.as_array()? {
            let mount = row.get("mount").and_then(Value::as_str)?;
            if slug(mount) == want {
                return row.get(field).and_then(Value::as_f64);
            }
        }
    }
    None
}

/// The three-part shape of an array metric, or `None`.
fn array_path(metric: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = metric.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    if !matches!(parts[0], "net" | "networks" | "disk" | "disks") {
        return None;
    }
    Some(parts.iter().map(|s| s.to_string()).collect())
}

fn array_field(rows: &Value, key_name: &str, want: &str, field: &str) -> Option<f64> {
    for row in rows.as_array()? {
        if row.get(key_name).and_then(Value::as_str) == Some(want) {
            return row.get(field).and_then(Value::as_f64);
        }
    }
    None
}

/// Percent / bytes / rate / celsius / count — enough for the UI to label the
/// series without knowing the collector's field names.
pub fn unit_for(metric: &str) -> &'static str {
    if metric.ends_with("_pct") || metric.ends_with(".pct") || metric.ends_with(".usage") {
        return "%";
    }
    if metric.ends_with("_bytes_sec") || metric.ends_with(".rx_bps") || metric.ends_with(".tx_bps") {
        return "B/s";
    }
    if metric.ends_with("_bytes") || metric.ends_with(".bytes") || metric.starts_with("memory.") {
        return "bytes";
    }
    if metric.starts_with("temp.") || metric == "temperature" {
        return "C";
    }
    if metric.starts_with("energy.") || metric.starts_with("power.") {
        return "W";
    }
    "n"
}

// ── downsampling ────────────────────────────────────────────────────────────

/// Reduce to at most `max_points` buckets, preserving min/max/count.
///
/// Under the limit every raw sample is returned as its own point. Over it, the
/// window is split into equal-time buckets and empty buckets are dropped — the
/// chart draws a gap rather than a straight line through missing data.
fn downsample(points: &[(f64, f64)], max_points: usize) -> Vec<Value> {
    if points.is_empty() || max_points == 0 {
        return Vec::new();
    }
    if points.len() <= max_points {
        return points
            .iter()
            .map(|(ts, v)| {
                let v = round3(*v);
                json!({ "ts": round3(*ts), "value": v, "min": v, "max": v, "avg": v, "count": 1 })
            })
            .collect();
    }

    let first = points[0].0;
    let last = points[points.len() - 1].0;
    let width = ((last - first) / max_points as f64).max(1e-6);

    // bucket → (min, max, sum, count)
    let mut buckets: Vec<(f64, f64, f64, u64)> = Vec::with_capacity(max_points);
    for (ts, v) in points {
        let idx = (((ts - first) / width).floor() as usize).min(max_points - 1);
        if idx == buckets.len() {
            buckets.push((*v, *v, *v, 1));
        } else if idx > buckets.len() {
            // A gap: pad with empty slots that will not be emitted.
            while buckets.len() < idx {
                buckets.push((f64::INFINITY, f64::NEG_INFINITY, 0.0, 0));
            }
            buckets.push((*v, *v, *v, 1));
        } else {
            let b = &mut buckets[idx];
            b.0 = b.0.min(*v);
            b.1 = b.1.max(*v);
            b.2 += *v;
            b.3 += 1;
        }
    }

    buckets
        .iter()
        .enumerate()
        .filter(|(_, b)| b.3 > 0)
        .map(|(i, b)| {
            let avg = round3(b.2 / b.3 as f64);
            json!({
                "ts": round3(first + i as f64 * width),
                "value": avg,
                "min": round3(b.0),
                "max": round3(b.1),
                "avg": avg,
                "count": b.3,
            })
        })
        .collect()
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> Value {
        serde_json::from_str(
            r#"{
                "timestamp": "2026-11-19T05:30:00Z",
                "uptime_secs": 135014,
                "cpu": {"total_pct": 30.194984, "freq_mhz": 3504},
                "memory": {"used_bytes": 6155337728, "total_bytes": 8589934592,
                           "pct": 71.65756225585938, "swap_used_bytes": 550764544},
                "temperature": 51.85,
                "energy": {"attributed_w": 1.411},
                "process_count": 431,
                "disks": [{"mount": "/", "used_pct": 58.28, "free_gb": 95.2}],
                "networks": [{"name": "en0", "rx_bps": 5741.1, "tx_bps": 1388.5}]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn manifest_ids_resolve_to_collector_fields() {
        let r = rec();
        assert_eq!(extract("cpu.total", &r), Some(30.194984));
        assert_eq!(extract("cpu.total_pct", &r), Some(30.194984));
        assert_eq!(extract("memory.pct", &r), Some(71.65756225585938));
        assert_eq!(extract("memory.used", &r), Some(6155337728.0));
        assert_eq!(extract("temperature", &r), Some(51.85));
        assert_eq!(extract("temp.cpu.die", &r), Some(51.85));
        assert_eq!(extract("energy.attributed_w", &r), Some(1.411));
    }

    #[test]
    fn array_metrics_resolve_by_name_and_slug() {
        let r = rec();
        assert_eq!(extract("net.en0.rx_bytes_sec", &r), Some(5741.1));
        assert_eq!(extract("net.en0.tx_bps", &r), Some(1388.5));
        assert_eq!(extract("disk.root.used_pct", &r), Some(58.28));
        assert_eq!(extract("net.en9.rx_bps", &r), None);
    }

    #[test]
    fn unknown_metric_is_none_never_zero() {
        let r = rec();
        assert_eq!(extract("battery.draw_w", &r), None);
        assert_eq!(extract("temp.nope", &r), None);
    }

    #[test]
    fn timestamp_formats() {
        let r = rec();
        // 2026-11-19T05:30:00Z — checked against
        // datetime.fromisoformat("2026-11-19T05:30:00+00:00").timestamp().
        assert_eq!(record_ts(&r), Some(1_795_066_200.0));
        let ms: Value = serde_json::json!({"ts": 1_795_066_200_000i64});
        assert_eq!(record_ts(&ms), Some(1_795_066_200.0));
    }

    #[test]
    fn file_windows_from_names() {
        let (start, w) = file_window("2026-11-19T05-30.jsonl").unwrap();
        assert_eq!(start, 1_795_066_200.0);
        assert_eq!(w, RAW_WINDOW_S);
        let (start, w) = file_window("2026-11-19.jsonl").unwrap();
        assert_eq!(w, DAILY_WINDOW_S);
        assert_eq!(start, 1_795_046_400.0);
        assert!(file_window("readme.txt").is_none());
    }

    #[test]
    fn downsample_keeps_raw_points_under_the_limit() {
        let pts = vec![(100.0, 1.0), (101.0, 2.0), (102.0, 3.0)];
        let out = downsample(&pts, 10);
        assert_eq!(out.len(), 3);
        assert_eq!(out[1]["value"], 2.0);
        assert_eq!(out[1]["count"], 1);
    }

    #[test]
    fn downsample_buckets_with_min_max_avg() {
        let pts: Vec<(f64, f64)> = (0..100).map(|i| (i as f64, i as f64)).collect();
        let out = downsample(&pts, 10);
        assert_eq!(out.len(), 10);
        let first = &out[0];
        assert_eq!(first["count"], 10);
        assert_eq!(first["min"], 0.0);
        assert_eq!(first["max"], 9.0);
        assert_eq!(first["avg"], 4.5);
        // Adjacent buckets share a boundary; totals must still cover all 100.
        let total: u64 = out.iter().map(|p| p["count"].as_u64().unwrap()).sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn downsample_drops_empty_buckets_instead_of_interpolating() {
        // Two dense clusters an hour apart: the middle must be a gap, not a line.
        let mut pts: Vec<(f64, f64)> = (0..50).map(|i| (i as f64, 1.0)).collect();
        pts.extend((0..50).map(|i| (3600.0 + i as f64, 2.0)));
        let out = downsample(&pts, 12);
        assert!(out.len() <= 12);
        assert!(out.iter().all(|p| p["count"].as_u64().unwrap() > 0));
    }
}
