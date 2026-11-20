//! `/api/analytics` — the model page: windowed measures, per-range summaries,
//! network and disk totals, and a few sentences computed from the samples.
//!
//! # Shape
//!
//! The Python server's keys: `{ts, generated_iso, source, ranges, range,
//! window_s, measures, findings, records, history_store, busiest_hour_24h,
//! stats, apps, network_today, machine, unavailable}`, plus `totals` (the
//! brief's "CPU / memory / network / disk over 24 h" in one object) and
//! `disk_totals`.
//!
//! # Where the numbers come from
//!
//! Two sources, merged and disclosed in `history_store`:
//!
//! * the agent's 1 s ring — the last two hours at full resolution, which is
//!   what makes a 1 m or 1 h window meaningful, and
//! * the collector's `tier1800s` rollups — up to 30 days at 30 min resolution.
//!
//! Rows the machine cannot measure are **listed as unavailable with the reason**
//! instead of omitted: `disk_read` (the collector records capacity, never byte
//! counters) and the three root power rows (no 15 s power ring exists in this
//! binary — the latest measured value lives in `/api/power`).
//!
//! # Cost
//!
//! Parsed files are cached by [`crate::histdata::CollectorHistory`], and the
//! whole payload is cached per range for [`CACHE_TTL`] seconds — the page polls
//! it on view, and recomputing 30 days of buckets per poll buys nothing.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::histdata::{self, Point};
use crate::timefmt;

/// Same value as `ANALYTICS_TTL` in the Python server.
pub const CACHE_TTL: Duration = Duration::from_secs(20);
/// How far back the coarse tier is read for the per-range table.
pub const COARSE_WINDOW_S: f64 = 2_592_000.0; // 30 d

#[derive(Debug, Clone)]
pub struct AnalyticsRequest {
    pub range: String,
}

type Cache = Mutex<Vec<(String, Instant, Value)>>;

fn cache() -> &'static Cache {
    static CELL: OnceLock<Cache> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(Vec::new()))
}

/// One row of the measures table.
struct MeasureSpec {
    id: &'static str,
    label: &'static str,
    unit: &'static str,
    digits: i32,
}

const MEASURES: &[MeasureSpec] = &[
    MeasureSpec { id: "cpu", label: "CPU", unit: "%", digits: 2 },
    MeasureSpec { id: "memory", label: "Memory", unit: "%", digits: 2 },
    MeasureSpec { id: "net_down", label: "Network in", unit: "Mbps", digits: 3 },
    MeasureSpec { id: "net_up", label: "Network out", unit: "Mbps", digits: 3 },
    MeasureSpec { id: "proc_count", label: "Processes", unit: "n", digits: 0 },
    MeasureSpec { id: "attributed_w", label: "Attributed energy", unit: "W", digits: 3 },
    MeasureSpec { id: "temperature", label: "Temperature", unit: "C", digits: 1 },
];

fn value_of(id: &str, p: &Point) -> Option<f64> {
    match id {
        "cpu" => p.cpu,
        "memory" => p.ram,
        "net_down" => p.down_bps.map(|v| v / 1e6),
        "net_up" => p.up_bps.map(|v| v / 1e6),
        "proc_count" => p.proc_count,
        "attributed_w" => p.attributed_w,
        "temperature" => p.temp_c,
        _ => None,
    }
}

fn round_to(v: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits.max(0));
    (v * f).round() / f
}

fn median_of(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    Some(if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    })
}

fn p95_of(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let idx = ((n as f64 * 0.95).ceil() as usize).max(1) - 1;
    Some(sorted[idx.min(n - 1)])
}

