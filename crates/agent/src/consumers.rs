//! `/api/consumers` (alias `/api/apps`) — who is consuming the machine.
//!
//! Source: the newest collector sample's `processes` block. On this machine
//! that block exists **only** in the raw 1 s tier
//! (`logs/raw/YYYY-MM-DDTHH-MM.jsonl`); the daily 15 s file drops it. So the
//! raw directory is read first and the daily file is a fallback.
//!
//! The collector splits the same processes over three ranked lists
//! (`by_cpu`, `by_mem`, `by_energy`) — each list is a different *ranking*, not a
//! different set, and each row carries all three numbers. This reader unions
//! the lists by pid so a row answers "cpu, memory and power for this process"
//! instead of making the caller join three arrays.
//!
//! Honesty rules from the sensor work carry over: a field the sample does not
//! carry is absent/null, never 0, and per-process GPU share is structurally
//! unavailable (no `ri_gpu_time` in this SDK) so it is reported as such.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use syswatch_sensors_macos::slug;

use crate::collector::CollectorLog;

pub struct ConsumerFeed {
    /// `logs/raw` — the only tier with a `processes` block.
    raw: CollectorLog,
    /// `logs` — fallback for an install where raw rotation has not started.
    daily: CollectorLog,
}

impl ConsumerFeed {
    pub fn new(logs_dir: std::path::PathBuf) -> Self {
        Self {
            raw: CollectorLog::new(logs_dir.join("raw")),
            daily: CollectorLog::new(logs_dir),
        }
    }

    /// Newest sample that actually has a `processes` block.
    fn newest_sample(&self) -> Option<crate::collector::CollectorRecord> {
        if let Some(rec) = self.raw.latest()
            && rec.value.get("processes").is_some()
        {
            return Some(rec);
        }
        let rec = self.daily.latest()?;
        rec.value.get("processes").is_some().then_some(rec)
    }

    pub fn consumers(&self, offset: usize, limit: usize) -> Value {
        let Some(rec) = self.newest_sample() else {
            return json!({
                "module": "apps",
                "metric": "cpu_pct",
                "unit": "%",
                "range": "latest",
                "count": 0,
                "returned": 0,
                "offset": offset,
                "limit": limit,
                "rows": [],
                "empty": true,
                "unavailable": unavailable(),
                "reason": "no collector sample on this machine carries a `processes` block yet \
                           (looked in logs/raw/ then logs/)",
            });
        };

        let mut rows = merge_processes(&rec.value);
        // Ranked by CPU, then name — deterministic order for a stable diff.
        rows.sort_by(|a, b| {
            let av = a.get("cpu_pct").and_then(Value::as_f64).unwrap_or(-1.0);
            let bv = b.get("cpu_pct").and_then(Value::as_f64).unwrap_or(-1.0);
            bv.partial_cmp(&av)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
                })
        });

        let count = rows.len();
        let page: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();

        json!({
            "module": "apps",
            "metric": "cpu_pct",
            "unit": "%",
            "range": "latest",
            "timestamp": rec.ts,
            "age_s": rec.age_s,
            "process_count": rec.value.get("process_count").cloned().unwrap_or(Value::Null),
            "count": count,
            "returned": page.len(),
            "offset": offset,
            "limit": limit,
            "empty": page.is_empty(),
            "energy": rec.value.get("energy").cloned().unwrap_or(Value::Null),
            "unavailable": unavailable(),
            "source": {
                "file": rec.path.display().to_string(),
                "read": "last complete JSON line (tail, 1 MB cap)",
                "merged_from": ["processes.by_cpu", "processes.by_mem", "processes.by_energy"],
            },
            "rows": page,
        })
    }
}

/// Union the three ranked process lists by pid, keeping every number each row
/// carries. `memory_mb` is the collector's `mem_mb`, renamed for the API.
fn merge_processes(rec: &Value) -> Vec<Value> {
    #[derive(Default)]
    struct Row {
        name: String,
        pid: i64,
        cpu_pct: Option<f64>,
        memory_mb: Option<f64>,
        threads: Option<f64>,
        watts: Option<f64>,
        energy_nj_delta: Option<f64>,
        energy_share_pct: Option<f64>,
        perm: Option<bool>,
    }

    let Some(processes) = rec.get("processes") else {
        return Vec::new();
    };

    let mut by_pid: BTreeMap<i64, Row> = BTreeMap::new();
    for list in ["by_cpu", "by_mem", "by_energy"] {
        let Some(rows) = processes.get(list).and_then(Value::as_array) else {
            continue;
        };
        for r in rows {
            let Some(pid) = r.get("pid").and_then(Value::as_i64) else {
                continue;
            };
            let row = by_pid.entry(pid).or_default();
            row.pid = pid;
            if row.name.is_empty()
                && let Some(name) = r.get("name").and_then(Value::as_str)
            {
                row.name = name.to_string();
            }
            row.cpu_pct = row.cpu_pct.or_else(|| r.get("cpu_pct").and_then(Value::as_f64));
            row.memory_mb = row
                .memory_mb
                .or_else(|| r.get("mem_mb").and_then(Value::as_f64));
            row.threads = row.threads.or_else(|| r.get("threads").and_then(Value::as_f64));
            row.watts = row.watts.or_else(|| r.get("watts").and_then(Value::as_f64));
            row.energy_nj_delta = row
                .energy_nj_delta
                .or_else(|| r.get("energy_nj_delta").and_then(Value::as_f64));
            row.energy_share_pct = row
                .energy_share_pct
                .or_else(|| r.get("energy_share_pct").and_then(Value::as_f64));
            row.perm = row.perm.or_else(|| r.get("perm").and_then(Value::as_bool));
        }
    }

    by_pid
        .into_values()
        .map(|r| {
            json!({
                "name": if r.name.is_empty() { format!("pid {}", r.pid) } else { r.name },
                "pid": r.pid,
                "cpu_pct": r.cpu_pct,
                "memory_mb": r.memory_mb,
                "threads": r.threads,
                "watts": r.watts,
                "energy_nj_delta": r.energy_nj_delta,
                "energy_share_pct": r.energy_share_pct,
                "perm": r.perm,
            })
        })
        .collect()
}

