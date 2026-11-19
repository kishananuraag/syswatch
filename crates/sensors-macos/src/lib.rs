//! macOS sensor implementations.
//!
//! Each sensor implements `syswatch_core::Sensor`. Platform specifics
//! (SMC keys, IORegistry properties, powermetrics snapshots) live only here.
//!
//! | sensor    | source                                            |
//! |-----------|---------------------------------------------------|
//! | `cpu`     | `sysinfo`                                         |
//! | `memory`  | `sysinfo` + `sysctl kern.memorystatus_vm_pressure_level` |
//! | `disk`    | `sysinfo` (space + IOKit byte counters)           |
//! | `network` | `sysinfo` interface counters                      |
//! | `battery` | IORegistry `AppleSmartBattery` (+ daemon `sample.json` for draw on AC) |
//! | `thermal` | AppleSMC over IOKit FFI, falling back to the root daemon's `sample.json` |
//!
//! Every sensor follows the same honesty rule: a value that could not be read
//! is *absent* from the sample, never 0. A sensor with no available source
//! probes to an empty manifest and samples to `SensorError`.

use sysinfo::System;
use syswatch_core::{Group, Kind, MetricDef, SampleBuf, Sensor, SensorError, Tier, Unit};

pub mod battery;
pub mod disk;
pub mod memory;
pub mod network;
pub mod rootsample;
pub mod smc;
pub mod thermal;

pub use battery::BatterySensor;
pub use disk::DiskSensor;
pub use memory::MemorySensor;
pub use network::NetworkSensor;
pub use thermal::ThermalSensor;

/// Metric-id-safe slug for a name that came from the OS: lowercase ASCII
/// alphanumerics, everything else collapsed to a single `_`.
///
/// `/` → `root` (the boot volume), `/System/Volumes/Data` →
/// `system_volumes_data`, `APPLE SSD AP0256Z Media` →
/// `apple_ssd_ap0256z_media`, `en0` → `en0`.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_sep = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_sep = false;
        } else if !last_sep {
            out.push('_');
            last_sep = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "root".to_string()
    } else {
        trimmed.to_string()
    }
}

// ── CPU ─────────────────────────────────────────────────────────────────────

pub struct CpuSensor {
    sys: System,
}

impl CpuSensor {
    pub fn new() -> Self {
        Self { sys: System::new() }
    }
}

impl Default for CpuSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl Sensor for CpuSensor {
    fn id(&self) -> &'static str {
        "cpu"
    }

    fn tier(&self) -> Tier {
        Tier::Unprivileged
    }

    fn probe(&mut self) -> Vec<MetricDef> {
        self.sys.refresh_cpu_all();
        let cores = self.sys.cpus().len();
        let mut defs = vec![
            MetricDef {
                id: "cpu.total".into(),
                label: "CPU".into(),
                unit: Unit::Percent,
                kind: Kind::Gauge,
                group: Group::Compute,
                default_interval_ms: 1000,
                min_interval_ms: 250,
                tier: Tier::Unprivileged,
            },
            MetricDef {
                id: "cpu.load_1m".into(),
                label: "Load 1m".into(),
                unit: Unit::Count,
                kind: Kind::Gauge,
                group: Group::Compute,
                default_interval_ms: 1000,
                min_interval_ms: 1000,
                tier: Tier::Unprivileged,
            },
        ];
        for i in 0..cores {
            defs.push(MetricDef {
                id: format!("cpu.core.{}", i),
                label: format!("Core {}", i),
                unit: Unit::Percent,
                kind: Kind::Gauge,
                group: Group::Compute,
                default_interval_ms: 1000,
                min_interval_ms: 250,
                tier: Tier::Unprivileged,
            });
        }
        defs
    }

    fn sample(&mut self, out: &mut SampleBuf) -> Result<(), SensorError> {
        self.sys.refresh_cpu_all();
        let total = self.sys.global_cpu_usage() as f64;
        out.push("cpu.total", total);
        for (i, cpu) in self.sys.cpus().iter().enumerate() {
            out.push(&format!("cpu.core.{}", i), cpu.cpu_usage() as f64);
        }
        Ok(())
    }
}

// ── registry ────────────────────────────────────────────────────────────────

/// Every macOS sensor, in manifest order. The agent probes each one and
/// publishes whatever defs come back; an empty list means "not on this
/// machine" (e.g. `battery` on a Mac Studio).
pub fn all_sensors() -> Vec<Box<dyn Sensor>> {
    vec![
        Box::new(CpuSensor::new()),
        Box::new(MemorySensor::new()),
        Box::new(DiskSensor::new()),
        Box::new(NetworkSensor::new()),
        Box::new(BatterySensor::new()),
        Box::new(ThermalSensor::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_sensor_probes() {
        let mut sensor = CpuSensor::new();
        let defs = sensor.probe();
        assert!(!defs.is_empty());
        assert!(defs.iter().any(|d| d.id == "cpu.total"));
    }

    #[test]
    fn cpu_sensor_samples() {
        let mut sensor = CpuSensor::new();
        let mut buf = SampleBuf::default();
        sensor.sample(&mut buf).unwrap();
        assert!(!buf.values.is_empty());
        assert!(buf.values.iter().any(|(id, _)| id == "cpu.total"));
    }

    #[test]
    fn slug_is_metric_safe() {
        assert_eq!(slug("/"), "root");
        assert_eq!(slug("/System/Volumes/Data"), "system_volumes_data");
        assert_eq!(slug("APPLE SSD AP0256Z Media"), "apple_ssd_ap0256z_media");
        assert_eq!(slug("en0"), "en0");
        assert_eq!(slug("--"), "root");
    }

    /// The acceptance for T23, in one test: every registered sensor probes to
    /// at least one def and samples to at least one value, without panicking.
    #[test]
    fn every_sensor_probes_and_samples() {
        let mut sensors = all_sensors();
        let mut report = Vec::new();
        for sensor in sensors.iter_mut() {
            let defs = sensor.probe();
            assert!(
                !defs.is_empty(),
                "sensor {} probed to zero MetricDefs",
                sensor.id()
            );
            let mut buf = SampleBuf::default();
            sensor
                .sample(&mut buf)
                .unwrap_or_else(|e| panic!("sensor {} sample failed: {e}", sensor.id()));
            assert!(
                !buf.values.is_empty(),
                "sensor {} sampled zero values",
                sensor.id()
            );
            // Every sampled id must have been announced by probe.
            for (id, _) in &buf.values {
                assert!(
                    defs.iter().any(|d| &d.id == id),
                    "sensor {} sampled undeclared metric {id}",
                    sensor.id()
                );
            }
            report.push(format!(
                "{}: {} defs, {} values",
                sensor.id(),
                defs.len(),
                buf.values.len()
            ));
        }
        println!("T23 sensor sweep:\n  {}", report.join("\n  "));
    }
}
