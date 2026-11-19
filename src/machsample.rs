//! Measured macOS inputs for the menu-bar app (T16B).
//!
//! Two sources, both read-only and both already produced elsewhere on this
//! machine — this module never samples anything expensive:
//!
//! 1. **The root daemon's snapshot** (`~/Library/Application Support/
//!    syswatch/root/sample.json`), written by `syswatch_power_sampler.sh`
//!    every 5 s: package/CPU/GPU/ANE watts, thermal pressure, SMC
//!    temperatures and powermetrics' per-cluster clocks.  A file older than
//!    [`ROOT_SAMPLE_TTL_S`] is treated as **absent** — a dead daemon must never
//!    leave a stale number on screen pretending to be live (same rule the
//!    dashboard applies).
//!
//! 2. **`ioreg` battery state** (unprivileged, always available on a laptop):
//!    pack voltage × amperage is the number a human means by "watts" — the
//!    whole machine's draw off the battery rail, not the sum of what we could
//!    attribute to processes.  It is cached for [`BATTERY_TTL`] seconds because
//!    every uncached read spawns a process (~20 ms), and the menu bar ticks
//!    every 2 s.
//!
//! Everything optional stays `Option`: a reading we do not have is absent,
//! never zero.

#![cfg(target_os = "macos")]

use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// A root snapshot older than this is withheld (matches dashboard/server.py
/// `ROOT_SAMPLE_STALE_S`).
pub const ROOT_SAMPLE_TTL_S: f64 = 30.0;

/// ioreg battery cache.  One spawn measures ~25 ms on this Mac, so 20 s costs
/// ~0.12 % of a core; at the 2 s tick it would be ~1 % on its own.  The title's
/// wattage is therefore at most 20 s old — V × A moves slowly enough that this
/// is invisible, and the menu bar's 1 % budget is real.
const BATTERY_TTL: Duration = Duration::from_secs(20);