/// Fields that cannot be measured on this platform, with the reason. Never
/// emitted as 0.
fn unavailable() -> Value {
    json!({
        "gpu_pct": "proc_pid_rusage(RUSAGE_INFO_V6) has no GPU-time field in this SDK \
                    (ri_gpu_time does not exist), so per-process GPU share is null, never 0",
    })
}

/// Kept because the disk/network metric ids in `/api/series` use the same
/// slug rule; a mount point with a space must round-trip identically.
#[allow(dead_code)]
fn mount_slug(mount: &str) -> String {
    slug(mount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "syswatch-agent-consumers-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("raw")).unwrap();
        d
    }

    fn sample() -> Value {
        json!({
            "timestamp": "2026-11-19T05:30:00Z",
            "process_count": 431,
            "energy": {"attributed_w": 1.411, "window_s": 5.986},
            "processes": {
                "by_cpu": [
                    {"pid": 10, "name": "chrome", "cpu_pct": 43.8, "mem_mb": 471.0,
                     "threads": 22, "watts": 0.015, "energy_nj_delta": 90620313,
                     "energy_share_pct": 1.07, "perm": true},
                    {"pid": 20, "name": "find", "cpu_pct": 21.7, "mem_mb": 16.5,
                     "threads": 1, "watts": 1.194, "energy_nj_delta": 7149712459,
                     "energy_share_pct": 84.6, "perm": true}
                ],
                "by_mem": [
                    {"pid": 10, "name": "chrome", "cpu_pct": 43.8, "mem_mb": 471.0},
                    {"pid": 30, "name": "kernel_task", "cpu_pct": 0.4, "mem_mb": 900.0}
                ],
                "by_energy": [
                    {"pid": 20, "name": "find", "cpu_pct": 21.7, "mem_mb": 16.5,
                     "watts": 1.194, "energy_share_pct": 84.6}
                ]
            }
        })
    }

    #[test]
    fn merges_the_three_lists_by_pid() {
        let rows = merge_processes(&sample());
        assert_eq!(rows.len(), 3, "chrome/find/kernel_task, deduped by pid");
        let chrome = rows.iter().find(|r| r["pid"] == 10).unwrap();
        assert_eq!(chrome["name"], "chrome");
        assert_eq!(chrome["cpu_pct"], 43.8);
        assert_eq!(chrome["memory_mb"], 471.0);
        assert_eq!(chrome["threads"], 22);
        let find = rows.iter().find(|r| r["pid"] == 20).unwrap();
        assert_eq!(find["watts"], 1.194);
    }

    #[test]
    fn missing_numbers_are_null_not_zero() {
        let rows = merge_processes(&sample());
        let kernel = rows.iter().find(|r| r["pid"] == 30).unwrap();
        assert!(kernel["watts"].is_null(), "watts must be null, not 0");
        assert!(kernel["energy_nj_delta"].is_null());
        assert_eq!(kernel["memory_mb"], 900.0);
    }

    #[test]
    fn ranks_by_cpu_and_pages_without_losing_the_total() {
        let dir = tmpdir("rank");
        let mut f = std::fs::File::create(dir.join("raw/2026-11-19T05-30.jsonl")).unwrap();
        writeln!(f, "{}", sample()).unwrap();
        f.flush().unwrap();
        drop(f);

        let feed = ConsumerFeed::new(dir.clone());
        let all = feed.consumers(0, 50);
        assert_eq!(all["count"], 3);
        assert_eq!(all["rows"][0]["name"], "chrome");
        assert_eq!(all["rows"][0]["name"], "chrome");
        assert!(all["unavailable"]["gpu_pct"].as_str().unwrap().contains("ri_gpu_time"));

        let page = feed.consumers(1, 1);
        assert_eq!(page["count"], 3, "count is the full total");
        assert_eq!(page["returned"], 1);
        assert_eq!(page["offset"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_log_is_a_clean_empty_answer() {
        let feed = ConsumerFeed::new(PathBuf::from("/nonexistent/syswatch/logs"));
        let out = feed.consumers(0, 50);
        assert_eq!(out["count"], 0);
        assert_eq!(out["rows"].as_array().unwrap().len(), 0);
        assert_eq!(out["empty"], true);
        assert!(out["reason"].as_str().unwrap().contains("processes"));
    }
}
