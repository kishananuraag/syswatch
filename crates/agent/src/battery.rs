//! `/api/battery` — pack state, capacities, life at the current draw, and the
//! degradation outlook from this machine's own recorded history.
//!
//! # Keys
//!
//! The Python server's `battery_payload()`: `{ts, pct, charging,
//! external_connected, draw_w, adapter_w, adapter_rated_w, voltage_v,
//! amperage_ma, temperature_c, time_to_empty_s, charge_to_full_s, capacity,
//! health_pct, condition, cycles, pack_wh, life_at_current_draw_h, life_note,
//! degradation, tariff, sources}`, plus the two aliases the brief names:
//! `charge_pct` (same value as `pct`) and `time_remaining_min`.
//!
//! # What is measured and what is an estimate
//!
//! * Measured now: V × A (draw), charge %, cycles, temperature, adapter watts.
//! * Static pack facts, read from `ioreg` and cached for a day: design and
//!   nominal capacity, `MaxCapacity` as a percentage or mAh — whichever the pack
//!   actually publishes.
//! * Estimates, labelled as such: `life_at_current_draw_h` (capacity ÷ draw
//!   *right now*, not a prediction) and `degradation` (a linear fit over our own
//!   recorded rows, and "insufficient data" until the span is
//!   [`MIN_DAYS`](degradation) long).
//!
//! # Battery history
//!
//! `battery-history.jsonl` is written by the Python server (`record_battery_history`,
//! one row per 30 min). This endpoint only ever **reads** it — the Rust agent
//! ships no mutation of its own — and says which file it read and how many rows
//! it found, so an empty outlook is diagnosable.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::histdata::{round1, round2, round3};
use crate::ioreg;
use crate::power::root_state;
use crate::timefmt;

/// A projection needs two weeks of our own rows before it means anything.
pub const MIN_DAYS: f64 = 14.0;
/// The record cadence the writer uses (30 min).
pub const CADENCE_S: f64 = 1_800.0;
const CAVEAT: &str = "estimate; assumes current usage pattern, not a prediction.";
const HISTORY_CACHE: Duration = Duration::from_secs(60);
const CAPACITY_CACHE: Duration = Duration::from_secs(86_400);

/// One recorded row of `battery-history.jsonl`.
#[derive(Debug, Clone)]
pub struct HistoryRow {
    pub ts: f64,
    pub cycles: Option<i64>,
    pub health_pct: Option<f64>,
    /// Recorded by the Python writer; kept so the reader is a faithful view of
    /// the file even though the outlook does not project from it.
    #[allow(dead_code)]
    pub pct: Option<f64>,
}

fn history_path(data_dir: &PathBuf) -> PathBuf {
    if let Ok(p) = std::env::var("SYSWATCH_BATTERY_HISTORY") {
        return PathBuf::from(p);
    }
    data_dir.join("battery-history.jsonl")
}

type RowCache = Mutex<Option<(Instant, PathBuf, Vec<HistoryRow>)>>;

fn rows_cache() -> &'static RowCache {
    static CELL: OnceLock<RowCache> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Every row we have recorded, oldest first. Never fails: a missing file is an
/// empty list, not an error.
pub fn history_rows(data_dir: &PathBuf) -> Vec<HistoryRow> {
    let path = history_path(data_dir);
    if let Ok(guard) = rows_cache().lock()
        && let Some((at, cached_path, rows)) = guard.as_ref()
        && *cached_path == path
        && at.elapsed() < HISTORY_CACHE
    {
        return rows.clone();
    }
    let rows = read_rows(&path);
    if let Ok(mut guard) = rows_cache().lock() {
        *guard = Some((Instant::now(), path, rows.clone()));
    }
    rows
}

fn read_rows(path: &std::path::Path) -> Vec<HistoryRow> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut rows: Vec<HistoryRow> = text
        .lines()
        .filter_map(|line| {
            let v: Value = serde_json::from_str(line.trim()).ok()?;
            Some(HistoryRow {
                ts: v.get("ts").and_then(Value::as_f64)?,
                cycles: v.get("cycles").and_then(Value::as_i64),
                health_pct: v.get("health_pct").and_then(Value::as_f64),
                pct: v.get("pct").and_then(Value::as_f64),
            })
        })
        .collect();
    rows.sort_by(|a, b| a.ts.total_cmp(&b.ts));
    rows
}

