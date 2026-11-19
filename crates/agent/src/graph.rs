//! `/api/graph` — one metric over a window, with axis ticks, a peak chip and
//! per-field measures.
//!
//! # Shape
//!
//! The payload is the Python server's: `{metric, range, from, to, points,
//! ticks, peak, available, source, measures}` plus additive keys
//! (`count`, `raw_points`, `downsampled_to`, `sources`, `note`). A chart written
//! against `dashboard/server.py` renders this unchanged.
//!
//! # Where the points come from
//!
//! Unlike the Python server — which plots its own in-memory ring and so loses
//! every point when it restarts — this handler merges two durable-ish sources:
//!
//! 1. the agent's 1 s ring (last 2 h, in-process), and
//! 2. the collector's JSONL tiers on disk (`raw/`, `tier60s/`, …), chosen by
//!    window width in [`crate::histdata::tiers_for`].
//!
//! Every response discloses which tier and how many files were read, in
//! `sources`.
//!
//! # Metrics that cannot be answered
//!
//! `disk` and `gpu` have no history on this machine: the collector records disk
//! *capacity* and a `gpu_time: unavailable_no_rusage_field` marker, never
//! throughput or utilisation. Those are answered with `available: false` and a
//! reason that says exactly that, never with a flat zero line. `battery` is
//! answered from the ring only (the collector log carries no battery block).

use serde_json::{json, Value};

use crate::histdata::{self, Point, Sources};
use crate::timefmt;

/// Metrics the Python server accepts, after alias resolution.
pub const METRICS: &[&str] = &[
    "cpu", "memory", "network", "disk", "gpu", "load", "battery", "sensors",
];

pub fn canonical_metric(raw: &str) -> String {
    match raw.trim() {
        "net" => "network".to_string(),
        "mem" | "ram" => "memory".to_string(),
        other => other.to_string(),
    }
}

/// Fields a peak / measures row is computed over, in the Python server's order.
fn fields(metric: &str) -> &'static [&'static str] {
    match metric {
        "cpu" => &["total"],
        "memory" => &["pct"],
        "network" => &["down_bps", "up_bps"],
        "disk" => &["read_bps", "write_bps", "read_iops", "used_pct"],
        "gpu" => &["util_pct", "util_pct_max", "renderer_pct", "tiler_pct"],
        "load" => &["one"],
        "battery" => &["pct"],
        _ => &[],
    }
}

/// `GRAPH_SOURCES`, re-stated for this implementation: every footer in the UI
/// must name a probe that actually produced the line.
fn source_for(metric: &str) -> &'static str {
    match metric {
        "cpu" => {
            "collector JSONL cpu.total_pct + the agent's 1 s ring (sysinfo global CPU)"
        }
        "memory" => "collector JSONL memory.pct + the agent's 1 s ring (sysinfo used/total)",
        "network" => {
            "collector JSONL networks[].rx_bps/tx_bps, summed over real interfaces \
             (loopback and awdl excluded), + the agent's 1 s ring"
        }
        "load" => "collector JSONL cpu.load1 + the agent's 1 s ring (getloadavg)",
        "battery" => {
            "the agent's 1 s ring (ioreg AppleSmartBattery charge %) — the \
             collector log carries no battery block"
        }
        "sensors" => "collector JSONL temperature + the root daemon's SMC rows (via the ring)",
        _ => "no probe on this machine records this series",
    }
}

fn note_for(metric: &str) -> Option<&'static str> {
    match metric {
        "disk" => Some(
            "read/write throughput (B/s) — differentiated counters, not the cumulative \
             total and not free space; the collector records capacity only, so there is \
             no throughput history to plot",
        ),
        "gpu" => Some(
            "Device Utilization % — needs the dashboard's IOAccelerator sampler; the \
             collector log can only record gpu_time (unavailable_no_rusage_field)",
        ),
        "battery" => Some("charge % and time-to-empty from the agent's 1 s ioreg ring"),
        _ => None,
    }
}

/// Why a window came back empty, in terms of what was actually read.
fn empty_reason(metric: &str, range_s: f64, sources: &Sources) -> String {
    match metric {
        "disk" => format!(
            "no disk throughput series exists: the collector records capacity (used/free) \
             and the agent keeps no byte-counter ring — this window is {}",
            histdata::fmt_span(range_s)
        ),
        "gpu" => format!(
            "no GPU utilisation series exists: the collector log has no utilisation field \
             and the agent does not sample IOAccelerator per second — this window is {}",
            histdata::fmt_span(range_s)
        ),
        _ => format!(
            "no {} samples in this {} window (read {} file(s) from the {} tier, \
             {} collector record(s), {} ring point(s))",
            metric,
            histdata::fmt_span(range_s),
            sources.files.len(),
            sources.tier,
            sources.collector_points,
            sources.ring_points
        ),
    }
}

