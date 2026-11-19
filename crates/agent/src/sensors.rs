//! `/api/sensors` — the sensor manifest.
//!
//! The manifest is what the UI renders from: every sensor the platform can
//! actually report, with each metric's id, label, unit, kind, group, interval
//! and privilege tier. It comes from `Sensor::probe()` on the real sensors in
//! `syswatch-sensors-macos` — never from a hardcoded list, so a machine
//! without a battery (Mac Studio) or without SMC access simply has no rows.
//!
//! Probing is not free: `DiskSensor` reads the IO counters once, `NetworkSensor`
//! enumerates interfaces, and `ThermalSensor` opens the AppleSMC. All of that
//! happens **once** per process and the resulting JSON is cached in a
//! `OnceLock`; a per-request probe would turn a 1 s dashboard poll into an
//! IOKit workload.
//!
//! Unlike the Python `/api/sensors` (a *live value* registry grouped by
//! category), this endpoint is the **manifest**: definitions, not samples.
//! Values live in `/api/current`; conflating the two would make this route a
//! second sampler.

use std::process::Command;
use std::sync::OnceLock;

use serde_json::{json, Value};
use syswatch_sensors_macos::all_sensors;

static MANIFEST: OnceLock<Value> = OnceLock::new();

/// The manifest, probed on first call and reused afterwards.
pub fn manifest() -> Value {
    MANIFEST.get_or_init(build).clone()
}

fn build() -> Value {
    let mut sensors = Vec::new();
    let mut metric_count = 0usize;

    for mut sensor in all_sensors() {
        let defs = sensor.probe();
        metric_count += defs.len();
        let id = sensor.id();
        sensors.push(json!({
            "id": id,
            "tier": sensor.tier(),
            "available": !defs.is_empty(),
            "metrics": defs,
        }));
    }

    // Sort by sensor id so the payload is stable across runs (the registry
    // order would otherwise be an implementation detail the UI cannot rely on).
    sensors.sort_by(|a, b| {
        a.get("id")
            .and_then(Value::as_str)
            .cmp(&b.get("id").and_then(Value::as_str))
    });

    json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "device_id": hostname(),
        "device_name": computer_name(),
        "os_version": os_version(),
        "sensor_count": sensors.len(),
        "metric_count": metric_count,
        "generated_ts": crate::timefmt::now_unix(),
        "mode": "live",
        "source": "syswatch-sensors-macos (Sensor::probe on this machine)",
        "sensors": sensors,
    })
}

/// Kernel hostname (`uname -n`), shortened at the first dot exactly like the
/// Python side does when it labels a device.
fn hostname() -> String {
    let mut buf = [0 as libc::c_char; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return "unknown".to_string();
    }
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    let name = String::from_utf8_lossy(&bytes).into_owned();
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name
    }
}

/// The user-facing name macOS shows in Sharing (`scutil --get ComputerName`).
/// Spawned once, inside the same cached probe; `hostname` is the fallback so a
/// missing binary can never leave the field empty.
fn computer_name() -> String {
    if let Ok(out) = Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    hostname()
}

/// `sw_vers -productVersion`, e.g. `15.3`. Empty when unavailable rather than
/// a guessed version.
fn os_version() -> Value {
    match Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
    {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                Value::Null
            } else {
                json!(s)
            }
        }
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_real_metric_defs_and_is_cached() {
        let m = manifest();
        let sensors = m["sensors"].as_array().expect("sensors must be an array");
        assert!(!sensors.is_empty(), "no sensors probed");

        let metric_count = m["metric_count"].as_u64().unwrap();
        assert!(metric_count > 0, "no metrics probed");

        // Every available sensor must carry at least one MetricDef, and every
        // def must be complete — a half-filled row would render blank in the UI.
        let mut seen = 0usize;
        for s in sensors {
            let defs = s["metrics"].as_array().unwrap();
            if s["available"].as_bool().unwrap() {
                assert!(!defs.is_empty(), "available sensor with no defs");
            }
            for d in defs {
                seen += 1;
                for key in ["id", "label", "unit", "kind", "group"] {
                    assert!(d.get(key).is_some(), "MetricDef missing {key}: {d}");
                }
                assert!(
                    d["default_interval_ms"].as_u64().unwrap_or(0) >= 1,
                    "MetricDef without a usable interval: {d}"
                );
            }
        }
        assert_eq!(seen, metric_count as usize);

        // Second call is the cached clone — same payload.
        assert_eq!(manifest(), m);
    }

    #[test]
    fn cpu_total_is_in_the_manifest() {
        let m = manifest();
        let found = m["sensors"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["metrics"].as_array().unwrap())
            .any(|d| d["id"] == "cpu.total");
        assert!(found, "cpu.total missing from the manifest");
    }

    #[test]
    fn identity_fields_are_present() {
        let m = manifest();
        assert_eq!(m["platform"], "macos");
        assert!(!m["device_id"].as_str().unwrap_or("").is_empty());
        assert!(!m["device_name"].as_str().unwrap_or("").is_empty());
        assert!(m["generated_ts"].as_i64().unwrap() > 1_700_000_000);
    }
}
