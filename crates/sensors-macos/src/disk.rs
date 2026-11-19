//! Disk sensor — `sysinfo` for both halves:
//!
//!   * `total_space` / `available_space` per mounted volume → `disk.<v>.usage`
//!   * `usage()` byte counters per volume → `disk.<v>.read_bytes_sec` /
//!     `disk.<v>.write_bytes_sec`
//!
//! On macOS `sysinfo` fills the byte counters from the same IOKit object the
//! kernel keeps per storage driver (`IOBlockStorageDriver` → `Statistics` →
//! `Bytes (Read)` / `Bytes (Write)`), matched to the volume by its BSD name.
//! That is why disk I/O needs no process spawn here.
//!
//! Two details that matter for honesty:
//!
//! 1. **Rates need two reads.** A counter delta over a known window is a rate;
//!    a single read is not. Rows are published only after the first full
//!    window, so the first sample reports usage only — never a fabricated
//!    0 B/s.
//! 2. **Every APFS volume on one physical disk reports the same device
//!    counters.** Publishing all of them would repeat one number per volume,
//!    so rows with an identical (total, available, read, written) tuple are
//!    collapsed to the first volume in mount order ("/" wins on this machine).
//!
//! Volume enumeration walks every APFS volume, and the IOKit io refresh walks
//! the service tree per disk: both are throttled to `DISK_IO_REFRESH` (5 s),
//! the same cadence `stats.rs` uses for `last_disk_refresh`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use sysinfo::{DiskRefreshKind, Disks};
use syswatch_core::{Group, Kind, MetricDef, SampleBuf, Sensor, SensorError, Tier, Unit};

use crate::slug;

const DISK_IO_REFRESH: Duration = Duration::from_secs(5);
/// The first rate only needs *a* window, not the full 5 s one: this makes the
/// first tick after start publish real bytes/sec instead of leaving the row
/// blank until the first full throttle window has passed.
const MIN_RATE_WINDOW: Duration = Duration::from_secs(1);

pub struct DiskSensor {
    disks: Disks,
    last_refresh: Option<Instant>,
    /// Wall time of the previous counter read, for the rate denominator.
    last_io_at: Option<Instant>,
    /// volume slug → (read B/s, write B/s) from the last full window.
    rates: Vec<(String, f64, f64)>,
    /// Volume slugs that carry an I/O row, resolved at probe time.
    io_ids: Vec<String>,
}

impl DiskSensor {
    pub fn new() -> Self {
        Self {
            disks: Disks::new_with_refreshed_list_specifics(
                DiskRefreshKind::nothing().with_storage(),
            ),
            last_refresh: Some(Instant::now()),
            last_io_at: None,
            rates: Vec::new(),
            io_ids: Vec::new(),
        }
    }

    /// One throttled refresh that re-reads space *and* the IOKit counters, then
    /// turns the counter delta into bytes/sec.
    fn poll(&mut self) {
        let now = Instant::now();
        let elapsed = self.last_io_at.map(|t| now.duration_since(t).as_secs_f64());
        self.disks.refresh_specifics(
            false,
            DiskRefreshKind::nothing().with_storage().with_io_usage(),
        );

        // Collapse volumes that report the same underlying device counters.
        let mut seen: HashSet<(u64, u64, u64, u64)> = HashSet::new();
        let mut rates = Vec::new();
        let mut io_ids = Vec::new();
        let mut volumes: Vec<_> = self.disks.iter().filter(|d| d.total_space() > 0).collect();
        volumes.sort_by(|a, b| {
            let (a, b) = (
                a.mount_point().to_string_lossy().into_owned(),
                b.mount_point().to_string_lossy().into_owned(),
            );
            a.cmp(&b)
        });
        for disk in volumes {
            let usage = disk.usage();
            let key = (
                disk.total_space(),
                disk.available_space(),
                usage.total_read_bytes,
                usage.total_written_bytes,
            );
            if !seen.insert(key) {
                continue;
            }
            let id = slug(&disk.mount_point().to_string_lossy());
            io_ids.push(id.clone());
            if let Some(elapsed) = elapsed.filter(|e| *e > 0.5) {
                let read = usage.read_bytes as f64 / elapsed;
                let write = usage.written_bytes as f64 / elapsed;
                rates.push((id, read, write));
            }
        }
        self.rates = rates;
        self.io_ids = io_ids;
        self.last_io_at = Some(now);
        self.last_refresh = Some(now);
    }
}

impl Default for DiskSensor {
    fn default() -> Self {
        Self::new()
    }
}