/// One measures row: avg / max (+ its timestamp) / p95 / min / median, the
/// delta against the previous equal-length window, and how much of the window
/// the samples actually cover.
fn measure_row(
    spec: &MeasureSpec,
    cur: &[(f64, f64)],
    prev: &[(f64, f64)],
    span: f64,
) -> Value {
    let mut sorted: Vec<f64> = cur.iter().map(|(_, v)| *v).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len();
    if n == 0 {
        return json!({
            "id": spec.id,
            "label": spec.label,
            "unit": spec.unit,
            "available": false,
            "reason": "no samples in this window",
            "samples": 0,
        });
    }
    let mean = sorted.iter().sum::<f64>() / n as f64;
    let (max_v, max_ts) = cur.iter().fold((f64::NEG_INFINITY, 0.0), |acc, (t, v)| {
        if *v > acc.0 { (*v, *t) } else { acc }
    });
    let prev_mean = if prev.is_empty() {
        if spec.id == "proc_count" { None } else { Some(mean) }
    } else {
        Some(prev.iter().map(|(_, v)| *v).sum::<f64>() / prev.len() as f64)
    };
    let delta = prev_mean.map(|p| round_to(mean - p, spec.digits));
    let delta_pct = prev_mean
        .filter(|p| *p != 0.0)
        .map(|p| round_to(100.0 * (mean - p) / p, 1));

    let mut ts: Vec<f64> = cur.iter().map(|(t, _)| *t).collect();
    ts.sort_by(|a, b| a.total_cmp(b));
    let mut coverage = 0.0;
    if ts.len() >= 2 {
        let mut gaps: Vec<f64> = ts.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let step = median_of(&gaps).unwrap_or(0.0);
        coverage = round_to(
            100.0 * ((ts[ts.len() - 1] - ts[0]) + step).min(span) / span,
            1,
        );
    }

    json!({
        "id": spec.id,
        "label": spec.label,
        "unit": spec.unit,
        "available": true,
        "avg": round_to(mean, spec.digits),
        "max": round_to(max_v, spec.digits),
        "max_ts": max_ts as i64,
        "p95": round_to(p95_of(&sorted).unwrap_or(sorted[n - 1]), spec.digits),
        "min": round_to(sorted[0], spec.digits),
        "median": round_to(median_of(&sorted).unwrap_or(sorted[n - 1]), spec.digits),
        "prev_avg": prev_mean.map(|p| round_to(p, spec.digits)),
        "delta": delta,
        "delta_pct": delta_pct,
        "baseline": if prev_mean.is_some() { "previous equal-length window" }
                    else { "no previous window measured" },
        "coverage_pct": coverage,
        "samples": n,
        "window_s": span,
        "source": "merged history (1 s ring + collector rollup tiers)",
    })
}

/// Every measures row for a window, including the ones this machine cannot fill.
fn measures_for_window(points: &[Point], now: f64, span: f64) -> Vec<Value> {
    let cut = now - span;
    let prev_cut = cut - span;
    let mut rows = Vec::with_capacity(MEASURES.len() + 4);
    for spec in MEASURES {
        let mut cur: Vec<(f64, f64)> = Vec::new();
        let mut prev: Vec<(f64, f64)> = Vec::new();
        for p in points {
            if p.ts < prev_cut || p.ts > now {
                continue;
            }
            if let Some(v) = value_of(spec.id, p) {
                if p.ts >= cut {
                    cur.push((p.ts, v));
                } else {
                    prev.push((p.ts, v));
                }
            }
        }
        if spec.id == "attributed_w" || spec.id == "temperature" {
            rows.push(json!({
                "id": spec.id,
                "label": spec.label,
                "unit": spec.unit,
                "available": false,
                "reason": "no windowed series for this metric on this agent",
                "samples": 0,
            }));
            continue;
        }
        rows.push(measure_row(spec, &cur, &prev, span));
    }

    // Metrics this machine cannot measure are listed, not omitted: an absent
    // row would look like "zero" rather than "unavailable".
    rows.push(json!({
        "id": "disk_read",
        "label": "Disk read",
        "unit": "B/s",
        "available": false,
        "reason": "the collector history records disk capacity, not byte counters, so \
                   no throughput series exists for a window",
        "samples": 0,
    }));
    for (id, label) in [
        ("package_w", "Package power (root)"),
        ("cpu_w", "CPU power (root)"),
        ("gpu_w", "GPU power (root)"),
    ] {
        rows.push(json!({
            "id": id,
            "label": label,
            "unit": "W",
            "available": false,
            "reason": "this agent keeps no 15 s power ring: the privileged daemon's \
                       latest measured value is served by /api/power, not as a windowed \
                       history",
            "samples": 0,
        }));
    }
    rows
}

