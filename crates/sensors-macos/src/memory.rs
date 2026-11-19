//! Memory sensor — `sysinfo` for RAM/swap, `sysctl` for the kernel's pressure
//! level.
//!
//! `sysinfo` reports total / used / available / swap directly. macOS has no
//! "pressure percent": the real number is
//! `kern.memorystatus_vm_pressure_level`, an integer the kernel maintains
//! (1 = normal, 2 = warning, 4 = critical). It is read with `sysctlbyname`,
//! in-process — no fork — and published raw so nothing is invented.
//!
//! Thresholds, for whoever renders it: 1 normal, 2 warning, 4 critical.
//! (Same key and same mapping the dashboard uses in `pressure_state()`.)

use sysinfo::System;
use syswatch_core::{Group, Kind, MetricDef, SampleBuf, Sensor, SensorError, Tier, Unit};

pub struct MemorySensor {
    sys: System,
}

impl MemorySensor {
    pub fn new() -> Self {
        Self { sys: System::new() }
    }
}

impl Default for MemorySensor {
    fn default() -> Self {
        Self::new()
    }
}

/// `kern.memorystatus_vm_pressure_level` — raw kernel level, `None` when the
/// key is missing (older kernels) or the syscall fails.
pub fn pressure_level() -> Option<f64> {
    let mut value: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            &mut value as *mut i32 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // A level of 0 / negative is "not reported", not "normal".
    (rc == 0 && value > 0).then_some(value as f64)
}

fn def(id: &str, label: &str, unit: Unit) -> MetricDef {
    MetricDef {
        id: id.into(),
        label: label.into(),
        unit,
        kind: Kind::Gauge,
        group: Group::Memory,
        default_interval_ms: 1000,
        min_interval_ms: 500,
        tier: Tier::Unprivileged,
    }
}

impl Sensor for MemorySensor {
    fn id(&self) -> &'static str {
        "memory"
    }

    fn tier(&self) -> Tier {
        Tier::Unprivileged
    }

    fn probe(&mut self) -> Vec<MetricDef> {
        self.sys.refresh_memory();
        vec![
            def("memory.total", "Memory total", Unit::Bytes),
            def("memory.used", "Memory used", Unit::Bytes),
            def("memory.available", "Memory available", Unit::Bytes),
            def(
                "memory.pressure",
                "Memory pressure level (1 normal, 2 warning, 4 critical)",
                Unit::Count,
            ),
            def("memory.swap_used", "Swap used", Unit::Bytes),
        ]
    }

    fn sample(&mut self, out: &mut SampleBuf) -> Result<(), SensorError> {
        self.sys.refresh_memory();
        let total = self.sys.total_memory();
        if total == 0 {
            return Err(SensorError::Unavailable(
                "sysinfo reported 0 total memory".into(),
            ));
        }
        let used = self.sys.used_memory();
        let sysinfo_available = self.sys.available_memory();
        out.push("memory.total", total as f64);
        out.push("memory.used", used as f64);
        // `sysinfo`'s own available_memory is `free + inactive + purgeable −
        // compressor` and saturates to 0 on this macOS build (measured: 0 B
        // while `vm.stat` reported thousands of free pages), so it is only
        // used when it is plausible. The fallback is the standard macOS
        // derivation: total − (active + wired + compressor + speculative),
        // i.e. the free plus reclaimable pages.
        let available = if sysinfo_available > 0 && sysinfo_available <= total {
            sysinfo_available
        } else {
            total.saturating_sub(used)
        };
        out.push("memory.available", available as f64);
        out.push("memory.swap_used", self.sys.used_swap() as f64);
        // Absent key → no value, rather than a fabricated 0.
        if let Some(level) = pressure_level() {
            out.push("memory.pressure", level);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_level_is_plausible() {
        match pressure_level() {
            Some(level) => assert!(level >= 1.0, "level {level}"),
            None => println!("kern.memorystatus_vm_pressure_level absent on this kernel"),
        }
    }

    #[test]
    fn probe_has_the_documented_metrics() {
        let mut s = MemorySensor::new();
        let defs = s.probe();
        assert!(!defs.is_empty());
        for id in [
            "memory.total",
            "memory.used",
            "memory.available",
            "memory.pressure",
            "memory.swap_used",
        ] {
            assert!(defs.iter().any(|d| d.id == id), "missing {id}");
        }
    }

    #[test]
    fn sample_fills_values() {
        let mut s = MemorySensor::new();
        let mut buf = SampleBuf::default();
        s.sample(&mut buf).expect("memory sample");
        for id in ["memory.total", "memory.used", "memory.available"] {
            let v = buf
                .values
                .iter()
                .find(|(k, _)| k == id)
                .unwrap_or_else(|| panic!("{id} not filled"));
            assert!(v.1 > 0.0, "{id} = {}", v.1);
        }
        let total = buf
            .values
            .iter()
            .find(|(k, _)| k == "memory.total")
            .unwrap()
            .1;
        let used = buf
            .values
            .iter()
            .find(|(k, _)| k == "memory.used")
            .unwrap()
            .1;
        assert!(used <= total, "used {used} > total {total}");
    }
}
