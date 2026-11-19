//! `/api/power` — where the watts come from, per source, with the tier that
//! measured each one named.
//!
//! # The three tiers, unchanged from the Python server
//!
//! | tier | what it answers | source |
//! |------|-----------------|--------|
//! | 1 | battery V × A, charging, per-app attribution | `ioreg AppleSmartBattery` + the collector's per-PID rusage sweep |
//! | 2 | adapter input watts while connected | `ioreg AppleRawAdapterDetails` |
//! | 3 | package / CPU / GPU / ANE watts, die temps, fans, cluster clocks | the root daemon's `sample.json` (read-only) |
//!
//! Nothing here runs as root. The privileged numbers are read out of the file
//! `syswatch-powerd` writes; when that file is missing or older than
//! [`ROOT_STALE_S`] the elevated block says so and every value in it is `null`
//! — a dead daemon must not leave a stale temperature on screen pretending to
//! be live.
//!
//! # Keys
//!
//! Same keys as `power_state()` in `dashboard/server.py`, plus two additive
//! ones the brief asks for at the top level: `clusters` (E/P cluster clock and
//! residency, then per logical CPU) and `ane_w` (mirrored out of `root`).
//! `by_app` comes from the newest `raw/*.jsonl` record, because the daily 15 s
//! tier carries no `processes` block at all.

use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use syswatch_sensors_macos::rootsample::{self, RootSample};

use crate::ioreg;
use crate::timefmt;

/// A sample older than this is treated as absent (the daemon writes every 15 s;
/// `ROOT_SAMPLE_STALE_S` in the Python server).
pub const ROOT_STALE_S: f64 = 30.0;
/// Per-app attribution cadence. The collector writes a `raw/` record every
/// ~5 s and `/api/power` is polled at 1 s, so the tail read is cached.
const BY_APP_TTL: Duration = Duration::from_secs(5);

/// The root daemon's latest sample, with the staleness rule applied.
pub struct RootState {
    pub available: bool,
    pub reason: Option<String>,
    pub age_s: Option<f64>,
    pub sample: Option<RootSample>,
}

/// Read the daemon file and apply the "missing or stale means absent" rule.
///
/// Shared with `/api/thermal`, which shows the same SMC rows.
pub fn root_state() -> RootState {
    match rootsample::read() {
        None => RootState {
            available: false,
            reason: Some(missing_reason()),
            age_s: None,
            sample: None,
        },
        Some(sample) => {
            let age = sample.age_s.map(crate::histdata::round1);
            if let Some(age) = age
                && age > ROOT_STALE_S
            {
                return RootState {
                    available: false,
                    reason: Some(format!(
                        "needs root — last privileged sample is {age:.0} s old \
                         (> {ROOT_STALE_S:.0} s), so its values are withheld rather \
                         than shown as live"
                    )),
                    age_s: Some(age),
                    sample: None,
                };
            }
            RootState {
                available: true,
                reason: None,
                age_s: age,
                sample: Some(sample),
            }
        }
    }
}

fn missing_reason() -> String {
    format!(
        "needs root — the privileged daemon has not written {} (temperatures, \
         package power and Neural Engine are root-only on macOS)",
        rootsample::sample_path()
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sample.json".to_string())
    )
}

/// The elevated (root) block, in the Python server's key order.
fn root_block(root: &RootState) -> Value {
    let s = root.sample.as_ref();
    let get = |f: fn(&RootSample) -> Option<f64>| s.and_then(f);
    json!({
        "available": root.available,
        "age_s": root.age_s,
        "reason": root.reason,
        "file": "sample.json",
        "source": "powermetrics, run by the root LaunchDaemon \
                   com.barbarian.syswatch.power; the dashboard only reads the file",
        "package_w": get(|s| s.package_w),
        "cpu_w": get(|s| s.cpu_w),
        "gpu_w": get(|s| s.gpu_w),
        "ane_w": get(|s| s.ane_w),
        "cpu_temp_c": get(|s| s.cpu_temp_c),
        "gpu_temp_c": get(|s| s.gpu_temp_c),
        "cpu_cluster_temps_c": s.map(|s| rows_to_json(&s.cpu_cluster_temps)),
        "temps": s.map(|s| json!(s.temps.iter().map(|t| json!({
            "key": t.key, "label": t.label, "c": crate::histdata::round2(t.c),
        })).collect::<Vec<_>>())),
        "cpu_avg_c": get(|s| s.cpu_avg_c),
        "gpu_avg_c": get(|s| s.gpu_avg_c),
        "battery_c": get(|s| s.battery_c),
        "ssd_c": get(|s| s.ssd_c),
        "e_cluster": s.and_then(|s| s.e_cluster.as_ref()).map(|c| c.to_json()),
        "p_cluster": s.and_then(|s| s.p_cluster.as_ref()).map(|c| c.to_json()),
        "gpu_freq_mhz": get(|s| s.gpu_freq_mhz),
        "gpu_residency_pct": get(|s| s.gpu_residency_pct),
        "per_cpu": s.map(|s| json!(s.per_cpu.iter().map(|c| c.to_json()).collect::<Vec<_>>())),
        "smc": s.and_then(|s| s.smc.clone()),
        "thermal_pressure": s.and_then(|s| s.thermal_pressure.clone()),
        "fans_rpm": s.map(|s| {
            if s.fans_rpm.is_empty() { Value::Null } else { json!(s.fans_rpm) }
        }),
        "ane_util_pct": get(|s| s.ane_util_pct),
    })
}