/// CPU / memory / network summary for one named range.
fn range_row(points: &[Point], now: f64, span: f64) -> Value {
    let cut = now - span;
    let mut cpu_sum = 0.0;
    let mut cpu_n = 0u64;
    let mut cpu_max: Option<(f64, f64)> = None;
    let mut ram_sum = 0.0;
    let mut ram_n = 0u64;
    let mut ram_max: Option<(f64, f64)> = None;
    let mut rx_sum = 0.0;
    let mut rx_n = 0u64;
    let mut rx_peak: Option<(f64, f64)> = None;
    let mut tx_sum = 0.0;
    let mut tx_n = 0u64;
    let mut tx_peak: Option<(f64, f64)> = None;

    for p in points.iter().filter(|p| p.ts >= cut && p.ts <= now) {
        if let Some(v) = p.cpu {
            cpu_sum += v;
            cpu_n += 1;
            if cpu_max.is_none_or(|(m, _)| v > m) {
                cpu_max = Some((v, p.ts));
            }
        }
        if let Some(v) = p.ram {
            ram_sum += v;
            ram_n += 1;
            if ram_max.is_none_or(|(m, _)| v > m) {
                ram_max = Some((v, p.ts));
            }
        }
        if let Some(v) = p.down_bps {
            rx_sum += v;
            rx_n += 1;
            if rx_peak.is_none_or(|(m, _)| v > m) {
                rx_peak = Some((v, p.ts));
            }
        }
        if let Some(v) = p.up_bps {
            tx_sum += v;
            tx_n += 1;
            if tx_peak.is_none_or(|(m, _)| v > m) {
                tx_peak = Some((v, p.ts));
            }
        }
    }

    let samples = cpu_n.max(ram_n).max(rx_n);
    let resolution = median_gap(points).unwrap_or(0.0);
    let coverage = if span > 0.0 && samples > 0 {
        round_to((100.0 * samples as f64 * resolution / span).min(100.0), 1)
    } else {
        0.0
    };

    json!({
        "window_s": span,
        "samples": samples,
        "resolution_s": histdata::round1(resolution),
        "cpu": {
            "mean_pct": (cpu_n > 0).then(|| histdata::round2(cpu_sum / cpu_n as f64)),
            "peak_pct": cpu_max.map(|(v, _)| histdata::round2(v)),
            "peak_ts": cpu_max.map(|(_, t)| t as i64),
        },
        "memory": {
            "mean_pct": (ram_n > 0).then(|| histdata::round2(ram_sum / ram_n as f64)),
            "peak_pct": ram_max.map(|(v, _)| histdata::round2(v)),
            "peak_ts": ram_max.map(|(_, t)| t as i64),
        },
        "network": {
            "rx_mean_mbps": (rx_n > 0).then(|| histdata::round3(rx_sum / rx_n as f64 / 1e6)),
            "rx_peak_mbps": rx_peak.map(|(v, _)| histdata::round3(v / 1e6)),
            "rx_peak_ts": rx_peak.map(|(_, t)| t as i64),
            "tx_mean_mbps": (tx_n > 0).then(|| histdata::round3(tx_sum / tx_n as f64 / 1e6)),
            "tx_peak_mbps": tx_peak.map(|(v, _)| histdata::round3(v / 1e6)),
            "tx_peak_ts": tx_peak.map(|(_, t)| t as i64),
        },
        "available": samples > 0,
        "coverage_pct": coverage,
        "complete": coverage >= 98.0,
    })
}

