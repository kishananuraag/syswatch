//! Core types and traits for syswatch.
//!
//! The `Sensor` trait and `MetricDef` are the most important abstractions.
//! Every platform implements `Sensor` in its own crate (`sensors-macos`, etc.).
//! The UI renders only from the manifest — no platform assumptions in core.

use serde::{Deserialize, Serialize};

// ── Units ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    Percent,
    Watt,
    Celsius,
    BytesPerSec,
    Hz,
    Bytes,
    Wh,
    Count,
    Milliseconds,
    Volts,
    Amps,
}

// ── Metric kind ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Gauge,   // point-in-time value
    Counter, // monotonically increasing
    State,   // discrete state (enum-like)
}

// ── Metric group ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Group {
    Compute,
    Memory,
    Thermal,
    Power,
    Storage,
    Network,
    Battery,
    Apps,
}

// ── Tier (privilege) ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    Unprivileged, // Tier 0: no admin prompt needed
    Privileged,   // Tier 1: requires opt-in admin prompt
}

// ── Metric definition ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDef {
    /// Stable, namespaced ID: "cpu.total", "cpu.core.3", "temp.cpu.die"
    pub id: String,
    /// Display label
    pub label: String,
    /// Unit of measurement
    pub unit: Unit,
    /// Gauge, Counter, or State
    pub kind: Kind,
    /// Which group this metric belongs to
    pub group: Group,
    /// Default sampling interval
    pub default_interval_ms: u64,
    /// Minimum allowed interval
    pub min_interval_ms: u64,
    /// Which privilege tier this metric needs
    pub tier: Tier,
}

// ── Sensor trait ─────────────────────────────────────────────────────────────

/// A sensor produces metrics for a platform.
///
/// On start, the agent calls `probe()` on every sensor and publishes the
/// manifest (list of MetricDefs). The UI renders only from the manifest.
/// Platform specifics (SMC keys, powermetrics, P/E cores) live only inside
/// `sensors-*` crates.
pub trait Sensor: Send {
    /// Unique sensor ID (e.g., "cpu", "memory", "thermal.smc")
    fn id(&self) -> &'static str;

    /// Which privilege tier this sensor needs
    fn tier(&self) -> Tier;

    /// What this device can actually report. Called once at startup.
    /// Returns empty if the sensor is unavailable on this device.
    fn probe(&mut self) -> Vec<MetricDef>;

    /// Sample into the provided buffer. Returns Ok(()) on success.
    /// Returns Err if sampling failed (sensor will be retried next cycle).
    fn sample(&mut self, out: &mut SampleBuf) -> Result<(), SensorError>;
}

// ── Sample buffer ────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct SampleBuf {
    pub values: Vec<(String, f64)>, // (metric_id, value)
    pub ts: u64,                    // unix timestamp ms
}

impl SampleBuf {
    pub fn push(&mut self, metric_id: &str, value: f64) {
        self.values.push((metric_id.to_string(), value));
    }
}

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SensorError {
    #[error("sensor unavailable: {0}")]
    Unavailable(String),
    #[error("sample failed: {0}")]
    SampleFailed(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

// ── Manifest ─────────────────────────────────────────────────────────────────

/// The manifest is what the agent publishes at startup.
/// The UI renders only from the manifest — no platform assumptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub device_id: String,
    pub device_name: String,
    pub os: String,
    pub arch: String,
    pub sensors: Vec<SensorManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorManifest {
    pub id: String,
    pub tier: Tier,
    pub metrics: Vec<MetricDef>,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_def_serializes() {
        let m = MetricDef {
            id: "cpu.total".into(),
            label: "CPU".into(),
            unit: Unit::Percent,
            kind: Kind::Gauge,
            group: Group::Compute,
            default_interval_ms: 1000,
            min_interval_ms: 250,
            tier: Tier::Unprivileged,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("cpu.total"));
    }

    #[test]
    fn sample_buf_push() {
        let mut buf = SampleBuf::default();
        buf.push("cpu.total", 42.0);
        assert_eq!(buf.values.len(), 1);
        assert_eq!(buf.values[0].0, "cpu.total");
    }
}
