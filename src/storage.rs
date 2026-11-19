//! T18 — disk-side storage cascade.
//!
//! The collector samples at 1 Hz and the raw line is ~6.7 KB (82% of it is the
//! three process tables), so a naive JSONL-per-day archive costs 24 MB/h,
//! 581 MB/day, 212 GB/year. This module applies the same max+mean cascade the
//! dashboard keeps in memory, at write time:
//!
//! ```text
//! logs/
//!   YYYY-MM-DD.jsonl          hard link -> that day's finest tier file
//!   raw/YYYY-MM-DDTHH.jsonl   1 s, hourly files, append-only
//!   tier15s/YYYY-MM-DD.jsonl  15 s buckets   (aged out at 1 d)
//!   tier60s/YYYY-MM-DD.jsonl  60 s buckets   (aged out at 7 d)
//!   tier300s/YYYY-MM-DD.jsonl 300 s buckets  (aged out at 30 d)
//!   tier1800s/YYYY-MM-DD.jsonl 1800 s buckets (aged out at 1 y)
//!   archive/YYYY-MM-DD.jsonl  1 merged record per day, kept forever
//! ```
//!
//! **Why the hard link.** `dashboard/server.py` discovers collector files with
//! `os.listdir(COLLECTOR_LOGS)` filtered on `.jsonl` — non-recursive — and the
//! dashboard is out of scope for this task. Day files therefore have to keep
//! appearing in the log root, so the root holds one name per day hard-linked to
//! the day's finest tier file (same inode: `du` counts it once, the file is
//! swapped atomically when the day is demoted a tier). `raw/` is deliberately
//! not linked: 1 s data would flood the dashboard's 120 k-point ring and evict
//! real history for no visual gain, since `bucketize()` caps every range at 500
//! points anyway.
//!
//! **Record shape.** Identical top-level keys to `Snapshot`; every continuous
//! metric keeps a plain number at its old key (the mean) and gains a
//! `<key>_stat` sibling `{min,max,mean,n}`. That is what makes this invisible to
//! `stats.rs`, `ui.rs` and `HistoryIndex._parse`, which all index scalars.
//!
//! **Idempotency.** The compactor is stateless: every decision is
//! `bucket_start < now - tier_retention`, so a second pass over already
//! compacted data finds nothing to do rather than trusting a watermark file.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::stats::Snapshot;

// ─── tiers ───────────────────────────────────────────────────────────────
/// Seconds per bucket for each cascade tier, finest first.
pub const TIER_15S: u64 = 15;
pub const TIER_60S: u64 = 60;
pub const TIER_300S: u64 = 300;
pub const TIER_1800S: u64 = 1800;

/// How long a tier keeps buckets before they are demoted to the next one.
const KEEP_15S: i64 = 86_400; // 1 d
const KEEP_60S: i64 = 7 * 86_400; // 7 d
const KEEP_300S: i64 = 30 * 86_400; // 30 d
const KEEP_1800S: i64 = 365 * 86_400; // 1 y

/// Default byte cap for the raw buffer. 6 h of 1 s samples at the measured
/// 6.73 KB/line would be 145 MB, which on its own breaks the 100 MB bound, so
/// bytes — not hours — are the binding cap on this machine.
pub const DEFAULT_RAW_MAX_MB: u64 = 24;

// ─── config ──────────────────────────────────────────────────────────────
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Master switch. `false` restores the pre-T18 layout: one raw
    /// `YYYY-MM-DD.jsonl` per day in the log root, kept for `retention_days`.
    pub cascade: bool,
    /// Age cap for raw 1 s data.
    pub raw_hours: u64,
    /// Byte cap for raw 1 s data (applied before the age cap).
    pub raw_max_bytes: u64,
    /// Write the one-record-per-day archive tail.
    pub archive: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            cascade: true,
            raw_hours: 6,
            raw_max_bytes: DEFAULT_RAW_MAX_MB * 1024 * 1024,
            archive: true,
        }
    }
}

// ─── time (no external crates; `stats.rs` sets the same precedent) ────────
pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse the collector's RFC 3339 UTC timestamps (`...T09:43:15Z`, optional
/// fractional seconds, optional `+HH:MM` offset) into epoch seconds.
pub fn parse_utc(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |s: &str| -> Option<i64> { s.parse::<i64>().ok() };
    let y = num(ts.get(0..4)?)?;
    let mo = num(ts.get(5..7)?)?;
    let d = num(ts.get(8..10)?)?;
    let h = num(ts.get(11..13)?)?;
    let mi = num(ts.get(14..16)?)?;
    let s = num(ts.get(17..19)?)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let mut epoch = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s;
    // Offset: 'Z' (or nothing) means UTC; ±HH:MM shifts back to UTC.
    if let Some(rest) = ts.get(19..) {
        let rest = rest.trim_start_matches('.');
        // Skip a fractional part if present.
        let rest: &str = if rest.starts_with(|c: char| c.is_ascii_digit()) {
            let idx = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            &rest[idx..]
        } else {
            rest
        };
        if let Some(sign) = rest.chars().next() {
            if sign == '+' || sign == '-' {
                let oh = num(rest.get(1..3)?)?;
                let om = num(rest.get(4..6)?).unwrap_or(0);
                let delta = oh * 3600 + om * 60;
                epoch += if sign == '+' { -delta } else { delta };
            }
        }
    }
    Some(epoch)
}

pub fn fmt_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, mo, d, h, m, s
    )
}

fn day_of(secs: i64) -> String {
    fmt_utc(secs)[..10].to_string()
}

/// Raw slots are 15 minutes, not an hour: one hour of 1 s samples is ~25 MB
/// on this machine, i.e. already larger than the whole raw byte budget, so an
/// hourly file would be deleted whole at every rotation and the 1 s window
/// would collapse to zero. At 15 min the byte cap trims at ~6 MB per file.
///
/// Stem: `YYYY-MM-DDTHH-MM` (e.g. `2026-11-19T04-30`).
const RAW_SLOT_SECS: u64 = 900;

fn raw_slot(secs: i64) -> String {
    let slot_min = (secs.rem_euclid(3600) / 60) as u64 / (RAW_SLOT_SECS / 60);
    format!(
        "{}-{:02}",
        &fmt_utc(secs)[..13],
        slot_min * (RAW_SLOT_SECS / 60)
    )
}

/// Newest sample timestamp in a raw file, read from the tail. Independent of
/// the file name, so a file written by an older naming scheme still ages
/// correctly.
fn newest_raw_ts(path: &Path) -> Option<i64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let back = len.min(16 * 1024);
    f.seek(SeekFrom::Start(len - back)).ok()?;
    let mut buf = String::new();
    f.take(back).read_to_string(&mut buf).ok()?;
    for line in buf.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if let Some(e) = parse_utc(ts) {
                return Some(e);
            }
        }
    }
    None
}

fn floor_to(secs: i64, width: u64) -> i64 {
    secs - secs.rem_euclid(width as i64)
}

// ─── statistics ──────────────────────────────────────────────────────────
/// One metric's spread inside a bucket. Serialises as `{min,max,mean,n}`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    pub n: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

impl Stat {
    fn add(&mut self, v: f64) {
        if !v.is_finite() {
            return;
        }
        if self.n == 0 {
            self.min = v;
            self.max = v;
        } else {
            if v < self.min {
                self.min = v;
            }
            if v > self.max {
                self.max = v;
            }
        }
        self.sum += v;
        self.n += 1;
    }