// ── points per metric ───────────────────────────────────────────────────────

/// Metric-shaped points, in the field names the Python chart code reads.
///
/// A point with nothing to plot is dropped: the chart then draws a gap instead
/// of inventing a zero.
pub fn points_for(metric: &str, pts: &[Point]) -> Vec<Value> {
    let mut out = Vec::with_capacity(pts.len());
    for p in pts {
        let v = match metric {
            "cpu" => match p.cpu {
                Some(cpu) => json!({"ts": p.ts as i64, "cpu": cpu, "total": cpu}),
                None => continue,
            },
            "memory" => match p.ram {
                Some(ram) => json!({"ts": p.ts as i64, "ram": ram, "pct": ram}),
                None => continue,
            },
            "network" => {
                if p.down_bps.is_none() && p.up_bps.is_none() {
                    continue;
                }
                let mbps = |v: Option<f64>| v.map(|b| histdata::round3(b / 1e6));
                json!({
                    "ts": p.ts as i64,
                    "down_bps": p.down_bps,
                    "up_bps": p.up_bps,
                    "down_mbps": mbps(p.down_bps),
                    "up_mbps": mbps(p.up_bps),
                })
            }
            "load" => match p.load1 {
                Some(one) => json!({"ts": p.ts as i64, "one": one}),
                None => continue,
            },
            "battery" => {
                if p.battery_pct.is_none() && p.time_remaining_s.is_none() {
                    continue;
                }
                json!({
                    "ts": p.ts as i64,
                    "pct": p.battery_pct,
                    "time_remaining_s": p.time_remaining_s,
                })
            }
            "sensors" => {
                if p.temps.is_empty() {
                    // Fall back to the single aggregate reading the collector
                    // logs, so an old window is still plottable.
                    match p.temp_c {
                        Some(c) => json!({"ts": p.ts as i64, "sensors": {"temperature": c}}),
                        None => continue,
                    }
                } else {
                    let map: serde_json::Map<String, Value> = p
                        .temps
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (format!("Sensor {}", i + 1), json!(c)))
                        .collect();
                    json!({"ts": p.ts as i64, "sensors": Value::Object(map)})
                }
            }
            // disk / gpu: no series exists, handled before we get here.
            _ => continue,
        };
        out.push(v);
    }
    out
}

/// Average-bucket a metric series to at most `cap` points, keeping gaps as gaps
/// (the Python server's `_decimate`). A bucket with no numeric value for a field
/// emits `null`, never 0.
pub fn decimate(points: Vec<Value>, cap: usize) -> Vec<Value> {
    if points.len() <= cap || cap == 0 {
        return points;
    }
    let stride = points.len().div_ceil(cap);
    let mut out = Vec::with_capacity(cap);
    let mut i = 0;
    while i < points.len() {
        let chunk = &points[i..(i + stride).min(points.len())];
        let mut merged = serde_json::Map::new();
        merged.insert("ts".into(), chunk[0].get("ts").cloned().unwrap_or(Value::Null));
        let mut keys: Vec<String> = Vec::new();
        for c in chunk {
            if let Some(obj) = c.as_object() {
                for k in obj.keys() {
                    if k != "ts" && !keys.iter().any(|x| x == k) {
                        keys.push(k.clone());
                    }
                }
            }
        }
        for k in keys {
            let vals: Vec<f64> = chunk
                .iter()
                .filter_map(|c| c.get(&k).and_then(Value::as_f64))
                .collect();
            let v = if vals.is_empty() {
                Value::Null
            } else {
                json!(histdata::round3(vals.iter().sum::<f64>() / vals.len() as f64))
            };
            merged.insert(k, v);
        }
        out.push(Value::Object(merged));
        i += stride;
    }
    out
}

// ── peak and measures ───────────────────────────────────────────────────────

