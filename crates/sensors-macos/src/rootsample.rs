//! Reader for the root daemon's `sample.json`.
//!
//! `syswatch-powerd` (T19) runs as root, spawns `powermetrics` and reads the
//! AppleSMC, then writes one atomically-renamed snapshot here:
//!
//!   `$HOME/Library/Application Support/syswatch/root/sample.json`
//!
//! It is mode 0644, so an unprivileged sensor can read it. This module only
//! ever *reads* it: the daemon owns the file.
//!
//! Missing keys stay missing. A snapshot without `battery_c` yields `None`,
//! never 0 °C — the same rule the daemon itself follows.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Sample files older than this are still returned, but the caller can see
/// `age_secs` and decide. (The daemon writes every 15 s by default.)
const CACHE_TTL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default)]
pub struct TempRow {
    pub key: String,
    pub label: String,
    pub c: f64,
}

#[derive(Debug, Clone, Default)]
pub struct RootSample {
    /// Whole-package power (CPU+GPU+ANE) in W, as powermetrics reports it.
    pub package_w: Option<f64>,
    pub cpu_w: Option<f64>,
    pub gpu_w: Option<f64>,
    pub thermal_pressure: Option<String>,
    /// Die / pack temperatures aggregated by the daemon.
    pub cpu_avg_c: Option<f64>,
    pub gpu_avg_c: Option<f64>,
    pub ssd_c: Option<f64>,
    pub battery_c: Option<f64>,
    /// Every SMC temperature row the daemon read this cycle.
    pub temps: Vec<TempRow>,
    /// `ts` as written by the daemon (unix seconds).
    pub ts: Option<u64>,
}

impl RootSample {
    /// Hottest sane row whose label starts with `prefix`, e.g. "CPU".
    pub fn temp_by_label(&self, prefix: &str) -> Option<f64> {
        self.temps
            .iter()
            .filter(|r| r.label.starts_with(prefix))
            .map(|r| r.c)
            .reduce(f64::max)
    }
}

/// Where the daemon writes. `$SYSWATCH_ROOT_DIR` overrides the default so a
/// test or a replay can point at a fixture without touching the live file.
pub fn sample_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SYSWATCH_ROOT_DIR") {
        return Some(PathBuf::from(dir).join("sample.json"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/syswatch/root/sample.json"))
}

struct Cached {
    at: Instant,
    mtime: Option<SystemTime>,
    sample: RootSample,
}

fn cell() -> &'static Mutex<Option<Cached>> {
    static CELL: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// The current snapshot, or `None` when the daemon has not written one (not
/// installed, or not root). Cached for 2 s / until the file's mtime moves, so
/// a 1 s tick costs one `stat` and no parse.
pub fn read() -> Option<RootSample> {
    let path = sample_path()?;
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

    if let Ok(guard) = cell().lock()
        && let Some(c) = guard.as_ref()
        && c.at.elapsed() < CACHE_TTL
        && c.mtime == mtime
    {
        return Some(c.sample.clone());
    }

    let text = std::fs::read_to_string(&path).ok()?;
    let sample = parse(&text)?;
    if let Ok(mut guard) = cell().lock() {
        *guard = Some(Cached {
            at: Instant::now(),
            mtime,
            sample: sample.clone(),
        });
    }
    Some(sample)
}

/// Parse one snapshot. Public so the tests can feed a fixture string without
/// depending on the daemon having run.
pub fn parse(text: &str) -> Option<RootSample> {
    let v: Value = serde_json::from_str(text).ok()?;
    let obj = v.as_object()?;
    let num = |k: &str| obj.get(k).and_then(Value::as_f64);

    let mut temps = Vec::new();
    if let Some(arr) = obj.get("temps").and_then(Value::as_array) {
        for row in arr {
            let (Some(key), Some(label), Some(c)) = (
                row.get("key").and_then(Value::as_str),
                row.get("label").and_then(Value::as_str),
                row.get("c").and_then(Value::as_f64),
            ) else {
                continue;
            };
            temps.push(TempRow {
                key: key.to_string(),
                label: label.to_string(),
                c,
            });
        }
    }

    Some(RootSample {
        package_w: num("package_w"),
        cpu_w: num("cpu_w"),
        gpu_w: num("gpu_w"),
        thermal_pressure: obj
            .get("thermal_pressure")
            .and_then(Value::as_str)
            .map(str::to_string),
        cpu_avg_c: num("cpu_avg_c"),
        gpu_avg_c: num("gpu_avg_c"),
        ssd_c: num("ssd_c"),
        battery_c: num("battery_c"),
        temps,
        ts: obj.get("ts").and_then(Value::as_u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_SHAPE: &str = r#"{"ts":1795113019,"sample_ms":15000,
        "samplers":"thermal,gpu_power,cpu_power,ane_power",
        "cpu_w":0.716,"gpu_w":0.186,"ane_w":0,"package_w":0.903,
        "thermal_pressure":"Nominal",
        "temps":[{"key":"TB0T","label":"Battery 1","c":29.8,"type":"flt "},
                 {"key":"Tp01","label":"CPU performance core 1 (Tp01)","c":42.04,"type":"flt "}],
        "cpu_avg_c":41.56,"gpu_avg_c":40.74,"battery_c":29.17,"ssd_c":32.77}"#;

    #[test]
    fn parses_the_daemon_shape() {
        let s = parse(LIVE_SHAPE).expect("live shape must parse");
        assert_eq!(s.ts, Some(1795113019));
        assert_eq!(s.package_w, Some(0.903));
        assert_eq!(s.thermal_pressure.as_deref(), Some("Nominal"));
        assert_eq!(s.temps.len(), 2);
        assert_eq!(s.temp_by_label("CPU"), Some(42.04));
        assert_eq!(s.battery_c, Some(29.17));
    }

    #[test]
    fn absent_keys_stay_absent() {
        let s = parse(r#"{"ts":1,"package_w":null}"#).unwrap();
        assert_eq!(s.package_w, None);
        assert_eq!(s.battery_c, None);
        assert!(s.temps.is_empty());
    }
}
