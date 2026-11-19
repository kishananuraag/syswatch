//! Battery sensor — charge, health, cycles, draw, time remaining.
//!
//! Two sources, in order of authority:
//!
//! 1. **IORegistry** (`AppleSmartBattery`, read through `ioreg -r -c
//!    AppleSmartBattery -w0`): charge, design/nominal capacity, cycles,
//!    voltage, amperage, `AvgTimeToEmpty` / `AvgTimeToFull`. Unprivileged.
//! 2. **`sample.json`** from the root daemon (T19): `package_w`, used only for
//!    draw when the gauge's V × A reads zero — which is exactly what happens
//!    on AC power, where the battery is not moving any current but the machine
//!    is still drawing watts.
//!
//! The `ioreg` spawn is cached for [`BATTERY_TTL`] (20 s, the same value
//! `machsample.rs` uses) because the gauge itself updates every ~30 s.
//!
//! Missing numbers stay missing: a desktop (no battery) probes to an empty
//! manifest and samples to `Err(Unavailable)`, never to zeros.

use std::process::Command;
use std::time::{Duration, Instant};

use syswatch_core::{Group, Kind, MetricDef, SampleBuf, Sensor, SensorError, Tier, Unit};

use crate::rootsample;

/// The gauge refreshes about every 30 s; reading the registry faster only
/// re-reads the same numbers.
const BATTERY_TTL: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Default)]
pub struct Battery {
    pub charge_pct: Option<f64>,
    pub health_pct: Option<f64>,
    pub cycles: Option<f64>,
    pub draw_w: Option<f64>,
    pub time_remaining_min: Option<f64>,
    pub temp_c: Option<f64>,
}

pub struct BatterySensor {
    cached: Option<(Instant, Battery)>,
}

impl BatterySensor {
    pub fn new() -> Self {
        Self { cached: None }
    }

    fn battery(&mut self) -> Option<Battery> {
        if let Some((at, b)) = &self.cached
            && at.elapsed() < BATTERY_TTL
        {
            return Some(b.clone());
        }
        let b = read_battery()?;
        self.cached = Some((Instant::now(), b.clone()));
        Some(b)
    }
}

impl Default for BatterySensor {
    fn default() -> Self {
        Self::new()
    }
}