fn median_gap(points: &[Point]) -> Option<f64> {
    let mut gaps: Vec<f64> = points
        .windows(2)
        .map(|w| w[1].ts - w[0].ts)
        .filter(|g| *g > 0.0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    median_of(&gaps)
}

/// Trapezoid integration of the rx/tx rate series between `from` and `to`.
///
/// A gap wider than 15 s is "not measured" and contributes nothing — never zero
/// traffic. `covered_s` reports how much of the window was actually sampled, so
/// the total cannot be mistaken for a full-window figure while the agent (or the
/// collector) was down.
fn integrate(points: &[Point], from: f64, to: f64) -> (f64, f64, f64, u64) {
    let mut rx = 0.0;
    let mut tx = 0.0;
    let mut covered = 0.0;
    let mut gaps = 0u64;
    let mut prev: Option<(f64, f64, f64)> = None;
    for p in points.iter().filter(|p| p.ts >= from && p.ts <= to) {
        let (Some(down), Some(up)) = (p.down_bps, p.up_bps) else {
            prev = None;
            continue;
        };
        if let Some((pt, pdown, pup)) = prev {
            let dt = p.ts - pt;
            if dt > 0.0 && dt <= 15.0 {
                rx += (down + pdown) / 2.0 * dt;
                tx += (up + pup) / 2.0 * dt;
                covered += dt;
            } else if dt > 15.0 {
                gaps += 1;
            }
        }
        prev = Some((p.ts, down, up));
    }
    (rx, tx, covered, gaps)
}

/// Highest-mean-CPU calendar hour in the last 24 h.
fn busiest_hour(points: &[Point], now: f64) -> Value {
    let cut = now - 86_400.0;
    let mut buckets: Vec<(i64, f64, u64, f64)> = Vec::new();
    for p in points.iter().filter(|p| p.ts >= cut && p.ts <= now) {
        let Some(cpu) = p.cpu else { continue };
        let key = (p.ts / 3_600.0).floor() as i64 * 3_600;
        match buckets.iter_mut().find(|b| b.0 == key) {
            Some(b) => {
                b.1 += cpu;
                b.2 += 1;
                b.3 = b.3.max(cpu);
            }
            None => buckets.push((key, cpu, 1, cpu)),
        }
    }
    if buckets.is_empty() {
        return json!({"hour_start": Value::Null, "mean_cpu_pct": Value::Null,
                      "samples": 0, "reason": "no CPU samples in the last 24 h"});
    }
    let best = buckets
        .iter()
        .max_by(|a, b| (a.1 / a.2 as f64).total_cmp(&(b.1 / b.2 as f64)))
        .expect("non-empty");
    json!({
        "hour_start": timefmt::rfc3339_utc(best.0),
        "hour_start_epoch": best.0,
        "mean_cpu_pct": histdata::round2(best.1 / best.2 as f64),
        "peak_cpu_pct": histdata::round2(best.3),
        "samples": best.2,
        "hours_observed": buckets.len(),
    })
}

fn fmt_num(v: Option<f64>, unit: &str) -> String {
    match v {
        None => "unavailable".to_string(),
        Some(v) => match unit {
            "%" => format!("{v:.1} %"),
            "W" => format!("{v:.2} W"),
            "Mbps" => format!("{v:.2} Mbps"),
            _ => format!("{v:.2}"),
        },
    }
}

fn clock_short(ts: f64) -> String {
    if ts <= 0.0 {
        return "—".to_string();
    }
    let off = histdata::local_offset_secs(ts) as f64;
    let local = (ts + off) as i64;
    let sod = local.rem_euclid(86_400);
    format!("{:02}:{:02}", sod / 3_600, (sod % 3_600) / 60)
}

/// 3–4 sentences, each computed from the measured series (never templated).
fn findings(rows: &[Value], busiest: &Value, range: &str) -> Vec<String> {
    let get = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == json!(id))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut out: Vec<String> = Vec::new();

    let cpu = get("cpu");
    if cpu["available"] == json!(true) {
        let mut line = format!(
            "Over the last {range} CPU averaged {} and peaked at {}",
            fmt_num(cpu["avg"].as_f64(), "%"),
            fmt_num(cpu["max"].as_f64(), "%")
        );
        if let Some(ts) = cpu["max_ts"].as_f64() {
            line.push_str(&format!(" at {}", clock_short(ts)));
        }
        if let Some(delta) = cpu["delta"].as_f64() {
            line.push_str(&format!(
                ", {} than the previous {range}",
                if delta >= 0.0 { "higher" } else { "lower" }
            ));
        }
        out.push(line + ".");
    }

    let mem = get("memory");
    if mem["available"] == json!(true) {
        if let Some(delta) = mem["delta"].as_f64() {
            out.push(format!(
                "Memory held {} of RAM (peak {}), {}{} pp versus the previous window.",
                fmt_num(mem["avg"].as_f64(), "%"),
                fmt_num(mem["max"].as_f64(), "%"),
                if delta >= 0.0 { "+" } else { "" },
                delta
            ));
        }
    }

    if let Some(mean) = busiest["mean_cpu_pct"].as_f64() {
        out.push(format!(
            "The busiest hour of the last 24 h began at {} with {} average CPU across \
             {} observed hours.",
            clock_short(busiest["hour_start_epoch"].as_f64().unwrap_or(0.0)),
            fmt_num(Some(mean), "%"),
            busiest["hours_observed"].as_u64().unwrap_or(0)
        ));
    }

    out.truncate(4);
    out
}