fn def(id: String, label: String, unit: Unit) -> MetricDef {
    MetricDef {
        id,
        label,
        unit,
        kind: Kind::Gauge,
        group: Group::Storage,
        default_interval_ms: 1000,
        min_interval_ms: 1000,
        tier: Tier::Unprivileged,
    }
}

impl Sensor for DiskSensor {
    fn id(&self) -> &'static str {
        "disk"
    }

    fn tier(&self) -> Tier {
        Tier::Unprivileged
    }

    fn probe(&mut self) -> Vec<MetricDef> {
        // The io row names are only known once the counters have been read
        // once; probe runs at startup, so paying for it here is correct.
        self.poll();

        let mut defs = Vec::new();
        let mut seen = HashSet::new();
        for disk in self.disks.iter() {
            if disk.total_space() == 0 {
                continue;
            }
            let mount = disk.mount_point().to_string_lossy().into_owned();
            let id = slug(&mount);
            if !seen.insert(id.clone()) {
                continue;
            }
            defs.push(def(
                format!("disk.{id}.usage"),
                format!("{mount} used"),
                Unit::Percent,
            ));
        }
        for id in &self.io_ids {
            defs.push(def(
                format!("disk.{id}.read_bytes_sec"),
                format!("{id} read"),
                Unit::BytesPerSec,
            ));
            defs.push(def(
                format!("disk.{id}.write_bytes_sec"),
                format!("{id} write"),
                Unit::BytesPerSec,
            ));
        }
        defs
    }

    fn sample(&mut self, out: &mut SampleBuf) -> Result<(), SensorError> {
        let due = match self.last_refresh {
            None => true,
            Some(t) => {
                let elapsed = t.elapsed();
                elapsed >= DISK_IO_REFRESH || (self.rates.is_empty() && elapsed >= MIN_RATE_WINDOW)
            }
        };
        if due {
            self.poll();
        }

        let mut filled = 0usize;
        let mut seen = HashSet::new();
        for disk in self.disks.iter() {
            let total = disk.total_space();
            if total == 0 {
                continue;
            }
            let mount = disk.mount_point().to_string_lossy().into_owned();
            let id = slug(&mount);
            if !seen.insert(id.clone()) {
                continue;
            }
            let free = disk.available_space();
            let used_pct = total.saturating_sub(free) as f64 / total as f64 * 100.0;
            out.push(&format!("disk.{id}.usage"), used_pct);
            filled += 1;
        }

        for (id, read, write) in &self.rates {
            out.push(&format!("disk.{id}.read_bytes_sec"), *read);
            out.push(&format!("disk.{id}.write_bytes_sec"), *write);
            filled += 1;
        }

        if filled == 0 {
            return Err(SensorError::Unavailable("no mounted volume".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_has_usage_and_io_rows() {
        let mut s = DiskSensor::new();
        let defs = s.probe();
        assert!(!defs.is_empty(), "no disks probed");
        assert!(
            defs.iter().any(|d| d.id.ends_with(".usage")),
            "no usage row: {:?}",
            defs.iter().map(|d| &d.id).collect::<Vec<_>>()
        );
        assert!(
            defs.iter().any(|d| d.id.ends_with(".read_bytes_sec")),
            "no read row"
        );
        assert!(
            defs.iter().any(|d| d.id.ends_with(".write_bytes_sec")),
            "no write row"
        );
    }

    #[test]
    fn sample_fills_usage_immediately() {
        let mut s = DiskSensor::new();
        s.probe();
        let mut buf = SampleBuf::default();
        s.sample(&mut buf).expect("disk sample");
        let usage: Vec<_> = buf
            .values
            .iter()
            .filter(|(id, _)| id.ends_with(".usage"))
            .collect();
        assert!(!usage.is_empty(), "no usage value: {:?}", buf.values);
        for (id, v) in usage {
            assert!((0.0..=100.0).contains(v), "{id} = {v}");
        }
    }

    #[test]
    fn rates_need_a_second_read() {
        let mut s = DiskSensor::new();
        s.probe();
        let mut first = SampleBuf::default();
        s.sample(&mut first).unwrap();
        assert!(
            !first
                .values
                .iter()
                .any(|(id, _)| id.ends_with("_bytes_sec")),
            "a rate was published from a single counter read"
        );

        std::thread::sleep(DISK_IO_REFRESH + Duration::from_millis(200));
        let mut second = SampleBuf::default();
        s.sample(&mut second).unwrap();
        let rates: Vec<_> = second
            .values
            .iter()
            .filter(|(id, _)| id.ends_with("_bytes_sec"))
            .collect();
        assert!(
            !rates.is_empty(),
            "no rate after a full window: {:?}",
            second.values
        );
        for (id, v) in rates {
            assert!(v.is_finite() && *v >= 0.0, "{id} = {v}");
        }
    }
}
