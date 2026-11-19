//! One cached `ioreg AppleSmartBattery` read, for `/api/power` and `/api/battery`.
//!
//! The sampler already reads the gauge (through
//! [`syswatch_sensors_macos::battery`]) on a 20 s cadence to fill
//! `/api/current`. It does not read the **static pack facts** (design capacity,
//! nominal capacity, raw max capacity) or the **adapter wattage**, which only
//! `/api/power` and `/api/battery` need — so this module parses those from one
//! more spawn, cached for [`TTL`].
//!
//! # Why one spawn and not two
//!
//! `/api/power` is polled at 1 s by the UI and `/api/battery` at 5 s. An
//! uncached `ioreg` per request would be ~3 spawns/s for numbers that change
//! once a minute. One [`TTL`]-cached read serves both endpoints, and the battery
//! scalars come from the same text through the sensor crate's parser — there is
//! exactly one ioreg parser in this product, not two.
//!
//! # Honesty rules carried over
//!
//! * `MaxCapacity` is a *percentage* on this pack (100) while `DesignCapacity`
//!   is mAh (5760) — the same value is published under the key it can actually
//!   support, never guessed into the other column.
//! * `AppleRawAdapterDetails` is an **empty array on battery power** (verified:
//!   `"AppleRawAdapterDetails" = ()`), so `adapter_w` is `None` while
//!   discharging — not 0 W.
//! * A missing key stays `None`.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use syswatch_sensors_macos::battery::{self, Battery};

/// The gauge updates about once a minute; 5 s is the same order as
/// `Sampler::BATTERY_SECS` while still letting a plugged-in event show up
/// quickly.
const TTL: Duration = Duration::from_secs(5);

/// Everything the power/battery endpoints need from the pack.
#[derive(Debug, Clone, Default)]
pub struct BatteryInfo {
    /// Charge %, health %, cycles, draw, time remaining, V/A, flags — parsed by
    /// the sensor crate's `parse_ioreg`, so both endpoints share one parser.
    pub gauge: Battery,
    pub design_mah: Option<f64>,
    pub nominal_mah: Option<f64>,
    pub raw_max_mah: Option<f64>,
    /// `MaxCapacity` published as a percentage, when it is ≤ 100 next to a
    /// design capacity in the thousands.
    pub max_capacity_pct: Option<f64>,
    pub max_capacity_mah: Option<f64>,
    /// Charging-port input watts from `AppleRawAdapterDetails` while on AC.
    pub adapter_w: Option<f64>,
    pub adapter_source: Option<String>,
    /// The adapter's own rating, independent of what it is delivering.
    pub adapter_rated_w: Option<f64>,
    pub cells_mv: Vec<f64>,
    /// Raw text, so a caller can look up a property this struct does not model
    /// and the tests can assert against what ioreg actually printed.
    #[allow(dead_code)]
    pub text: String,
}

impl BatteryInfo {
    /// Health % from the pack's own capacities — the Python server's
    /// `NominalChargeCapacity ÷ DesignCapacity` basis. `None` when the pack does
    /// not publish both.
    pub fn capacity_health_pct(&self) -> Option<f64> {
        match (self.nominal_mah, self.design_mah) {
            (Some(n), Some(d)) if d > 0.0 => Some(round1(n / d * 100.0)),
            _ => None,
        }
    }

    /// `good` / `fair` / `poor`, the same thresholds the Python server uses.
    pub fn condition(&self) -> &'static str {
        match self.capacity_health_pct().or(self.gauge.health_pct) {
            None => "unavailable",
            Some(h) if h >= 80.0 => "good",
            Some(h) if h >= 60.0 => "fair",
            Some(_) => "poor",
        }
    }

    /// Pack energy in Wh from the pack's own mAh and the measured voltage.
    pub fn pack_wh(&self) -> Option<f64> {
        let mah = self.nominal_mah.or(self.raw_max_mah)?;
        let v = self.gauge.voltage_v?;
        Some(round2(mah * v / 1000.0))
    }
}

fn cell() -> &'static Mutex<Option<(Instant, Option<BatteryInfo>)>> {
    static CELL: OnceLock<Mutex<Option<(Instant, Option<BatteryInfo>)>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// The current pack state, cached for [`TTL`]. `None` on a desktop (no