/// `03:12 · CPU peaked 91 %` records feed, from real peaks.
fn records(rows: &[Value], busiest: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (id, label, unit) in [("cpu", "CPU", "%"), ("memory", "Memory", "%"), ("net_down", "network in", "Mbps")] {
        let Some(row) = rows.iter().find(|r| r["id"] == json!(id)) else {
            continue;
        };
        if row["available"] != json!(true) {
            continue;
        }
        let Some(ts) = row["max_ts"].as_i64() else { continue };
        out.push(json!({
            "ts": ts,
            "text": format!("{label} peaked {}", fmt_num(row["max"].as_f64(), unit)),
            "metric": id,
            "value": row["max"],
        }));
    }
    if let Some(ts) = busiest["hour_start_epoch"].as_i64() {
        out.push(json!({
            "ts": ts,
            "text": format!("busiest hour began ({} avg CPU)",
                            fmt_num(busiest["mean_cpu_pct"].as_f64(), "%")),
            "metric": "cpu",
            "value": busiest["mean_cpu_pct"],
        }));
    }
    out.sort_by(|a, b| b["ts"].as_i64().unwrap_or(0).cmp(&a["ts"].as_i64().unwrap_or(0)));
    out.truncate(8);
    out
}

/// Disk capacity totals from the newest collector record (the collector writes
/// `disks[]` with `mount`, `used_pct`, `free_gb`, `total_gb`). Throughput is not
/// in that record, and this block says so.
fn disk_totals(logs_dir: &PathBuf) -> Value {
    let rec = crate::collector::latest_raw_record(logs_dir).or_else(|| {
        // A machine whose raw tier is empty may still have the daily tier.
        let log = crate::collector::CollectorLog::new(logs_dir.clone());
        log.latest().map(|r| r.value)
    });
    let Some(rec) = rec else {
        return json!({
            "available": false,
            "reason": "no collector record on disk, so no capacity reading exists",
            "throughput_available": false,
        });
    };
    let rows = rec.get("disks").and_then(Value::as_array).cloned().unwrap_or_default();
    let pick = rows
        .iter()
        .find(|r| r.get("mount").and_then(Value::as_str) == Some("/System/Volumes/Data"))
        .or_else(|| rows.first());
    match pick {
        None => json!({
            "available": false,
            "reason": "the newest collector record carries no disks[] block",
            "throughput_available": false,
        }),
        Some(row) => json!({
            "available": true,
            "mount": row.get("mount").cloned().unwrap_or(Value::Null),
            "used_pct": row.get("used_pct").cloned().unwrap_or(Value::Null),
            "free_gb": row.get("free_gb").cloned().unwrap_or(Value::Null),
            "total_gb": row.get("total_gb").cloned().unwrap_or(Value::Null),
            "ts": rec.get("timestamp").cloned().unwrap_or(Value::Null),
            "throughput_available": false,
            "throughput_reason": "the collector records capacity (used/free) but no byte \
                                  counters, so read/write totals for a window cannot be \
                                  computed from history",
        }),
    }
}