    fn merge(&mut self, other: &Stat) {
        if other.n == 0 {
            return;
        }
        if self.n == 0 {
            *self = *other;
            return;
        }
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.sum += other.sum;
        self.n += other.n;
    }

    /// Add `k` copies of `v` at once. Used to account for a network
    /// interface that was absent (`v = 0`) for part of a bucket: an interface
    /// that is down moved zero bytes, it did not stop existing.
    fn add_repeat(&mut self, v: f64, k: u64) {
        if k == 0 {
            return;
        }
        if !v.is_finite() {
            return;
        }
        if self.n == 0 {
            self.min = v;
            self.max = v;
        } else {
            if v < self.min {
                self.min = v;
            }
            if v > self.max {
                self.max = v;
            }
        }
        self.sum += v * k as f64;
        self.n += k;
    }

    fn mean(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.sum / self.n as f64
        }
    }

    fn to_json(&self) -> Value {
        json!({"min": round3(self.min), "max": round3(self.max),
               "mean": round3(self.mean()), "n": self.n})
    }

    fn from_json(v: &Value) -> Stat {
        let g = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        Stat {
            n: v.get("n").and_then(|x| x.as_u64()).unwrap_or(0),
            sum: g("mean") * v.get("n").and_then(|x| x.as_u64()).unwrap_or(0) as f64,
            min: g("min"),
            max: g("max"),
        }
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

// ─── bucket accumulator ──────────────────────────────────────────────────
#[derive(Debug, Clone, Default)]
struct DiskBucket {
    mount: String,
    fs: String,
    used_pct: Stat,
    free_gb: Stat,
    total_gb: f64,
}

#[derive(Debug, Clone, Default)]
struct NetBucket {
    name: String,
    rx: Stat,
    tx: Stat,
}

#[derive(Debug, Clone, Default)]
struct EnergyBucket {
    rusage_version: Option<u8>,
    attribution: String,
    source: String,
    gpu_time: String,
    note: Option<String>,
    window_s: Stat,
    attributed_w: Stat,
    processes_total: Stat,
    processes_measured: Stat,
    unreadable_pids: Stat,
    counter_resets: Stat,
    zero_energy_pids: Stat,
    package_w: Stat,
    unaccounted_w: Stat,
    over_attribution_pct: Stat,
}

/// One bucket of samples, accumulated in memory and then written as one line.
/// The same type is used by the hot path (built from `Snapshot`s) and by the
/// compactor (built from existing lines), so the merge maths is shared.
#[derive(Debug, Clone)]
pub struct Bucket {
    pub tier: u64,
    pub start: i64,
    pub n: u64,
    uptime: f64,
    cpu_total: Stat,
    cpu_freq: Stat,
    mem_pct: Stat,
    mem_used: Stat,
    mem_total: f64,
    swap_used: Stat,
    swap_total: f64,
    temperature: Stat,
    proc_count: Stat,
    disks: Vec<DiskBucket>,
    nets: Vec<NetBucket>,
    energy: EnergyBucket,
}

impl Bucket {
    pub fn new(tier: u64, start: i64) -> Self {
        Self {
            tier,
            start,
            n: 0,
            uptime: 0.0,
            cpu_total: Stat::default(),
            cpu_freq: Stat::default(),
            mem_pct: Stat::default(),
            mem_used: Stat::default(),
            mem_total: 0.0,
            swap_used: Stat::default(),
            swap_total: 0.0,
            temperature: Stat::default(),
            proc_count: Stat::default(),
            disks: Vec::new(),
            nets: Vec::new(),
            energy: EnergyBucket::default(),
        }
    }

    /// Fold one live sample into the bucket.
    fn add(&mut self, s: &Snapshot) {
        self.n += 1;
        self.uptime = s.uptime_secs as f64;
        self.cpu_total.add(s.cpu.total_pct as f64);
        self.cpu_freq.add(s.cpu.freq_mhz as f64);
        self.mem_pct.add(s.memory.pct);
        self.mem_used.add(s.memory.used_bytes as f64);
        self.mem_total = self.mem_total.max(s.memory.total_bytes as f64);
        self.swap_used.add(s.memory.swap_used_bytes as f64);
        self.swap_total = self.swap_total.max(s.memory.swap_total_bytes as f64);
        if let Some(t) = s.temperature {
            self.temperature.add(t as f64);
        }
        self.proc_count.add(s.process_count as f64);

        for d in &s.disks {
            let slot = match self.disks.iter_mut().find(|x| x.mount == d.mount) {
                Some(x) => x,
                None => {
                    self.disks.push(DiskBucket {
                        mount: d.mount.clone(),
                        fs: d.fs.clone(),
                        ..Default::default()
                    });
                    self.disks.last_mut().unwrap()
                }
            };
            slot.used_pct.add(d.used_pct);
            slot.free_gb.add(d.free_gb);
            slot.total_gb = slot.total_gb.max(d.total_gb);
        }
        // An interface missing from this sample moved no bytes. Counting only
        // the samples where it was present would inflate the per-interface
        // mean, and `down_bps`/`up_bps` are sums of those means.
        for slot in self.nets.iter_mut() {
            if !s.networks.iter().any(|n| n.name == slot.name) {
                slot.rx.add(0.0);
                slot.tx.add(0.0);
            }
        }
        for n in &s.networks {
            match self.nets.iter_mut().find(|x| x.name == n.name) {
                Some(slot) => {
                    slot.rx.add(n.rx_bps);
                    slot.tx.add(n.tx_bps);
                }
                None => {
                    // First sighting inside this bucket: backfill the samples
                    // that already happened.
                    let mut rx = Stat::default();
                    let mut tx = Stat::default();
                    rx.add_repeat(0.0, self.n - 1);
                    tx.add_repeat(0.0, self.n - 1);
                    rx.add(n.rx_bps);
                    tx.add(n.tx_bps);
                    self.nets.push(NetBucket {
                        name: n.name.clone(),
                        rx,
                        tx,
                    });
                }
            }
        }

        let e = &s.energy;
        if self.energy.attribution.is_empty() {
            self.energy.attribution = e.attribution.clone();
            self.energy.source = e.source.clone();
            self.energy.gpu_time = e.gpu_time.clone();
            self.energy.note = e.note.clone();
            self.energy.rusage_version = e.rusage_version;
        }
        self.energy.window_s.add(e.window_s);
        self.energy.attributed_w.add(e.attributed_w);
        self.energy.processes_total.add(e.processes_total as f64);
        self.energy.processes_measured.add(e.processes_measured as f64);
        self.energy.unreadable_pids.add(e.unreadable_pids as f64);
        self.energy.counter_resets.add(e.counter_resets as f64);
        self.energy.zero_energy_pids.add(e.zero_energy_pids as f64);
        if let Some(v) = e.package_w {
            self.energy.package_w.add(v);
        }
        if let Some(v) = e.unaccounted_w {
            self.energy.unaccounted_w.add(v);
        }
        if let Some(v) = e.over_attribution_pct {
            self.energy.over_attribution_pct.add(v);
        }
    }

    /// Fold another bucket into this one. Weighted by `n`, so the mean of a
    /// merge equals the mean of the underlying samples.
    fn merge(&mut self, o: &Bucket) {
        let prev_n = self.n;
        self.n += o.n;
        self.start = self.start.min(o.start);
        self.uptime = self.uptime.max(o.uptime);
        self.cpu_total.merge(&o.cpu_total);
        self.cpu_freq.merge(&o.cpu_freq);
        self.mem_pct.merge(&o.mem_pct);
        self.mem_used.merge(&o.mem_used);
        self.mem_total = self.mem_total.max(o.mem_total);
        self.swap_used.merge(&o.swap_used);
        self.swap_total = self.swap_total.max(o.swap_total);
        self.temperature.merge(&o.temperature);
        self.proc_count.merge(&o.proc_count);

        for d in &o.disks {
            match self.disks.iter_mut().find(|x| x.mount == d.mount) {
                Some(x) => {
                    x.used_pct.merge(&d.used_pct);
                    x.free_gb.merge(&d.free_gb);
                    x.total_gb = x.total_gb.max(d.total_gb);
                }
                None => self.disks.push(d.clone()),
            }
        }
        // Same rule across buckets: a name this bucket carried and `o` did not
        // moved zero bytes during `o`'s samples, and vice versa.
        for x in self.nets.iter_mut() {
            if !o.nets.iter().any(|y| y.name == x.name) {
                x.rx.add_repeat(0.0, o.n);
                x.tx.add_repeat(0.0, o.n);
            }
        }
        for n in &o.nets {
            match self.nets.iter_mut().find(|x| x.name == n.name) {
                Some(x) => {
                    x.rx.merge(&n.rx);
                    x.tx.merge(&n.tx);
                }
                None => {
                    let mut x = n.clone();
                    x.rx.add_repeat(0.0, prev_n);
                    x.tx.add_repeat(0.0, prev_n);
                    self.nets.push(x);
                }
            }
        }

        let e = &mut self.energy;
        if e.attribution.is_empty() {
            e.attribution = o.energy.attribution.clone();
            e.source = o.energy.source.clone();
            e.gpu_time = o.energy.gpu_time.clone();
            e.note = o.energy.note.clone();
            e.rusage_version = o.energy.rusage_version;
        }
        e.window_s.merge(&o.energy.window_s);
        e.attributed_w.merge(&o.energy.attributed_w);
        e.processes_total.merge(&o.energy.processes_total);
        e.processes_measured.merge(&o.energy.processes_measured);
        e.unreadable_pids.merge(&o.energy.unreadable_pids);
        e.counter_resets.merge(&o.energy.counter_resets);
        e.zero_energy_pids.merge(&o.energy.zero_energy_pids);
        e.package_w.merge(&o.energy.package_w);
        e.unaccounted_w.merge(&o.energy.unaccounted_w);
        e.over_attribution_pct.merge(&o.energy.over_attribution_pct);
    }

    fn mean_u64(&self, s: &Stat) -> u64 {
        s.mean().round().max(0.0) as u64
    }

    /// The 15 s tier is the only one that carries the full spread on every
    /// metric. Coarser tiers keep `{min,max,mean,n}` for the headline series
    /// only (cpu, memory %, temperature) and plain means elsewhere: a year at
    /// 1800 s with full per-interface spreads would be 40 MB on its own.
    fn full_detail(&self) -> bool {
        self.tier == TIER_15S
    }

    /// Serialise to a line that still deserialises as `Snapshot`.
    fn to_json(&self) -> Value {
        let full = self.full_detail();
        let disks: Vec<Value> = self
            .disks
            .iter()
            .map(|d| {
                let mut o = json!({
                    "mount": d.mount, "fs": d.fs,
                    "used_pct": round3(d.used_pct.mean()),
                    "free_gb": round3(d.free_gb.mean()),
                    "total_gb": round3(d.total_gb),
                });
                if full {
                    o["used_pct_stat"] = d.used_pct.to_json();
                    o["free_gb_stat"] = d.free_gb.to_json();
                }
                o
            })
            .collect();
        let nets: Vec<Value> = self
            .nets
            .iter()
            .map(|n| {
                let mut o = json!({
                    "name": n.name,
                    "rx_bps": round3(n.rx.mean()),
                    "tx_bps": round3(n.tx.mean()),
                });
                if full {
                    o["rx_bps_stat"] = n.rx.to_json();
                    o["tx_bps_stat"] = n.tx.to_json();
                }
                o
            })
            .collect();

        let e = &self.energy;
        let mut energy = json!({
            "rusage_version": e.rusage_version,
            "attribution": if e.attribution.is_empty() { "unavailable" } else { &e.attribution },
            "source": if e.source.is_empty() { "unavailable" } else { &e.source },
            "window_s": round3(e.window_s.mean()),
            "attributed_w": round3(e.attributed_w.mean()),
            "processes_total": self.mean_u64(&e.processes_total),
            "processes_measured": self.mean_u64(&e.processes_measured),
            "unreadable_pids": self.mean_u64(&e.unreadable_pids),
            "counter_resets": self.mean_u64(&e.counter_resets),
            "zero_energy_pids": self.mean_u64(&e.zero_energy_pids),
            "gpu_time": if e.gpu_time.is_empty() { "unavailable" } else { &e.gpu_time },
            // `Energy::units` is a required field of the `Snapshot` contract.
            "units": {"energy": "nJ", "power": "W", "share": "%"},
        });
        if e.package_w.n > 0 {
            energy["package_w"] = json!(round3(e.package_w.mean()));
        }
        if e.unaccounted_w.n > 0 {
            energy["unaccounted_w"] = json!(round3(e.unaccounted_w.mean()));
        }
        if e.over_attribution_pct.n > 0 {
            energy["over_attribution_pct"] = json!(round3(e.over_attribution_pct.mean()));
        }
        if full {
            energy["attributed_w_stat"] = e.attributed_w.to_json();
        }
        // `energy.note` is a 232-byte constant explanation repeated on every
        // line (13 MB a year). It stays on the raw lines, where a human reads
        // it; `attribution` + the absent `package_w` already say the same.

        let mut cpu = json!({
            "total_pct": round3(self.cpu_total.mean()) as f32,
            "per_core_pct": Vec::<f32>::new(),
            "freq_mhz": self.mean_u64(&self.cpu_freq),
            "total_pct_stat": self.cpu_total.to_json(),
        });
        if full && self.cpu_freq.n > 0 {
            cpu["freq_mhz_stat"] = self.cpu_freq.to_json();
        }

        let mut obj = json!({
            "timestamp": fmt_utc(self.start),
            "uptime_secs": self.uptime.round().max(0.0) as u64,
            "cpu": cpu,
            "memory": {
                "used_bytes": self.mean_u64(&self.mem_used),
                "total_bytes": self.mem_total.round().max(0.0) as u64,
                "pct": round3(self.mem_pct.mean()),
                "swap_used_bytes": self.mean_u64(&self.swap_used),
                "swap_total_bytes": self.swap_total.round().max(0.0) as u64,
                "pct_stat": self.mem_pct.to_json(),
            },
            "disks": disks,
            "networks": nets,
            "energy": energy,
            "process_count": self.mean_u64(&self.proc_count),
            "agg": {"tier": self.tier, "start": fmt_utc(self.start), "n": self.n},
        });
        if full {
            obj["memory"]["used_bytes_stat"] = self.mem_used.to_json();
        }
        if self.temperature.n > 0 {
            obj["temperature"] = json!(round3(self.temperature.mean()) as f32);
            obj["temperature_stat"] = self.temperature.to_json();
        }
        obj
    }

    /// Rebuild a bucket from a stored line. Returns `None` for raw 1 s lines
    /// (no `agg` block) and for junk.
    fn from_json(v: &Value) -> Option<Bucket> {
        let agg = v.get("agg")?;
        let tier = agg.get("tier").and_then(|x| x.as_u64())?;
        let start = parse_utc(agg.get("start")?.as_str()?)?;
        let n = agg.get("n").and_then(|x| x.as_u64()).unwrap_or(1).max(1);

        let st = |obj: &Value, k: &str| -> Stat {
            obj.get(k).map(Stat::from_json).unwrap_or_default()
        };
        let num = |obj: &Value, k: &str| obj.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let unum = |obj: &Value, k: &str| obj.get(k).and_then(|x| x.as_u64()).unwrap_or(0);

        let cpu = v.get("cpu")?;
        let mem = v.get("memory")?;
        let mut b = Bucket::new(tier, start);
        b.n = n;
        b.uptime = num(v, "uptime_secs");
        b.cpu_total = st(cpu, "total_pct_stat");
        if b.cpu_total.n == 0 {
            b.cpu_total.add(num(cpu, "total_pct"));
        }
        b.cpu_freq = st(cpu, "freq_mhz_stat");
        if b.cpu_freq.n == 0 {
            b.cpu_freq.add(num(cpu, "freq_mhz"));
        }
        b.mem_pct = st(mem, "pct_stat");
        if b.mem_pct.n == 0 {
            b.mem_pct.add(num(mem, "pct"));
        }
        b.mem_used = st(mem, "used_bytes_stat");
        if b.mem_used.n == 0 {
            b.mem_used.add(num(mem, "used_bytes"));
        }
        b.mem_total = num(mem, "total_bytes");
        b.swap_used.add(num(mem, "swap_used_bytes"));
        b.swap_total = num(mem, "swap_total_bytes");
        b.temperature = st(v, "temperature_stat");
        if b.temperature.n == 0 {
            if let Some(t) = v.get("temperature").and_then(|x| x.as_f64()) {
                b.temperature.add(t);
            }
        }
        b.proc_count.add(num(v, "process_count"));

        if let Some(list) = v.get("disks").and_then(|x| x.as_array()) {
            for d in list {
                b.disks.push(DiskBucket {
                    mount: d.get("mount").and_then(|x| x.as_str()).unwrap_or("").into(),
                    fs: d.get("fs").and_then(|x| x.as_str()).unwrap_or("").into(),
                    used_pct: {
                        let s = st(d, "used_pct_stat");
                        if s.n > 0 { s } else { let mut s = Stat::default(); s.add(num(d, "used_pct")); s }
                    },
                    free_gb: st(d, "free_gb_stat"),
                    total_gb: num(d, "total_gb"),
                });
            }
        }
        if let Some(list) = v.get("networks").and_then(|x| x.as_array()) {
            for nw in list {
                let mut rx = st(nw, "rx_bps_stat");
                let mut tx = st(nw, "tx_bps_stat");
                if rx.n == 0 {
                    rx.add(num(nw, "rx_bps"));
                }
                if tx.n == 0 {
                    tx.add(num(nw, "tx_bps"));
                }
                b.nets.push(NetBucket {
                    name: nw.get("name").and_then(|x| x.as_str()).unwrap_or("").into(),
                    rx,
                    tx,
                });
            }
        }

        if let Some(e) = v.get("energy") {
            let eb = &mut b.energy;
            eb.rusage_version = e.get("rusage_version").and_then(|x| x.as_u64()).map(|x| x as u8);
            eb.attribution = e.get("attribution").and_then(|x| x.as_str()).unwrap_or("").into();
            eb.source = e.get("source").and_then(|x| x.as_str()).unwrap_or("").into();
            eb.gpu_time = e.get("gpu_time").and_then(|x| x.as_str()).unwrap_or("").into();
            eb.note = e.get("note").and_then(|x| x.as_str()).map(|s| s.to_string());
            eb.window_s.add(num(e, "window_s"));
            eb.attributed_w = st(e, "attributed_w_stat");
            if eb.attributed_w.n == 0 {
                eb.attributed_w.add(num(e, "attributed_w"));
            }
            eb.processes_total.add(unum(e, "processes_total") as f64);
            eb.processes_measured.add(unum(e, "processes_measured") as f64);
            eb.unreadable_pids.add(unum(e, "unreadable_pids") as f64);
            eb.counter_resets.add(unum(e, "counter_resets") as f64);
            eb.zero_energy_pids.add(unum(e, "zero_energy_pids") as f64);
            if let Some(v) = e.get("package_w").and_then(|x| x.as_f64()) {
                eb.package_w.add(v);
            }
            if let Some(v) = e.get("unaccounted_w").and_then(|x| x.as_f64()) {
                eb.unaccounted_w.add(v);
            }
            if let Some(v) = e.get("over_attribution_pct").and_then(|x| x.as_f64()) {
                eb.over_attribution_pct.add(v);
            }
        }
        Some(b)
    }
}

// ─── layout ──────────────────────────────────────────────────────────────
fn raw_dir(root: &Path) -> PathBuf {
    root.join("raw")
}
fn tier_dir(root: &Path, tier: u64) -> PathBuf {
    root.join(format!("tier{}s", tier))
}
fn archive_dir(root: &Path) -> PathBuf {
    root.join("archive")
}

fn ensure_dirs(root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    fs::create_dir_all(raw_dir(root))?;
    for t in [TIER_15S, TIER_60S, TIER_300S, TIER_1800S] {
        fs::create_dir_all(tier_dir(root, t))?;
    }
    fs::create_dir_all(archive_dir(root))?;
    fs::create_dir_all(root.join("state"))?;
    Ok(())
}

fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// Raw files ordered oldest-data-first. File names cannot be trusted for this:
/// `T04-45` sorts before `T04` (hyphen is below the period), yet it holds the
/// *newer* samples. The byte cap must always evict the oldest data.
fn raw_files_by_age(dir: &Path) -> Vec<(i64, PathBuf, u64)> {
    let mut out: Vec<(i64, PathBuf, u64)> = jsonl_files(dir)
        .into_iter()
        .filter_map(|p| {
            let size = fs::metadata(&p).ok().map(|m| m.len()).unwrap_or(0);
            newest_raw_ts(&p).map(|t| (t, p, size))
        })
        .collect();
    out.sort_by_key(|(t, _, _)| *t);
    out
}

fn read_buckets(path: &Path) -> Vec<Bucket> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(b) = Bucket::from_json(&v) {
                out.push(b);
            }
        }
    }
    out
}

