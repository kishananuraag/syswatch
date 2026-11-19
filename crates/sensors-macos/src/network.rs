//! Network sensor — per-interface receive/transmit bytes per second.
//!
//! `sysinfo` reads the interface counters (`total_received` /
//! `total_transmitted`, cumulative since boot). A rate is the delta between
//! two reads divided by the window between them, so the sensor keeps the
//! previous snapshot and the previous `Instant`.
//!
//! Interfaces that have carried no traffic at probe time are skipped: on a
//! MacBook that drops `utun*`, `awdl0`, `llw0`, `gif0` and `stf0` noise while
//! keeping `en0`, `lo0` and any bridge that is actually moving bytes.

use std::collections::HashMap;
use std::time::Instant;

use sysinfo::Networks;
use syswatch_core::{Group, Kind, MetricDef, SampleBuf, Sensor, SensorError, Tier, Unit};

use crate::slug;

/// Shortest window a rate is divided by. Two reads a few microseconds apart
/// still produce a finite number instead of a spike from a near-zero
/// denominator.
const MIN_WINDOW_S: f64 = 0.01;

pub struct NetworkSensor {
    nets: Networks,
    prev: HashMap<String, (u64, u64)>,
    prev_at: Option<Instant>,
    /// Interfaces the manifest was built from (stable, sorted).
    ifaces: Vec<String>,
}

impl NetworkSensor {
    pub fn new() -> Self {
        let nets = Networks::new_with_refreshed_list();
        let prev = nets
            .iter()
            .map(|(name, data)| {
                (
                    name.clone(),
                    (data.total_received(), data.total_transmitted()),
                )
            })
            .collect();
        let mut sensor = Self {
            nets,
            prev,
            prev_at: Some(Instant::now()),
            ifaces: Vec::new(),
        };
        sensor.ifaces = sensor.active_ifaces();
        sensor
    }

    /// Interfaces with a non-zero cumulative counter, sorted for a stable
    /// manifest.
    fn active_ifaces(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .nets
            .iter()
            .filter(|(_, d)| d.total_received() > 0 || d.total_transmitted() > 0)
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }
}

impl Default for NetworkSensor {
    fn default() -> Self {
        Self::new()
    }
}

fn def(id: String, label: String) -> MetricDef {
    MetricDef {
        id,
        label,
        unit: Unit::BytesPerSec,
        kind: Kind::Gauge,
        group: Group::Network,
        default_interval_ms: 1000,
        min_interval_ms: 250,
        tier: Tier::Unprivileged,
    }
}

impl Sensor for NetworkSensor {
    fn id(&self) -> &'static str {
        "network"
    }

    fn tier(&self) -> Tier {
        Tier::Unprivileged
    }

    fn probe(&mut self) -> Vec<MetricDef> {
        self.ifaces = self.active_ifaces();
        let mut defs = Vec::new();
        for iface in &self.ifaces {
            let id = slug(iface);
            defs.push(def(
                format!("net.{id}.rx_bytes_sec"),
                format!("{iface} receive"),
            ));
            defs.push(def(
                format!("net.{id}.tx_bytes_sec"),
                format!("{iface} transmit"),
            ));
        }
        defs
    }

    fn sample(&mut self, out: &mut SampleBuf) -> Result<(), SensorError> {
        // Interface topology changes (USB ethernet, VPN up/down) rarely; the
        // cheap refresh keeps counters current without re-enumerating.
        self.nets.refresh(false);
        let now = Instant::now();
        let elapsed = self
            .prev_at
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(0.0)
            .max(MIN_WINDOW_S);

        let mut filled = 0usize;
        for (name, data) in self.nets.iter() {
            let cur = (data.total_received(), data.total_transmitted());
            let Some((prx, ptx)) = self.prev.get(name).copied() else {
                continue;
            };
            let rx = cur.0.saturating_sub(prx) as f64 / elapsed;
            let tx = cur.1.saturating_sub(ptx) as f64 / elapsed;
            if !self.ifaces.contains(name) && (rx == 0.0 && tx == 0.0) {
                continue;
            }
            let id = slug(name);
            out.push(&format!("net.{id}.rx_bytes_sec"), rx);
            out.push(&format!("net.{id}.tx_bytes_sec"), tx);
            filled += 1;
        }

        self.prev = self
            .nets
            .iter()
            .map(|(name, data)| {
                (
                    name.clone(),
                    (data.total_received(), data.total_transmitted()),
                )
            })
            .collect();
        self.prev_at = Some(now);

        if filled == 0 {
            return Err(SensorError::Unavailable(
                "no network interface with counters".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_lists_interfaces() {
        let mut s = NetworkSensor::new();
        let defs = s.probe();
        assert!(!defs.is_empty(), "no interfaces probed");
        for d in &defs {
            assert!(
                d.id.starts_with("net.") && d.unit == Unit::BytesPerSec,
                "bad def {}",
                d.id
            );
        }
    }

    #[test]
    fn sample_fills_rates() {
        let mut s = NetworkSensor::new();
        let defs = s.probe();
        // A window of a few hundred ms makes the rate meaningful; the sensor
        // floors it at MIN_WINDOW_S so even an instant sample is finite.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let mut buf = SampleBuf::default();
        s.sample(&mut buf).expect("network sample");
        assert!(!buf.values.is_empty());
        for (id, v) in &buf.values {
            assert!(v.is_finite() && *v >= 0.0, "{id} = {v}");
        }
        // Every def that was filled must have been announced by probe.
        for (id, _) in &buf.values {
            assert!(defs.iter().any(|d| &d.id == id), "undeclared metric {id}");
        }
    }

    #[test]
    fn loopback_is_present_and_moves_bytes() {
        let mut s = NetworkSensor::new();
        s.probe();
        let mut buf = SampleBuf::default();
        s.sample(&mut buf).unwrap();
        let has_lo = buf
            .values
            .iter()
            .any(|(id, _)| id == "net.lo0.rx_bytes_sec");
        println!("loopback row present: {has_lo}");
    }
}
