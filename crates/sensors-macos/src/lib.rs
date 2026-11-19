//! macOS sensor implementations.
//!
//! Each sensor implements `syswatch_core::Sensor`. Platform specifics
//! (SMC keys, powermetrics, P/E cores) live only here.

use syswatch_core::{MetricDef, SampleBuf, Sensor, SensorError, Tier, Unit, Kind, Group};
use sysinfo::System;

pub struct CpuSensor {
    sys: System,
}

impl CpuSensor {
    pub fn new() -> Self {
        Self { sys: System::new() }
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
}