/// `write .tmp-<name>` → fsync → rename → fsync(dir).
fn write_atomic(dir: &Path, name: &str, lines: &[String]) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp-{}", name));
    {
        let f = fs::File::create(&tmp)?;
        let mut w = BufWriter::with_capacity(64 * 1024, f);
        for l in lines {
            w.write_all(l.as_bytes())?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir);
    Ok(())
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// Merge `incoming` into `dir/name`, deduping by `(bucket_start, tier)` so a
/// crash between "target written" and "source deleted" cannot double-count.
fn merge_into_file(dir: &Path, name: &str, incoming: Vec<Bucket>) -> std::io::Result<()> {
    let path = dir.join(name);
    let mut by_start: BTreeMap<i64, Bucket> = BTreeMap::new();
    for b in read_buckets(&path).into_iter().chain(incoming.into_iter()) {
        match by_start.get_mut(&b.start) {
            Some(cur) => cur.merge(&b),
            None => {
                by_start.insert(b.start, b);
            }
        }
    }
    let lines: Vec<String> = by_start
        .values()
        .map(|b| serde_json::to_string(&b.to_json()).unwrap_or_default())
        .filter(|s| !s.is_empty())
        .collect();
    write_atomic(dir, name, &lines)
}

// ─── root day links ──────────────────────────────────────────────────────
#[cfg(unix)]
fn inode(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|m| m.ino())
}

