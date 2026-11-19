//! `/api/thermal` — every SMC temperature the root daemon read, plus the
//! thermal pressure the machine is under.
//!
//! # Why this endpoint is new
//!
//! The Python server has no `/api/thermal`: its temperature data is spread
//! across `/api/power`'s `root` block and `/api/sensors`' manifest. The brief
//! asks for one place that answers "all SMC temps: cpu die, gpu, ssd, battery,
//! plus thermal pressure", so this handler reads the same
//! `root/sample.json` the power endpoint reads and shapes it for that question.
//! Nothing is sampled twice: [`crate::power::root_state`] applies the same
//! staleness rule, and the daemon file is behind a 2 s cache.
//!
//! # What is withheld
//!
//! Temperatures on Apple Silicon are root-only. Without the privileged daemon
//! the answer is `available: false` with the reason `needs root …` and an empty
//! `sensors` array — never a fabricated 0 °C. A daemon that died leaves a file
//! older than 30 s, which is treated as absent for exactly the same reason.
//!
//! # Grouping
//!
//! `sensors` is every row in emission order (the UI's table), and `groups`
//! classifies the same rows by label prefix — `cpu`, `gpu`, `ssd`, `battery`,
//! `other` — so a caller wanting "the SSD temperature" does not have to know
//! that this machine labels it `SSD 4`.

use serde_json::{json, Value};

use crate::histdata::round2;
use crate::power::root_state;

/// The shape `/api/thermal` answers.
pub fn payload() -> Value {
    let root = root_state();

    let Some(sample) = root.sample.as_ref() else {
        return json!({
            "available": false,
            "reason": root.reason,
            "age_s": root.age_s,
            "file": "sample.json",
            "source": source_text(),
            "unit": "C",
            "thermal_pressure": Value::Null,
            "ts": Value::Null,
            "sensors": [],
            "count": 0,
            "groups": {"cpu": [], "gpu": [], "ssd": [], "battery": [], "other": []},
            "cpu_die_c": Value::Null,
            "gpu_c": Value::Null,
            "ssd_c": Value::Null,
            "battery_c": Value::Null,
            "hot_c": Value::Null,
            "hot_sensor": Value::Null,
            "cold_c": Value::Null,
            "cold_sensor": Value::Null,
            "fans_rpm": Value::Null,
        });
    };

    let mut sensors: Vec<Value> = Vec::with_capacity(sample.temps.len());
    let mut groups: Vec<(&str, Vec<Value>)> = vec![
        ("cpu", Vec::new()),
        ("gpu", Vec::new()),
        ("ssd", Vec::new()),
        ("battery", Vec::new()),
        ("other", Vec::new()),
    ];
    let mut hot: Option<(f64, String)> = None;
    let mut cold: Option<(f64, String)> = None;

    for row in &sample.temps {
        let c = round2(row.c);
        let entry = json!({"key": row.key, "label": row.label, "c": c});
        sensors.push(entry.clone());
        let slot = match classify(&row.label) {
            "cpu" => 0,
            "gpu" => 1,
            "ssd" => 2,
            "battery" => 3,
            _ => 4,
        };
        groups[slot].1.push(entry);
        if hot.as_ref().is_none_or(|(h, _)| c > *h) {
            hot = Some((c, row.label.clone()));
        }
        if cold.as_ref().is_none_or(|(l, _)| c < *l) {
            cold = Some((c, row.label.clone()));
        }
    }

    let mut groups_out = serde_json::Map::new();
    let mut counts = serde_json::Map::new();
    for (name, rows) in groups {
        counts.insert(name.to_string(), json!(rows.len()));
        groups_out.insert(name.to_string(), Value::Array(rows));
    }

    json!({
        "available": true,
        "reason": Value::Null,
        "age_s": root.age_s,
        "ts": sample.ts,
        "sample_ms": sample.sample_ms,
        "file": "sample.json",
        "source": source_text(),
        "unit": "C",
        "thermal_pressure": sample.thermal_pressure,
        "sensors": sensors,
        "count": sensors.len(),
        "group_counts": Value::Object(counts),
        "groups": Value::Object(groups_out),
        "cpu_die_c": sample.cpu_avg_c,
        "cpu_temp_c": sample.cpu_temp_c,
        "cpu_cluster_temps_c": if sample.cpu_cluster_temps.is_empty() {
            Value::Null
        } else {
            json!(sample
                .cpu_cluster_temps
                .iter()
                .map(|(label, c)| json!({"label": label, "c": c}))
                .collect::<Vec<_>>())
        },
        "gpu_c": sample.gpu_avg_c,
        "gpu_temp_c": sample.gpu_temp_c,
        "ssd_c": sample.ssd_c,
        "battery_c": sample.battery_c,
        "hot_c": hot.as_ref().map(|(c, _)| *c),
        "hot_sensor": hot.map(|(_, l)| l),
        "cold_c": cold.as_ref().map(|(c, _)| *c),
        "cold_sensor": cold.map(|(_, l)| l),
        "fans_rpm": if sample.fans_rpm.is_empty() { Value::Null } else { json!(sample.fans_rpm) },
        "smc": sample.smc,
    })
}

fn source_text() -> &'static str {
    "powermetrics + AppleSMC through the root LaunchDaemon \
     com.barbarian.syswatch.power; this agent only reads the file it writes"
}

/// Label prefix → group. The daemon labels rows `CPU performance core 1 (Tp01)`,
/// `GPU 1`, `SSD 4`, `Battery 1`, `Palm rest` (verified against the live file).
pub fn classify(label: &str) -> &'static str {
    let low = label.to_ascii_lowercase();
    if low.starts_with("cpu") || low.contains("p-core") || low.contains("e-core") {
        "cpu"
    } else if low.starts_with("gpu") {
        "gpu"
    } else if low.starts_with("ssd") || low.contains("nvme") {
        "ssd"
    } else if low.starts_with("battery") || low.contains("pack") {
        "battery"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_follow_the_daemon_labels() {
        assert_eq!(classify("CPU performance core 1 (Tp01)"), "cpu");
        assert_eq!(classify("GPU 1"), "gpu");
        assert_eq!(classify("SSD 4"), "ssd");
        assert_eq!(classify("Battery 1"), "battery");
        assert_eq!(classify("Palm rest"), "other");
        assert_eq!(classify("CPU efficiency core 1"), "cpu");
    }

    #[test]
    fn without_the_daemon_the_answer_withholds_every_temperature() {
        // No privileged file on a CI box: the shape stays, every number is null.
        if crate::power::root_state().available {
            return; // granted machine: this case cannot be asserted here
        }
        let out = payload();
        assert_eq!(out["available"], false);
        assert_eq!(out["sensors"].as_array().unwrap().len(), 0);
        assert_eq!(out["thermal_pressure"], Value::Null);
        assert_eq!(out["groups"]["cpu"].as_array().unwrap().len(), 0);
        assert!(out["reason"].as_str().unwrap().contains("needs root"));
    }
}
