//! Reader for the Rust collector's daily JSONL log — the same file
//! `dashboard/server.py` tails in `_latest_collector_snapshot()`.
//!
//! Two rules carried over from the Python server, both measured in:
//!
//! 1. **Tail, never full read.** The live file is multi-megabyte by mid-day;
//!    only the newest 1 MB is read, and the last complete JSON line wins.
//! 2. **Cache on (path, mtime, size) with a TTL.** The collector appends every
//!    ~5 s, so an mtime-only cache would re-parse 1 MB on nearly every 1 s
//!    poll. The TTL used here is the store crate's own 15 s tier, which is the
//!    cadence the collector publishes its aggregated records at.
//!
//! The log directory is optional: a machine where the collector never ran must
//! still serve `/api/current` (the agent's own sensors are the primary source;
//! the log only fills in fields the unprivileged path cannot see).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use syswatch_store::Resolution;

/// How much of the file tail is searched for the last line.
const TAIL_BYTES: u64 = 1_000_000;

/// Cache lifetime, in seconds, taken from the store's 15 s rollup tier.
fn ttl() -> Duration {
    Duration::from_millis(Resolution::Tier15s.interval_ms())
}

#[derive(Debug, Clone)]
pub struct CollectorRecord {
    pub path: PathBuf,
    /// `timestamp` from the record itself (`"2026-11-19T05:42:00Z"`), when the
    /// collector wrote one.
    pub ts: Option<String>,
    /// Age of the *record*, from its own `timestamp` field. `None` when the
    /// collector wrote no timestamp or it does not parse — never "now".
    pub age_s: Option<f64>,
    /// Age of the *file*, from its mtime. Present even when the record carries
    /// no timestamp: an old file is a stale file.
    pub file_age_s: Option<f64>,
    pub value: Value,
}

impl CollectorRecord {
    /// One field out of the collector snapshot, if present and finite.
    pub fn num(&self, path: &[&str]) -> Option<f64> {
        let mut cur = &self.value;
        for key in path {
            cur = cur.get(*key)?;
        }
        cur.as_f64()
    }
}

struct Cached {
    at: Instant,
    key: (PathBuf, Option<SystemTime>, u64),
    record: Option<CollectorRecord>,
}

pub struct CollectorLog {
    dir: PathBuf,
    cache: Mutex<Option<Cached>>,
}

impl CollectorLog {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cache: Mutex::new(None),
        }
    }

    /// Newest `*.jsonl` in the log dir with its last JSON line parsed.
    ///
    /// `None` means "no collector log on this machine (yet)" — a normal state
    /// on a fresh install, not an error.
    pub fn latest(&self) -> Option<CollectorRecord> {
        let path = newest_log(&self.dir)?;
        let meta = std::fs::metadata(&path).ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta.as_ref().and_then(|m| m.modified().ok());
        let key = (path.clone(), mtime, size);

        if let Ok(guard) = self.cache.lock()
            && let Some(c) = guard.as_ref()
            && c.key == key
            && c.at.elapsed() < ttl()
        {
            return c.record.clone();
        }

        let record = read_last_line(&path, size).map(|value| {
            let ts = value
                .get("timestamp")
                .and_then(Value::as_str)
                .map(str::to_string);
            let age_s = ts
                .as_deref()
                .and_then(crate::timefmt::parse_rfc3339)
                .map(|t| round1(crate::timefmt::now_unix() as f64 - t));
            CollectorRecord {
                ts,
                age_s,
                file_age_s: mtime
                    .and_then(|m| SystemTime::now().duration_since(m).ok())
                    .map(|d| round1(d.as_secs_f64())),
                path: path.clone(),
                value,
            }
        });

        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(Cached {
                at: Instant::now(),
                key,
                record: record.clone(),
            });
        }
        record
    }
}

/// Sorted `*.jsonl` names in `dir`, newest last — the same ordering rule the
/// Python server uses (the collector rotates by date, so name order is time
/// order).
fn newest_log(dir: &Path) -> Option<PathBuf> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    names.sort();
    names.pop().map(|n| dir.join(n))
}

/// Last complete JSON line of the file's tail.
///
/// A partially-written trailing line (the collector appends while we read) is
/// simply skipped: the loop keeps the last line that parses, and a truncated
/// tail yields the previous record rather than an error.
fn read_last_line(path: &Path, size: u64) -> Option<Value> {
    let mut file = File::open(path).ok()?;
    let start = size.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity(TAIL_BYTES.min(size) as usize);
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    // BTreeMap keyed by nothing useful — we keep the last value that parses.
    let mut last: Option<Value> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue; // first fragment of a cut-line tail
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            last = Some(v);
        }
    }
    last
}

/// Flatten one collector record's numeric leaves, so `/api/current` can show
/// exactly which collector fields backed its numbers. Kept as a helper for the
/// tests and for future endpoints (`/api/power` reads `energy.*`).
#[allow(dead_code)]
pub fn numeric_leaves(v: &Value) -> BTreeMap<String, f64> {
    fn walk(prefix: &str, v: &Value, out: &mut BTreeMap<String, f64>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&key, child, out);
                }
            }
            Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    out.insert(prefix.to_string(), f);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    walk("", v, &mut out);
    out
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "syswatch-agent-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn last_line_wins_and_truncated_tail_is_skipped() {
        let dir = tmpdir("collector");
        let path = dir.join("2026-11-19.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-11-19T05:41:00Z","cpu":{{"total_pct":10.0}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-11-19T05:42:00Z","cpu":{{"total_pct":21.5}}}}"#).unwrap();
        // A half-written trailing record — must not win, must not error.
        write!(f, r#"{{"timestamp":"2026-11-19T05:43:00Z","cpu":{{"total_pct":"#).unwrap();
        f.flush().unwrap();
        drop(f);

        let log = CollectorLog::new(dir.clone());
        let rec = log.latest().expect("a record must be found");
        assert_eq!(rec.ts.as_deref(), Some("2026-11-19T05:42:00Z"));
        assert_eq!(rec.num(&["cpu", "total_pct"]), Some(21.5));
        // The record is from 2026-11-19: its data age is real, and the file
        // age comes from mtime. Both are present and neither is "now".
        assert!(rec.age_s.expect("data age") > 0.0);
        assert!(rec.file_age_s.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dir_is_none_not_an_error() {
        let log = CollectorLog::new(PathBuf::from("/nonexistent/syswatch/logs"));
        assert!(log.latest().is_none());
    }

    #[test]
    fn newest_by_name() {
        let dir = tmpdir("newest");
        for name in ["2026-11-17.jsonl", "2026-11-19.jsonl", "2026-11-18.jsonl"] {
            std::fs::write(dir.join(name), "{\"timestamp\":\"x\"}\n").unwrap();
        }
        assert_eq!(
            newest_log(&dir).unwrap().file_name().unwrap(),
            "2026-11-19.jsonl"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn numeric_leaves_flattens() {
        let v: Value = serde_json::from_str(r#"{"a":{"b":1.5,"c":"x"},"d":2}"#).unwrap();
        let leaves = numeric_leaves(&v);
        assert_eq!(leaves.get("a.b"), Some(&1.5));
        assert_eq!(leaves.get("d"), Some(&2.0));
        assert!(!leaves.contains_key("a.c"));
    }
}