/// Point `root/YYYY-MM-DD.jsonl` at `target` (that day's finest tier file).
/// Same inode, so `du` counts the bytes once; the swap is a link-to-temp then
/// `rename` over the day name, so the dashboard never sees the file vanish.
fn link_day(root: &Path, target: &Path, day: &str) -> std::io::Result<()> {
    let name = format!("{}.jsonl", day);
    let link = root.join(&name);
    #[cfg(unix)]
    {
        let n = format!(".syswatch-link-{}.tmp", day);
        let tmp = root.join(&n);
        let _ = fs::remove_file(&tmp);
        match fs::hard_link(target, &tmp) {
            Ok(()) => fs::rename(&tmp, &link)?,
            Err(_) => {
                // Different filesystem or no hard-link support: copy instead.
                let _ = fs::remove_file(&tmp);
                fs::copy(target, &tmp)?;
                fs::rename(&tmp, &link)?;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let tmp = root.join(format!(".syswatch-link-{}.tmp", day));
        let _ = fs::remove_file(&tmp);
        fs::copy(target, &tmp)?;
        fs::rename(&tmp, &link)?;
    }
    Ok(())
}

fn day_link_is(current: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        match (inode(current), inode(target)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = current;
        let _ = target;
        false
    }
}

/// The finest tier file that exists for `day`, if any.
fn finest_day_file(root: &Path, day: &str) -> Option<PathBuf> {
    for t in [TIER_15S, TIER_60S, TIER_300S, TIER_1800S] {
        let p = tier_dir(root, t).join(format!("{}.jsonl", day));
        if p.exists() {
            return Some(p);
        }
    }
    let a = archive_dir(root).join(format!("{}.jsonl", day));
    if a.exists() {
        return Some(a);
    }
    None
}

// ─── compaction report ───────────────────────────────────────────────────
#[derive(Debug, Default)]
pub struct Report {
    pub dry_run: bool,
    pub actions: Vec<String>,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub buckets_moved: u64,
}

impl Report {
    fn note(&mut self, s: String) {
        self.actions.push(s);
    }
}

/// Bytes held under `root`, counting each inode once. The day links in the
/// log root are hard links to tier files, so a naive sum would report the
/// cascade twice.
fn tree_bytes(root: &Path) -> u64 {
    fn walk(dir: &Path, seen: &mut std::collections::HashSet<u64>, total: &mut u64) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                walk(&e.path(), seen, total);
            } else if m.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if !seen.insert(m.ino()) {
                        continue;
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = &seen;
                }
                *total += m.len();
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    walk(root, &mut seen, &mut total);
    total
}

// ─── compaction steps ────────────────────────────────────────────────────
/// Newest bucket start in a tier file, or `None`. Reads the tail only: these
/// files reach a few MB and this runs on every raw-prune candidate.
fn newest_bucket(path: &Path) -> Option<i64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let back = len.min(16 * 1024);
    f.seek(SeekFrom::Start(len - back)).ok()?;
    let mut buf = String::new();
    f.take(back).read_to_string(&mut buf).ok()?;
    for line in buf.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(a) = v.get("agg").and_then(|a| a.get("start")).and_then(|s| s.as_str()) {
            if let Some(e) = parse_utc(a) {
                return Some(e);
            }
        }
    }
    None
}

