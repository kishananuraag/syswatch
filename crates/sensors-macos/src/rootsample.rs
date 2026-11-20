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

/// One cluster's clock and residency (`e_cluster`, `p_cluster`, `per_cpu[]`).
///
/// The daemon emits `{freq_mhz, residency_pct}`; either half may be missing and
/// then stays `None` — a missing clock is not a 0 MHz reading.
#[derive(Debug, Clone, Default)]
pub struct ClusterStat {
    pub freq_mhz: Option<f64>,
    pub residency_pct: Option<f64>,
}

impl ClusterStat {
    pub fn to_json(&self) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        if let Some(v) = self.freq_mhz {
            out.insert("freq_mhz".into(), serde_json::json!(v));
        }
        if let Some(v) = self.residency_pct {
            out.insert("residency_pct".into(), serde_json::json!(v));
        }
        serde_json::Value::Object(out)
    }

    pub fn is_empty(&self) -> bool {
        self.freq_mhz.is_none() && self.residency_pct.is_none()
    }
}

#[derive(Debug, Clone, Default)]
pub struct RootSample {
    /// Whole-package power (CPU+GPU+ANE) in W, as powermetrics reports it.
    pub package_w: Option<f64>,
    pub cpu_w: Option<f64>,
    pub gpu_w: Option<f64>,
    /// Neural Engine power — 0 W on a package that is not using it, never None
    /// when powermetrics reported one.
    pub ane_w: Option<f64>,
    pub ane_util_pct: Option<f64>,
    pub thermal_pressure: Option<String>,
    /// Die / pack temperatures aggregated by the daemon.
    pub cpu_avg_c: Option<f64>,
    pub gpu_avg_c: Option<f64>,
    pub ssd_c: Option<f64>,
    pub battery_c: Option<f64>,
    /// powermetrics' own die readings, present on daemon versions that emit
    /// them; `None` means "this daemon did not write it", not "0 °C".
    pub cpu_temp_c: Option<f64>,
    pub gpu_temp_c: Option<f64>,
    /// Cluster clocks / residencies (E and P cluster, then per logical CPU).
    pub e_cluster: Option<ClusterStat>,
    pub p_cluster: Option<ClusterStat>,
    pub per_cpu: Vec<ClusterStat>,
    pub gpu_freq_mhz: Option<f64>,
    pub gpu_residency_pct: Option<f64>,
    /// Fan RPMs when the machine has fans (a fanless Air reports none).
    pub fans_rpm: Vec<f64>,
    /// The daemon's own SMC reader statistics (`ok`, `mode`, `keys`, …).
    pub smc: Option<serde_json::Value>,
    /// Per-cluster temperatures, normalised to `[{label, c}]`.
    pub cpu_cluster_temps: Vec<(String, f64)>,
    /// Every SMC temperature row the daemon read this cycle.
    pub temps: Vec<TempRow>,
    /// `ts` as written by the daemon (unix seconds).
    pub ts: Option<u64>,
    /// The parse interval the daemon was started with, in ms.
    pub sample_ms: Option<u64>,
    /// Where the numbers came from, as the daemon wrote it.
    pub source: Option<String>,
    /// The file as written — lets a new endpoint read a field this struct does
    /// not model yet without a second reader.
    pub raw: serde_json::Value,
    /// Age of the file at read time, in seconds. `None` only for a sample the
    /// tests constructed by hand.
    pub age_s: Option<f64>,
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
    let meta = std::fs::metadata(&path).ok();
    let mtime = meta.as_ref().and_then(|m| m.modified().ok());

    if let Ok(guard) = cell().lock()
        && let Some(c) = guard.as_ref()
        && c.at.elapsed() < CACHE_TTL
        && c.mtime == mtime
    {
        return Some(c.sample.clone());
    }

    let text = std::fs::read_to_string(&path).ok()?;
    let mut sample = parse(&text)?;
    sample.age_s = mtime
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .map(|d| d.as_secs_f64());
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

    let cluster = |k: &str| obj.get(k).and_then(parse_cluster);
    let mut per_cpu = Vec::new();
    if let Some(rows) = obj.get("per_cpu").and_then(Value::as_array) {
        for row in rows {
            if let Some(c) = parse_cluster(row) {
                per_cpu.push(c);
            }
        }
    }