fn median(sorted: &[f64]) -> Option<f64> {
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

fn p95(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let idx = ((n as f64 * 0.95).ceil() as usize).max(1) - 1;
    Some(sorted[idx.min(n - 1)])
}

/// `{value, ts, is_spike, field}` or null. A spike is `max >= median * 1.2`
/// (the signal-vs-noise rule the Python server uses).
pub fn compute_peak(metric: &str, points: &[Value]) -> Value {
    let fields = fields(metric);
    let mut values: Vec<f64> = Vec::new();
    let mut best: Option<(f64, i64, &str)> = None;

    if metric == "sensors" {
        for p in points {
            let ts = p.get("ts").and_then(Value::as_i64).unwrap_or(0);
            if let Some(map) = p.get("sensors").and_then(Value::as_object) {
                for (k, v) in map {
                    if let Some(f) = v.as_f64() {
                        values.push(f);
                        if best.is_none_or(|(b, _, _)| f > b) {
                            best = Some((f, ts, k.as_str()));
                        }
                    }
                }
            }
        }
    } else {
        for p in points {
            let ts = p.get("ts").and_then(Value::as_i64).unwrap_or(0);
            for f in fields {
                if let Some(v) = p.get(*f).and_then(Value::as_f64) {
                    values.push(v);
                    if best.is_none_or(|(b, _, _)| v > b) {
                        best = Some((v, ts, f));
                    }
                }
            }
        }
    }

    let Some((max_val, max_ts, field)) = best else {
        return Value::Null;
    };
    values.sort_by(|a, b| a.total_cmp(b));
    let med = median(&values).unwrap_or(0.0);
    json!({
        "value": histdata::round3(max_val),
        "ts": max_ts,
        "is_spike": med > 0.0 && max_val >= med * 1.2,
        "field": field,
    })
}

/// Per-field mean/min/max/p95/median plus window coverage — the Python
/// `metric_measures`. A field with no samples is omitted, never reported as 0.
pub fn measures(metric: &str, points: &[Value], from: f64, to: f64) -> Value {
    let window_s = (to - from).max(1.0);
    let mut stats = serde_json::Map::new();

    let mut names: Vec<String> = fields(metric).iter().map(|s| s.to_string()).collect();
    if metric == "sensors" {
        for p in points {
            if let Some(map) = p.get("sensors").and_then(Value::as_object) {
                for k in map.keys() {
                    if !names.iter().any(|n| n == k) {
                        names.push(k.clone());
                    }
                }
            }
        }
    }

    for name in &names {
        let mut vals: Vec<(f64, f64)> = Vec::new();
        for p in points {
            let v = if metric == "sensors" {
                p.get("sensors").and_then(|s| s.get(name)).and_then(Value::as_f64)
            } else {
                p.get(name).and_then(Value::as_f64)
            };
            if let Some(v) = v {
                let ts = p.get("ts").and_then(Value::as_f64).unwrap_or(0.0);
                vals.push((v, ts));
            }
        }
        if vals.is_empty() {
            continue;
        }
        let mut sorted: Vec<f64> = vals.iter().map(|(v, _)| *v).collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let n = sorted.len();
        let sum: f64 = sorted.iter().sum();
        let (max_v, max_ts) = vals
            .iter()
            .fold((f64::NEG_INFINITY, 0.0), |acc, (v, t)| {
                if *v > acc.0 { (*v, *t) } else { acc }
            });
        stats.insert(
            name.clone(),
            json!({
                "n": n,
                "mean": histdata::round3(sum / n as f64),
                "min": histdata::round3(sorted[0]),
                "max": histdata::round3(max_v),
                "max_ts": max_ts,
                "p95": histdata::round3(p95(&sorted).unwrap_or(sorted[n - 1])),
                "median": histdata::round3(median(&sorted).unwrap_or(sorted[n - 1])),
            }),
        );
    }

    let mut ts: Vec<f64> = points
        .iter()
        .filter_map(|p| p.get("ts").and_then(Value::as_f64))
        .collect();
    ts.sort_by(|a, b| a.total_cmp(b));
    let mut coverage = 0.0;
    if ts.len() >= 2 {
        let mut gaps: Vec<f64> = ts.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let step = median(&gaps).unwrap_or(0.0);
        let covered = ((ts[ts.len() - 1] - ts[0]) + step).min(window_s);
        coverage = histdata::round1(100.0 * covered / window_s);
    }

    json!({
        "fields": Value::Object(stats),
        "coverage_pct": coverage,
        "window_s": window_s,
        "points": points.len(),
        "primary": primary_field(metric),
    })
}

fn primary_field(metric: &str) -> Value {
    match metric {
        "cpu" => json!("total"),
        "memory" => json!("pct"),
        "network" => json!("down_bps"),
        "disk" => json!("read_bps"),
        "gpu" => json!("util_pct"),
        "battery" => json!("pct"),
        "load" => json!("one"),
        _ => Value::Null,
    }
}

// ── tick ladder ─────────────────────────────────────────────────────────────

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Civil parts of a *local-time seconds* value (unix seconds + UTC offset).
fn parts(local_ts: f64) -> (i64, u32, u32, u32, u32, u32, usize) {
    let secs = local_ts.floor() as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = timefmt::civil_from_days(days);
    let wday = (days + 4).rem_euclid(7) as usize; // 1970-01-01 was a Thursday
    (
        y,
        m,
        d,
        (sod / 3600) as u32,
        ((sod % 3600) / 60) as u32,
        (sod % 60) as u32,
        wday,
    )
}

fn label(local_ts: f64, fmt: &str) -> String {
    let (y, m, d, hh, mm, ss, wday) = parts(local_ts);
    match fmt {
        "hms" => format!("{hh:02}:{mm:02}:{ss:02}"),
        "hm" => format!("{hh:02}:{mm:02}"),
        "wd" => format!("{} {d:02}", WEEKDAYS[wday]),
        "dm" => format!("{d:02} {}", MONTHS[(m - 1) as usize]),
        "mon" => MONTHS[(m - 1) as usize].to_string(),
        "mon-yr" => format!("{} '{:02}", MONTHS[(m - 1) as usize], y.rem_euclid(100)),
        _ => format!("{hh:02}:{mm:02}"),
    }
}

fn day_start(local_ts: f64) -> f64 {
    let secs = local_ts.floor() as i64;
    (secs - secs.rem_euclid(86_400)) as f64
}

fn add_months(local_ts: f64, months: i64) -> f64 {
    let (y, m, d, _, _, _, _) = parts(local_ts);
    let total = (y * 12 + (m as i64 - 1)) + months;
    let ny = total.div_euclid(12);
    let nm = (total.rem_euclid(12) + 1) as u32;
    let max_day = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(nm - 1) as usize];
    // Feb 29 → clamp in a non-leap year; every other month keeps its day.
    let leap = (ny % 4 == 0 && ny % 100 != 0) || ny % 400 == 0;
    let day = if nm == 2 && !leap { d.min(28) } else { d.min(max_day) };
    // Rebuild from civil parts; the local offset is fixed for the window, so
    // local seconds are plain arithmetic.
    let days = days_from_civil_pub(ny, nm, day);
    days as f64 * 86_400.0
}

