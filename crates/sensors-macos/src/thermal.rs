//! Thermal sensor — AppleSMC temperatures over IOKit.
//!
//! Four rows, each a mean over the key family that this chip publishes:
//!
//! | row              | keys                                            |
//! |------------------|-------------------------------------------------|
//! | `temp.cpu.die`   | `Tc0*` (Intel die), else `Tp*` (Apple core)      |
//! | `temp.gpu`       | `Tg0*`                                           |
//! | `temp.ssd`       | `TH0*`                                           |
//! | `temp.battery`   | `TB0*`, `TB1T`, `TB2T`                           |
//!
//! On Apple Silicon the AppleSMC user client is root-only, so an unprivileged
//! agent gets `kIOReturnNotPrivileged` from `IOServiceOpen`. That is reported
//! as an error, not as 0 °C — and the same four rows then come from the root
//! daemon's `sample.json` (`cpu_avg_c`, `gpu_avg_c`, `ssd_c`, `battery_c`),
//! which is what the daemon already computes from the same keys.
//!
//! Every key read is throttled to [`THERMAL_REFRESH`]: a named-key sweep is
//! ~10 ms on a connection that stays open, and die temperature does not move
//! meaningfully faster than 2 s.

use std::time::{Duration, Instant};

use syswatch_core::{Group, Kind, MetricDef, SampleBuf, Sensor, SensorError, Tier, Unit};

use crate::rootsample;
use crate::smc;

const THERMAL_REFRESH: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, Default)]
struct Readings {
    cpu_die: Option<f64>,
    gpu: Option<f64>,
    ssd: Option<f64>,
    battery: Option<f64>,
    /// True when at least one row came from the SMC itself, false when the
    /// whole set came from the root daemon's snapshot.
    from_smc: bool,
}

pub struct ThermalSensor {
    smc: Option<smc::Smc>,
    last: Option<Instant>,
    cached: Readings,
}

impl ThermalSensor {
    pub fn new() -> Self {
        Self {
            smc: None,
            last: None,
            cached: Readings::default(),
        }
    }

    fn refresh(&mut self) {
        let mut r = Readings::default();
        if let Some(smc) = &self.smc {
            r.cpu_die = smc
                .temp_avg(smc::CPU_DIE_KEYS)
                .or_else(|| smc.temp_avg(smc::CPU_CORE_KEYS));
            r.gpu = smc.temp_avg(smc::GPU_KEYS);
            r.ssd = smc.temp_avg(smc::SSD_KEYS);
            r.battery = smc.temp_avg(smc::BATTERY_KEYS);
            r.from_smc =
                r.cpu_die.is_some() || r.gpu.is_some() || r.ssd.is_some() || r.battery.is_some();
        }
        if !r.from_smc
            && let Some(sample) = rootsample::read()
        {
            r.cpu_die = sample.cpu_avg_c;
            r.gpu = sample.gpu_avg_c;
            r.ssd = sample.ssd_c;
            r.battery = sample.battery_c;
        }
        self.cached = r;
        self.last = Some(Instant::now());
    }
}

impl Default for ThermalSensor {
    fn default() -> Self {
        Self::new()
    }
}

fn def(id: &str, label: &str) -> MetricDef {
    MetricDef {
        id: id.into(),
        label: label.into(),
        unit: Unit::Celsius,
        kind: Kind::Gauge,
        group: Group::Thermal,
        default_interval_ms: 2000,
        min_interval_ms: 1000,
        tier: Tier::Unprivileged,
    }
}

impl Sensor for ThermalSensor {
    fn id(&self) -> &'static str {
        "thermal"
    }

    fn tier(&self) -> Tier {
        Tier::Unprivileged
    }

    fn probe(&mut self) -> Vec<MetricDef> {
        // One open attempt for the life of the sensor: either the SMC answers
        // (root) or the root daemon's snapshot does. Both failing means no
        // temperature rows at all rather than four permanent blanks.
        if self.smc.is_none() {
            match smc::Smc::open() {
                Ok(s) => self.smc = Some(s),
                Err(e) => eprintln!("thermal: AppleSMC unavailable ({e}); using sample.json"),
            }
        }
        self.refresh();
        if self.smc.is_none() && rootsample::read().is_none() {
            return Vec::new();
        }
        vec![
            def("temp.cpu.die", "CPU die"),
            def("temp.gpu", "GPU"),
            def("temp.ssd", "SSD"),
            def("temp.battery", "Battery"),
        ]
    }

    fn sample(&mut self, out: &mut SampleBuf) -> Result<(), SensorError> {
        if self
            .last
            .map(|t| t.elapsed() >= THERMAL_REFRESH)
            .unwrap_or(true)
        {
            self.refresh();
        }

        let mut filled = 0usize;
        for (id, value) in [
            ("temp.cpu.die", self.cached.cpu_die),
            ("temp.gpu", self.cached.gpu),
            ("temp.ssd", self.cached.ssd),
            ("temp.battery", self.cached.battery),
        ] {
            if let Some(c) = value {
                out.push(id, c);
                filled += 1;
            }
        }
        if filled == 0 {
            return Err(SensorError::Unavailable(
                "no SMC key and no readable sample.json".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_the_four_rows() {
        let mut s = ThermalSensor::new();
        let defs = s.probe();
        assert!(!defs.is_empty(), "no temperature source available");
        for id in ["temp.cpu.die", "temp.gpu", "temp.ssd", "temp.battery"] {
            assert!(defs.iter().any(|d| d.id == id), "missing {id}");
        }
        for d in &defs {
            assert_eq!(d.unit, Unit::Celsius);
        }
    }

    #[test]
    fn sample_fills_temperatures_in_a_plausible_band() {
        let mut s = ThermalSensor::new();
        s.probe();
        let mut buf = SampleBuf::default();
        s.sample(&mut buf).expect("thermal sample");
        assert!(!buf.values.is_empty());
        for (id, c) in &buf.values {
            assert!(
                *c > -20.0 && *c < 120.0,
                "{id} = {c} °C is outside the sane band"
            );
        }
        println!(
            "thermal rows from {}: {:?}",
            if s.cached.from_smc {
                "AppleSMC"
            } else {
                "sample.json"
            },
            buf.values
        );
    }
}