fn rows_to_json(rows: &[(String, f64)]) -> Value {
    if rows.is_empty() {
        return Value::Null;
    }
    json!(rows
        .iter()
        .map(|(label, c)| json!({"label": label, "c": c}))
        .collect::<Vec<_>>())
}

/// One row of per-application energy attribution.
#[derive(Debug, Clone)]
struct AppRow {
    pid: Option<i64>,
    name: String,
    watts: f64,
}

fn by_app_cache() -> &'static Mutex<Option<(Instant, Vec<AppRow>, Option<String>)>> {
    static CELL: OnceLock<Mutex<Option<(Instant, Vec<AppRow>, Option<String>)>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Top-10 apps by measured energy, from the newest collector `raw/` record.
///
/// The daily 15 s tier writes no `processes` block (verified), so this reads the
/// raw tier specifically — and says so in `by_app_basis` when it cannot.
fn by_app(logs_dir: &Path) -> (Vec<Value>, Option<String>) {
    if let Ok(guard) = by_app_cache().lock()
        && let Some((at, rows, reason)) = guard.as_ref()
        && at.elapsed() < BY_APP_TTL
    {
        return (rows.iter().map(app_json).collect(), reason.clone());
    }

    let rec = crate::collector::latest_raw_record(logs_dir);
    let (rows, reason) = match rec {
        None => (
            Vec::new(),
            Some(
                "the collector's raw tier has no record on disk, so no per-PID energy \
                 sweep exists to rank"
                    .to_string(),
            ),
        ),
        Some(value) => {
            let mut rows: Vec<AppRow> = Vec::new();
            if let Some(list) = value
                .get("processes")
                .and_then(|p| p.get("by_energy"))
                .and_then(Value::as_array)
            {
                for r in list.iter().take(10) {
                    let Some(watts) = r.get("watts").and_then(Value::as_f64) else {
                        continue;
                    };
                    rows.push(AppRow {
                        pid: r.get("pid").and_then(Value::as_i64),
                        name: r
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                        watts: crate::histdata::round3(watts),
                    });
                }
            }
            let reason = if rows.is_empty() {
                Some("the newest collector record carries no processes.by_energy rows".to_string())
            } else {
                None
            };
            (rows, reason)
        }
    };

    if let Ok(mut guard) = by_app_cache().lock() {
        *guard = Some((Instant::now(), rows.clone(), reason.clone()));
    }
    (rows.iter().map(app_json).collect(), reason)
}

fn app_json(row: &AppRow) -> Value {
    json!({"pid": row.pid, "name": row.name, "watts": row.watts})
}