/// Does the 15 s tier already hold `upto`? The hot path writes those buckets on
/// the tick, so normally yes and a raw file can just be deleted. A raw file with
/// no equal-or-newer bucket behind it gets compacted first — that is what makes
/// "older raw lines are compacted, not deleted abruptly" true on the very first
/// pass over a pre-T18 file.
fn tier_covers(root: &Path, newest: i64) -> bool {
    let day = day_of(newest);
    let p = tier_dir(root, TIER_15S).join(format!("{}.jsonl", day));
    newest_bucket(&p)
        .map(|u| u + TIER_15S as i64 >= newest)
        .unwrap_or(false)
}

/// Split every file in `src` at `cutoff`, move the old buckets into `dst`.
fn demote(
    root: &Path,
    src_tier: u64,
    dst_tier: u64,
    cutoff: i64,
    rep: &mut Report,
) -> std::io::Result<()> {
    let src = tier_dir(root, src_tier);
    let dst = tier_dir(root, dst_tier);
    let mut touched_days: BTreeMap<String, ()> = BTreeMap::new();

    for path in jsonl_files(&src) {
        let buckets = read_buckets(&path);
        if buckets.is_empty() {
            continue;
        }
        // Never rewrite the file the running collector is appending to. At
        // most a few buckets of today's 15 s data then live a day longer.
        if path.file_stem().and_then(|s| s.to_str()) == Some(day_of(now_epoch()).as_str()) {
            continue;
        }
        let (mut old, keep): (Vec<Bucket>, Vec<Bucket>) =
            buckets.into_iter().partition(|b| b.start < cutoff);
        if old.is_empty() {
            continue;
        }
        // Retier so `agg.tier` (and therefore how much spread is written)
        // matches where the bucket actually lives.
        for b in old.iter_mut() {
            b.tier = dst_tier;
        }
        let moved = old.len() as u64;
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown.jsonl")
            .to_string();
        rep.buckets_moved += moved;
        rep.note(format!(
            "{} {} -> tier{}s: {} bucket(s){}",
            if rep.dry_run { "would demote" } else { "demote" },
            name,
            dst_tier,
            moved,
            if keep.is_empty() { ", delete source" } else { ", rewrite remainder" }
        ));
        if rep.dry_run {
            continue;
        }

        // Group by the day each bucket belongs to so a day file never leaks
        // into a neighbour.
        let mut by_day: BTreeMap<String, Vec<Bucket>> = BTreeMap::new();
        for b in old {
            by_day.entry(day_of(b.start)).or_default().push(b);
        }
        for (day, list) in by_day {
            let fname = format!("{}.jsonl", day);
            merge_into_file(&dst, &fname, list)?;
            touched_days.insert(day, ());
        }

        // Only now is the target on disk and fsync'd: the source may go.
        if keep.is_empty() {
            fs::remove_file(&path)?;
        } else {
            let lines: Vec<String> = keep
                .iter()
                .map(|b| serde_json::to_string(&b.to_json()).unwrap_or_default())
                .filter(|s| !s.is_empty())
                .collect();
            write_atomic(&src, &name, &lines)?;
        }
    }

    if !rep.dry_run {
        for day in touched_days.keys() {
            refresh_day_link(root, day, rep);
        }
    }
    Ok(())
}

fn refresh_day_link(root: &Path, day: &str, rep: &mut Report) {
    let Some(target) = finest_day_file(root, day) else {
        return;
    };
    let link = root.join(format!("{}.jsonl", day));
    if day_link_is(&link, &target) {
        return;
    }
    match link_day(root, &target, day) {
        Ok(()) => rep.note(format!(
            "relink {}.jsonl -> {}",
            day,
            target.strip_prefix(root).unwrap_or(&target).display()
        )),
        Err(e) => rep.note(format!("LINK FAILED {}: {}", day, e)),
    }
}