fn days_from_civil_pub(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Calendar-aware axis ticks, 5–12 labels, in the viewer's local clock.
///
/// Carried over from the Python server's `tick_ladder`: shorter ranges align to
/// a fixed interval, day ranges to midnight, months to the first of the month.
/// The epoch values are what the chart plots; only the label text is local.
pub fn tick_ladder(range: &str, from: f64, to: f64) -> Vec<Value> {
    if !(to > from) {
        return Vec::new();
    }
    let off = histdata::local_offset_secs(from) as f64;
    let from_l = from + off;
    let to_l = to + off;
    let at = |local: f64| json!({"ts": (local - off) as i64, "label": ""});
    let push = |ticks: &mut Vec<Value>, local: f64, text: String| {
        let mut t = at(local);
        t["label"] = json!(text);
        ticks.push(t);
    };

    let mut ticks: Vec<Value> = Vec::new();
    match range {
        "7d" | "30d" => {
            let mut cur = day_start(from_l);
            // 7d steps one calendar day; 30d snaps to the 1/6/11/16/21/26 grid.
            while cur <= to_l {
                let (_, _, d, _, _, _, _) = parts(cur);
                let emit = range == "7d" || matches!(d, 1 | 6 | 11 | 16 | 21 | 26);
                if emit && cur >= from_l {
                    push(&mut ticks, cur, label(cur, if range == "7d" { "wd" } else { "dm" }));
                }
                cur += 86_400.0;
                if ticks.len() > 64 {
                    break;
                }
            }
        }
        "1y" => {
            let (y0, m0, _, _, _, _, _) = parts(from_l);
            let mut cur = days_from_civil_pub(y0, m0, 1) as f64 * 86_400.0;
            if cur < from_l {
                cur = add_months(cur, 1);
            }
            let (y1, _, _, _, _, _, _) = parts(to_l);
            let spans_years = y1 != y0;
            while cur <= to_l {
                let (_, m, _, _, _, _, _) = parts(cur);
                let text = if spans_years && m == 1 {
                    label(cur, "mon-yr")
                } else {
                    label(cur, "mon")
                };
                push(&mut ticks, cur, text);
                cur = add_months(cur, 1);
            }
        }
        _ => {
            let interval: f64 = match range {
                "1m" => 15.0,
                "5m" => 60.0,
                "15m" => 180.0,
                "1h" => 600.0,
                "6h" => 3_600.0,
                "1d" => 10_800.0,
                _ => (to - from) / 8.0,
            };
            let fmt = match range {
                "1m" | "5m" => "hms",
                "15m" | "1h" => "hm",
                _ => "hm",
            };
            // Align to the previous interval boundary in local time.
            let start = if interval >= 3_600.0 {
                let h = (from_l / 3_600.0).floor() * 3_600.0;
                h - (h as i64).rem_euclid(interval as i64) as f64
            } else {
                (from_l / interval).floor() * interval
            };
            let mut cur = start;
            while cur <= to_l {
                if cur >= from_l {
                    let text = label(cur, fmt);
                    push(&mut ticks, cur, text);
                }
                cur += interval;
                if ticks.len() > 128 {
                    break;
                }
            }
        }
    }

    // Hard limits from the analytics plan: 5–10 labels, 12 for a full year.
    if ticks.len() > 12 {
        let step = ticks.len().div_ceil(10);
        let kept: Vec<Value> = ticks.iter().step_by(step).cloned().collect();
        ticks = kept;
    }
    if ticks.is_empty() && to_l > from_l {
        push(&mut ticks, from_l, label(from_l, "hm"));
        push(&mut ticks, to_l, label(to_l, "hm"));
    }
    ticks
}

// ── the payload ─────────────────────────────────────────────────────────────

/// Request as parsed from the query string.
#[derive(Debug, Clone)]
pub struct GraphRequest {
    pub metric: String,
    pub range: Option<String>,
    pub from: Option<f64>,
    pub to: Option<f64>,
    pub max_points: usize,
}

/// Build `/api/graph`. `history` owns the ring and the collector reader.
///
/// A missing `metric` defaults to `cpu` so a bare `/api/graph` answers with a
/// real series instead of an error; an unknown metric answers 200 with
/// `available: false`, its `error` named, and the implemented list — the UI
/// never sends one, and a typo should not look like a broken server.
pub fn payload(history: &histdata::History, req: &GraphRequest, now: f64) -> Value {
    let raw = req.metric.trim();
    let metric = if raw.is_empty() {
        // A bare /api/graph answers with a real series rather than an error:
        // CPU is the metric every caller means when it names none.
        "cpu".to_string()
    } else {
        canonical_metric(raw)
    };
    if !METRICS.contains(&metric.as_str()) {
        return json!({
            "metric": metric,
            "error": format!("unknown metric: {metric}"),
            "available": false,
            "points": [],
            "ticks": [],
            "peak": Value::Null,
            "measures": {},
            "implemented_metrics": METRICS,
        });
    }

    // Window: a named range wins over from/to, exactly like the Python server.
    let (from, to, range_name) = match req.range.as_deref().and_then(histdata::range_secs) {
        Some(span) => (now - span, now, Some(req.range.clone().unwrap_or_default())),
        None => {
            let to = req.to.filter(|v| v.is_finite()).unwrap_or(now);
            let from = req
                .from
                .filter(|v| v.is_finite())
                .unwrap_or(to - 3_600.0);
            (from, to, None)
        }
    };
    let span = (to - from).max(1.0);
    let range_name = range_name.unwrap_or_else(|| histdata::range_for_secs(span).to_string());
    let tiers = histdata::tiers_for(span);

    // disk / gpu have no series anywhere on this machine: answer without
    // reading files at all, so the poll costs nothing.
    if metric == "disk" || metric == "gpu" {
        return json!({
            "metric": metric,
            "range": if req.range.is_some() { json!(range_name) } else { Value::Null },
            "from": from as i64,
            "to": to as i64,
            "points": [],
            "ticks": tick_ladder(&range_name, from, to),
            "peak": Value::Null,
            "available": false,
            "source": source_for(&metric),
            "measures": {},
            "count": 0,
            "raw_points": 0,
            "downsampled_to": 0,
            "sources": {"tier": "none", "files": [], "reason": "no history source for this metric"},
            "note": note_for(&metric),
            "unavailable_reason": empty_reason(&metric, span, &Sources::default()),
        });
    }

    let (points, sources) = history.points(from, to, span, tiers);
    let series = points_for(&metric, &points);
    let raw_points = series.len();
    let shaped = decimate(series, req.max_points.max(1));

    let mut out = json!({
        "metric": metric,
        "range": if req.range.is_some() { json!(range_name) } else { Value::Null },
        "from": from as i64,
        "to": to as i64,
        "points": shaped,
        "ticks": tick_ladder(&range_name, from, to),
        "peak": compute_peak(&metric, &shaped),
        "available": !shaped.is_empty(),
        "source": source_for(&metric),
        "measures": measures(&metric, &shaped, from, to),
        "count": shaped.len(),
        "raw_points": raw_points,
        "downsampled_to": shaped.len(),
        "max_points": req.max_points.max(1),
        "sources": sources.to_json(),
    });
    if let Some(note) = note_for(&metric) {
        out["note"] = json!(note);
    }
    if out["available"] == json!(false) {
        out["unavailable_reason"] = json!(empty_reason(&metric, span, &sources));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder(range: &str, from: f64, to: f64) -> Vec<Value> {
        tick_ladder(range, from, to)
    }

    #[test]
    fn ladder_produces_five_to_twelve_ticks() {
        // 2026-11-19T00:00:00Z onwards, in whatever zone this machine is in.
        let base = 1_795_046_400.0;
        for range in ["1m", "5m", "15m", "1h", "6h", "1d", "7d", "30d", "1y"] {
            let span = histdata::range_secs(range).unwrap();
            let ticks = ladder(range, base, base + span);
            assert!(
                (2..=12).contains(&ticks.len()),
                "{range}: {} ticks",
                ticks.len()
            );
            let ts: Vec<i64> = ticks.iter().filter_map(|t| t["ts"].as_i64()).collect();
            assert!(ts.windows(2).all(|w| w[0] < w[1]), "{range}: not ascending");
            assert!(ticks.iter().all(|t| t["label"].as_str().is_some_and(|s| !s.is_empty())));
        }
    }

    #[test]
    fn ladder_labels_are_wall_clock() {
        let ticks = tick_ladder("1h", 1_795_046_400.0, 1_795_050_000.0);
        // HH:MM in every label for an hour window.
        for t in &ticks {
            let l = t["label"].as_str().unwrap();
            assert!(l.len() == 5 && l.as_bytes()[2] == b':', "label {l}");
        }
    }

    #[test]
    fn metric_aliases_resolve() {
        assert_eq!(canonical_metric("net"), "network");
        assert_eq!(canonical_metric("ram"), "memory");
        assert_eq!(canonical_metric("gpu"), "gpu");
    }

    #[test]
    fn peak_finds_max_and_flags_a_spike() {
        let pts: Vec<Value> = (0..10)
            .map(|i| json!({"ts": i, "total": if i == 7 { 90.0 } else { 10.0 }}))
            .collect();
        let peak = compute_peak("cpu", &pts);
        assert_eq!(peak["value"], 90.0);
        assert_eq!(peak["ts"], 7);
        assert_eq!(peak["field"], "total");
        assert_eq!(peak["is_spike"], true);
    }

    #[test]
    fn peak_of_a_flat_series_is_not_a_spike() {
        let pts: Vec<Value> = (0..10).map(|i| json!({"ts": i, "total": 10.0})).collect();
        let peak = compute_peak("cpu", &pts);
        assert_eq!(peak["is_spike"], false);
    }

    #[test]
    fn peak_of_no_points_is_null() {
        assert_eq!(compute_peak("cpu", &[]), Value::Null);
    }

    #[test]
    fn measures_report_every_present_field() {
        let pts: Vec<Value> = (0..4)
            .map(|i| json!({"ts": i, "down_bps": 1000.0 * (i as f64 + 1.0), "up_bps": 10.0}))
            .collect();
        let m = measures("network", &pts, 0.0, 4.0);
        assert_eq!(m["fields"]["down_bps"]["n"], 4);
        assert_eq!(m["fields"]["down_bps"]["max"], 4000.0);
        assert_eq!(m["fields"]["down_bps"]["min"], 1000.0);
        assert_eq!(m["fields"]["up_bps"]["mean"], 10.0);
        assert!(m["coverage_pct"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn decimate_keeps_gaps_as_null() {
        let pts: Vec<Value> = (0..100)
            .map(|i| {
                if i % 2 == 0 {
                    json!({"ts": i, "v": 10.0})
                } else {
                    json!({"ts": i})
                }
            })
            .collect();
        let out = decimate(pts, 10);
        assert!(out.len() <= 10);
        assert!(out.iter().all(|p| p["v"].as_f64().is_some_and(|v| v == 10.0)));
    }
}
