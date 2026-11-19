//! `/api/history` — the whole-machine series for a named range, bucketed to at
//! most [`crate::histdata::MAX_POINTS`] points.
//!
//! # Shape parity
//!
//! Same keys as `Handler.api_history` in `dashboard/server.py`:
//! `{range, labels, points, series{cpu,ram,down_bps,up_bps}, temp_series,
//! sensor_labels, history_sources}`, plus `empty` + `data_started_iso` when the
//! window holds no raw point at all, and the additive keys this agent adds:
//! `metric` (the echoed query), `metric_series` (when `?metric=` was given),
//! `bucket_s` and `sources` (which tier/files answered).
//!
//! # Why points are ISO strings here
//!
//! The Python server emits `datetime.isoformat()` labels and the chart code
//! feeds them straight to `new Date(...)`. `/api/series` in this agent answers
//! unix seconds because it is a different endpoint with a different shape; this
//! one keeps ISO-8601 so the existing UI needs no change.
//!
//! # Down-sampling
//!
//! Buckets are anchored at `now − range` and are `range / n` seconds wide,
//! where `n = min(500, max(1, raw_points))` — the Python server's own scheme.
//! Empty buckets are dropped (the chart draws a gap) and every value inside a
//! bucket is an average of measured samples, never an interpolation.

use serde_json::{json, Value};

use crate::histdata::{self, Bucket, Sources};
use crate::timefmt;

/// Request as parsed from the query string.
#[derive(Debug, Clone)]
pub struct HistoryRequest {
    pub range: String,
    /// Optional single metric, answered as an extra `metric_series` array.
    pub metric: Option<String>,
}

/// Bucket the points and shape the payload.
pub fn payload(
    history: &histdata::History,
    req: &HistoryRequest,
    now: f64,
) -> Result<Value, String> {
    let span = histdata::range_secs(&req.range)
        .ok_or_else(|| format!("range must be one of {}", range_names()))?;
    let from = now - span;
    let tiers = histdata::tiers_for(span);
    let (points, sources) = history.points(from, now, span, tiers);

    let n_buckets = histdata::MAX_POINTS.min(points.len().max(1));
    let bucket_s = span / n_buckets as f64;
    let buckets = histdata::bucketize(&points, from, bucket_s, n_buckets);

    let labels: Vec<Value> = buckets
        .iter()
        .map(|b| json!(timefmt::rfc3339_utc((b.ts + bucket_s / 2.0) as i64)))
        .collect();
    let n_sensors = buckets.iter().map(|b| b.temps_mean().len()).max().unwrap_or(0);

    let temp_series: Vec<Value> = (0..n_sensors)
        .map(|i| {
            Value::Array(
                buckets
                    .iter()
                    .map(|b| match b.temps_mean().get(i) {
                        Some(Some(v)) => json!(v),
                        _ => Value::Null,
                    })
                    .collect(),
            )
        })
        .collect();

    let sensor_labels: Vec<String> = sensor_labels(n_sensors);
    let points_json: Vec<Value> = buckets.iter().map(bucket_json).collect();

    let mut out = json!({
        "range": req.range,
        "labels": labels,
        "points": points_json,
        "series": {
            "cpu": buckets.iter().map(Bucket::cpu).collect::<Vec<_>>(),
            "ram": buckets.iter().map(Bucket::ram).collect::<Vec<_>>(),
            "down_bps": buckets.iter().map(Bucket::down_bps).collect::<Vec<_>>(),
            "up_bps": buckets.iter().map(Bucket::up_bps).collect::<Vec<_>>(),
        },
        "temp_series": temp_series,
        "sensor_labels": sensor_labels,
        "history_sources": {
            "collector_points": sources.collector_points,
            "in_range_points": points.len(),
            "downsampled_to": buckets.len(),
            "ring_points": sources.ring_points,
            "tier": sources.tier,
            "files": sources.files,
            "bucket_s": histdata::round3(bucket_s),
        },
        "bucket_s": histdata::round3(bucket_s),
        "window_s": span,
        "sources": sources.to_json(),
    });

    // `?metric=` answers that one metric as a plain {ts,value} series, which is
    // what the brief asks for and what a single-series chart wants.
    if let Some(metric) = req.metric.as_deref().filter(|m| !m.trim().is_empty()) {
        let metric = crate::graph::canonical_metric(metric);
        let series_points: Vec<Value> = buckets
            .iter()
            .filter_map(|b| metric_value(&metric, b))
            .collect();
        out["metric"] = json!(metric);
        out["metric_series"] = json!(series_points);
        out["metric_available"] = json!(!series_points.is_empty());
        if series_points.is_empty() {
            out["metric_reason"] = json!(format!(
                "the {} tier in this {} window carries no `{}` sample",
                sources.tier,
                req.range,
                metric
            ));
        }
    }

    if points.is_empty() {
        out["empty"] = json!(true);
        out["reason"] = json!(empty_reason(&sources, span));
        if let Some(first) = earliest_point(history, now) {
            out["data_started_iso"] = json!(timefmt::rfc3339_utc(first as i64));
        }
    }
    Ok(out)
}

fn range_names() -> String {
    histdata::RANGES
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(",")
}

fn empty_reason(sources: &Sources, span: f64) -> String {
    format!(
        "no samples in this {} window: read {} file(s) from the {} tier ({} collector \
         record(s)) and {} ring point(s); the ring holds the last two hours of 1 s samples \
         and the collector log is the only durable history",
        histdata::fmt_span(span),
        sources.files.len(),
        sources.tier,
        sources.collector_points,
        sources.ring_points
    )
}

