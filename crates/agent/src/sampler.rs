//! The 1 s sampler behind `/api/current`.
//!
//! # Where each number comes from
//!
//! | block            | source                                                    |
//! |------------------|-----------------------------------------------------------|
//! | cpu, ram, net    | `sysinfo` in this process (deltas over the tick)           |
//! | disk (capacity)  | `sysinfo` `Disks` (statfs), `/System/Volumes/Data` preferred |
//! | disk_io (B/s)    | the same `Disks` IOKit byte counters, differentiated       |
//! | gpu              | `ioreg -r -d 1 -w0 -c IOAccelerator` (5 s cache)            |
//! | battery          | `syswatch_sensors_macos::battery` (20 s cache, IORegistry)  |
//! | power, temps     | the root daemon's `sample.json` via `rootsample::read()`     |
//! | processes        | `sysinfo` process list (60 s cadence, like the Python server)|
//! | collector        | the newest collector JSONL record (`collector.rs`)          |
//!
//! # Why a dedicated thread instead of a tokio task
//!
//! Every source above except `sysinfo` is a subprocess (`ioreg`) or a file
//! read, and `sysinfo`'s process scan wants a real 200 ms window between two
//! refreshes. A plain OS thread with `sleep` keeps all of that off the async
//! runtime and away from the HTTP handlers, which only ever read the cached
//! snapshot. Nothing here can make a request wait.
//!
//! # Honesty rules
//!
//! A value that could not be read is `null` with a reason, never `0`. That is
//! the same rule the Python server, the collector and the sensor crates follow.
//!
//! # Throttles
//!
//! `DISK_IO_SECS`, `BATTERY_SECS`, `PROC_SCAN_SECS` and the GPU's own 5 s cache
//! exist because the underlying calls are expensive (an `ioreg` spawn, a 400-PID
//! sweep). They are measured in, not decorative: see the collector's
//! `last_disk_refresh` / `ENERGY_REFRESH` / `last_sweep` for the same argument.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sysinfo::{DiskRefreshKind, Disks, Networks, ProcessesToUpdate, System};
use syswatch_sensors_macos::{battery, memory, rootsample};

use crate::collector::CollectorLog;
use crate::gpu::GpuProbe;
use crate::timefmt;

/// Process list cadence. `sysinfo`'s scan over ~450 PIDs is the single most
/// expensive call available here; the Python server settled on 60 s
/// (`Sampler.PROC_SCAN_SECS`) after measuring it, and this mirrors that.
pub const PROC_SCAN_SECS: u64 = 60;
/// Disk byte-counter cadence (IOKit walk). The sensor crate uses the same 5 s.
pub const DISK_IO_SECS: u64 = 5;
/// Battery gauge cadence: the gauge itself updates every ~30 s.
pub const BATTERY_SECS: u64 = 20;
const TOP_N: usize = 5;
/// Mirrors `Sampler._net_rates` in the Python server: these pseudo interfaces
/// always read 0 B/s and crowd real rows out of a top-5 list.
const NET_SKIP_PREFIX: &[&str] = &[
    "lo", "loopback", "gif", "stf", "anpi", "awdl", "llw", "ap1", "bridge", "p2p", "xhc", "vnic",
    "ipsec",
];

/// Is this interface name a pseudo interface that never carries traffic?
///
/// Shared with `histdata` so a bandwidth chart built from collector records
/// agrees with `/api/current`'s hero number (both exclude loopback, awdl and
/// friends).
pub(crate) fn is_pseudo_iface(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    NET_SKIP_PREFIX.iter().any(|p| low.starts_with(p))
}
const CLUSTER_BASIS: &str = "hw.perflevel1.logicalcpu efficiency cores occupy logical CPUs 0..E-1; \
                             hw.perflevel0.logicalcpu performance cores follow — verified with a \
                             single-thread load (spikes at indices 4-6)";

#[derive(Debug, Clone)]
pub struct Cluster {
    pub index: usize,
    pub name: String,
    pub cores: usize,
}

pub struct Sampler {
    sys: System,
    procs: System,
    nets: Networks,
    disks: Disks,
    prev_net: HashMap<String, (u64, u64)>,
    prev_net_at: Option<Instant>,
    prev_disk_io: HashMap<String, (u64, u64)>,
    last_disk_io: Option<Instant>,
    disk_io: (Option<f64>, Option<f64>, Option<f64>),
    battery: Option<(Instant, Option<battery::Battery>)>,
    battery_reads: u64,
    gpu: GpuProbe,
    collector: CollectorLog,
    clusters: Vec<Cluster>,
    boot_ts: Option<String>,
    ncpu: usize,
    proc_rows: (Vec<Value>, Vec<Value>, usize),
    proc_scans: u64,
    last_proc_scan: Option<Instant>,
    ticks: u64,
    cascade: Option<crate::cascade_write::CascadeWriter>,
}