/// `AppleSmartBattery`) or when `ioreg` cannot be spawned.
pub fn read() -> Option<BatteryInfo> {
    if let Ok(guard) = cell().lock()
        && let Some((at, info)) = guard.as_ref()
        && at.elapsed() < TTL
    {
        return info.clone();
    }
    let info = read_uncached();
    if let Ok(mut guard) = cell().lock() {
        *guard = Some((Instant::now(), info.clone()));
    }
    info
}

/// Uncached read — public so a test can exercise the parser directly.
pub fn read_uncached() -> Option<BatteryInfo> {
    let out = std::process::Command::new("/usr/sbin/ioreg")
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
    Some(parse(&text))
}

/// Parse `ioreg -r -c AppleSmartBattery -w0` text.
///
/// Top-level properties print as `"Key" = value`; nested dictionary members
/// print as `"Key"=value`. That difference is what makes the spaced form safe
/// for the scalars (a `"DesignCapacity"` also appears inside `BatteryData`).
pub fn parse(text: &str) -> BatteryInfo {
    let design = num(text, "DesignCapacity");
    let nominal = num(text, "NominalChargeCapacity");
    let raw_max = num(text, "AppleRawMaxCapacity");
    let max_cap = num(text, "MaxCapacity");

    let mut info = BatteryInfo {
        gauge: battery::parse_ioreg(text),
        design_mah: design.filter(|v| *v > 0.0),
        nominal_mah: nominal.filter(|v| *v > 0.0),
        raw_max_mah: raw_max.filter(|v| *v > 0.0),
        // The pack's raw max capacity is a mAh figure and is published whether
        // or not `MaxCapacity` is a percentage (the Python server's rule).
        max_capacity_pct: None,
        max_capacity_mah: raw_max.filter(|v| *v > 0.0),
        adapter_w: None,
        adapter_source: None,
        adapter_rated_w: None,
        cells_mv: cell_voltages(text),
        text: text.to_string(),
    };

    if let Some(max) = max_cap.filter(|v| *v > 0.0) {
        let design_ma = info.design_mah.unwrap_or(0.0);
        if design_ma > 0.0 && max <= 100.0 && design_ma / 10.0 > max {
            info.max_capacity_pct = Some(round1(max));
        } else if max > 1_000.0 {
            info.max_capacity_mah = info.raw_max_mah.or(Some(max));
        }
    }

    let (watts, source, rated) = adapter_watts(text);
    info.adapter_w = watts;
    info.adapter_source = source;
    info.adapter_rated_w = rated;
    info
}

/// Charging-port input from `AppleRawAdapterDetails`.
///
/// Only keys with an unambiguous unit are used — `Watts` (W) and
/// `AdapterPowerTenthWatt` (0.1 W). A key whose unit is unknown is skipped
/// rather than guessed, which is why an empty array yields `(None, None, None)`
/// instead of 0 W.
fn adapter_watts(text: &str) -> (Option<f64>, Option<String>, Option<f64>) {
    let Some(line) = text
        .lines()
        .find(|l| l.contains("\"AppleRawAdapterDetails\""))
    else {
        return (None, None, None);
    };
    let mut best: Option<f64> = None;
    let mut used: Option<String> = None;
    let mut rated: Option<f64> = None;
    for (key, scale) in [("Watts", 1.0), ("AdapterPowerTenthWatt", 0.1)] {
        for v in nested_numbers(line, key) {
            if v <= 0.0 {
                continue;
            }
            let w = v * scale;
            if best.is_none_or(|b| w > b) {
                best = Some(round2(w));
                used = Some(format!("AppleRawAdapterDetails.{key}"));
            }
            // A rating is a nameplate number, so it is the max of the two keys
            // regardless of what the adapter is currently delivering.
            if rated.is_none_or(|r| w > r) {
                rated = Some(round1(w));
            }
        }
    }
    (best, used, rated)
}

/// Every value of `"Key"=value` inside `line` (nested-dictionary form).
fn nested_numbers(line: &str, key: &str) -> Vec<f64> {
    let needle = format!("\"{key}\"=");
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(idx) = rest.find(&needle) {
        let after = &rest[idx + needle.len()..];
        let end = after
            .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
            .unwrap_or(after.len());
        if end > 0
            && let Ok(v) = after[..end].parse::<f64>()
        {
            out.push(v);
        }
        rest = &after[end.min(after.len())..];
    }
    out
}