/// The battery block: the gauge's own numbers, with the reason for each gap.
fn battery_block(info: Option<&ioreg::BatteryInfo>, root_package_w: Option<f64>) -> Value {
    let Some(info) = info else {
        return json!({
            "pct": Value::Null,
            "time_remaining_s": Value::Null,
            "charging": Value::Null,
            "cycles": Value::Null,
            "condition": "unavailable",
            "health_pct": Value::Null,
            "cells": Value::Null,
            "battery_w": root_package_w,
            "temp_c": Value::Null,
            "external_connected": Value::Null,
            "adapter_w": Value::Null,
            "adapter_source": Value::Null,
            "adapter_rated_w": Value::Null,
            "voltage_v": Value::Null,
            "amperage_ma": Value::Null,
            "nominal_wh": Value::Null,
            "unavailable_reason": "no AppleSmartBattery in the IORegistry (desktop Mac, or ioreg refused)",
        });
    };
    let g = &info.gauge;
    let remaining_s = g.time_remaining_min.map(|m| (m * 60.0).round());
    // On AC the gauge moves no current, so V × A reads 0 while the machine is
    // still drawing watts — the root daemon's package power is the real draw.
    let battery_w = g.draw_w.or(root_package_w);
    json!({
        "pct": g.charge_pct,
        "time_remaining_s": remaining_s,
        "charging": g.charging,
        "cycles": g.cycles,
        "condition": info.condition(),
        "health_pct": info.capacity_health_pct().or(g.health_pct),
        "cells": if info.cells_mv.is_empty() { Value::Null } else { json!(info.cells_mv) },
        "battery_w": battery_w,
        "temp_c": g.temp_c,
        "external_connected": g.external_connected,
        "adapter_w": info.adapter_w,
        "adapter_source": info.adapter_source,
        "adapter_rated_w": info.adapter_rated_w,
        "voltage_v": g.voltage_v,
        "amperage_ma": g.amperage_ma,
        "nominal_wh": info.pack_wh(),
        "unavailable_reason": if battery_w.is_none() {
            Value::String("voltage/current not exposed by the battery API".to_string())
        } else {
            Value::Null
        },
    })
}