/// Fold the oldest tier into one record per day in `archive/`.
fn archive_old(root: &Path, cutoff: i64, enabled: bool, rep: &mut Report) -> std::io::Result<()> {
    let src = tier_dir(root, TIER_1800S);
    let mut touched: BTreeMap<String, ()> = BTreeMap::new();
    for path in jsonl_files(&src) {
        let Some(newest) = newest_bucket(&path) else {
            continue;
        };
        if newest >= cutoff {
            continue;
        }
        let buckets = read_buckets(&path);
        if buckets.is_empty() {
            continue;
        }
        let day = day_of(buckets[0].start);
        rep.note(format!(
            "{} {} -> archive/{}.jsonl ({} bucket(s) -> 1 record){}",
            if rep.dry_run { "would fold" } else { "fold" },
            path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
            day,
            buckets.len(),
            if enabled { "" } else { " [archive=false: divide by 10 instead]" }
        ));
        if rep.dry_run {
            continue;
        }
        // `archive = false` keeps the coarse tier but thins it to 1/day so the
        // footprint bound still holds; `true` writes the archive tail.
        let mut acc = Bucket::new(TIER_1800S, buckets[0].start);
        for b in &buckets {
            acc.merge(b);
        }
        acc.tier = TIER_1800S;
        acc.start = buckets[0].start;
        let line = serde_json::to_string(&acc.to_json()).unwrap_or_default();
        if enabled {
            write_atomic(&archive_dir(root), &format!("{}.jsonl", day), &[line])?;
        }
        fs::remove_file(&path)?;
        touched.insert(day, ());
    }
    if !rep.dry_run {
        for day in touched.keys() {
            refresh_day_link(root, day, rep);
        }
    }
    Ok(())
}

/// Enforce the raw window: drop hourly raw files past the age cap or the byte
/// cap — but compact any line the 15 s tier does not already cover first, so
/// "older raw lines are compacted, not deleted abruptly" holds even on the
/// very first pass over a pre-T18 file.
fn prune_raw(root: &Path, cfg: &StorageConfig, now: i64, rep: &mut Report) -> std::io::Result<()> {
    let files = raw_files_by_age(&raw_dir(root));
    if files.is_empty() {
        return Ok(());
    }
    let age_cutoff = now - (cfg.raw_hours as i64) * 3600;
    let mut total: u64 = files.iter().map(|(_, _, sz)| *sz).sum();

    for (newest, path, size) in files {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        // Never pull the slot the running collector has open.
        if stem == raw_slot(now) {
            continue;
        }
        let slot_end = newest + 1;
        let over_age = slot_end < age_cutoff;
        let over_bytes = total > cfg.raw_max_bytes;
        if !over_age && !over_bytes {
            continue;
        }
        let day = day_of(newest);
        // Does the 15 s tier already hold this window? If not, bucket the raw
        // lines into it before the file goes away.
        let needs_compaction = !tier_covers(root, newest);

        rep.note(format!(
            "{} raw/{} ({:.1} MB, {} by {}){}",
            if rep.dry_run { "would prune" } else { "prune" },
            path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
            size as f64 / (1024.0 * 1024.0),
            if over_age { "age" } else { "bytes" },
            if over_age {
                format!("{}h > {}h", (now - slot_end) / 3600, cfg.raw_hours)
            } else {
                format!("{} MB > {} MB", total / (1024 * 1024), cfg.raw_max_bytes / (1024 * 1024))
            },
            if needs_compaction { ", compacting into tier15s first" } else { "" }
        ));
        if rep.dry_run {
            total = total.saturating_sub(size);
            continue;
        }

        if needs_compaction {
            compact_raw_into_15s(root, &path)?;
            refresh_day_link(root, &day, rep);
        }
        fs::remove_file(&path)?;
        total = total.saturating_sub(size);
    }
    Ok(())
}

/// Bucket a raw 1 s file into the 15 s tier. Used only when the tier does not
/// already cover the file's window (first run, or a pre-T18 file).
fn compact_raw_into_15s(root: &Path, path: &Path) -> std::io::Result<()> {
    let file = fs::File::open(path)?;
    let mut by_day: BTreeMap<String, Vec<Bucket>> = BTreeMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(line) else {
            continue;
        };
        let Some(t) = parse_utc(&snap.timestamp) else {
            continue;
        };
        let start = floor_to(t, TIER_15S);
        let day = day_of(start);
        let list = by_day.entry(day).or_default();
        match list.iter_mut().find(|b| b.start == start) {
            Some(b) => b.add(&snap),
            None => {
                let mut b = Bucket::new(TIER_15S, start);
                b.add(&snap);
                list.push(b);
            }
        }
    }
    for (day, list) in by_day {
        merge_into_file(&tier_dir(root, TIER_15S), &format!("{}.jsonl", day), list)?;
    }
    Ok(())
}

/// One-time migration of pre-T18 `logs/YYYY-MM-DD.jsonl` files, which are raw
/// 1 s lines sitting in the log root. Recent lines move to `raw/`, older lines
/// are bucketed into `tier15s` and then walked down the cascade by the same
/// pass. The legacy file is removed only after both writes are fsync'd; the day
/// name is then re-pointed at the resulting tier file.
fn migrate_legacy(root: &Path, cfg: &StorageConfig, now: i64, rep: &mut Report) -> std::io::Result<()> {
    let raw_cutoff = now - (cfg.raw_hours as i64) * 3600;
    for path in jsonl_files(root) {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue; // a day link to a tier file
        }
        // A day link is a hard link to a tier file, i.e. a regular file, so it
        // shows up here too. Peek at the first record: anything carrying the
        // `agg` block is already compacted and must be left alone (reading a
        // 6 MB tier file on every startup just to learn that is wasteful).
        {
            use std::io::Read;
            let mut head = [0u8; 2048];
            let mut f = match fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let n = f.read(&mut head).unwrap_or(0);
            let head = String::from_utf8_lossy(&head[..n]);
            if head.contains("\"agg\"") {
                continue;
            }
        }
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        let mut recent: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut old: BTreeMap<String, Vec<Bucket>> = BTreeMap::new();
        let mut saw_raw = false;
        let mut bad = 0u64;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
                bad += 1;
                continue;
            };
            if v.get("agg").is_some() {
                continue; // already compacted
            }
            let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) else {
                bad += 1;
                continue;
            };
            let Some(t) = parse_utc(ts) else {
                bad += 1;
                continue;
            };
            saw_raw = true;
            let Ok(snap) = serde_json::from_value::<Snapshot>(v) else {
                bad += 1;
                continue;
            };
            // Every sample lands in the 15 s tier: that file is what the log
            // root's day link points at, so it is the dashboard's only source
            // while the sample is still inside the raw window. Without this a
            // freshly migrated day would be invisible to /api/graph until it
            // aged out of `raw/`.
            let start = floor_to(t, TIER_15S);
            let list = old.entry(day_of(start)).or_default();
            match list.iter_mut().find(|b| b.start == start) {
                Some(b) => b.add(&snap),
                None => {
                    let mut b = Bucket::new(TIER_15S, start);
                    b.add(&snap);
                    list.push(b);
                }
            }
            if t >= raw_cutoff {
                recent
                    .entry(raw_slot(t))
                    .or_default()
                    .push(trimmed.to_string());
            }
        }
        if !saw_raw {
            continue;
        }
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?").to_string();
        let n_recent: usize = recent.values().map(|v| v.len()).sum();
        let n_old: usize = old.values().map(|v| v.len()).sum();
        rep.note(format!(
            "{} legacy {} ({:.1} MB): {} sample(s) -> raw/ (last {}h), {} 15s bucket(s) -> tier15s{}",
            if rep.dry_run { "would migrate" } else { "migrate" },
            name,
            meta.len() as f64 / (1024.0 * 1024.0),
            n_recent,
            cfg.raw_hours,
            n_old,
            if bad > 0 { format!(", {} unreadable line(s) skipped", bad) } else { String::new() }
        ));
        if rep.dry_run {
            continue;
        }

        for (hour, lines) in recent {
            let p = raw_dir(root).join(format!("{}.jsonl", hour));
            let mut f = fs::OpenOptions::new().create(true).append(true).open(&p)?;
            for l in lines {
                f.write_all(l.as_bytes())?;
                f.write_all(b"\n")?;
            }
            f.sync_all()?;
        }
        for (day, list) in old {
            merge_into_file(&tier_dir(root, TIER_15S), &format!("{}.jsonl", day), list)?;
        }
        // Both sinks are fsync'd; the legacy file can go.
        fs::remove_file(&path)?;
        let day = name.trim_end_matches(".jsonl").to_string();
        let target = finest_day_file(root, &day);
        if let Some(t) = target {
            let _ = link_day(root, &t, &day);
        }
    }
    Ok(())
}

