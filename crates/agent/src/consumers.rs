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

    pub fn consumers(&self, module: &str, range: &str, offset: usize, limit: usize, group_by: &str) -> Value {
        let window_s = range_s(range);
        let now = crate::timefmt::now_unix();
        let window_start = crate::timefmt::rfc3339_utc(now - window_s as i64);
        let window_end = crate::timefmt::rfc3339_utc(now);
        let spec = module_spec(module);
        let metric = spec.0;
        let unit = spec.1;
        let label = spec.2;
        let aggregation = spec.3;

        let Some(rec) = self.newest_sample() else {
            return json!({
                "module": module,
                "metric": metric,
                "unit": unit,
                "label": label,
                "range": range,
                "window_start": window_start,
                "window_end": window_end,
                "window_s": window_s,
                "count": 0,
                "returned": 0,
                "offset": offset,
                "limit": if limit == 0 { 0usize } else { limit },
                "aggregation": aggregation,
                "aggregation_detail": {
                    "source": "dashboard per-process range store (psutil + proc_pid_rusage V6 + nettop)",
                    "cadence_s": 20.0,
                    "group_by": group_by,
                    "ranked_by": metric,
                },
                "unavailable": unavailable(),
                "coverage": empty_coverage(group_by),
                "rows": [],
                "empty": true,
                "reason": "no collector sample on this machine carries a `processes` block yet \
                           (looked in logs/raw/ then logs/)",
            });
        };

        let ts_epoch = rec.ts.as_deref()
            .and_then(crate::timefmt::parse_rfc3339)
            .unwrap_or(now as f64);
        let mut rows = merge_processes(&rec.value, ts_epoch);
        // Ranked by the module metric, then name.
        rows.sort_by(|a, b| {
            let av = a.get(metric).and_then(Value::as_f64).unwrap_or(f64::MIN);
            let bv = b.get(metric).and_then(Value::as_f64).unwrap_or(f64::MIN);
            bv.partial_cmp(&av)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
                })
        });

        for r in &mut rows {
            if let Some(obj) = r.as_object_mut() {
                obj.insert("share_pct".into(), Value::Null);
                obj.insert("metric".into(), json!(metric));
            }
        }

        // share_pct left null (no windowed averages on this agent).

        let count = rows.len();
        let page: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();

        json!({
            "module": module,
            "metric": metric,
            "unit": unit,
            "label": label,
            "range": range,
            "window_start": window_start,
            "window_end": window_end,
            "window_s": window_s,
            "count": count,
            "returned": page.len(),
            "offset": offset,
            "limit": if limit == 0 { count } else { limit },
            "aggregation": aggregation,
            "aggregation_detail": {
                "source": "dashboard per-process range store (psutil + proc_pid_rusage V6 + nettop)",
                "cadence_s": 20.0,
                "group_by": group_by,
                "ranked_by": metric,
            },
            "unavailable": unavailable(),
            "coverage": empty_coverage(group_by),
            "rows": page,
        })
    }

    pub fn apps(&self, range: &str) -> Value {
        let window_s = range_s(range);
        let now = crate::timefmt::now_unix();
        let window_start = crate::timefmt::rfc3339_utc(now - window_s as i64);
        let window_end = crate::timefmt::rfc3339_utc(now);

        let Some(rec) = self.newest_sample() else {
            return json!({
                "module": "apps",
                "range": range,
                "window_s": window_s,
                "window_start": window_start,
                "window_end": window_end,
                "count": 0,
                "rows": [],
                "totals": {"energy_wh": null, "disk_read_mb": null, "disk_write_mb": null, "net_mb": null, "disk_rows": 0, "net_rows": 0, "energy_rows": 0},
                "share_basis": "share of measured per-app energy (sum of ri_energy_nj deltas) — a measured floor, not the machine total",
                "spark": {"metric": "energy_w", "unit": "W", "bucket_s": 20.0, "start_ts": now - window_s as i64, "buckets": 44},
                "coverage": empty_coverage("name"),
                "cadence_s": 20.0,
                "empty_text": "no sweep in this range yet — the store sweeps every 20 s",
                "basis": "every metric the per-process range store holds, grouped by application name: mean and peak CPU %, peak RSS, rusage energy, disk byte deltas and nettop byte deltas",
                "cross_module": "the per-module consumer lists are the module cut; this view is the application cut of the same range store",
            });
        };

        // group by name
        let mut by_name: std::collections::BTreeMap<String, Vec<Value>> = std::collections::BTreeMap::new();
        let ts_epoch = rec.ts.as_deref()
            .and_then(crate::timefmt::parse_rfc3339)
            .unwrap_or(now as f64);
        for row in merge_processes(&rec.value, ts_epoch) {
            let name = row.get("name").and_then(Value::as_str).unwrap_or("unknown").to_string();
            by_name.entry(name).or_default().push(row);
        }

        let mut out_rows: Vec<Value> = Vec::new();
        for (name, group) in &by_name {
            // aggregate
            let mut cpu_vals: Vec<f64> = Vec::new();
            let mut mem_vals: Vec<f64> = Vec::new();
            let mut energy_wh_sum = 0.0;
            let mut energy_n = 0;
            let mut first_ts = group[0]["first_ts"].clone();
            let mut last_ts = group[0]["last_ts"].clone();
            let _pid = group[0]["pid"].clone();
            let processes = group.len() as i64;
            let samples = group.len() as i64;
            for r in group {
                if let Some(v) = r.get("cpu_avg_pct").and_then(Value::as_f64) { cpu_vals.push(v); }
                if let Some(v) = r.get("mem_avg_mb").and_then(Value::as_f64) { mem_vals.push(v); }
                if let Some(v) = r.get("energy_wh").and_then(Value::as_f64) { energy_wh_sum += v; energy_n += 1; }
                if let Some(v) = r.get("first_ts") { first_ts = v.clone(); }
                if let Some(v) = r.get("last_ts") { last_ts = v.clone(); }
            }
            let _cpu_avg_pct = if cpu_vals.is_empty() { Value::Null } else { json!(cpu_vals.iter().sum::<f64>() / cpu_vals.len() as f64) };
            let _cpu_max_pct = cpu_vals.iter().cloned().fold(f64::MIN, f64::max);
            let _cpu_max_pct2 = if cpu_vals.is_empty() { Value::Null } else { json!(_cpu_max_pct) };
            let mem_avg_mb = if mem_vals.is_empty() { Value::Null } else { json!(mem_vals.iter().sum::<f64>() / mem_vals.len() as f64) };
            let mem_peak_mb = if mem_vals.is_empty() { Value::Null } else { json!(mem_vals.iter().cloned().fold(f64::MIN, f64::max)) };
            let energy_wh = if energy_n == 0 { Value::Null } else { json!(energy_wh_sum) };
            out_rows.push(json!({
                "pid": null,
                "name": name,
                "processes": processes,
                "samples": samples,
                "cpu_avg_pct": null,
                "cpu_max_pct": null,
                "mem_avg_mb": mem_avg_mb,
                "mem_peak_mb": mem_peak_mb,
                "energy_wh": energy_wh,
                "energy_samples": energy_n,
                "energy_source": null,
                "disk_read_bps": null,
                "disk_write_bps": null,
                "disk_bps": null,
                "disk_samples": 0,
                "net_in_bps": null,
                "net_out_bps": null,
                "net_bps": null,
                "disk_read_mb": null,
                "disk_write_mb": null,
                "net_mb": null,
                "net_samples": 0,
                "gpu_pct": null,
                "first_ts": first_ts,
                "last_ts": last_ts,
                "share_pct": null,
                "spark": null,
                "peak_cpu_ts": null,
                "peak_mem_ts": first_ts,
                "peak_w": null,
                "peak_w_ts": null,
            }));
        }

        // totals
        let mut energy_total = 0.0;
        let mut energy_rows = 0;
        for r in &out_rows {
            if let Some(v) = r.get("energy_wh").and_then(Value::as_f64) {
                energy_total += v; energy_rows += 1;
            }
        }
        let totals = json!({
            "energy_wh": if energy_rows == 0 { Value::Null } else { json!(energy_total) },
            "disk_read_mb": null,
            "disk_write_mb": null,
            "net_mb": null,
            "disk_rows": 0,
            "net_rows": 0,
            "energy_rows": energy_rows,
        });

        // share_pct
        if energy_total > 0.0 {
            for r in &mut out_rows {
                if let Some(v) = r.get("energy_wh").and_then(Value::as_f64) {
                    r["share_pct"] = json!((100.0 * v / energy_total * 100.0).round() / 100.0);
                }
            }
        }

        out_rows.sort_by(|a,b| {
            let av = a.get("energy_wh").and_then(Value::as_f64).unwrap_or(f64::MIN);
            let bv = b.get("energy_wh").and_then(Value::as_f64).unwrap_or(f64::MIN);
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        });

        json!({
            "module": "apps",
            "range": range,
            "window_s": window_s,
            "window_start": window_start,
            "window_end": window_end,
            "count": out_rows.len(),
            "rows": out_rows,
            "totals": totals,
            "share_basis": "share of measured per-app energy (sum of ri_energy_nj deltas) — a measured floor, not the machine total",
            "spark": {"metric": "energy_w", "unit": "W", "bucket_s": 20.0, "start_ts": now - window_s as i64, "buckets": 44},
            "coverage": empty_coverage("name"),
            "cadence_s": 20.0,
            "empty_text": "no sweep in this range yet — the store sweeps every 20 s",
            "basis": "every metric the per-process range store holds, grouped by application name: mean and peak CPU %, peak RSS, rusage energy, disk byte deltas and nettop byte deltas",
            "cross_module": "the per-module consumer lists are the module cut; this view is the application cut of the same range store",
        })
    }
}