    let mut cpu_cluster_temps = Vec::new();
    if let Some(rows) = obj.get("cpu_cluster_temps_c").and_then(Value::as_array) {
        for (i, row) in rows.iter().enumerate() {
            let label = row
                .get("label")
                .or_else(|| row.get("cluster"))
                .or_else(|| row.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("cluster {}", i + 1));
            let c = row
                .get("c")
                .or_else(|| row.get("temp_c"))
                .and_then(Value::as_f64);
            if let Some(c) = c {
                cpu_cluster_temps.push((label, round2(c)));
            }
        }
    }

    let fans_rpm: Vec<f64> = obj
        .get("fans_rpm")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();

    Some(RootSample {
        package_w: num("package_w"),
        cpu_w: num("cpu_w"),
        gpu_w: num("gpu_w"),
        ane_w: num("ane_w"),
        ane_util_pct: num("ane_util_pct"),
        thermal_pressure: obj
            .get("thermal_pressure")
            .and_then(Value::as_str)
            .map(str::to_string),
        cpu_avg_c: num("cpu_avg_c"),
        gpu_avg_c: num("gpu_avg_c"),
        ssd_c: num("ssd_c"),
        battery_c: num("battery_c"),
        cpu_temp_c: num("cpu_temp_c"),
        gpu_temp_c: num("gpu_temp_c"),
        e_cluster: cluster("e_cluster"),
        p_cluster: cluster("p_cluster"),
        per_cpu,
        gpu_freq_mhz: num("gpu_freq_mhz"),
        gpu_residency_pct: num("gpu_residency_pct"),
        fans_rpm,
        smc: obj.get("smc").cloned(),
        cpu_cluster_temps,
        temps,
        ts: obj.get("ts").and_then(Value::as_u64),
        sample_ms: obj.get("sample_ms").and_then(Value::as_u64),
        source: obj.get("source").and_then(Value::as_str).map(str::to_string),
        raw: v,
        age_s: None,
    })
}

fn parse_cluster(v: &Value) -> Option<ClusterStat> {
    let obj = v.as_object()?;
    let c = ClusterStat {
        freq_mhz: obj.get("freq_mhz").and_then(Value::as_f64),
        residency_pct: obj.get("residency_pct").and_then(Value::as_f64),
    };
    (!c.is_empty()).then_some(c)
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
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

    #[test]
    fn parses_clusters_residency_and_smc_block() {
        // The shape the live daemon wrote on 2026-11-20 (trimmed).
        let text = r#"{"ts":1795118510,"sample_ms":15000,
            "source":"powermetrics (root LaunchDaemon com.barbarian.syswatch.power)",
            "cpu_w":0.437,"gpu_w":0.112,"ane_w":0,"package_w":0.549,
            "thermal_pressure":"Nominal",
            "e_cluster":{"freq_mhz":1205,"residency_pct":67.34},
            "p_cluster":{"freq_mhz":918,"residency_pct":54.4},
            "gpu_freq_mhz":493,"gpu_residency_pct":14,
            "per_cpu":[{"freq_mhz":1329,"residency_pct":26.67},
                       {"freq_mhz":1816,"residency_pct":12.99}],
            "temps":[{"key":"Tp01","label":"CPU performance core 1 (Tp01)","c":43.04}],
            "cpu_avg_c":42.75,"gpu_avg_c":41.79,"battery_c":31.9,"ssd_c":35.52,
            "smc":{"ok":true,"mode":"fast","keys":170}}"#;
        let s = parse(text).expect("live shape");
        assert_eq!(s.ane_w, Some(0.0));
        assert_eq!(s.sample_ms, Some(15_000));
        assert!(s.source.as_deref().unwrap().starts_with("powermetrics"));
        let e = s.e_cluster.as_ref().unwrap();
        assert_eq!(e.freq_mhz, Some(1205.0));
        assert_eq!(e.residency_pct, Some(67.34));
        assert_eq!(s.p_cluster.as_ref().unwrap().residency_pct, Some(54.4));
        assert_eq!(s.per_cpu.len(), 2);
        assert_eq!(s.gpu_freq_mhz, Some(493.0));
        assert_eq!(s.gpu_residency_pct, Some(14.0));
        assert_eq!(s.smc.as_ref().unwrap()["mode"], serde_json::json!("fast"));
        // The whole file travels too, for fields the struct does not model.
        assert_eq!(s.raw["samplers"], serde_json::Value::Null);
        // No fans on this machine → empty, never [0].
        assert!(s.fans_rpm.is_empty());
    }
}