/// Walk every day that exists in a tier and make sure the log root has a link
/// for it. Cheap (a handful of files) and idempotent.
fn refresh_all_links(root: &Path, rep: &mut Report) {
    let mut days: BTreeMap<String, ()> = BTreeMap::new();
    for t in [TIER_15S, TIER_60S, TIER_300S, TIER_1800S] {
        for p in jsonl_files(&tier_dir(root, t)) {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                days.insert(stem.to_string(), ());
            }
        }
    }
    for p in jsonl_files(&archive_dir(root)) {
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            days.insert(stem.to_string(), ());
        }
    }
    for day in days.keys() {
        refresh_day_link(root, day, rep);
    }
}

/// One full compaction pass.
pub fn compact(root: &Path, cfg: &StorageConfig, dry_run: bool) -> std::io::Result<Report> {
    let mut rep = Report {
        dry_run,
        ..Default::default()
    };
    let now = now_epoch();
    rep.bytes_before = tree_bytes(root);

    if !dry_run {
        ensure_dirs(root)?;
        flush_hot(); // the cascade reads what the hot path wrote
    }

    migrate_legacy(root, cfg, now, &mut rep)?;
    prune_raw(root, cfg, now, &mut rep)?;
    demote(root, TIER_15S, TIER_60S, now - KEEP_15S, &mut rep)?;
    demote(root, TIER_60S, TIER_300S, now - KEEP_60S, &mut rep)?;
    demote(root, TIER_300S, TIER_1800S, now - KEEP_300S, &mut rep)?;
    archive_old(root, now - KEEP_1800S, cfg.archive, &mut rep)?;
    if !dry_run {
        refresh_all_links(root, &mut rep);
        rep.bytes_after = tree_bytes(root);
    } else {
        rep.bytes_after = rep.bytes_before;
    }
    Ok(rep)
}

/// Startup pass: create the layout, compact, then start the hot path.
pub fn init(root: &Path, cfg: &StorageConfig) -> std::io::Result<()> {
    ensure_dirs(root)?;
    let _ = compact(root, cfg, false);
    let mut g = HOT.lock().unwrap();
    *g = Some(HotState::new(cfg.clone(), root.to_path_buf()));
    Ok(())
}

// ─── hot path ────────────────────────────────────────────────────────────
struct HotState {
    cfg: StorageConfig,
    root: PathBuf,
    acc: Option<Bucket>,
    raw: Option<(PathBuf, BufWriter<fs::File>)>,
    tier: Option<(PathBuf, BufWriter<fs::File>)>,
    link_cache: Option<(String, PathBuf)>,
}

static HOT: Mutex<Option<HotState>> = Mutex::new(None);

impl HotState {
    fn new(cfg: StorageConfig, root: PathBuf) -> Self {
        Self {
            cfg,
            root,
            acc: None,
            raw: None,
            tier: None,
            link_cache: None,
        }
    }