/// `"CellVoltage"=(3954,3951,3956)` → the three millivolt readings.
fn cell_voltages(text: &str) -> Vec<f64> {
    let Some(idx) = text.find("\"CellVoltage\"=(") else {
        return Vec::new();
    };
    let rest = &text[idx + "\"CellVoltage\"=(".len()..];
    let Some(end) = rest.find(')') else {
        return Vec::new();
    };
    rest[..end]
        .split(',')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        .collect()
}

/// Top-level scalar: `"Key" = 123`.
fn num(text: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\" = ");
    let idx = text.find(&needle)?;
    let rest = text[idx + needle.len()..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse::<f64>().ok()
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live shape on this machine on battery power (trimmed to fields read).
    const ON_BATTERY: &str = r#"+-o AppleSmartBattery  <class AppleSmartBattery>
  {
    "AppleRawAdapterDetails" = ()
    "CurrentCapacity" = 71
    "AvgTimeToFull" = 65535
    "ExternalConnected" = No
    "BatteryData" = {"CellVoltage"=(3954,3951,3956),"MaxCapacity"=100,"DesignCapacity"=5760}
    "AppleRawExternalConnected" = No
    "NominalChargeCapacity" = 4705
    "MaxCapacity" = 100
    "Temperature" = 3034
    "AvgTimeToEmpty" = 470
    "DesignCapacity" = 5760
    "IsCharging" = No
    "AppleRawMaxCapacity" = 4561
    "AppleRawBatteryVoltage" = 11860
    "Amperage" = 18446744073709551187
    "CycleCount" = 350
  }"#;

    #[test]
    fn parses_the_live_pack() {
        let b = parse(ON_BATTERY);
        assert_eq!(b.design_mah, Some(5760.0));
        assert_eq!(b.nominal_mah, Some(4705.0));
        assert_eq!(b.raw_max_mah, Some(4561.0));
        // MaxCapacity = 100 next to a 5760 mAh design → a percentage, not mAh.
        assert_eq!(b.max_capacity_pct, Some(100.0));
        assert_eq!(b.max_capacity_mah, Some(4561.0));
        assert_eq!(b.cells_mv, vec![3954.0, 3951.0, 3956.0]);
        assert_eq!(b.capacity_health_pct(), Some(81.7));
        assert_eq!(b.condition(), "good");
        assert_eq!(b.gauge.charge_pct, Some(71.0));
        assert_eq!(b.gauge.cycles, Some(350.0));
    }

    #[test]
    fn on_battery_there_is_no_adapter_wattage() {
        let b = parse(ON_BATTERY);
        assert_eq!(b.adapter_w, None, "an empty adapter array is not 0 W");
        assert_eq!(b.adapter_source, None);
        assert_eq!(b.adapter_rated_w, None);
    }

    #[test]
    fn on_ac_the_adapter_watts_are_read() {
        let ac = ON_BATTERY.replace(
            r#""AppleRawAdapterDetails" = ()"#,
            r#""AppleRawAdapterDetails" = ({"Watts"=96,"AdapterPowerTenthWatt"=960,"FamilyCode"=0})"#,
        );
        let b = parse(&ac);
        assert_eq!(b.adapter_w, Some(96.0));
        assert_eq!(b.adapter_rated_w, Some(96.0));
        assert_eq!(
            b.adapter_source.as_deref(),
            Some("AppleRawAdapterDetails.Watts")
        );
    }

    #[test]
    fn pack_energy_uses_measured_voltage() {
        let b = parse(ON_BATTERY);
        // 4705 mAh × 11.86 V = 55.8 Wh
        assert_eq!(b.pack_wh(), Some(55.8));
    }

    #[test]
    fn a_desktop_with_no_pack_parses_to_empty() {
        let b = parse("");
        assert_eq!(b.design_mah, None);
        assert_eq!(b.capacity_health_pct(), None);
        assert_eq!(b.condition(), "unavailable");
        assert!(b.cells_mv.is_empty());
    }
}