/// One bucket as a point object — the same fields the Python `bucketize` emits.
fn bucket_json(b: &Bucket) -> Value {
    json!({
        "ts": timefmt::rfc3339_utc(b.ts as i64),
        "cpu": b.cpu(),
        "ram": b.ram(),
        "down_bps": b.down_bps(),
        "up_bps": b.up_bps(),
        "temps": b.temps_mean(),
        "samples": b.samples,
    })
}

/// One metric out of a bucket, as `{ts, value}` — the brief's history series.
///
/// The metric is resolved with the same extractor `/api/series` uses, but on a
/// bucket rather than a raw record, so `?metric=memory.used` is answered from
/// the fields a bucket retained. A metric a bucket does not carry is dropped,
/// not zeroed.
fn metric_value(metric: &str, b: &Bucket) -> Option<Value> {
    let value = match metric {
        "cpu" | "cpu.total" | "cpu.total_pct" | "cpu.pct" | "cpu.usage" => b.cpu(),
        "memory" | "memory.pct" | "memory.used_pct" | "ram" => b.ram(),
        "network" | "net.down" | "net.rx_bytes_sec" => b.down_bps(),
        "net.up" | "net.tx_bytes_sec" => b.up_bps(),
        "load" | "load1" | "cpu.load_1m" | "cpu.load1" => b.load1(),
        "process_count" | "processes.count" => b.proc_count(),
        "temperature" | "temp.cpu" | "temp.cpu.die" => b.temp_c(),
        _ => return None,
    }?;
    Some(json!({"ts": b.ts as i64, "value": value, "samples": b.samples}))
}

/// Sensor names for the temperature chart.
///
/// The collector's rollup tiers carry a `temperatures` array with no names, and
/// `dashboard/sensors.json` describes SMC keys, not this order — so the labels
/// are positional and say so, rather than pretending to know which die sensor
/// each row is. `/api/thermal` answers the named rows from the root daemon.
fn sensor_labels(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("Sensor {}", i + 1)).collect()
}

/// The oldest timestamp any source knows about, for the "data starts …" hint.
///
/// A directory listing, not a parse: the rollup tiers rotate by date, so the
/// oldest file name *is* the oldest instant this machine recorded.
fn earliest_point(history: &histdata::History, _now: f64) -> Option<f64> {
    history
        .collector
        .oldest_available_ts(&["tier1800s", "tier300s", "tier60s", "raw"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_logs(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "syswatch-history-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("raw")).unwrap();
        d
    }

    fn write_raw(dir: &PathBuf, name: &str, rows: &[(u32, f64)]) {
        let mut text = String::new();
        for (sec, cpu) in rows {
            text.push_str(&format!(
                "{{\"timestamp\":\"2026-11-19T05:{:02}:{:02}Z\",\"cpu\":{{\"total_pct\":{}}},\
                 \"memory\":{{\"pct\":50.0}},\"networks\":[{{\"name\":\"en0\",\"rx_bps\":100.0,\
                 \"tx_bps\":50.0}}]}}\n",
                sec / 60,
                sec % 60,
                cpu
            ));
        }
        std::fs::write(dir.join("raw").join(name), text).unwrap();
    }

    #[test]
    fn history_buckets_collector_rows() {
        let dir = tmp_logs("bucket");
        // 10 s of 1 s samples starting at 05:00:00Z on 2026-11-19.
        let rows: Vec<(u32, f64)> = (0..10).map(|i| (i as u32, 10.0 + i as f64)).collect();
        write_raw(&dir, "2026-11-19T05-00.jsonl", &rows);

        let history = histdata::History::new(dir.clone());
        let now = histdata::record_ts(&serde_json::json!({"timestamp": "2026-11-19T05:00:09Z"})).unwrap();
        let out = payload(
            &history,
            &HistoryRequest {
                range: "1h".into(),
                metric: Some("cpu".into()),
            },
            now,
        )
        .unwrap();

        assert_eq!(out["range"], "1h");
        assert!(!out["points"].as_array().unwrap().is_empty());
        assert_eq!(out["history_sources"]["collector_points"], 10);
        // cpu mean over the whole 1 h window: the ten samples average to 14.5
        let cpu = out["series"]["cpu"].as_array().unwrap();
        let first = cpu[0].as_f64().unwrap();
        assert!((first - 14.5).abs() < 0.01, "cpu mean {first}");
        assert_eq!(out["metric"], "cpu");
        assert_eq!(out["metric_available"], true);
        assert!(out["metric_series"].as_array().unwrap()[0]["value"].is_number());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_window_says_so_without_inventing_points() {
        let dir = tmp_logs("empty");
        let history = histdata::History::new(dir.clone());
        let out = payload(
            &history,
            &HistoryRequest {
                range: "1h".into(),
                metric: None,
            },
            histdata::record_ts(&serde_json::json!({"timestamp": "2026-11-19T05:00:09Z"})).unwrap(),
        )
        .unwrap();
        assert_eq!(out["empty"], true);
        assert!(out["reason"].as_str().unwrap().contains("no samples"));
        assert_eq!(out["points"].as_array().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_range_is_an_error() {
        let dir = tmp_logs("badrange");
        let history = histdata::History::new(dir.clone());
        let err = payload(
            &history,
            &HistoryRequest {
                range: "2h".into(),
                metric: None,
            },
            0.0,
        )
        .unwrap_err();
        assert!(err.contains("range must be one of"), "{err}");
        assert!(err.contains("1y"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
