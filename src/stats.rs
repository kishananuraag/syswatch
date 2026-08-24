use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use sysinfo::{Components, Disks, Networks, System};

const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * MB;

/// One frame's worth of collected data. Also the JSON output shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub timestamp: String,
    pub uptime_secs: u64,
    pub cpu: Cpu,
    pub memory: Memory,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    pub disks: Vec<Disk>,
    pub networks: Vec<Net>,
    /// Top processes by CPU usage and by memory.
    pub processes: Processes,
    pub process_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Processes {
    /// Top processes by CPU usage.
    pub by_cpu: Vec<ProcRow>,
    /// Top processes by resident memory.
    pub by_mem: Vec<ProcRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cpu {
    pub total_pct: f32,
    pub per_core_pct: Vec<f32>,
    pub freq_mhz: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub pct: f64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Disk {
    pub mount: String,
    pub fs: String,
    pub used_pct: f64,
    pub free_gb: f64,
    pub total_gb: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Net {
    pub name: String,
    /// Received bytes per second since last tick.
    pub rx_bps: f64,
    /// Transmitted bytes per second since last tick.
    pub tx_bps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcRow {
    pub pid: i32,
    pub name: String,
    pub cpu_pct: f32,
    pub mem_mb: f64,
}

/// Stateful collector holding sysinfo handles plus previous network counters
/// so we can compute per-tick rates instead of lifetime totals.
pub struct Collector {
    sys: System,
    disks: Disks,
    networks: Networks,
    components: Option<Components>,
    prev_net: HashMap<String, (u64, u64)>,
    prev_instant: Instant,
    last_cpu_refresh: Instant,
    top_n: usize,
}

impl Collector {
    pub fn new(top_n: usize) -> Self {
        let mut c = Self {
            sys: System::new_all(),
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list().into(),
            prev_net: HashMap::new(),
            prev_instant: Instant::now(),
            last_cpu_refresh: Instant::now(),
            top_n,
        };
        c.sys.refresh_all();
        // First sample of CPU usage is garbage; prime it.
        std::thread::sleep(Duration::from_millis(200));
        c.sys.refresh_cpu_all();
        c.snapshot_network_counters();
        c.prev_instant = Instant::now();
        c.last_cpu_refresh = Instant::now();
        c
    }

    fn snapshot_network_counters(&mut self) {
        self.prev_net.clear();
        for (name, data) in &self.networks {
            self.prev_net.insert(
                name.clone(),
                (data.total_received(), data.total_transmitted()),
            );
        }
    }

    pub fn refresh(&mut self) {
        // sysinfo computes CPU usage as the delta between consecutive CPU
        // refreshes. If they happen back-to-back the delta window is ~0 ms and
        // usage gets reported as a bogus constant 100%. Enforce the minimum
        // interval between CPU refreshes.
        let since_last = self.last_cpu_refresh.elapsed();
        if since_last < sysinfo::MINIMUM_CPU_UPDATE_INTERVAL {
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL - since_last);
        }
        self.sys.refresh_all();
        self.last_cpu_refresh = Instant::now();
        self.networks.refresh(true);
        self.disks.refresh(true);
    }

    pub fn snapshot(&mut self, with_temp: bool) -> Snapshot {
        let elapsed = self.prev_instant.elapsed().as_secs_f64().max(1e-6);

        let cores = self.sys.cpus();
        let total_pct = cores.iter().map(|c| c.cpu_usage()).sum::<f32>() / cores.len() as f32;
        let per_core: Vec<f32> = cores.iter().map(|c| c.cpu_usage()).collect();
        let freq_mhz = cores.first().map(|c| c.frequency()).unwrap_or(0);

        let total_ram = self.sys.total_memory();
        let used_ram = self.sys.used_memory();

        let mut temp: Option<f32> = None;
        if with_temp {
            if let Some(comps) = &mut self.components {
                comps.refresh(true);
                // Prefer anything that looks like a CPU sensor; fall back to hottest.
                for c in comps.iter() {
                    let label = c.label().to_ascii_lowercase();
                    if label.contains("cpu") || label.contains("package") || label.contains("tctl")
                    {
                        temp = c.temperature();
                        break;
                    }
                }
                if temp.is_none() {
                    temp = comps
                        .iter()
                        .filter_map(|c| c.temperature())
                        .reduce(f32::max);
                }
            }
        }

        let mut disks: Vec<Disk> = self
            .disks
            .iter()
            .filter(|d| d.total_space() > 0)
            .map(|d| {
                let total = d.total_space();
                let free = d.available_space();
                Disk {
                    mount: d.mount_point().to_string_lossy().into_owned(),
                    fs: d.file_system().to_string_lossy().into_owned(),
                    used_pct: (total - free) as f64 / total as f64 * 100.0,
                    free_gb: free as f64 / GB,
                    total_gb: total as f64 / GB,
                }
            })
            .collect();
        // Skip tiny/ephemeral mounts and dedupe by mount point.
        disks.retain(|d| d.total_gb > 0.5);
        disks.sort_by(|a, b| a.mount.cmp(&b.mount));
        disks.dedup_by(|a, b| a.mount == b.mount);

        let networks: Vec<Net> = self
            .networks
            .iter()
            .map(|(name, data)| {
                let cur = (data.total_received(), data.total_transmitted());
                let (prx, ptx) = self.prev_net.get(name).copied().unwrap_or(cur);
                Net {
                    name: name.clone(),
                    rx_bps: cur.0.saturating_sub(prx) as f64 / elapsed,
                    tx_bps: cur.1.saturating_sub(ptx) as f64 / elapsed,
                }
            })
            .filter(|n| n.rx_bps > 0.0 || n.tx_bps > 0.0)
            .collect();
        self.snapshot_network_counters();
        self.prev_instant = Instant::now();

        let procs: Vec<ProcRow> = self
            .sys
            .processes()
            .values()
            .map(|p| ProcRow {
                pid: p.pid().as_u32() as i32,
                name: p.name().to_string_lossy().chars().take(24).collect(),
                cpu_pct: p.cpu_usage(),
                mem_mb: p.memory() as f64 / MB,
            })
            .collect();

        let process_count = procs.len();
        let mut by_cpu = procs.clone();
        by_cpu.sort_by(|a, b| {
            b.cpu_pct
                .partial_cmp(&a.cpu_pct)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut by_mem = procs;
        by_mem.sort_by(|a, b| {
            b.mem_mb
                .partial_cmp(&a.mem_mb)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Snapshot {
            timestamp: chrono_now(),
            uptime_secs: System::uptime(),
            cpu: Cpu {
                total_pct,
                per_core_pct: per_core,
                freq_mhz,
            },
            memory: Memory {
                used_bytes: used_ram,
                total_bytes: total_ram,
                pct: used_ram as f64 * 100.0 / total_ram.max(1) as f64,
                swap_used_bytes: self.sys.used_swap(),
                swap_total_bytes: self.sys.total_swap(),
            },
            temperature: match temp {
                Some(t) if t > 0.0 => Some(t),
                _ => None,
            },
            disks,
            networks,
            processes: Processes {
                by_cpu: by_cpu.into_iter().take(self.top_n).collect(),
                by_mem: by_mem.into_iter().take(self.top_n).collect(),
            },
            process_count,
        }
    }
}

/// Current UTC timestamp without external crates.
fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(secs)
}

fn format_utc(secs: u64) -> String {
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days algorithm (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
}