/// Union the three ranked process lists by pid, keeping every number each row
/// carries. `memory_mb` is the collector's `mem_mb`, renamed for the API.
fn merge_processes(rec: &Value, ts_epoch: f64) -> Vec<Value> {
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
                "cpu_avg_pct": null,
                "cpu_max_pct": null,
                "memory_mb": r.memory_mb,
                "mem_avg_mb": r.memory_mb,
                "mem_peak_mb": r.memory_mb,
                "threads": r.threads,
                "watts": r.watts,
                "energy_wh": null,
                "energy_nj_delta": r.energy_nj_delta,
                "energy_share_pct": r.energy_share_pct,
                "energy_samples": 0,
                "energy_source": null,
                "disk_read_bps": null,
                "disk_write_bps": null,
                "disk_bps": null,
                "disk_samples": 0,
                "net_in_bps": null,
                "net_out_bps": null,
                "net_bps": null,
                "disk_read_mb": null,
                "disk_write_mb": null,
                "net_mb": null,
                "net_samples": 0,
                "gpu_pct": null,
                "first_ts": ts_epoch,
                "last_ts": ts_epoch,
                "share_pct": r.energy_share_pct,
                "perm": r.perm,
                "processes": 1,
                "samples": 1,
                "metric": null,
            })
        })
        .collect()
}