// ─────────────────────────── root sample.json ───────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Cluster {
    #[serde(default)]
    pub freq_mhz: Option<f64>,
    #[serde(default)]
    pub residency_pct: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TempRow {
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    pub c: f64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RootSample {
    #[serde(default)]
    pub ts: u64,
    #[serde(default)]
    pub package_w: Option<f64>,
    #[serde(default)]
    pub cpu_w: Option<f64>,
    #[serde(default)]
    pub gpu_w: Option<f64>,
    #[serde(default)]
    pub ane_w: Option<f64>,
    #[serde(default)]
    pub thermal_pressure: Option<String>,
    #[serde(default)]
    pub temps: Vec<TempRow>,
    #[serde(default)]
    pub cpu_avg_c: Option<f64>,
    #[serde(default)]
    pub gpu_avg_c: Option<f64>,
    #[serde(default)]
    pub battery_c: Option<f64>,
    #[serde(default)]
    pub ssd_c: Option<f64>,
    #[serde(default)]
    pub e_cluster: Option<Cluster>,
    #[serde(default)]
    pub p_cluster: Option<Cluster>,
    #[serde(default)]
    pub gpu_freq_mhz: Option<f64>,
    #[serde(default)]
    pub gpu_residency_pct: Option<f64>,
    #[serde(default)]
    pub per_cpu: Vec<Cluster>,
    /// Seconds since the file was written.  Set by [`read_root_sample`].
    #[serde(skip)]
    pub age_s: f64,
}

impl RootSample {
    pub fn is_fresh(&self) -> bool {
        self.age_s <= ROOT_SAMPLE_TTL_S
    }

    fn family_mean(&self, prefix: &str) -> Option<f64> {
        let vals: Vec<f64> = self
            .temps
            .iter()
            .filter(|t| {
                t.label
                    .as_deref()
                    .map(|l| l.starts_with(prefix))
                    .unwrap_or(false)
            })
            .map(|t| t.c)
            .collect();
        if vals.is_empty() {
            None
        } else {
            Some(vals.iter().sum::<f64>() / vals.len() as f64)
        }
    }

    /// Mean of the CPU temperature sensors.  Computed from the per-sensor rows
    /// so the number can always be traced back to the readings it came from;
    /// falls back to the daemon's own aggregate.
    pub fn cpu_temp_c(&self) -> Option<f64> {
        self.family_mean("CPU").or(self.cpu_avg_c)
    }

    pub fn gpu_temp_c(&self) -> Option<f64> {
        self.family_mean("GPU").or(self.gpu_avg_c)
    }
}

pub fn root_sample_path() -> PathBuf {
    // Same override the dashboard honours (`SYSWATCH_ROOT_SAMPLE`), so a
    // recorded sample can be rendered without touching the live one.
    if let Ok(p) = std::env::var("SYSWATCH_ROOT_SAMPLE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let mut p = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    p.push("Library/Application Support/syswatch/root/sample.json");
    p
}

/// Read the root daemon's snapshot.  `None` when the file is missing or
/// unparseable; a stale file is returned with `age_s` set so the caller can
/// decide (the menu bar always checks [`RootSample::is_fresh`]).
pub fn read_root_sample() -> Option<RootSample> {
    let path = root_sample_path();
    let age_s = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(f64::MAX);
    let text = std::fs::read_to_string(&path).ok()?;
    let mut sample: RootSample = serde_json::from_str(&text).ok()?;
    sample.age_s = age_s;
    Some(sample)
}

// ───────────────────────────── battery (ioreg) ──────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Battery {
    /// State of charge, percent.
    pub pct: Option<f64>,
    /// |V × A| in W: what the pack is sourcing (discharge) or sinking
    /// (charge).  This is the number the title shows.
    pub watts: Option<f64>,
    pub charging: Option<bool>,
    pub external_connected: Option<bool>,
    /// Seconds until empty (discharging) or full (charging).
    pub time_left_s: Option<i64>,
    pub cycles: Option<i64>,
    /// `NominalChargeCapacity / DesignCapacity` in percent.
    pub health_pct: Option<f64>,
    /// Pack temperature from ioreg `Temperature` (centi-°C → °C).
    pub temp_c: Option<f64>,
    pub voltage_v: Option<f64>,
    pub amperage_ma: Option<i64>,
    /// Whole-system load as the battery gauge measures it
    /// (`PowerTelemetryData.SystemLoad`, mW).  Available on AC as well, which
    /// is exactly when V × A reads 0 and says nothing about the machine.
    pub system_load_w: Option<f64>,
    /// Adapter input as the gauge measures it (`PowerTelemetryData.
    /// SystemPowerIn`, mW) — wall-side, includes the adapter's own loss.
    pub system_in_w: Option<f64>,
}

static BATTERY_CACHE: OnceLock<Mutex<Option<(Instant, Battery)>>> = OnceLock::new();

/// Battery state, cached for [`BATTERY_TTL`].  `None` on a machine with no
/// battery (desktop), never a zero-filled struct pretending to be a reading.
pub fn read_battery() -> Option<Battery> {
    let cell = BATTERY_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cell.lock() {
        if let Some((at, b)) = guard.as_ref() {
            if at.elapsed() < BATTERY_TTL {
                return Some(b.clone());
            }
        }
    }
    let fresh = read_battery_uncached()?;
    if let Ok(mut guard) = cell.lock() {
        *guard = Some((Instant::now(), fresh.clone()));
    }
    Some(fresh)
}

fn read_battery_uncached() -> Option<Battery> {
    // -w0 = never wrap the property list onto continuation lines.
    let out = Command::new("/usr/sbin/ioreg")
        .args(["-r", "-c", "AppleSmartBattery", "-w0"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !text.contains("AppleSmartBattery") {
        return None;
    }

    // Top-level properties are printed as:  "Key" = value
    // (nested dicts use  "Key"=value  without spaces, so the spaced form is
    // unambiguous for the keys we read here).
    let num = |key: &str| -> Option<f64> {
        let needle = format!("\"{}\" = ", key);
        let idx = text.find(&needle)?;
        let rest = &text[idx + needle.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
            .unwrap_or(rest.len());
        rest[..end].parse::<f64>().ok()
    };
    // Nested dictionaries print as `"Key"=value` (no spaces around `=`), which
    // is how the gauge's own telemetry block is laid out.
    let num_nested = |key: &str| -> Option<f64> {
        let needle = format!("\"{}\"=", key);
        let idx = text.find(&needle)?;
        let rest = &text[idx + needle.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
            .unwrap_or(rest.len());
        rest[..end].parse::<f64>().ok()
    };
    let flag = |key: &str| -> Option<bool> {
        let needle = format!("\"{}\" = ", key);
        let idx = text.find(&needle)?;
        let rest = text[idx + needle.len()..].trim_start();
        if rest.starts_with("Yes") {
            Some(true)
        } else if rest.starts_with("No") {
            Some(false)
        } else {
            None
        }
    };

    let voltage_mv = num("AppleRawBatteryVoltage").or_else(|| num("Voltage"));
    let amperage_ma = num("Amperage").or_else(|| num("InstantAmperage"));

    let mut b = Battery {
        voltage_v: voltage_mv.map(|mv| (mv / 1000.0 * 1000.0).round() / 1000.0),
        amperage_ma: amperage_ma.map(|ma| ma as i64),
        charging: flag("IsCharging"),
        external_connected: flag("ExternalConnected"),
        cycles: num("CycleCount").map(|v| v as i64),
        ..Default::default()
    };

    if let (Some(mv), Some(ma)) = (voltage_mv, amperage_ma) {
        // mV × mA / 1e6 = W, reported as a positive magnitude: the sign only
        // says which way the charge is flowing.
        b.watts = Some((mv * ma / 1e6).abs());
    }

    let max_cap = num("MaxCapacity");
    if let Some(cur) = num("CurrentCapacity") {
        if let Some(max) = max_cap {
            if max > 0.0 {
                b.pct = Some((cur / max * 100.0 * 10.0).round() / 10.0);
            }
        }
    }

    let charging = b.charging.unwrap_or(false);
    let minutes = if charging {
        num("AvgTimeToFull").filter(|m| *m > 0.0 && *m < 65535.0)
    } else {
        num("AvgTimeToEmpty").filter(|m| *m > 0.0 && *m < 65535.0)
    };
    b.time_left_s = minutes.map(|m| (m * 60.0) as i64);

    if let (Some(nominal), Some(design)) = (num("NominalChargeCapacity"), num("DesignCapacity")) {
        if design > 0.0 {
            b.health_pct = Some((nominal / design * 1000.0).round() / 10.0);
        }
    }

    // Gauge telemetry: mW.  Only ever used when the key is present and sane.
    if let Some(mw) = num_nested("SystemLoad") {
        if mw > 0.0 && mw < 200_000.0 {
            b.system_load_w = Some((mw / 1000.0 * 100.0).round() / 100.0);
        }
    }
    if let Some(mw) = num_nested("SystemPowerIn") {
        if mw > 0.0 && mw < 200_000.0 {
            b.system_in_w = Some((mw / 1000.0 * 100.0).round() / 100.0);
        }
    }

    if let Some(centi) = num("Temperature") {
        if centi > 0.0 {
            b.temp_c = Some((centi / 100.0 * 100.0).round() / 100.0);
        }
    }

    Some(b)
}

/// `4:26` for 4 h 26 min, `41 min` under an hour; `None` when unknown.
pub fn format_minutes(seconds: i64) -> Option<String> {
    if seconds <= 0 {
        return None;
    }
    let total_min = seconds / 60;
    if total_min < 60 {
        Some(format!("{} min", total_min))
    } else {
        Some(format!("{}:{:02}", total_min / 60, total_min % 60))
    }
}