/// Cycle rate and a linear projection to 80 % health, from our own history.
///
/// Honest by construction: under two weeks of rows the answer is "insufficient
/// data", and with rows but no measurable decline it says that instead of
/// inventing a slope.
pub fn degradation(rows: &[HistoryRow], file_name: &str) -> Value {
    let usable: Vec<&HistoryRow> = rows
        .iter()
        .filter(|r| r.cycles.is_some() && r.health_pct.is_some())
        .collect();
    let mut out = json!({
        "available": false,
        "reason": Value::Null,
        "caveat": CAVEAT,
        "min_days": MIN_DAYS,
        "rows": usable.len(),
        "path": file_name,
        "cadence_s": CADENCE_S,
        "span_days": Value::Null,
        "cycles_per_day": Value::Null,
        "health_pct_per_month": Value::Null,
        "health_pct": usable.last().and_then(|r| r.health_pct),
        "cycles": usable.last().and_then(|r| r.cycles),
        "days_to_80": Value::Null,
        "projected_80_ts": Value::Null,
        "projected_80_date": Value::Null,
        "method": "linear fit over our own recorded (ts, cycles, health %); 80 % is \
                   the threshold, not a failure date",
    });

    if usable.len() < 2 {
        out["reason"] = json!(format!(
            "insufficient data — {} recorded sample{} of battery health (needs 2 \
             spanning {:.0} days)",
            usable.len(),
            if usable.len() == 1 { "" } else { "s" },
            MIN_DAYS
        ));
        return out;
    }

    let first = usable[0];
    let last = usable[usable.len() - 1];
    let span_days = (last.ts - first.ts) / 86_400.0;
    out["span_days"] = json!(round2(span_days));
    if span_days <= 0.0 {
        out["reason"] = json!("insufficient data — the recorded samples share one timestamp");
        return out;
    }

    let cycles_per_day = (last.cycles.unwrap_or(0) - first.cycles.unwrap_or(0)) as f64 / span_days;
    out["cycles_per_day"] = json!(round3(cycles_per_day));
    if span_days < MIN_DAYS {
        out["reason"] = json!(format!(
            "insufficient data — {span_days:.1} days of battery history (needs {MIN_DAYS:.0})"
        ));
        return out;
    }

    let h_first = first.health_pct.unwrap_or(0.0);
    let h_last = last.health_pct.unwrap_or(0.0);
    let drop_per_day = (h_first - h_last) / span_days;
    out["health_pct_per_month"] = json!(round3(drop_per_day * 30.0));
    if h_last <= 80.0 {
        out["reason"] = json!(format!(
            "health is already at or below 80 % ({h_last:.1} %) — no projection to make"
        ));
        return out;
    }
    if drop_per_day <= 0.0005 {
        out["reason"] = json!(format!(
            "no measurable health decline over {span_days:.1} days — a projection \
             would be invented, so none is shown"
        ));
        return out;
    }

    let days_to_80 = (h_last - 80.0) / drop_per_day;
    let projected_ts = (last.ts + days_to_80 * 86_400.0) as i64;
    out["available"] = json!(true);
    out["days_to_80"] = json!(round1(days_to_80));
    out["projected_80_ts"] = json!(projected_ts);
    out["projected_80_date"] = json!(timefmt::rfc3339_utc(projected_ts)[..10].to_string());
    out["reason"] = Value::Null;
    out
}

/// The static-capacity block, cached for a day (it changes when the pack does,
/// which is to say never).
fn capacity_block(info: &ioreg::BatteryInfo) -> Value {
    let health = info.capacity_health_pct();
    json!({
        "available": health.is_some(),
        "design_mah": info.design_mah.map(|v| v as i64),
        "nominal_mah": info.nominal_mah.map(|v| v as i64),
        "max_capacity_mah": info.max_capacity_mah.map(|v| v as i64),
        "max_capacity_pct": info.max_capacity_pct,
        "health_pct": health,
        "health_basis": if health.is_some() {
            Value::String(
                "NominalChargeCapacity ÷ DesignCapacity (both mAh, read from the pack)"
                    .to_string(),
            )
        } else {
            Value::Null
        },
        "source": "ioreg AppleSmartBattery",
        "cache_ttl_s": CAPACITY_CACHE.as_secs_f64(),
        "read_ts": timefmt::now_unix(),
        "reason": if health.is_some() {
            Value::Null
        } else {
            Value::String(
                "the pack does not expose both DesignCapacity and NominalChargeCapacity, \
                 so health % cannot be computed"
                    .to_string(),
            )
        },
    })
}