    fn write_raw(&mut self, snap: &Snapshot, t: i64) {
        let path = raw_dir(&self.root).join(format!("{}.jsonl", raw_slot(t)));
        if self.raw.as_ref().map(|(p, _)| p != &path).unwrap_or(true) {
            self.flush_raw();
            if let Ok(f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                self.raw = Some((path.clone(), BufWriter::with_capacity(16 * 1024, f)));
            }
            self.enforce_raw_budget();
        }
        if let Some((_, w)) = self.raw.as_mut() {
            if serde_json::to_writer(&mut *w, snap).is_err() {
                self.raw = None;
                return;
            }
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }

    fn flush_raw(&mut self) {
        if let Some((_, mut w)) = self.raw.take() {
            let _ = w.flush();
            if let Ok(f) = w.into_inner() {
                let _ = f.sync_all();
            }
        }
    }

    /// Keep the raw window inside `raw_max_bytes` by whole hourly files. Done
    /// at rotation time, so it costs nothing on the 1 Hz path.
    fn enforce_raw_budget(&mut self) {
        let mut files = raw_files_by_age(&raw_dir(&self.root));
        if files.len() < 2 {
            return;
        }
        let mut total: u64 = files.iter().map(|(_, _, sz)| *sz).sum();
        while total > self.cfg.raw_max_bytes && files.len() > 1 {
            let (_, victim, size) = files.remove(0);
            // Never drop the file we are currently appending to.
            if self.raw.as_ref().map(|(p, _)| p == &victim).unwrap_or(false) {
                files.insert(0, (0, victim, size));
                break;
            }
            // Compact before delete only if the 15 s tier does not cover the
            // slot. The hot path normally already wrote those buckets, and
            // re-bucketing them here would merge each record with itself.
            let covered = newest_raw_ts(&victim)
                .map(|newest| tier_covers(&self.root, newest))
                .unwrap_or(false);
            if !covered {
                let _ = compact_raw_into_15s(&self.root, &victim);
            }
            if fs::remove_file(&victim).is_ok() {
                total = total.saturating_sub(size);
            } else {
                break;
            }
        }
    }

    fn flush_acc(&mut self) {
        let Some(b) = self.acc.take() else { return };
        if b.n == 0 {
            return;
        }
        let day = day_of(b.start);
        let path = tier_dir(&self.root, TIER_15S).join(format!("{}.jsonl", day));
        if self.tier.as_ref().map(|(p, _)| p != &path).unwrap_or(true) {
            if let Some((_, mut w)) = self.tier.take() {
                let _ = w.flush();
                if let Ok(f) = w.into_inner() {
                    let _ = f.sync_all();
                }
            }
            let _ = fs::create_dir_all(tier_dir(&self.root, TIER_15S));
            if let Ok(f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                self.tier = Some((path.clone(), BufWriter::with_capacity(16 * 1024, f)));
            }
        }
        if let Some((_, w)) = self.tier.as_mut() {
            let line = serde_json::to_string(&b.to_json()).unwrap_or_default();
            if !line.is_empty() {
                let _ = w.write_all(line.as_bytes());
                let _ = w.write_all(b"\n");
                let _ = w.flush();
            }
        }
        // Keep the day name in the log root pointing at this file.
        let fresh = match &self.link_cache {
            Some((d, p)) => d != &day || p != &path,
            None => true,
        };
        if fresh {
            let link = self.root.join(format!("{}.jsonl", day));
            if !day_link_is(&link, &path) {
                let _ = link_day(&self.root, &path, &day);
            }
            self.link_cache = Some((day, path));
        }
    }
}

/// Fold one sample into the on-disk cascade. Called once per tick instead of
/// `logging::append` when `storage.cascade` is on.
pub fn observe(snap: &Snapshot) {
    let mut guard = match HOT.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let Some(h) = guard.as_mut() else { return };
    let t = parse_utc(&snap.timestamp).unwrap_or_else(now_epoch);

    h.write_raw(snap, t);

    let start = floor_to(t, TIER_15S);
    let rotate = h.acc.as_ref().map(|b| b.start != start).unwrap_or(false);
    if rotate {
        h.flush_acc();
    }
    if h.acc.is_none() {
        h.acc = Some(Bucket::new(TIER_15S, start));
    }
    if let Some(b) = h.acc.as_mut() {
        b.add(snap);
    }
}

/// Flush + fsync the open writers (graceful shutdown, and before the compactor
/// reads what the hot path wrote).
pub fn flush_hot() {
    let mut guard = match HOT.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(h) = guard.as_mut() {
        h.flush_acc();
        h.flush_raw();
    }
}

// ─── formatting for `--compact` ──────────────────────────────────────────
pub fn format_report(rep: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "syswatch storage cascade {} — {} action(s), {} bucket(s) demoted\n",
        if rep.dry_run { "(dry run)" } else { "(applied)" },
        rep.actions.len(),
        rep.buckets_moved
    ));
    for a in rep.actions.iter().take(20) {
        out.push_str(&format!("  {}\n", a));
    }
    if rep.actions.len() > 20 {
        out.push_str(&format!("  … {} more\n", rep.actions.len() - 20));
    }
    out.push_str(&format!(
        "  bytes: {} -> {} ({:.1} MB -> {:.1} MB)\n",
        rep.bytes_before,
        rep.bytes_after,
        rep.bytes_before as f64 / (1024.0 * 1024.0),
        rep.bytes_after as f64 / (1024.0 * 1024.0)
    ));
    out.push_str(&format!(
        "  buckets moved: {}{}\n",
        rep.buckets_moved,
        if rep.actions.is_empty() { "  (nothing to do — already compacted)" } else { "" }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: &str, cpu: f32) -> Snapshot {
        use crate::stats::{Cpu, Disk, Memory, Net};
        Snapshot {
            timestamp: ts.to_string(),
            uptime_secs: 1000,
            cpu: Cpu {
                total_pct: cpu,
                per_core_pct: vec![cpu],
                freq_mhz: 2400,
            },
            memory: Memory {
                used_bytes: 8_000_000_000,
                total_bytes: 16_000_000_000,
                pct: 50.0,
                swap_used_bytes: 0,
                swap_total_bytes: 0,
            },
            temperature: Some(50.0),
            disks: vec![Disk {
                mount: "/".into(),
                fs: "apfs".into(),
                used_pct: 58.0,
                free_gb: 95.0,
                total_gb: 228.0,
            }],
            networks: vec![Net {
                name: "en0".into(),
                rx_bps: 100.0,
                tx_bps: 200.0,
            }],
            processes: Default::default(),
            energy: Default::default(),
            process_count: 400,
        }
    }

    /// The whole contract with `ui.rs` and `dashboard/server.py` is that a
    /// compact line still deserialises as `Snapshot`.
    #[test]
    fn compact_line_round_trips_as_snapshot() {
        let mut b = Bucket::new(TIER_15S, 1_763_000_000);
        b.add(&sample("2026-11-19T09:43:15Z", 10.0));
        b.add(&sample("2026-11-19T09:43:16Z", 30.0));
        let line = serde_json::to_string(&b.to_json()).unwrap();
        let snap: Snapshot = serde_json::from_str(&line).expect("compact line must parse");
        assert!((snap.cpu.total_pct - 20.0).abs() < 1e-3);
        assert!((snap.memory.pct - 50.0).abs() < 1e-6);
        assert_eq!(snap.disks.len(), 1);
        assert_eq!(snap.networks[0].name, "en0");
        assert_eq!(snap.process_count, 400);
        assert_eq!(snap.processes.by_cpu.len(), 0); // omitted on disk
        assert!((snap.temperature.unwrap() - 50.0).abs() < 1e-3);

        // And it parses back into a bucket, so compaction is chainable.
        let v: Value = serde_json::from_str(&line).unwrap();
        let back = Bucket::from_json(&v).expect("bucket must parse");
        assert_eq!(back.tier, TIER_15S);
        assert_eq!(back.start, 1_763_000_000);
        assert_eq!(back.n, 2);
        assert_eq!(back.cpu_total.n, 2);
    }

    #[test]
    fn utc_roundtrip() {
        for s in [0i64, 1_700_000_000, 1_763_000_000, 1_892_000_000] {
            let f = fmt_utc(s);
            assert_eq!(parse_utc(&f), Some(s), "{}", f);
        }
        assert_eq!(parse_utc("2026-11-19T09:43:15Z"), parse_utc("2026-11-19T11:43:15+02:00"));
        assert_eq!(parse_utc("junk"), None);
    }

    #[test]
    fn stat_merge_is_weighted() {
        let mut a = Stat::default();
        a.add(10.0);
        a.add(20.0);
        let mut b = Stat::default();
        for _ in 0..6 {
            b.add(40.0);
        }
        let mut m = a;
        m.merge(&b);
        assert_eq!(m.n, 8);
        assert!((m.mean() - 33.75).abs() < 1e-9); // (10+20+6*40)/8
        assert_eq!(m.min, 10.0);
        assert_eq!(m.max, 40.0);
    }

    #[test]
    fn bucket_merge_matches_flat_mean() {
        let mut x = Bucket::new(TIER_15S, 1000);
        x.n = 3;
        x.cpu_total.add(10.0);
        x.cpu_total.add(20.0);
        x.cpu_total.add(30.0);
        let mut y = Bucket::new(TIER_15S, 1015);
        y.n = 15;
        y.cpu_total.add(50.0);
        let mut merged = x.clone();
        merged.merge(&y);
        assert_eq!(merged.n, 18);
        // Each statistic carries its own n, so the merge is exact over the
        // underlying samples (4 of them here), not a mean of means.
        assert_eq!(merged.cpu_total.n, 4);
        assert!((merged.cpu_total.mean() - 27.5).abs() < 1e-9); // (10+20+30+50)/4

        // 4 x 15 s buckets of 15 samples each must average to the flat mean.
        let mut flat = 0.0;
        let mut z = Bucket::new(TIER_15S, 1000);
        for k in 0..4 {
            let mut q = Bucket::new(TIER_15S, 1000 + 15 * k);
            q.n = 15;
            for i in 0..15 {
                let v = (k * 15 + i) as f64;
                flat += v;
                q.cpu_total.add(v);
            }
            z.merge(&q);
        }
        assert_eq!(z.cpu_total.n, 60);
        assert!((z.cpu_total.mean() - flat / 60.0).abs() < 1e-9);
    }
}