/// Read `AppleSmartBattery` from the IORegistry. `None` on a machine without
/// one (desktop), or when `ioreg` cannot be run at all.
pub fn read_battery() -> Option<Battery> {
    // -w0: never wrap a property line onto continuations.
    let out = Command::new("/usr/sbin/ioreg")
        .args(["-r", "-c", "AppleSmartBattery", "-w0"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !text.contains("AppleSmartBattery") {
        return None;
    }
    Some(parse_ioreg(&text))
}

/// Top-level properties print as `"Key" = value`; nested dictionaries print
/// as `"Key"=value`. The spaced form is therefore unambiguous for the keys
/// read here, and it is what `machsample.rs` already relies on.
fn raw(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\" = ");
    let idx = text.find(&needle)?;
    let rest = &text[idx + needle.len()..];
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
        .unwrap_or(rest.len());
    (end > 0).then(|| rest[..end].to_string())
}

fn num(text: &str, key: &str) -> Option<f64> {
    raw(text, key)?.parse::<f64>().ok()
}

/// Exact integer, tolerating the unsigned rendering `ioreg` uses for negative
/// values: `Amperage = 18446744073709551187` is `-429` mA. Parsing that as an
/// `f64` would lose the low bits *and* the sign, so it is read as `u64` and
/// reinterpreted.
fn int(text: &str, key: &str) -> Option<i64> {
    let s = raw(text, key)?;
    if let Ok(v) = s.parse::<i64>() {
        return Some(v);
    }
    s.parse::<u64>().ok().map(|v| v as i64)
}

/// Google-free, allocation-light parse of `ioreg -r -c AppleSmartBattery -w0`.
pub fn parse_ioreg(text: &str) -> Battery {
    let mut b = Battery::default();

    let current = num(text, "CurrentCapacity");
    let max = num(text, "MaxCapacity");
    if let (Some(cur), Some(max)) = (current, max) {
        if max > 0.0 {
            b.charge_pct = Some(round1(cur / max * 100.0));
        }
    } else if let Some(cur) = current {
        // Some packs publish only a percentage in CurrentCapacity.
        if (0.0..=100.0).contains(&cur) {
            b.charge_pct = Some(round1(cur));
        }
    }

    if let (Some(nominal), Some(design)) = (
        num(text, "NominalChargeCapacity"),
        num(text, "DesignCapacity"),
    ) && design > 0.0
    {
        b.health_pct = Some(round1(nominal / design * 100.0));
    }

    b.cycles = int(text, "CycleCount").map(|v| v as f64);

    // mV × mA / 1e6 = W, reported as a magnitude: the sign only says which way
    // the charge is flowing.
    let mv = num(text, "AppleRawBatteryVoltage").or_else(|| num(text, "Voltage"));
    let ma = int(text, "Amperage").or_else(|| int(text, "InstantAmperage"));
    if let (Some(mv), Some(ma)) = (mv, ma) {
        let watts = (mv * ma as f64 / 1e6).abs();
        if watts > 0.05 {
            b.draw_w = Some(round2(watts));
        }
    }

    let charging = text.contains("\"IsCharging\" = Yes");
    let minutes = if charging {
        num(text, "AvgTimeToFull")
    } else {
        num(text, "AvgTimeToEmpty")
    };
    // 65535 is the gauge's "unknown" sentinel.
    if let Some(m) = minutes
        && m > 0.0
        && m < 65535.0
    {
        b.time_remaining_min = Some(m.round());
    }

    if let Some(centi) = num(text, "Temperature")
        && centi > 0.0
    {
        b.temp_c = Some(round2(centi / 100.0));
    }

    b
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn def(id: &str, label: &str, unit: Unit) -> MetricDef {
    MetricDef {
        id: id.into(),
        label: label.into(),
        unit,
        kind: Kind::Gauge,
        group: Group::Battery,
        default_interval_ms: 5000,
        min_interval_ms: 1000,
        tier: Tier::Unprivileged,
    }
}

impl Sensor for BatterySensor {
    fn id(&self) -> &'static str {
        "battery"
    }

    fn tier(&self) -> Tier {
        Tier::Unprivileged
    }

    fn probe(&mut self) -> Vec<MetricDef> {
        // No battery → no rows. (Desktop Macs answer with no AppleSmartBattery.)
        if self.battery().is_none() {
            return Vec::new();
        }
        vec![
            def("battery.charge_pct", "Battery charge", Unit::Percent),
            def("battery.health_pct", "Battery health", Unit::Percent),
            def("battery.cycles", "Charge cycles", Unit::Count),
            def("battery.draw_w", "Battery draw", Unit::Watt),
            def(
                "battery.time_remaining_min",
                "Time remaining (min)",
                Unit::Count,
            ),
        ]
    }

    fn sample(&mut self, out: &mut SampleBuf) -> Result<(), SensorError> {
        let Some(b) = self.battery() else {
            return Err(SensorError::Unavailable(
                "no AppleSmartBattery in the IORegistry".into(),
            ));
        };

        let mut filled = 0usize;
        for (id, value) in [
            ("battery.charge_pct", b.charge_pct),
            ("battery.health_pct", b.health_pct),
            ("battery.cycles", b.cycles),
            ("battery.time_remaining_min", b.time_remaining_min),
        ] {
            if let Some(v) = value {
                out.push(id, v);
                filled += 1;
            }
        }

        // Draw: the gauge's own V × A when it is moving current; otherwise the
        // root daemon's package power, which is the machine's real draw on AC.
        let draw = b
            .draw_w
            .or_else(|| rootsample::read().and_then(|s| s.package_w));
        if let Some(w) = draw {
            out.push("battery.draw_w", w);
            filled += 1;
        }

        if filled == 0 {
            return Err(SensorError::SampleFailed(
                "AppleSmartBattery present but no usable property".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape this machine prints (values trimmed to the fields read).
    const LIVE: &str = r#"+-o AppleSmartBattery  <class AppleSmartBattery, id 0x100000265, registered, matched, active, busy 0 (0 ms), retain 10>
  {
    "CurrentCapacity" = 93
    "Amperage" = 18446744073709551187
    "AvgTimeToFull" = 65535
    "ExternalConnected" = No
    "AppleRawBatteryVoltage" = 12403
    "NominalChargeCapacity" = 4695
    "MaxCapacity" = 100
    "InstantAmperage" = 18446744073709551187
    "Temperature" = 3024
    "AvgTimeToEmpty" = 472
    "DesignCapacity" = 5760
    "IsCharging" = No
    "Voltage" = 12403
    "CycleCount" = 350
  }"#;

    #[test]
    fn parses_live_battery_block() {
        let b = parse_ioreg(LIVE);
        assert_eq!(b.charge_pct, Some(93.0));
        assert_eq!(b.cycles, Some(350.0));
        // 4695 / 5760 = 81.5 %
        assert_eq!(b.health_pct, Some(81.5));
        assert_eq!(b.time_remaining_min, Some(472.0));
        // 12.403 V × −0.429 A = 5.32 W
        assert_eq!(b.draw_w, Some(5.32));
        assert_eq!(b.temp_c, Some(30.24));
    }

    #[test]
    fn unsigned_amperage_keeps_its_sign() {
        assert_eq!(int(LIVE, "Amperage"), Some(-429));
    }

    #[test]
    fn unknown_sentinels_are_dropped() {
        // AvgTimeToFull = 65535 is the gauge's "unknown".
        let text = "\"AvgTimeToFull\" = 65535\n\"IsCharging\" = Yes\n";
        let b = parse_ioreg(text);
        assert_eq!(b.time_remaining_min, None);
        assert_eq!(b.draw_w, None);
        assert_eq!(b.charge_pct, None);
    }

    #[test]
    fn probe_and_sample_on_this_machine() {
        let mut s = BatterySensor::new();
        let defs = s.probe();
        if defs.is_empty() {
            println!("no battery on this host — skipping sample assertions");
            return;
        }
        assert!(defs.iter().any(|d| d.id == "battery.charge_pct"));
        let mut buf = SampleBuf::default();
        s.sample(&mut buf).expect("battery sample");
        assert!(!buf.values.is_empty());
        for (id, v) in &buf.values {
            assert!(v.is_finite(), "{id} = {v}");
            if id == "battery.charge_pct" {
                assert!((0.0..=100.0).contains(v), "charge {v}");
            }
        }
    }
}