/// Build the whole `/api/battery` payload.
pub fn payload(data_dir: &PathBuf) -> Value {
    let now = timefmt::now_unix();
    let root = root_state();
    let root_w = root.sample.as_ref().and_then(|s| s.package_w);
    let info = ioreg::read();

    let Some(info) = info else {
        // A desktop Mac: answer the full shape with nulls and a reason, never
        // a wall of zeroes that reads as "0 % charged".
        return json!({
            "ts": now,
            "pct": Value::Null,
            "charge_pct": Value::Null,
            "charging": Value::Null,
            "external_connected": Value::Null,
            "draw_w": root_w,
            "adapter_w": Value::Null,
            "adapter_rated_w": Value::Null,
            "voltage_v": Value::Null,
            "amperage_ma": Value::Null,
            "temperature_c": Value::Null,
            "time_to_empty_s": Value::Null,
            "charge_to_full_s": Value::Null,
            "time_remaining_min": Value::Null,
            "capacity": {"available": false, "reason": "no AppleSmartBattery in the IORegistry"},
            "health_pct": Value::Null,
            "condition": "unavailable",
            "cycles": Value::Null,
            "pack_wh": Value::Null,
            "life_at_current_draw_h": Value::Null,
            "life_note": "no battery API exposed on this machine, so capacity ÷ draw has \
                          nothing to divide",
            "degradation": degradation(&[], "battery-history.jsonl"),
            "tariff": tariff_block(),
            "sources": sources_block("battery-history.jsonl"),
            "unavailable_reason": "no AppleSmartBattery in the IORegistry (desktop Mac, or ioreg refused)",
        });
    };

    let g = &info.gauge;
    let capacity = capacity_block(&info);
    let pack_wh = info.pack_wh();
    let draw = g.draw_w.or(root_w);
    let discharging = g.external_connected == Some(false);

    let (life_h, life_note) = match (draw, pack_wh) {
        (Some(w), Some(wh)) if w > 0.05 && wh > 0.0 => {
            let h = round2(wh / w);
            let mut note = format!(
                "at current draw: pack energy ({wh:.2} Wh) ÷ draw right now ({w:.2} W)"
            );
            if !discharging {
                note.push_str(
                    " — the pack is on AC and not draining, so read this as capacity ÷ \
                     draw, not as remaining time",
                );
            }
            (json!(h), json!(note))
        }
        _ => (
            Value::Null,
            json!(
                "no positive draw or no pack energy reading, so capacity ÷ draw has \
                 nothing to divide"
            ),
        ),
    };

    let time_s = g.time_remaining_min.map(|m| m * 60.0);
    let rows = history_rows(data_dir);
    let file_name = history_path(data_dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "battery-history.jsonl".to_string());

    json!({
        "ts": now,
        "pct": g.charge_pct,
        "charge_pct": g.charge_pct,
        "charging": g.charging,
        "external_connected": g.external_connected,
        "draw_w": draw,
        "adapter_w": info.adapter_w,
        "adapter_rated_w": info.adapter_rated_w,
        "voltage_v": g.voltage_v,
        "amperage_ma": g.amperage_ma,
        "temperature_c": g.temp_c,
        "time_remaining_min": g.time_remaining_min,
        "time_to_empty_s": if g.charging != Some(true) { json!(time_s) } else { Value::Null },
        "charge_to_full_s": if g.charging == Some(true) { json!(time_s) } else { Value::Null },
        "capacity": capacity,
        "health_pct": capacity["health_pct"].clone(),
        "condition": info.condition(),
        "cycles": g.cycles.map(|c| c as i64),
        "pack_wh": pack_wh,
        "life_at_current_draw_h": life_h,
        "life_note": life_note,
        "degradation": degradation(&rows, &file_name),
        "tariff": tariff_block(),
        "sources": sources_block(&file_name),
        "history_rows": rows.len(),
    })
}