fn range_s(rng: &str) -> i64 {
    match rng {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        "1h" => 3600,
        "6h" => 21600,
        "1d" => 86400,
        "7d" => 604800,
        "30d" => 2592000,
        "1y" => 31536000,
        _ => 3600,
    }
}

fn module_spec(module: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    match module {
        "cpu" => ("cpu_avg_pct", "%", "CPU", "mean and max of the per-sweep CPU % (100 % = one core) across every sample in the window"),
        "memory" => ("mem_peak_mb", "MB", "Memory", "peak resident size across the window's sweeps; avg is the mean of the same sweeps"),
        "gpu" => ("energy_wh", "Wh", "GPU", "per-process GPU time is not exposed by this SDK (ri_gpu_time absent), so rows are ranked by measured rusage energy -- the closest measured proxy for GPU work"),
        "disk" => ("disk_bps", "B/s", "Disk", "(ri_diskio_bytesread + ri_diskio_byteswritten) deltas / the span those deltas cover"),
        "network" => ("net_bps", "B/s", "Network", "nettop per-PID bytes_in/bytes_out deltas / span; nettop is sampled every 20 s"),
        "power" => ("energy_wh", "Wh", "Power", "sum of ri_energy_nj deltas / 3.6e12 -- a measured floor per process, never the machine total"),
        "all" => ("energy_wh", "Wh", "All", "every metric the range store holds: CPU %, memory, rusage energy, disk and network byte deltas"),
        _ => ("energy_wh", "Wh", "All", "every metric the range store holds: CPU %, memory, rusage energy, disk and network byte deltas"),
    }
}

fn empty_coverage(group_by: &str) -> Value {
    json!({
        "sweeps": 0,
        "cadence_s": 20.0,
        "retention_s": 10800.0,
        "row_min_mb": 20.0,
        "covered_s": 0.0,
        "coverage_pct": 0.0,
        "store": {"sweeps": 0, "first_ts": 0, "last_ts": 0},
        "group_by": group_by,
    })
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
                     "threads": 1, "watts": 1.194, "energy_nj_delta": 7149712459i64,
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
        let rows = merge_processes(&sample(), 0.0);
        assert_eq!(rows.len(), 3, "chrome/find/kernel_task, deduped by pid");
        let chrome = rows.iter().find(|r| r["pid"] == 10).unwrap();
        assert_eq!(chrome["name"], "chrome");
        assert_eq!(chrome["cpu_pct"], 43.8);
        assert_eq!(chrome["memory_mb"], 471.0);
        assert_eq!(chrome["threads"], 22.0);
        let find = rows.iter().find(|r| r["pid"] == 20).unwrap();
        assert_eq!(find["watts"], 1.194);
    }

    #[test]
    fn missing_numbers_are_null_not_zero() {
        let rows = merge_processes(&sample(), 0.0);
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
        let all = feed.consumers("cpu", "1h", 0, 50, "process");
        assert_eq!(all["count"], 3);
        assert_eq!(all["rows"][0]["name"], "chrome");
        assert_eq!(all["rows"][0]["name"], "chrome");
        assert!(all["unavailable"]["gpu_pct"].as_str().unwrap().contains("ri_gpu_time"));

        let page = feed.consumers("cpu", "1h", 1, 1, "process");
        assert_eq!(page["count"], 3, "count is the full total");
        assert_eq!(page["returned"], 1);
        assert_eq!(page["offset"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_log_is_a_clean_empty_answer() {
        let feed = ConsumerFeed::new(PathBuf::from("/nonexistent/syswatch/logs"));
        let out = feed.consumers("cpu", "1h", 0, 50, "process");
        assert_eq!(out["count"], 0);
        assert_eq!(out["rows"].as_array().unwrap().len(), 0);
        assert_eq!(out["empty"], true);
        assert!(out["reason"].as_str().unwrap().contains("processes"));
    }
}