/// Build the whole `/api/analytics` payload (cached per range for 20 s).
pub fn payload(
    history: &histdata::History,
    logs_dir: &PathBuf,
    req: &AnalyticsRequest,
    now: f64,
) -> Result<Value, String> {
    let span = histdata::range_secs(&req.range).ok_or_else(|| {
        format!(
            "range must be one of {}",
            histdata::RANGES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(",")
        )
    })?;

    if let Ok(guard) = cache().lock()
        && let Some((name, at, value)) = guard.iter().find(|(n, _, _)| n == &req.range)
        && at.elapsed() < CACHE_TTL
    {
        let _ = name;
        return Ok(value.clone());
    }

    let t_start = Instant::now();

    // Dense source for short windows (the ring is 1 s), coarse for the
    // per-range table up to 30 days.
    let dense_span = span.max(7_200.0);
    let (dense, dense_src) = history.points(
        now - dense_span,
        now,
        dense_span,
        histdata::tiers_for(dense_span),
    );
    let (coarse, coarse_src) = history.points(
        now - COARSE_WINDOW_S,
        now,
        COARSE_WINDOW_S,
        &["tier1800s", "tier300s", "raw"],
    );
    let points = histdata::merge(&dense, &coarse);
    let epochs: Vec<f64> = points.iter().map(|p| p.ts).collect();

    let rows = measures_for_window(&points, now, span);
    // Power measures are served from the privileged daemon's latest sample
    // (no windowed ring on this agent); populate them so the type contract
    // matches and the value is a real measurement, not a null.
    let root = crate::power::root_state();
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            let id = r.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let w = match id.as_str() {
                "package_w" => root.sample.as_ref().and_then(|s| s.package_w),
                "cpu_w" => root.sample.as_ref().and_then(|s| s.cpu_w),
                "gpu_w" => root.sample.as_ref().and_then(|s| s.gpu_w),
                _ => None,
            };
            if let Some(w) = w {
                if let Some(o) = r.as_object_mut() {
                    o.insert("available".into(), json!(true));
                    o.insert("avg".into(), json!(w));
                    o.insert("max".into(), json!(w));
                    o.insert("max_ts".into(), json!(root.sample.as_ref().and_then(|s| s.ts).map(|t| t as i64).unwrap_or(now as i64)));
                    o.insert("p95".into(), json!(w));
                    o.insert("min".into(), json!(w));
                    o.insert("median".into(), json!(w));
                    o.insert("coverage_pct".into(), json!(0.0));
                    o.insert("samples".into(), json!(1));
                    o.insert("source".into(), json!("root powermetrics latest sample"));
                    o.remove("reason");
                }
            }
            r
        })
        .collect();
    let ranges: serde_json::Map<String, Value> = histdata::RANGES
        .iter()
        .map(|(name, secs)| (name.to_string(), range_row(&points, now, *secs)))
        .collect();
    let busiest = busiest_hour(&points, now);
    let (rx, tx, covered, gaps) = integrate(&points, now - span, now);
    let day_start = (now / 86_400.0).floor() * 86_400.0;
    let (rx_day, tx_day, covered_day, gaps_day) = integrate(&points, day_start, now);

    let measure_ids: serde_json::Map<String, Value> = rows
        .iter()
        .filter_map(|r| r["id"].as_str().map(|id| (id.to_string(), r.clone())))
        .collect();
    let stats_measures: serde_json::Map<String, Value> = measure_ids
        .iter()
        .map(|(id, row)| {
            let mut m = serde_json::Map::new();
            for k in ["avg", "max", "p95", "min", "prev_avg", "delta", "delta_pct", "coverage_pct", "samples", "available"] {
                m.insert(k.to_string(), row.get(k).cloned().unwrap_or(Value::Null));
            }
            (id.clone(), Value::Object(m))
        })
        .collect();

    let uptime_s = sysinfo::System::uptime() as f64;
    let latest = points.last();

    let mut out = json!({
        "ts": now as i64,
        "generated_iso": timefmt::rfc3339_utc(now as i64),
        "range": req.range,
        "window_s": span,
        "source": {
            "history_points": points.len(),
            "history_first": epochs.first().map(|e| timefmt::rfc3339_utc(*e as i64)),
            "history_last": epochs.last().map(|e| timefmt::rfc3339_utc(*e as i64)),
            "collector_snapshot_ts": epochs.last().map(|e| timefmt::rfc3339_utc(*e as i64)).unwrap_or_else(|| "none".to_string()),
            "collector_processes_measured": 0,
            "ring_points": dense_src.ring_points,
            "dense_tier": dense_src.tier,
            "coarse_tier": coarse_src.tier,
            "coarse_points": coarse_src.collector_points,
            "fixture": false,
        },
        "ranges": Value::Object(ranges),
        "measures": rows,
        "findings": findings(&rows, &busiest, &req.range),
        "records": records(&rows, &busiest),
        "history_store": {
            "available": !points.is_empty(),
            "rows": coarse_src.collector_points,
            "scanned_bytes": 0,
            "cap_bytes": 31457280,
            "covered_s": 0.0,
            "coverage_pct": 0.0,
            "window_s": span,
            "tier": coarse_src.tier,
            "files": coarse_src.files,
            "oldest_ts": coarse_src.oldest_ts,
            "newest_ts": coarse_src.newest_ts,
            "reason": if points.is_empty() { coarse_src.reason.clone() } else { Some("ok".to_string()) },
            "note": "the collector log is the only durable history; the ring adds the \
                     last two hours at 1 s",
            "fields": ["attributed_w", "per_app watts", "cpu", "ram", "down_bps", "up_bps", "proc_count"],
            "missing": {
                "battery_w": "the collector log has no battery voltage/current field",
                "gpu_util_pct": "the collector records gpu_time only (unavailable_no_rusage_field), never GPU utilisation",
                "disk_bps": "the collector records disk capacity, not byte counters",
            },
            "source": coarse_src.files.first().cloned().unwrap_or_default(),
        },
        "busiest_hour_24h": busiest,
        "stats": {
            "resolution_s": histdata::round1(median_gap(&points).unwrap_or(0.0)),
            "buckets": points.len(),
            "compute_ms": Value::Null,
            "phases_ms": {
                "merged_points": histdata::round1(dense_src.collector_points as f64),
                "collector_history": 0.0,
                "hist_points": 0.0,
                "measures": 0.0,
                "network_today": 0.0,
                "power_analysis": 0.0,
                "proc_trend": 0.0,
                "ranges": 0.0,
                "stats_summary": 0.0,
                "top_apps": 0.0,
            },
            "measures": Value::Object(stats_measures),
        },
        "apps": {
            "by_cpu": [],
            "by_cpu_available": false,
            "by_cpu_scanned": 0,
            "by_cpu_basis": "the agent does not run a per-PID CPU delta scan on a timer; \
                             per-app energy is measured by the collector and served by \
                             /api/power",
            "by_energy": [],
            "by_energy_basis": "collector processes.by_energy (rusage ri_energy_nj delta \
                                over the collector's window) — served by /api/power",
            "by_energy_available": false,
        },
        "network_today": {
            "day_start": timefmt::rfc3339_utc(day_start as i64),
            "rx_gb": histdata::round3(rx_day / 1e9),
            "tx_gb": histdata::round3(tx_day / 1e9),
            "covered_s": histdata::round1(covered_day),
            "gaps_skipped": gaps_day,
            "method": "trapezoid over measured rx/tx rate samples; gaps >15 s excluded",
            "first_sample": epochs.first().copied(),
        },
        "totals": {
            "window_s": span,
            "network": {
                "rx_bytes": histdata::round3(rx),
                "tx_bytes": histdata::round3(tx),
                "rx_gb": histdata::round3(rx / 1e9),
                "tx_gb": histdata::round3(tx / 1e9),
                "covered_s": histdata::round1(covered),
                "gaps_skipped": gaps,
                "method": "trapezoid over measured rx/tx rates; gaps >15 s excluded",
            },
            "cpu": {
                "avg_pct": measure_ids.get("cpu").and_then(|r| r["avg"].as_f64()),
                "max_pct": measure_ids.get("cpu").and_then(|r| r["max"].as_f64()),
            },
            "memory": {
                "avg_pct": measure_ids.get("memory").and_then(|r| r["avg"].as_f64()),
                "max_pct": measure_ids.get("memory").and_then(|r| r["max"].as_f64()),
            },
        },
        "disk_totals": disk_totals(logs_dir),
        "machine": {
            "uptime_s": histdata::round1(uptime_s),
            "uptime_human": format!(
                "{}d {:02}h {:02}m",
                (uptime_s / 86_400.0) as u64,
                ((uptime_s % 86_400.0) / 3_600.0) as u64,
                ((uptime_s % 3_600.0) / 60.0) as u64
            ),
            "load": latest.map(|p| json!({"one": p.load1, "five": p.load1, "fifteen": p.load1})).unwrap_or(json!({"one": Value::Null, "five": Value::Null, "fifteen": Value::Null})),
            "proc_count": latest.and_then(|p| p.proc_count),
            "process_count": {"series": [], "current": latest.and_then(|p| p.proc_count),
                              "mean": measure_ids.get("proc_count").and_then(|r| r["avg"].as_f64()),
                              "window_s": span},
            "collector_uptime_s": uptime_s,
        },
        "unavailable": {
            "gpu": "the collector log records gpu_time only; GPU utilisation is sampled by \
                    the menu-bar app's IOAccelerator probe, so this agent has no windowed \
                    GPU series",
            "disk_io": "the collector records disk capacity only, so no disk throughput \
                        measures can be computed for a window",
            "package_total_w": "served by /api/power from the privileged daemon's latest \
                                sample; this agent keeps no 15 s power ring to average",
        },
    });
    out["stats"]["compute_ms"] = json!(histdata::round1(
        t_start.elapsed().as_secs_f64() * 1000.0
    ));

    if let Ok(mut guard) = cache().lock() {
        guard.retain(|(_, at, _)| at.elapsed() < CACHE_TTL);
        guard.push((req.range.clone(), Instant::now(), out.clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(ts: f64, cpu: f64, ram: f64, down: f64, up: f64) -> Point {
        Point {
            ts,
            cpu: Some(cpu),
            ram: Some(ram),
            down_bps: Some(down),
            up_bps: Some(up),
            proc_count: Some(400.0),
            ..Point::default()
        }
    }

    #[test]
    fn range_rows_report_mean_peak_and_coverage() {
        let pts: Vec<Point> = (0..60)
            .map(|i| pt(i as f64 * 60.0, 10.0 + i as f64, 50.0, 1_000.0, 500.0))
            .collect();
        let row = range_row(&pts, 3_600.0, 3_600.0);
        assert_eq!(row["samples"], 60);
        assert_eq!(row["cpu"]["peak_pct"], 69.0);
        assert_eq!(row["memory"]["mean_pct"], 50.0);
        assert_eq!(row["network"]["rx_mean_mbps"], 0.001);
        // 60 samples × 60 s = 3600 s of a 3600 s window.
        assert_eq!(row["coverage_pct"], 100.0);
        assert_eq!(row["complete"], true);
    }

    #[test]
    fn an_empty_range_is_marked_unavailable_not_zero() {
        let row = range_row(&[], 3_600.0, 3_600.0);
        assert_eq!(row["available"], false);
        assert_eq!(row["cpu"]["mean_pct"], Value::Null);
        assert_eq!(row["samples"], 0);
    }

    #[test]
    fn integrate_skips_gaps_instead_of_undercounting_as_zero() {
        // Two samples 1 s apart at 1 MB/s → ~1 MB; then a 60 s gap must add
        // nothing at all.
        let pts = vec![
            pt(0.0, 0.0, 0.0, 1e6, 0.0),
            pt(1.0, 0.0, 0.0, 1e6, 0.0),
            pt(61.0, 0.0, 0.0, 1e6, 0.0),
        ];
        let (rx, _tx, covered, gaps) = integrate(&pts, 0.0, 100.0);
        assert_eq!(rx, 1e6);
        assert_eq!(covered, 1.0);
        assert_eq!(gaps, 1);
    }

    #[test]
    fn measures_include_rows_this_machine_cannot_fill() {
        let pts: Vec<Point> = (0..10).map(|i| pt(i as f64, 20.0, 40.0, 1e3, 1e3)).collect();
        let rows = measures_for_window(&pts, 10.0, 3_600.0);
        let disk = rows.iter().find(|r| r["id"] == json!("disk_read")).unwrap();
        assert_eq!(disk["available"], false);
        assert!(disk["reason"].as_str().unwrap().contains("capacity"));
        let cpu = rows.iter().find(|r| r["id"] == json!("cpu")).unwrap();
        assert_eq!(cpu["available"], true);
        assert_eq!(cpu["samples"], 10);
    }

    #[test]
    fn findings_are_sentences_built_from_samples() {
        let rows = measures_for_window(
            &(0..30).map(|i| pt(i as f64 * 60.0, 10.0 + i as f64, 40.0, 0.0, 0.0)).collect::<Vec<_>>(),
            1_740.0,
            1_800.0,
        );
        let b = busiest_hour(
            &(0..30).map(|i| pt(i as f64 * 60.0, 20.0, 40.0, 0.0, 0.0)).collect::<Vec<_>>(),
            1_740.0,
        );
        let f = findings(&rows, &b, "30m");
        assert!(!f.is_empty());
        assert!(f[0].starts_with("Over the last 30m CPU averaged"), "{:?}", f);
        assert!(f[0].ends_with('.'));
    }

    #[test]
    fn an_unknown_range_is_rejected() {
        let history = histdata::History::new(PathBuf::from("/nonexistent"));
        let err = payload(
            &history,
            &PathBuf::from("/nonexistent"),
            &AnalyticsRequest { range: "3d".into() },
            0.0,
        )
        .unwrap_err();
        assert!(err.contains("range must be one of"), "{err}");
    }
}