impl Sampler {
    pub fn new(logs_dir: PathBuf) -> Self {
        // CPU deltas need two reads: prime here so the first snapshot is a real
        // measurement instead of 0 %.
        let mut sys = System::new();
        sys.refresh_cpu_all();
        sys.refresh_memory();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sys.refresh_cpu_all();

        let ncpu = sys.cpus().len().max(1);
        let disks = Disks::new_with_refreshed_list_specifics(
            DiskRefreshKind::nothing().with_storage(),
        );
        // Process CPU is a delta between two scans of the same System: one
        // refresh is a baseline, the second is still 0 % (measured on this
        // machine), the third reports real numbers. Prime both baselines here
        // so the very first snapshot is not a wall of zeroes.
        let mut procs = System::new();
        procs.refresh_processes(ProcessesToUpdate::All, true);
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        procs.refresh_processes(ProcessesToUpdate::All, true);

        let nets = Networks::new_with_refreshed_list();
        let prev_net = nets
            .iter()
            .map(|(n, d)| (n.clone(), (d.total_received(), d.total_transmitted())))
            .collect();

        Self {
            sys,
            procs,
            nets,
            disks,
            prev_net,
            prev_net_at: Some(Instant::now()),
            prev_disk_io: HashMap::new(),
            last_disk_io: None,
            disk_io: (None, None, None),
            battery: None,
            battery_reads: 0,
            gpu: GpuProbe::new(),
            collector: CollectorLog::new(logs_dir),
            clusters: read_clusters(ncpu),
            boot_ts: Some(timefmt::rfc3339_utc(System::boot_time() as i64)),
            ncpu,
            proc_rows: (Vec::new(), Vec::new(), 0),
            proc_scans: 0,
            last_proc_scan: None,
            ticks: 0,
            cascade: None,
        }
    }

    /// Write each tick through the disk cascade as well (T18, opt-in via
    /// `--cascade-dir`). Without this the agent stays read-only.
    pub fn with_cascade(mut self, writer: crate::cascade_write::CascadeWriter) -> Self {
        self.cascade = Some(writer);
        self
    }

    pub fn clusters(&self) -> &[Cluster] {
        &self.clusters
    }

    pub fn ncpu(&self) -> usize {
        self.ncpu
    }