fn tariff_block() -> Value {
    json!({
        "configured": false,
        "cost": Value::Null,
        "detail": "no electricity tariff configured — cost stays hidden until a rate is \
                   entered in settings",
    })
}

fn sources_block(history_file: &str) -> Value {
    json!({
        "draw_w": "AppleSmartBattery Voltage × Amperage via ioreg (unprivileged)",
        "capacity": "ioreg AppleSmartBattery DesignCapacity / NominalChargeCapacity, \
                     read once per 24 h",
        "cycles": "ioreg AppleSmartBattery CycleCount",
        "time": "ioreg AvgTimeToEmpty / AvgTimeToFull (Apple's own estimate)",
        "degradation": format!(
            "this machine's own battery history ({history_file}), one row per 30 min"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "syswatch-battery-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn degradation_needs_two_rows() {
        let d = degradation(&[], "battery-history.jsonl");
        assert_eq!(d["available"], false);
        assert!(d["reason"].as_str().unwrap().contains("insufficient data"));
        assert_eq!(d["rows"], 0);
        assert_eq!(d["caveat"], CAVEAT);
    }

    #[test]
    fn degradation_says_insufficient_below_two_weeks() {
        let rows = vec![
            HistoryRow { ts: 1_000_000.0, cycles: Some(340), health_pct: Some(82.5), pct: None },
            HistoryRow { ts: 1_000_000.0 + 86_400.0 * 2.0, cycles: Some(341), health_pct: Some(82.4), pct: None },
        ];
        let d = degradation(&rows, "battery-history.jsonl");
        assert_eq!(d["available"], false);
        assert_eq!(d["span_days"], 2.0);
        assert!(d["reason"].as_str().unwrap().contains("2.0 days"));
    }

    #[test]
    fn degradation_projects_only_with_a_real_decline() {
        // 30 days, health 82.5 → 81.0 (0.05 pp/day) → ~20 days to 80 %.
        let rows = vec![
            HistoryRow { ts: 0.0, cycles: Some(300), health_pct: Some(82.5), pct: None },
            HistoryRow { ts: 86_400.0 * 30.0, cycles: Some(330), health_pct: Some(81.0), pct: None },
        ];
        let d = degradation(&rows, "battery-history.jsonl");
        assert_eq!(d["available"], true);
        assert_eq!(d["days_to_80"], 20.0);
        assert_eq!(d["cycles_per_day"], 1.0);
        assert_eq!(d["health_pct_per_month"], 1.5);
        assert!(d["projected_80_date"].as_str().unwrap().len() == 10);
    }

    #[test]
    fn a_flat_health_series_is_not_projected() {
        let rows = vec![
            HistoryRow { ts: 0.0, cycles: Some(350), health_pct: Some(81.8), pct: None },
            HistoryRow { ts: 86_400.0 * 20.0, cycles: Some(350), health_pct: Some(81.8), pct: None },
        ];
        let d = degradation(&rows, "battery-history.jsonl");
        assert_eq!(d["available"], false);
        assert!(d["reason"].as_str().unwrap().contains("no measurable health decline"));
    }

    #[test]
    fn history_rows_are_read_oldest_first_and_junk_is_skipped() {
        let d = tmp("rows");
        std::fs::write(
            d.join("battery-history.jsonl"),
            "{\"ts\":300,\"cycles\":10,\"health_pct\":80.0}\n\
             not json\n\
             {\"ts\":100,\"cycles\":8,\"health_pct\":81.0}\n\
             {\"cycles\":9}\n",
        )
        .unwrap();
        let rows = read_rows(&d.join("battery-history.jsonl"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ts, 100.0);
        assert_eq!(rows[1].ts, 300.0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn capacity_block_reports_the_basis_it_used() {
        let info = ioreg::parse(
            r#"  {
                "NominalChargeCapacity" = 4705
                "DesignCapacity" = 5760
                "AppleRawMaxCapacity" = 4561
                "MaxCapacity" = 100
              }"#,
        );
        let cap = capacity_block(&info);
        assert_eq!(cap["available"], true);
        assert_eq!(cap["design_mah"], 5760);
        assert_eq!(cap["nominal_mah"], 4705);
        assert_eq!(cap["max_capacity_pct"], 100.0);
        assert_eq!(cap["health_pct"], 81.7);
    }
}