/// Build the whole `/api/power` payload.
pub fn payload(history: &crate::histdata::History) -> Value {
    let root = root_state();
    let info = ioreg::read();
    let battery = battery_block(info.as_ref(), root.sample.as_ref().and_then(|s| s.package_w));

    let (apps, apps_reason) = by_app(history.collector.logs_dir());

    let root_package_w = root.sample.as_ref().and_then(|s| s.package_w);
    let (package, package_reason) = match (&root.sample, root_package_w) {
        (Some(s), Some(w)) => (
            json!({
                "cpu_w": s.cpu_w,
                "gpu_w": s.gpu_w,
                "ane_w": s.ane_w,
                "package_w": w,
                "source": "powermetrics via the root daemon file \
                           (package total = CPU + GPU + ANE)",
                "age_s": root.age_s,
            }),
            Value::String(format!(
                "root powermetrics sample, {} s old",
                root.age_s.unwrap_or(0.0)
            )),
        ),
        _ => (
            json!({"cpu_w": Value::Null, "gpu_w": Value::Null, "ane_w": Value::Null,
                   "package_w": Value::Null}),
            json!("SMC/IOReport package power unreadable without root on this machine"),
        ),
    };

    let clusters = json!({
        "e_cluster": root.sample.as_ref().and_then(|s| s.e_cluster.as_ref()).map(|c| c.to_json()),
        "p_cluster": root.sample.as_ref().and_then(|s| s.p_cluster.as_ref()).map(|c| c.to_json()),
        "per_cpu": root.sample.as_ref().map(|s| {
            json!(s.per_cpu.iter().map(|c| c.to_json()).collect::<Vec<_>>())
        }),
        "gpu_freq_mhz": root.sample.as_ref().and_then(|s| s.gpu_freq_mhz),
        "gpu_residency_pct": root.sample.as_ref().and_then(|s| s.gpu_residency_pct),
        "thermal_pressure": root.sample.as_ref().and_then(|s| s.thermal_pressure.clone()),
        "source": "root powermetrics clusters (frequency + residency % of the interval)",
    });

    let mut out = json!({
        "battery_w": battery.get("battery_w").cloned().unwrap_or(Value::Null),
        "charging": battery.get("charging").cloned().unwrap_or(Value::Null),
        "external_connected": battery.get("external_connected").cloned().unwrap_or(Value::Null),
        "adapter_w": battery.get("adapter_w").cloned().unwrap_or(Value::Null),
        "adapter_source": battery.get("adapter_source").cloned().unwrap_or(Value::Null),
        "adapter_rated_w": battery.get("adapter_rated_w").cloned().unwrap_or(Value::Null),
        "package_w": root_package_w,
        "cpu_w": root.sample.as_ref().and_then(|s| s.cpu_w),
        "gpu_w": root.sample.as_ref().and_then(|s| s.gpu_w),
        "ane_w": root.sample.as_ref().and_then(|s| s.ane_w),
        "thermal_pressure": root.sample.as_ref().and_then(|s| s.thermal_pressure.clone()),
        "clusters": clusters,
        "root_sample": {
            "available": root.available,
            "reason": root.reason,
            "age_s": root.age_s,
        },
        "root": root_block(&root),
        "by_app": apps,
        "by_app_basis": "sum of per-PID proc_pid_rusage(RUSAGE_INFO_V6) energy deltas \
                         over the collector's last sweep — a measured floor, not the \
                         machine total",
        "package": package,
        "package_unavailable_reason": package_reason,
        "battery": battery,
        "tiers": {
            "tier1": "battery V*A + charging + per-app attribution",
            "tier2": "adapter input W from ioreg AppleRawAdapterDetails (while connected)",
            "tier3": "root powermetrics package power + die temps + fans + ANE \
                      (the daemon file, read-only)",
        },
        "ts": timefmt::now_unix(),
    });
    if let Some(reason) = apps_reason {
        out["by_app_unavailable_reason"] = json!(reason);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_block_nulls_every_value_when_the_daemon_is_absent() {
        let state = RootState {
            available: false,
            reason: Some("needs root".into()),
            age_s: None,
            sample: None,
        };
        let block = root_block(&state);
        assert_eq!(block["available"], false);
        assert_eq!(block["package_w"], Value::Null);
        assert_eq!(block["temps"], Value::Null);
        assert_eq!(block["thermal_pressure"], Value::Null);
        assert_eq!(block["e_cluster"], Value::Null);
    }

    #[test]
    fn root_block_carries_clusters_and_pressure_when_present() {
        let sample = rootsample::parse(
            r#"{"ts":1795118510,"package_w":0.549,"cpu_w":0.437,"gpu_w":0.112,"ane_w":0,
                "thermal_pressure":"Nominal",
                "e_cluster":{"freq_mhz":1205,"residency_pct":67.34},
                "p_cluster":{"freq_mhz":918,"residency_pct":54.4},
                "per_cpu":[{"freq_mhz":1329,"residency_pct":26.67}],
                "temps":[{"key":"Tp01","label":"CPU p-core 1","c":43.04}],
                "cpu_avg_c":42.75,"gpu_avg_c":41.79,"battery_c":31.9,"ssd_c":35.52}"#,
        )
        .unwrap();
        let state = RootState {
            available: true,
            reason: None,
            age_s: Some(15.4),
            sample: Some(sample),
        };
        let block = root_block(&state);
        assert_eq!(block["available"], true);
        assert_eq!(block["package_w"], 0.549);
        assert_eq!(block["ane_w"], 0.0);
        assert_eq!(block["thermal_pressure"], "Nominal");
        assert_eq!(block["e_cluster"]["freq_mhz"], 1205.0);
        assert_eq!(block["p_cluster"]["residency_pct"], 54.4);
        assert_eq!(block["per_cpu"][0]["residency_pct"], 26.67);
        assert_eq!(block["temps"][0]["label"], "CPU p-core 1");
    }

    #[test]
    fn battery_block_without_a_pack_says_why() {
        let block = battery_block(None, None);
        assert_eq!(block["pct"], Value::Null);
        assert_eq!(block["condition"], "unavailable");
        assert!(block["unavailable_reason"].as_str().unwrap().contains("no AppleSmartBattery"));
    }

    #[test]
    fn battery_block_reports_the_gauge_when_there_is_one() {
        let info = ioreg::parse(
            r#"  {
                "CurrentCapacity" = 71
                "MaxCapacity" = 100
                "NominalChargeCapacity" = 4705
                "DesignCapacity" = 5760
                "AppleRawMaxCapacity" = 4561
                "AppleRawBatteryVoltage" = 11860
                "Amperage" = 18446744073709551187
                "ExternalConnected" = No
                "IsCharging" = No
                "AvgTimeToEmpty" = 470
                "CycleCount" = 350
              }"#,
        );
        let block = battery_block(Some(&info), None);
        assert_eq!(block["pct"], 71.0);
        assert_eq!(block["cycles"], 350.0);
        assert_eq!(block["health_pct"], 81.7);
        assert_eq!(block["condition"], "good");
        assert_eq!(block["external_connected"], false);
        assert_eq!(block["adapter_w"], Value::Null);
        // 11.86 V × −0.429 A = 5.09 W
        assert_eq!(block["battery_w"], 5.09);
        // 470 min × 60
        assert_eq!(block["time_remaining_s"], 28_200.0);
    }
}