    /// Build one snapshot. Called on the sampler thread; never on a request.
    pub fn build(&mut self) -> Value {
        self.ticks += 1;
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();

        let per_core: Vec<f64> = self
            .sys
            .cpus()
            .iter()
            .map(|c| round1(c.cpu_usage() as f64))
            .collect();
        let total_pct = round1(self.sys.global_cpu_usage() as f64);
        // macOS publishes no package clock through sysinfo on Apple Silicon;
        // a 0 there means "not reported", not "0 MHz".
        let freq_mhz = self
            .sys
            .cpus()
            .first()
            .map(|c| c.frequency())
            .filter(|f| *f > 0)
            .map(|f| f as f64);
        let (l1, l5, l15) = load_averages();

        let total_mem = self.sys.total_memory();
        let used_mem = self.sys.used_memory();
        let ram_pct = if total_mem > 0 {
            round1(used_mem as f64 / total_mem as f64 * 100.0)
        } else {
            0.0
        };

        let net = self.net_block();
        let disk = self.disk_block();
        let disk_io = self.disk_io_block();
        let gpu = self.gpu.sample();
        let bat = self.battery_block();
        let root = rootsample::read();
        let temps = temps_block(root.as_ref());
        let (by_cpu, by_mem, proc_count) = self.process_block();
        let collector = self.collector.latest();

        let load = json!({"one": l1, "five": l5, "fifteen": l15});
        let cpu = json!({
            "total_pct": total_pct,
            "per_core_pct": per_core,
            "freq_mhz": freq_mhz,
            "cores": self.sys.cpus().len(),
            "clusters": cluster_payload(&self.clusters, &per_core, freq_mhz),
            "cluster_basis": CLUSTER_BASIS,
            "load": load.clone(),
        });
        // `used` and `pct` are the same quantity here (pct = used / total).
        // psutil's macOS payload is not self-consistent: its `used_bytes` is
        // "total − free" while its `pct` is "total − available" (78 % against
        // 3.7 GB of 8.6 GB). The agent reports one basis and names the other
        // as `available_bytes`, so a client can pick either.
        let ram = json!({
            "used_bytes": used_mem,
            "total_bytes": total_mem,
            "available_bytes": total_mem.saturating_sub(used_mem),
            "pct": ram_pct,
            "swap_used_bytes": self.sys.used_swap(),
            "swap_total_bytes": self.sys.total_swap(),
        });
        let power = power_block(root.as_ref(), &gpu);
        let pressure = pressure_block(load.clone(), total_pct, self.ncpu, ram_pct, &disk);
        let disk_used_pct = disk.get("used_pct").and_then(Value::as_f64);

        // Key order follows dashboard/server.py's /api/current so a diff of the
        // two payloads is a diff of values.
        let mut snap = json!({
            "ts": timefmt::rfc3339_utc(timefmt::now_unix()),
            "source": "syswatch-agent (rust)",
            "cpu": cpu,
            "ram": ram,
            "net": net,
            "temps_c": temps,
            "processes": {"by_cpu": by_cpu, "by_mem": by_mem},
            "proc_count": proc_count,
            "boot_ts": self.boot_ts,
            "load": load,
            "battery": bat,
            "gpu": gpu,
            "disk_io": disk_io,
            "disk": disk,
            "pressure": pressure,
        });
        // legacy alias: alert rules in the Python server read "memory".
        snap["memory"] = snap["ram"].clone();
        // Extras the agent can answer that the psutil server could not: the
        // privileged daemon's package power / thermal pressure, and what the
        // collector log contributed.
        snap["power"] = power;
        snap["unavailable"] = unavailable(root.is_some(), &snap);
        snap["collector"] = collector_block(collector.as_ref());
        snap["agent"] = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "ticks": self.ticks,
            "uptime_s": System::uptime(),
            "disk_used_pct": disk_used_pct,
            "boot_ts_source": "sysinfo System::boot_time() — the machine's real boot instant. The Python server's `BOOT_TS` is its own start time despite the key name; no client reads `boot_ts` (checked app.js), so this key reports the truth.",
        });
        // T18: optional disk cascade. Nothing is written unless the agent was
        // started with `--cascade-dir`.
        if let Some(writer) = self.cascade.as_mut() {
            writer.record(&snap);
        }
        snap
    }

    // ── net ────────────────────────────────────────────────────────────────
    fn net_block(&mut self) -> Value {
        self.nets.refresh(false);
        let now = Instant::now();
        let elapsed = self
            .prev_net_at
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(0.0);

        let mut rates: Vec<(String, f64, f64)> = Vec::new();
        if elapsed >= 1.0 {
            for (name, data) in self.nets.iter() {
                let cur = (data.total_received(), data.total_transmitted());
                let Some(prev) = self.prev_net.get(name) else {
                    continue;
                };
                let low = name.to_ascii_lowercase();
                if NET_SKIP_PREFIX.iter().any(|p| low.starts_with(p)) {
                    continue;
                }
                let down = (cur.0.saturating_sub(prev.0)) as f64 / elapsed;
                let up = (cur.1.saturating_sub(prev.1)) as f64 / elapsed;
                rates.push((name.clone(), round1(down), round1(up)));
            }
        }
        rates.sort_by(|a, b| (b.1 + b.2).total_cmp(&(a.1 + a.2)));
        rates.truncate(TOP_N);

        self.prev_net = self
            .nets
            .iter()
            .map(|(n, d)| (n.clone(), (d.total_received(), d.total_transmitted())))
            .collect();
        self.prev_net_at = Some(now);

        let down: f64 = rates.iter().map(|r| r.1).sum();
        let up: f64 = rates.iter().map(|r| r.2).sum();
        json!({
            "ifaces": rates.iter().map(|(name, d, u)| json!({
                "name": name, "down_bps": d, "up_bps": u,
            })).collect::<Vec<_>>(),
            "down_bps": round1(down),
            "up_bps": round1(up),
        })
    }

    // ── disk capacity + volumes ────────────────────────────────────────────
    fn disk_block(&mut self) -> Value {
        self.disks.refresh_specifics(false, DiskRefreshKind::nothing().with_storage());

        let mut volumes = Vec::new();
        for disk in self.disks.iter().filter(|d| d.total_space() > 0) {
            let total = disk.total_space() as f64;
            let free = disk.available_space() as f64;
            volumes.push(json!({
                "mount": disk.mount_point().to_string_lossy(),
                "device": disk.name().to_string_lossy(),
                "used_pct": round2((total - free) / total * 100.0),
                "free_gb": round2(free / 1_073_741_824.0),
                "total_gb": round2(total / 1_073_741_824.0),
            }));
        }
        volumes.sort_by(|a, b| {
            a["mount"]
                .as_str()
                .unwrap_or("")
                .cmp(b["mount"].as_str().unwrap_or(""))
        });

        // The root filesystem is a read-only system volume (10 % used) while
        // user data lives on /System/Volumes/Data — reporting "/" would say
        // "10 % full" on a half-full disk. Same rule as the Python server.
        let wanted = if std::path::Path::new("/System/Volumes/Data").is_dir() {
            "/System/Volumes/Data"
        } else {
            "/"
        };
        let pick = self
            .disks
            .iter()
            .find(|d| d.mount_point() == std::path::Path::new(wanted))
            .or_else(|| self.disks.iter().find(|d| d.mount_point() == std::path::Path::new("/")));

        let Some(d) = pick else {
            return json!({
                "available": false,
                "reason": "no mounted volume reported by sysinfo",
                "used_pct": Value::Null,
                "volumes": volumes,
            });
        };
        let total = d.total_space() as f64;
        let free = d.available_space() as f64;
        json!({
            "available": true,
            "reason": Value::Null,
            "used_pct": round2((total - free) / total * 100.0),
            "free_gb": round2(free / 1_073_741_824.0),
            "total_gb": round2(total / 1_073_741_824.0),
            "mount": d.mount_point().to_string_lossy(),
            "free_bytes": free as u64,
            "total_bytes": total as u64,
            "source": "statfs via sysinfo Disks",
            "used_pct_basis": "(total − available) / total — the unprivileged-available basis, so used_pct matches the free_gb and total_gb beside it; psutil's percent excludes reserved blocks and reads a few points lower",
            "volumes": volumes,
        })
    }

    // ── disk throughput ────────────────────────────────────────────────────
    fn disk_io_block(&mut self) -> Value {
        let due = match self.last_disk_io {
            None => true,
            Some(t) => t.elapsed() >= Duration::from_secs(DISK_IO_SECS),
        };
        if due {
            let now = Instant::now();
            let elapsed = self
                .last_disk_io
                .map(|t| now.duration_since(t).as_secs_f64())
                .unwrap_or(0.0);
            self.disks.refresh_specifics(
                false,
                DiskRefreshKind::nothing().with_storage().with_io_usage(),
            );

            // Every APFS volume on one physical disk reports the same device
            // counters; dedupe before summing or the number doubles.
            let mut seen: HashSet<(u64, u64)> = HashSet::new();
            let mut cur: HashMap<String, (u64, u64)> = HashMap::new();
            for disk in self.disks.iter().filter(|d| d.total_space() > 0) {
                let usage = disk.usage();
                if !seen.insert((usage.total_read_bytes, usage.total_written_bytes)) {
                    continue;
                }
                let key = disk.mount_point().to_string_lossy().into_owned();
                cur.insert(key, (usage.total_read_bytes, usage.total_written_bytes));
            }

            if elapsed > 0.5 {
                let (mut read, mut write, mut counted) = (0.0f64, 0.0f64, 0usize);
                for (key, (r, w)) in &cur {
                    let Some((pr, pw)) = self.prev_disk_io.get(key) else {
                        continue;
                    };
                    // A counter that went backwards (drive reset) is a gap for
                    // that row, not a negative rate.
                    if r >= pr && w >= pw {
                        read += (r - pr) as f64 / elapsed;
                        write += (w - pw) as f64 / elapsed;
                        counted += 1;
                    }
                }
                if counted > 0 {
                    self.disk_io = (Some(round1(read)), Some(round1(write)), Some(elapsed));
                }
            }
            self.prev_disk_io = cur;
            self.last_disk_io = Some(now);
        }

        let (read, write, window) = self.disk_io;
        let available = read.is_some() || write.is_some();
        json!({
            "read_bps": read,
            "write_bps": write,
            "read_iops": json!(0.0),
            "write_iops": json!(0.0),
            "available": available,
            "reason": if available { Value::Null } else { json!(
                "disk counters primed — the first B/s delta needs two reads 5 s apart") },
            "iops_reason": "sysinfo exposes byte counters only; op counts would need a second ioreg read",
            "window_s": window,
            "source": "sysinfo Disks IOKit byte counters, differentiated",
            "sampled_ts": self.last_disk_io.map(|_| timefmt::now_unix() as f64),
        })
    }

    // ── battery ────────────────────────────────────────────────────────────
    fn battery_block(&mut self) -> Value {
        let due = match &self.battery {
            None => true,
            Some((at, _)) => at.elapsed() >= Duration::from_secs(BATTERY_SECS),
        };
        if due {
            let b = battery::read_battery();
            self.battery_reads += 1;
            self.battery = Some((Instant::now(), b));
        }
        let root_package_w = rootsample::read().and_then(|s| s.package_w);
        let ioreg_info = crate::ioreg::read();
        let Some(Some(b)) = self.battery.as_ref().map(|(_, b)| b.clone()) else {
            return json!({
                "pct": Value::Null,
                "time_remaining_s": Value::Null,
                "charging": Value::Null,
                "cycles": Value::Null,
                "condition": Value::Null,
                "health_pct": Value::Null,
                "cells": json!([]),
                "battery_w": root_package_w,
                "temp_c": Value::Null,
                "external_connected": Value::Null,
                "adapter_w": Value::Null,
                "adapter_source": Value::Null,
                "adapter_rated_w": Value::Null,
                "voltage_v": Value::Null,
                "amperage_ma": Value::Null,
                "nominal_wh": Value::Null,
                "source": "ioreg AppleSmartBattery",
                "unavailable_reason": "no AppleSmartBattery in the IORegistry (desktop Mac, or ioreg refused)",
            });
        };
        // On AC the gauge moves no current, so V × A reads 0 while the machine
        // is still drawing watts — the root daemon's package power is the real
        // draw there.
        let watts = b.draw_w;
        let mut missing: Vec<&str> = Vec::new();
        if b.charge_pct.is_none() {
            missing.push("charge");
        }
        if b.health_pct.is_none() {
            missing.push("health");
        }
        json!({
            "pct": b.charge_pct,
            "time_remaining_s": b.time_remaining_min.map(|m| (m * 60.0).round()),
            "charging": b.charging,
            "cycles": b.cycles,
            "condition": ioreg_info.as_ref().map(|i| i.condition()).unwrap_or("unavailable"),
            "health_pct": b.health_pct,
            "cells": json!([]),
            "battery_w": watts,
            "temp_c": b.temp_c,
            "external_connected": b.external_connected,
            "adapter_w": ioreg_info.as_ref().and_then(|i| i.adapter_w),
            "adapter_source": ioreg_info.as_ref().and_then(|i| i.adapter_source.clone()).unwrap_or_else(|| "none".to_string()),
            "adapter_rated_w": ioreg_info.as_ref().and_then(|i| i.adapter_rated_w),
            "voltage_v": b.voltage_v,
            "amperage_ma": b.amperage_ma,
            "nominal_wh": ioreg_info.as_ref().and_then(|i| i.pack_wh()),
            "source": "ioreg AppleSmartBattery (+ root daemon package W on AC)",
            "unavailable_reason": if watts.is_none() {
                json!("voltage/current not exposed by battery API")
            } else {
                Value::Null
            },
        })
    }

    // ── processes ──────────────────────────────────────────────────────────
    fn process_block(&mut self) -> (Vec<Value>, Vec<Value>, usize) {
        let due = match self.last_proc_scan {
            None => true,
            Some(t) => t.elapsed() >= Duration::from_secs(PROC_SCAN_SECS),
        };
        if !due {
            let (a, b, n) = &self.proc_rows;
            return (a.clone(), b.clone(), *n);
        }

        self.procs.refresh_processes(ProcessesToUpdate::All, true);
        self.proc_scans += 1;
        self.last_proc_scan = Some(Instant::now());

        let skip = ["system idle process", "memory compression"];
        let mut rows: Vec<(u32, String, f64, f64)> = self
            .procs
            .processes()
            .values()
            .filter(|p| {
                let name = p.name().to_string_lossy().to_ascii_lowercase();
                !skip.contains(&name.as_str())
            })
            .map(|p| {
                (
                    p.pid().as_u32(),
                    p.name().to_string_lossy().into_owned(),
                    round1(p.cpu_usage() as f64),
                    round1(p.memory() as f64 / 1_048_576.0),
                )
            })
            .collect();

        let row = |r: &(u32, String, f64, f64)| {
            json!({"pid": r.0, "name": r.1, "cpu_pct": r.2, "mem_mb": r.3})
        };

        rows.sort_by(|a, b| b.2.total_cmp(&a.2));
        let by_cpu: Vec<Value> = rows.iter().take(TOP_N).map(row).collect();
        rows.sort_by(|a, b| b.3.total_cmp(&a.3));
        let by_mem: Vec<Value> = rows.iter().take(TOP_N).map(row).collect();
        let count = self.procs.processes().len();

        self.proc_rows = (by_cpu.clone(), by_mem.clone(), count);
        (by_cpu, by_mem, count)
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

/// P/E cluster topology, in logical-CPU index order.
///
/// On Apple Silicon the efficiency cluster is enumerated first: logical CPUs
/// `0..E-1` are efficiency and the performance cores follow. The counts are
/// keyed off `hw.perflevelN.name` ("Performance"/"Efficiency"), never off the
/// perflevel number — that was the old bug (perflevel0 labelled cores 0-3
/// "performance" while they were the efficiency cores).
fn read_clusters(ncpu: usize) -> Vec<Cluster> {
    let mut e_cores = None;
    let mut p_cores = None;
    for idx in 0..4 {
        let name = sysctl_string(&format!("hw.perflevel{idx}.name"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        // `hw.perflevel*.logicalcpu` is an INTEGER sysctl (4 binary bytes), not
        // a string — parsing it as text silently yields "no perflevels".
        let Some(cores) = sysctl_int(&format!("hw.perflevel{idx}.logicalcpu"))
            .map(|c| c as usize)
            .filter(|c| *c > 0)
        else {
            continue;
        };
        match name.as_str() {
            "efficiency" => e_cores = Some(cores),
            "performance" => p_cores = Some(cores),
            _ => {}
        }
    }
    match (e_cores, p_cores) {
        (Some(e), Some(p)) => {
            let mut out = vec![
                Cluster { index: 0, name: "efficiency".into(), cores: e },
                Cluster { index: e, name: "performance".into(), cores: p },
            ];
            if ncpu > e + p {
                out.push(Cluster { index: e + p, name: "unattributed".into(), cores: ncpu - e - p });
            }
            out
        }
        (Some(e), None) => vec![Cluster { index: 0, name: "efficiency".into(), cores: e }],
        (None, Some(p)) => vec![Cluster { index: 0, name: "performance".into(), cores: p }],
        (None, None) => vec![Cluster { index: 0, name: "cpu".into(), cores: ncpu }],
    }
}

/// One tick's cluster rows: static topology + this sample's own slice of
/// `per_core_pct`. A cluster with no sampled cores carries `util_pct: null`
/// and the reason — never 0 %.
fn cluster_payload(clusters: &[Cluster], per_core: &[f64], freq_mhz: Option<f64>) -> Value {
    let rows: Vec<Value> = clusters
        .iter()
        .map(|c| {
            let slice = per_core
                .get(c.index..(c.index + c.cores).min(per_core.len()))
                .unwrap_or(&[]);
            let mut row = json!({
                "index": c.index,
                "name": c.name,
                "cores": c.cores,
                "util_pct": if slice.is_empty() { Value::Null } else {
                    json!(round1(slice.iter().sum::<f64>() / slice.len() as f64))
                },
            });
            if slice.is_empty() {
                row["util_reason"] = json!(format!(
                    "per_core_pct[{}..{}] missing — the sampler has no per-core delta for this cluster yet",
                    c.index,
                    c.index + c.cores - 1
                ));
            } else {
                row["util_source"] = json!(format!(
                    "mean of per_core_pct[{}..{}]",
                    c.index,
                    c.index + slice.len() - 1
                ));
            }
            if let Some(f) = freq_mhz {
                row["freq_mhz"] = json!(f.round());
                row["freq_scope"] = json!("package");
            }
            row
        })
        .collect();
    json!(rows)
}

fn temps_block(root: Option<&rootsample::RootSample>) -> Value {
    let Some(s) = root else { return json!([]) };
    json!(
        s.temps
            .iter()
            .map(|t| json!({"label": t.label, "c": round2(t.c)}))
            .collect::<Vec<_>>()
    )
}

fn power_block(root: Option<&rootsample::RootSample>, gpu: &Value) -> Value {
    let Some(s) = root else {
        return json!({
            "available": false,
            "reason": "root daemon has written no sample.json (powermetrics needs root)",
            "package_w": Value::Null,
            "cpu_w": Value::Null,
            "gpu_w": Value::Null,
            "thermal_pressure": Value::Null,
            "ts": Value::Null,
            "age_s": Value::Null,
            "gpu_util_pct": gpu.get("util_pct").cloned().unwrap_or(Value::Null),
        });
    };
    let age = s.ts.map(|ts| (timefmt::now_unix() - ts as i64) as f64);
    json!({
        "available": s.package_w.is_some() || s.thermal_pressure.is_some(),
        "reason": if s.package_w.is_none() { json!(
            "sample.json present but carries no package_w this cycle") } else { Value::Null },
        "package_w": s.package_w,
        "cpu_w": s.cpu_w,
        "gpu_w": s.gpu_w,
        "thermal_pressure": s.thermal_pressure,
        "ts": s.ts,
        "age_s": age,
        "gpu_util_pct": gpu.get("util_pct").cloned().unwrap_or(Value::Null),
        "source": "sample.json from the root daemon (powermetrics + AppleSMC)",
    })
}

fn pressure_block(load: Value, cpu_pct: f64, ncpu: usize, ram_pct: f64, disk: &Value) -> Value {
    let load1 = load.get("one").and_then(Value::as_f64);
    let cpu = match load1 {
        Some(l) if ncpu > 0 => {
            let per_core = l / ncpu as f64;
            json!({
                "available": true,
                "pct": round1(per_core.clamp(0.0, 1.0) * 100.0),
                "level": if per_core >= 1.0 { "critical" } else if per_core >= 0.7 { "warning" } else { "normal" },
                "load_per_core": round3(per_core),
                "util_pct": cpu_pct,
                "source": "getloadavg(1) / hw.ncpu",
                "derivation": "load1 / logical cores; 100 % = one runnable thread waiting per core. Not Apple's internal number.",
            })
        }
        _ => json!({"available": false, "reason": "load average unavailable"}),
    };
    let memory_block = match memory::pressure_level() {
        Some(level) => json!({
            "available": true,
            "level": match level as i64 { 1 => "normal", 2 => "warning", 4 => "critical", _ => "level" },
            "raw_level": level as i64,
            "pct": ram_pct,
            "source": "kern.memorystatus_vm_pressure_level",
        }),
        None => json!({
            "available": false,
            "reason": "kern.memorystatus_vm_pressure_level absent on this kernel",
            "pct": ram_pct,
        }),
    };
    let storage = match disk.get("used_pct").and_then(Value::as_f64) {
        Some(used) => {
            let free = round1(100.0 - used);
            json!({
                "available": true,
                "pct": free,
                "level": if free < 10.0 { "critical" } else if free < 25.0 { "warning" } else { "normal" },
                "free_pct": free,
                "used_pct": round1(used),
                "source": "statfs free ratio",
                "derivation": "<10 % free = critical, <25 % = warning",
            })
        }
        None => json!({"available": false, "reason": "free space unavailable"}),
    };
    json!({"cpu": cpu, "memory": memory_block, "storage": storage})
}

/// Name what is still genuinely unavailable, with the exact reason. A missing
/// number must never be rendered as 0 by the UI.
fn unavailable(_root_present: bool, snap: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "neural_engine".into(),
        json!("powermetrics ANEMetrics needs root — arrives with the T15 privileged daemon"),
    );
    if snap["gpu"]["freq_mhz"].is_null() {
        out.insert(
            "gpu_frequency".into(),
            json!("PerformanceStatistics carries no GPU clock on Apple Silicon; needs powermetrics (root)"),
        );
    }
    out.insert(
        "package_power".into(),
        json!("CPU/GPU/ANE package watts need powermetrics (root) — T15"),
    );
    out.insert(
        "temperatures".into(),
        json!("no named temperature sensors unprivileged; powermetrics needs root — T15"),
    );
    if snap["battery"]["condition"].is_null() {
        out.insert(
            "battery_condition".into(),
            json!("AppleSmartBattery exposes no condition string; the IOPS report needs the battery service"),
        );
    }
    Value::Object(out)
}

fn collector_block(rec: Option<&crate::collector::CollectorRecord>) -> Value {
    match rec {
        None => json!({
            "available": false,
            "reason": "the Rust collector has written no JSONL log in the logs dir",
            "path": Value::Null,
            "ts": Value::Null,
            "age_s": Value::Null,
            "file_age_s": Value::Null,
        }),
        Some(r) => json!({
            "available": true,
            "reason": Value::Null,
            "path": r.path.to_string_lossy(),
            "ts": r.ts,
            "age_s": r.age_s,
            "file_age_s": r.file_age_s,
            "process_count": r.num(&["process_count"]),
            "uptime_secs": r.num(&["uptime_secs"]),
            "temperature_c": r.num(&["temperature"]),
            "attributed_w": r.num(&["energy", "attributed_w"]),
            "note": "read-only: the collector owns this log; the agent never writes it",
        }),
    }
}

fn load_averages() -> (Option<f64>, Option<f64>, Option<f64>) {
    #[cfg(unix)]
    {
        let mut loads = [0f64; 3];
        let n = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
        if n == 3 {
            return (
                Some(round3(loads[0])),
                Some(round3(loads[1])),
                Some(round3(loads[2])),
            );
        }
    }
    (None, None, None)
}

/// One `sysctlbyname` value as a trimmed string. `None` when the key does not
/// exist (older kernels, Intel Macs for `hw.perflevel*`).
#[cfg(unix)]
fn sysctl_string(key: &str) -> Option<String> {
    use std::ffi::CString;
    let name = CString::new(key).ok()?;
    let mut size: libc::size_t = 0;
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let s = String::from_utf8_lossy(&buf[..size])
        .trim_end_matches('\0')
        .trim()
        .to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(not(unix))]
fn sysctl_string(_key: &str) -> Option<String> {
    None
}

/// Integer sysctl (the `hw.perflevel*.logicalcpu` family is binary, not text).
#[cfg(unix)]
fn sysctl_int(key: &str) -> Option<i64> {
    use std::ffi::CString;
    let name = CString::new(key).ok()?;
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>() as libc::size_t;
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut value as *mut libc::c_int as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some(value as i64)
}

#[cfg(not(unix))]
fn sysctl_int(_key: &str) -> Option<i64> {
    None
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On this Apple Silicon host the perflevel keys exist, so the cluster
    /// split must be real P/E rows — not one unnamed "cpu" cluster.
    #[test]
    fn perflevel_keys_split_the_clusters_on_this_machine() {
        let e = sysctl_int("hw.perflevel1.logicalcpu").unwrap_or(0);
        let p = sysctl_int("hw.perflevel0.logicalcpu").unwrap_or(0);
        if e == 0 || p == 0 {
            println!("no hw.perflevel* counters on this host — split not assertable");
            return;
        }
        let c = read_clusters((e + p) as usize);
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(c[0].name, "efficiency");
        assert_eq!(c[0].index, 0);
        assert_eq!(c[0].cores, e as usize);
        assert_eq!(c[1].name, "performance");
        assert_eq!(c[1].index, e as usize);
    }

    #[test]
    fn clusters_are_in_index_order_and_cover_every_core() {
        let c = read_clusters(8);
        assert!(!c.is_empty());
        let total: usize = c.iter().map(|c| c.cores).sum();
        assert_eq!(total, 8, "clusters must cover every logical CPU: {c:?}");
        assert_eq!(c[0].index, 0);
        for w in c.windows(2) {
            assert_eq!(w[1].index, w[0].index + w[0].cores);
        }
    }

    #[test]
    fn cluster_payload_uses_its_own_slice() {
        let clusters = vec![
            Cluster { index: 0, name: "efficiency".into(), cores: 4 },
            Cluster { index: 4, name: "performance".into(), cores: 4 },
        ];
        let per_core = vec![10.0, 20.0, 30.0, 40.0, 90.0, 90.0, 90.0, 90.0];
        let rows = cluster_payload(&clusters, &per_core, Some(3504.0));
        assert_eq!(rows[0]["util_pct"], json!(25.0));
        assert_eq!(rows[1]["util_pct"], json!(90.0));
        assert_eq!(rows[1]["index"], json!(4));
        assert_eq!(rows[1]["util_source"], json!("mean of per_core_pct[4..7]"));
    }

    #[test]
    fn unsampled_cluster_is_null_not_zero() {
        let clusters = vec![Cluster { index: 0, name: "efficiency".into(), cores: 2 }];
        let rows = cluster_payload(&clusters, &[], None);
        assert_eq!(rows[0]["util_pct"], Value::Null);
        assert!(rows[0]["util_reason"].as_str().unwrap().contains("missing"));
    }

    /// The acceptance for T24's data path: one snapshot, every block present,
    /// and no block silently reporting a fabricated zero for a missing source.
    #[test]
    fn snapshot_has_the_python_shape() {
        let mut s = Sampler::new(std::env::temp_dir().join("syswatch-agent-nonexistent-logs"));
        let snap = s.build();
        for key in [
            "ts", "source", "cpu", "ram", "net", "temps_c", "processes", "proc_count", "boot_ts",
            "load", "battery", "gpu", "disk_io", "disk", "pressure", "unavailable", "memory",
            "power", "collector",
        ] {
            assert!(snap.get(key).is_some(), "missing top-level key {key}");
        }
        assert_eq!(snap["memory"], snap["ram"], "legacy alias must mirror ram");
        assert!(snap["cpu"]["cores"].as_u64().unwrap() > 0);
        assert!(!snap["cpu"]["per_core_pct"].as_array().unwrap().is_empty());
        assert!(snap["ram"]["total_bytes"].as_u64().unwrap() > 0);
        assert!(snap["disk"]["available"].as_bool().unwrap());
        assert!(snap["disk"]["mount"].as_str().unwrap().starts_with('/'));
        // app.js reads `disk.volumes`; it must be inside disk, not beside it.
        assert!(snap["disk"]["volumes"].is_array());
        assert!(snap.get("volumes").is_none());
        // The collector log was pointed at a directory that does not exist: the
        // block must say so instead of pretending to have data.
        assert_eq!(snap["collector"]["available"], json!(false));
        assert!(snap["collector"]["reason"].as_str().unwrap().contains("no JSONL"));
        assert!(snap["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn every_number_is_finite() {
        fn walk(v: &Value) {
            match v {
                Value::Number(n) => {
                    let f = n.as_f64().unwrap();
                    assert!(f.is_finite(), "non-finite number in snapshot");
                }
                Value::Array(a) => a.iter().for_each(walk),
                Value::Object(o) => o.values().for_each(walk),
                _ => {}
            }
        }
        let mut s = Sampler::new(std::env::temp_dir().join("syswatch-agent-nonexistent-logs"));
        walk(&s.build());
    }
}
