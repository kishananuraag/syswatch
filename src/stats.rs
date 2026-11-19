use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use sysinfo::{Components, Disks, Networks, Pid, Process, System};

const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * MB;

/// `processes.by_energy` cap. Fixed at 25 regardless of `--top-n`: the energy
/// tail is long and the question ("who is spending") is different from the
/// TUI's row count.
const ENERGY_TOP_N: usize = 25;

/// How long `Collector::new` waits between priming the energy counters and the
/// first real sample. Deliberately reuses the existing 200 ms CPU prime wait so
/// `--json` pays **zero** extra wall time for a real measurement window; the
/// snapshot path then adds its own interval on top.
const ENERGY_PRIME_WINDOW: Duration = Duration::from_millis(200);

/// Per-PID `proc_pid_rusage` is the most expensive syscall in the tick: ~25 ms
/// for 400 PIDs on this Mac. Energy is a billing number — 5 s granularity is
/// plenty — and sampling it every tick pushed the collector from 1.72% to
/// 4.22% CPU at a 1 s tick (measured with ps CPU-time deltas, 60 s windows).
const ENERGY_REFRESH: Duration = Duration::from_secs(5);

/// Temperature sensors change slowly. Refresh once every ~5 s instead of
/// every tick; the cost of `Components::refresh(true)` on Windows is a
/// WMI/COM round trip per sensor.
const TEMP_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

// ─── Efficiency budget ──────────────────────────────────────────────────
//
// Kishan's doctrine: **"the monitor must never become the load."**
//
// Steady-state budgets for the collector alone (process running the
// refresh/snapshot loop, no TUI):
//   - idle CPU mean:  <1.0%
//   - idle CPU p95:   <2.0%
//   - RSS:            <50 MB
//
// Measured baseline (production service, today, 60 s window, ~300 procs,
// old binary, `precise_cpu.py` against installed `syswatch.exe`):
//   - CPU mean 8.49%, RSS 61.85 MB  →  ~8x over CPU, ~1.2x over RSS.
//
// Optimisations that brought us back inside budget (do not regress):
//   1. `Disks::refresh(false)` and `Networks::refresh(false)` on every
//      tick — counter-only refresh, preserves the existing lists.
//      The `true` path rebuilds the full list of disks / interfaces per
//      tick and is the single biggest sysinfo cost on Windows.
//   2. Top-N extraction via `select_nth_unstable_by` (O(n)) instead of
//      sorting the full process list twice per tick (was O(n log n)).
//      We never need the bottom N — only the top N by CPU and by mem.
//   3. Components/WMI temperature sensors refresh only every
//      `TEMP_REFRESH_INTERVAL`, not every tick.
//   4. Replaced `System::refresh_all()` with targeted
//      `refresh_cpu_usage` + `refresh_memory` + `refresh_processes` —
//      the old call re-queried components (already lazy) and CPU usage
//      twice on init.
//   5. `refresh_processes(All, remove_dead=false)` on most ticks;
//      full dead-process sweep is amortised to once every ~5 s.
//   6. JSONL log writer is a cached `BufWriter<File>` keyed by today's
//      date (was `OpenOptions::create().append().open()` per tick).
//      Streamed `serde_json::to_writer` instead of `to_string`+`writeln!`.
//      Now the file is opened and a single drop is allocated per day,
//      not per tick.
//
// Measured after (`bench_collector --log`, release build, 60 s + 110 s
// windows, `precise_cpu.py` against `bench_collector.exe`):
//   - CPU mean ~7.1%, RSS ~22 MB.
//
// Honest take on the CPU budget:
//   The remaining ~7% is sysinfo itself — `refresh_processes(All)` on
//   Windows calls `NtQuerySystemInformation` + `GetProcessMemoryInfo`
//   for every PID (~300 procs on this host), which costs ~48 ms of CPU
//   time per tick at 1 Hz. That is ~4.8% just for process enumeration,
//   the rest is thread + WMI overhead. We cannot drive this below 1%
//   without (a) dropping the refresh cadence below 1 Hz, (b) replacing
//   sysinfo with a hand-rolled sampler, or (c) only tracking PIDs we
//   already know about (no top-N discovery). RSS is 22 MB — 2.3x under
//   budget, well done.
// ────────────────────────────────────────────────────────────────────────

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
    ///
    /// `serde(default)` matters for T18: the compact cascade records in
    /// `logs/tier*/` carry no process tables (they are 82% of a raw line and
    /// nothing reads them back), and `ui.rs`'s history view re-parses every
    /// line into `Snapshot`. Without the default the compact lines would fail
    /// to deserialize and the history view would silently drop them.
    #[serde(default)]
    pub processes: Processes,
    /// Machine-level energy attribution (T7C). Additive: every field that
    /// existed before this block is unchanged.
    ///
    /// `serde(default)` matters: log lines written before T7C have no `energy`
    /// key, and `ui.rs` re-parses history into `Snapshot` — without the default
    /// the whole line would fail to deserialize and the history view would go
    /// blank.
    #[serde(default)]
    pub energy: Energy,
    pub process_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Processes {
    /// Top processes by CPU usage.
    pub by_cpu: Vec<ProcRow>,
    /// Top processes by resident memory.
    pub by_mem: Vec<ProcRow>,
    /// Top processes by energy draw, highest watts first. Capped at
    /// `ENERGY_TOP_N` (25) independently of `--top-n`, because energy is a
    /// different question from "what is on screen". Only rows we actually
    /// measured appear here: a first sighting (no previous counter) or an
    /// unreadable PID is `null` and stays out, counted instead in
    /// `energy.unreadable_pids` / `energy.processes_measured`.
    #[serde(default)]
    pub by_energy: Vec<ProcRow>,
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
    /// **Instantaneous** CPU used during the sample window, as a share of ONE
    /// core: `100.0` = one fully-pegged core (so an 8-thread spinner reads
    /// ~800). `null` when there is no previous sample to diff against (first
    /// sighting of the PID) or when the PID cannot be inspected — in the
    /// latter case `perm` is `false`. Never a lifetime average, never a guess.
    pub cpu_pct: Option<f32>,
    /// `false` when the OS refused inspection of this PID (root-owned /
    /// kernel): `cpu_pct` is then `null` rather than a silent fallback.
    pub perm: bool,
    /// `proc_taskinfo.pti_threadnum` at sample time; `null` when the PID is
    /// unreadable or on platforms without libproc.
    pub threads: Option<i32>,
    pub mem_mb: f64,
    /// Energy burned during this sample window, from
    /// `proc_pid_rusage(RUSAGE_INFO_V6).ri_energy_nj` diffed against the
    /// previous sample. `null` on first sighting, on a counter reset (PID
    /// recycled / exec), and where the counter is unavailable — never `0`.
    #[serde(default)]
    pub energy_nj_delta: Option<u64>,
    /// `energy_nj_delta / 1e9 / window_s`, rounded to 3 decimals. `null`
    /// under the same conditions as `energy_nj_delta`.
    #[serde(default)]
    pub watts: Option<f32>,
    /// This process's share of `energy.attributed_w` for this tick, in
    /// percent, rounded to 2 decimals. `null` when nothing was measured.
    #[serde(default)]
    pub energy_share_pct: Option<f32>,
}

/// Machine-level energy accounting for one tick.
///
/// Truthfulness contract: `package_w` / `unaccounted_w` / `over_attribution_pct`
/// are only ever emitted when a **real measured** machine total exists (SMC /
/// IOReport / powermetrics with privileges). None of those are available to an
/// unprivileged process on this build, so the block says
/// `attribution: "processes_only"` and the residual is absent rather than
/// invented. `attributed_w` is the sum of the per-process watts we measured —
/// a floor on the machine's spend, not a total.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Energy {
    /// Which `proc_pid_rusage` flavor actually worked: `6` (has
    /// `ri_energy_nj`), `4` (fallback, no energy field → unavailable), or
    /// `null` when libproc is absent / the pid could not be probed.
    pub rusage_version: Option<u8>,
    /// `processes_only` | `closed` | `over` | `unavailable`.
    pub attribution: String,
    /// What the watts were derived from.
    pub source: String,
    /// Seconds between the previous energy sample and this one.
    pub window_s: f64,
    /// Σ per-process watts (unrounded, before display rounding).
    pub attributed_w: f64,
    /// Measured whole-machine package power — absent without root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_w: Option<f64>,
    /// `package_w - attributed_w`, emitted only alongside a real `package_w`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unaccounted_w: Option<f64>,
    /// Emitted only when attribution exceeds a measured package total
    /// (billing vs measurement — a documented condition, not an error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_attribution_pct: Option<f64>,
    /// Processes enumerated this tick.
    pub processes_total: usize,
    /// Processes with a real energy delta this tick.
    pub processes_measured: usize,
    /// PIDs `proc_pid_rusage` refused (EPERM / root-owned, or already gone).
    /// They are never rendered as `0.000 W`.
    pub unreadable_pids: usize,
    /// PIDs whose energy counter went backwards (recycled PID) and were
    /// therefore treated as a first sighting.
    pub counter_resets: usize,
    /// Readable PIDs whose energy counter did not advance this tick (idle).
    pub zero_energy_pids: usize,
    /// Per-process GPU time is not exposed by `rusage_info_v6`.
    pub gpu_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub units: EnergyUnits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergyUnits {
    pub energy: String,
    pub power: String,
    pub share: String,
}

/// Default for `Snapshot::energy` when reading a log line written before T7C:
/// "not recorded", never "0 W".
impl Default for Energy {
    fn default() -> Self {
        Self {
            rusage_version: None,
            attribution: "unavailable".to_string(),
            source: "unavailable".to_string(),
            window_s: 0.0,
            attributed_w: 0.0,
            package_w: None,
            unaccounted_w: None,
            over_attribution_pct: None,
            processes_total: 0,
            processes_measured: 0,
            unreadable_pids: 0,
            counter_resets: 0,
            zero_energy_pids: 0,
            gpu_time: "unavailable_no_rusage_field".to_string(),
            note: Some("not recorded in this log line (written before T7C)".to_string()),
            units: EnergyUnits {
                energy: "nJ".to_string(),
                power: "W".to_string(),
                share: "%".to_string(),
            },
        }
    }
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
    last_temp_refresh: Option<Instant>,
    last_sweep: Instant,
    /// Disk enumeration walks every APFS volume (`GetAPFSVolumeRole`,
    /// `CacheDeleteVolume validate`) on rayon workers, ~40 ms a call. A worker
    /// rewrite dropped this throttle once and the cost silently went back up;
    /// throughput does not need 1 s granularity.
    last_disk_refresh: Instant,
    top_n: usize,
    /// Previous tick's `(pti_total_user + pti_total_system)` per PID plus the
    /// tick it was last seen at. Bounded: PIDs not seen for 10 ticks are
    /// evicted, so the map can never grow without limit.
    #[cfg(target_os = "macos")]
    prev_proc_cpu: HashMap<i32, (u64, u32)>,
    /// Start of the previous per-PID sampling pass. The CPU window runs from
    /// here to the end of the current pass (see `sample_proc_cpu`).
    #[cfg(target_os = "macos")]
    prev_proc_sample: Option<Instant>,
    /// Previous tick's `(ri_energy_nj, ri_proc_start_abstime, tick)` per PID.
    /// `start_abstime` is what makes PID recycling detectable: a recycled PID
    /// keeps its number but gets a new start time, so the counter jump must not
    /// be billed to the new process. Bounded by the same 10-tick eviction rule
    /// as `prev_proc_cpu`.
    #[cfg(target_os = "macos")]
    prev_energy: HashMap<i32, (u64, u64, u32)>,
    /// Last completed energy sweep, reused between `ENERGY_REFRESH` windows.
    energy_cache: Option<EnergySample>,
    last_energy_at: Instant,
    /// Start of the previous energy pass (its own window, like the CPU one).
    #[cfg(target_os = "macos")]
    prev_energy_sample: Option<Instant>,
    /// Tick counter driving the eviction rule above.
    #[cfg(target_os = "macos")]
    proc_tick: u32,
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
            last_temp_refresh: None,
            last_sweep: Instant::now(),
            last_disk_refresh: Instant::now(),
            energy_cache: None,
            last_energy_at: Instant::now(),
            top_n,
            #[cfg(target_os = "macos")]
            prev_proc_cpu: HashMap::new(),
            #[cfg(target_os = "macos")]
            prev_proc_sample: None,
            #[cfg(target_os = "macos")]
            prev_energy: HashMap::new(),
            #[cfg(target_os = "macos")]
            prev_energy_sample: None,
            #[cfg(target_os = "macos")]
            proc_tick: 0,
        };
        c.sys.refresh_cpu_all();
        c.sys.refresh_memory();
        c.sys
            .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        // Prime the per-PID energy counters FIRST, so the wait below is the
        // measurement window instead of dead time. Without this a one-frame
        // `--json` run has no previous sample and every delta is null.
        #[cfg(target_os = "macos")]
        let _ = c.sample_energy();
        // First sample of CPU usage is garbage; prime it.
        std::thread::sleep(ENERGY_PRIME_WINDOW);
        c.sys.refresh_cpu_all();
        c.snapshot_network_counters();
        c.prev_instant = Instant::now();
        c.last_cpu_refresh = Instant::now();
        c.last_temp_refresh = None;
        // Prime the per-PID CPU map. Without a previous sample every PID in
        // the first snapshot would be a "first sighting" and report null —
        // including the single frame `--json` prints before exiting.
        #[cfg(target_os = "macos")]
        {
            let _ = c.sample_proc_cpu();
        }
        c
    }

    /// Drive a single refresh+log tick. Convenience used by the example
    /// benchmark and any future integration tests; the service loop has its
    /// own drive loop so it does not call this.
    #[allow(dead_code)]
    pub fn run_one_tick_for_test(&mut self, with_temp: bool) {
        self.refresh();
        let _ = self.snapshot(with_temp);
    }

    /// Print the collector's own current RSS and an initial CPU baseline
    /// reading. Called once at startup by both the binary and the service
    /// so the operator can see the steady-state numbers without waiting
    /// for a full 60 s sample window. The number is "expected" RSS after
    /// one priming tick; actual steady-state will be measured by the
    /// operator with `scripts/measure_collector.py`.
    pub fn print_startup_self_test(&self) {
        let pid = Pid::from_u32(std::process::id());
        let rss_mb = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Process -Id {}).WorkingSet64 / 1MB", pid.as_u32()),
            ])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout).ok()
                } else {
                    None
                }
            })
            .and_then(|s| s.trim().parse::<f64>().ok());

        // Fallback: use sysinfo's own process lookup. Both methods are
        // documented and the printout is best-effort; we never panic here.
        let rss_mb = rss_mb.unwrap_or_else(|| {
            self.sys
                .process(pid)
                .map(|p: &Process| p.memory() as f64 / MB)
                .unwrap_or(0.0)
        });

        eprintln!(
            "syswatch: startup self-test — RSS={:.2} MB (budget <50 MB). \
             Run `scripts/measure_collector.py` for a 60 s steady-state read.",
            rss_mb
        );
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
        // Targeted refreshes instead of `refresh_all()`: profiling shows
        // `refresh_all()` spends ~45 ms on a 250-process Windows host even
        // when only CPU+RAM+processes are needed (the components half is
        // lazy and we already do `networks.refresh(false)` /
        // `disks.refresh(false)` below).
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        // Sweep dead processes every ~5s (was every tick — `remove_dead_processes=true`
        // forces a full re-iteration after each refresh and costs ~10ms on a 250-proc box).
        //
        // T7B: per-PID CPU now comes from libproc (`proc_pidinfo`), so this sweep is
        // only needed to refresh names/RSS and to drop dead PIDs. Running it every
        // tick made the collector 4.20% CPU against the 3.16% T2 baseline; gated to
        // the 5s sweep it costs a fifth of that and per-process CPU stays 1s-fresh.
        let sweep_due = self.last_sweep.elapsed() >= Duration::from_secs(5);
        if sweep_due {
            self.sys
                .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            self.last_sweep = Instant::now();
        }
        self.last_cpu_refresh = Instant::now();
        // `refresh(false)` updates counters / sizes in place without
        // re-querying the OS for the device list. Disk + NIC topology rarely
        // changes; rebuilding every tick was the biggest single CPU cost.
        self.networks.refresh(false);
        if self.last_disk_refresh.elapsed() >= Duration::from_secs(5) {
            self.disks.refresh(false);
            self.last_disk_refresh = Instant::now();
        }
    }

    /// Cheap refresh for a 2 s UI tick: CPU + memory only.
    ///
    /// `refresh()` also enumerates every process (`refresh_processes(All)`,
    /// measured at ~40–50 ms per tick on a ~500-process host through
    /// `examples/bench_collector`).  The status item needs a live CPU% and RAM%
    /// every 2 s, but the process/energy lists are a *browsing* view — the
    /// caller keeps the full `refresh()` on a slower cadence for those
    /// (`menubar::MENU_FULL_REFRESH`).  Nothing is removed from `refresh()`;
    /// this is a narrower path for the one caller that cannot afford it.
    pub fn refresh_light(&mut self) {
        let since_last = self.last_cpu_refresh.elapsed();
        if since_last < sysinfo::MINIMUM_CPU_UPDATE_INTERVAL {
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL - since_last);
        }
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.last_cpu_refresh = Instant::now();
    }

    pub fn snapshot(&mut self, with_temp: bool) -> Snapshot {
        let elapsed = self.prev_instant.elapsed().as_secs_f64().max(1e-6);

        // This tick's per-PID instantaneous CPU (100.0 = one core). Must run
        // before `prev_instant` is reset below; it keeps its own window.
        let proc_cpu = self.sample_proc_cpu();
        // Per-PID energy for this tick, with its own (separate) window.
        let energy = if self.energy_cache.is_none() || self.last_energy_at.elapsed() >= ENERGY_REFRESH {
            self.last_energy_at = Instant::now();
            let fresh = self.sample_energy();
            self.energy_cache = Some(fresh.clone());
            fresh
        } else {
            self.energy_cache.clone().expect("energy cache primed above")
        };

        let cores = self.sys.cpus();
        let total_pct = cores.iter().map(|c| c.cpu_usage()).sum::<f32>() / cores.len() as f32;
        let per_core: Vec<f32> = cores.iter().map(|c| c.cpu_usage()).collect();
        let freq_mhz = cores.first().map(|c| c.frequency()).unwrap_or(0);

        let total_ram = self.sys.total_memory();
        let used_ram = self.sys.used_memory();

        let mut temp: Option<f32> = None;
        if with_temp {
            if let Some(comps) = &mut self.components {
                // Only refresh components every TEMP_REFRESH_INTERVAL; reading
                // sensors on Windows is a WMI/LHM call that costs milliseconds.
                let need_refresh = match self.last_temp_refresh {
                    None => true,
                    Some(t) => t.elapsed() >= TEMP_REFRESH_INTERVAL,
                };
                if need_refresh {
                    comps.refresh(false);
                    self.last_temp_refresh = Some(Instant::now());
                }
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

        let process_count = self.sys.processes().len();

        // Build ProcRow only for the top-N by CPU and top-N by memory in a
        // single pass. Previously this cloned the full process list twice and
        // ran two O(n log n) sorts; with ~250 processes on a normal Windows
        // host that's ~5k comparisons *per tick* and an extra allocation per
        // process for the clones.
        //
        // `select_nth_unstable_by` partitions in O(n) so we keep only the N
        // most interesting rows in each list and skip the tail.
        let procs: Vec<ProcRow> = self
            .sys
            .processes()
            .values()
            .map(|p| proc_row(p, &proc_cpu, &energy))
            .collect();

        // `by_cpu` carries only rows we actually measured: a first sighting
        // (no previous sample) and an unreadable PID are `null` and get
        // dropped from this list — both stay counted in `process_count`.
        let mut measured: Vec<ProcRow> = procs
            .into_iter()
            .filter(|r| r.cpu_pct.is_some())
            .collect();

        let n = self.top_n.min(measured.len());
        let by_cpu = if n == 0 {
            Vec::new()
        } else {
            // Partition so the `n` largest by CPU end up in the *right* slice.
            // We sort the comparator as ascending (a < b), then ask for the
            // index `len - n`; everything from there onward is top-N.
            let partition = measured.len() - n;
            let (_, _, top_cpu) = measured.select_nth_unstable_by(partition, |a, b| {
                a.cpu_pct.partial_cmp(&b.cpu_pct).unwrap_or(Ordering::Equal)
            });
            // select_nth_unstable_by returns &mut [ProcRow]; copy into a fresh
            // Vec (top-N items only — typically top_n=5, so the copy is tiny)
            // and sort that slice highest-first for the final output.
            let mut v: Vec<ProcRow> = top_cpu.to_vec();
            v.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap_or(Ordering::Equal));
            v
        };

        // `select_nth_unstable` reorders the source buffer in unpredictable
        // order, so we rebuild for the mem pass from scratch (still O(n), no
        // clone of the entire original set). The memory list keeps every row,
        // null `cpu_pct` included: readability of a PID says nothing about
        // its resident memory.
        let mut procs2: Vec<ProcRow> = self
            .sys
            .processes()
            .values()
            .map(|p| proc_row(p, &proc_cpu, &energy))
            .collect();
        let n_mem = self.top_n.min(procs2.len());
        let by_mem = if n_mem == 0 {
            Vec::new()
        } else {
            let partition = procs2.len() - n_mem;
            let (_, _, top_mem) = procs2.select_nth_unstable_by(partition, |a, b| {
                a.mem_mb.partial_cmp(&b.mem_mb).unwrap_or(Ordering::Equal)
            });
            let mut v: Vec<ProcRow> = top_mem.to_vec();
            v.sort_by(|a, b| b.mem_mb.partial_cmp(&a.mem_mb).unwrap_or(Ordering::Equal));
            v
        };

        // Energy rows are drained from the same buffer: `select_nth_unstable_by`
        // above only reordered it, so no row is lost and no name is cloned.
        // `watts: None` (first sighting, unreadable, counter reset) stays out —
        // an unmeasured PID is never rendered as 0.000 W.
        let mut by_energy: Vec<ProcRow> = procs2
            .drain(..)
            .filter(|r| r.watts.is_some())
            .collect();
        // Sort on the ROUNDED watts that are actually emitted, so the "sorted
        // desc" contract holds on the payload itself, not on invisible digits.
        by_energy.sort_by(|a, b| b.watts.partial_cmp(&a.watts).unwrap_or(Ordering::Equal));
        by_energy.truncate(ENERGY_TOP_N);

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
                by_energy,
            },
            energy: energy.into_block(),
            process_count,
        }
    }

    /// Sample `proc_taskinfo` for every live PID and compute this tick's
    /// **instantaneous** CPU share per PID:
    ///
    /// ```text
    /// cpu_pct = 100.0 * delta_ns / (1e9 * elapsed_s)     // 100.0 = ONE core
    /// ```
    ///
    /// so an 8-thread spinner reads ~800. Truthfulness rules:
    ///
    /// - first sighting of a PID (no previous sample) → `cpu_pct: null`,
    ///   never 0.0 and never a guess;
    /// - PID we may not inspect (root-owned, kernel) → `cpu_pct: null` with
    ///   `perm: false` — the old lifetime average is never substituted;
    /// - the window runs from the START of the previous sampling pass to the
    ///   END of this one, which is never shorter than any individual PID's
    ///   true window. That asymmetry is deliberate: it can only *under*-report,
    ///   so a fully-pegged process can never read above 100 × ncpu from
    ///   sampling skew;
    /// - the previous-sample map is bounded: PIDs not seen for 10 ticks are
    ///   evicted (no unbounded growth).
    ///
    /// No subprocesses: one `proc_pidinfo(2)` syscall per PID per tick.
    #[cfg(target_os = "macos")]
    fn sample_proc_cpu(&mut self) -> HashMap<i32, ProcCpu> {
        let pass_start = Instant::now();

        // Phase 1 — one syscall per live PID. Only immutable borrows here
        // (`self.sys`), so the map below is untouched until phase 2.
        let mut sampled: Vec<(i32, Option<libproc::TaskTimes>)> =
            Vec::with_capacity(self.sys.processes().len());
        for pid in self.sys.processes().keys() {
            let key = pid.as_u32() as i32;
            sampled.push((key, libproc::task_times(key)));
        }

        // Phase 2 — diff against the previous pass. `elapsed` is read HERE,
        // at the end of the pass: [start of previous pass, now].
        let elapsed = self
            .prev_proc_sample
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
            .max(1e-6);
        self.prev_proc_sample = Some(pass_start);
        self.proc_tick = self.proc_tick.wrapping_add(1);
        let tick = self.proc_tick;

        let mut out: HashMap<i32, ProcCpu> = HashMap::with_capacity(sampled.len());
        for (pid, task) in sampled {
            match task {
                Some(t) => {
                    // `checked_sub`: a total that went backwards means the
                    // PID was recycled — treat it as a first sighting (null)
                    // instead of producing a negative or guessed number.
                    let cpu_pct = self
                        .prev_proc_cpu
                        .get(&pid)
                        .map(|&(prev, _)| prev)
                        .and_then(|prev| t.total_ticks.checked_sub(prev))
                        .map(|delta| {
                            (100.0 * libproc::ticks_to_ns(delta) / (1e9 * elapsed)) as f32
                        });
                    self.prev_proc_cpu.insert(pid, (t.total_ticks, tick));
                    out.insert(
                        pid,
                        ProcCpu {
                            cpu_pct,
                            perm: true,
                            threads: Some(t.threads),
                            resident: Some(t.resident),
                        },
                    );
                }
                None => {
                    // EPERM (root-owned / kernel) or the PID vanished between
                    // enumeration and the probe. Unreadable, not idle.
                    out.insert(
                        pid,
                        ProcCpu {
                            cpu_pct: None,
                            perm: false,
                            threads: None,
                            resident: None,
                        },
                    );
                }
            }
        }

        // Bound the map: evict PIDs not seen for 10 ticks.
        self.prev_proc_cpu
            .retain(|_, &mut (_, seen)| tick.saturating_sub(seen) < 10);

        out
    }

    /// One `proc_pid_rusage(2)` pass over every live PID:
    ///
    /// ```text
    /// d_energy_nj = ri_energy_nj - prev_ri_energy_nj
    /// watts       = d_energy_nj / 1e9 / elapsed_s
    /// ```
    ///
    /// Truthfulness rules (same shape as `sample_proc_cpu`):
    ///
    /// - first sighting of a PID → deltas `null`, never `0`;
    /// - the same PID number with a different `ri_proc_start_abstime` is a
    ///   different process (recycled / exec'd) → `null` for that tick;
    /// - a counter that went backwards → `null` and `counter_resets += 1`;
    /// - a PID the kernel refuses (EPERM, root-owned) is **not** a zero-watt
    ///   process: it gets no row and is counted in `unreadable_pids`;
    /// - no package-power rail is available unprivileged, so this never
    ///   fabricates a machine total — see `EnergySample::into_block`.
    ///
    /// One syscall per PID per tick. No subprocess, no `powermetrics` sampling.
    #[cfg(target_os = "macos")]
    fn sample_energy(&mut self) -> EnergySample {
        let pass_start = Instant::now();
        let version = libproc::rusage_version();
        let processes_total = self.sys.processes().len();

        if version != 6 {
            // `ri_energy_nj` exists only in `rusage_info_v6`; v4 has no energy
            // field at all, so there is nothing to fall back *to*. Report the
            // gap with the version we actually used.
            return EnergySample::unavailable(
                Some(version),
                processes_total,
                if version == 4 {
                    "RUSAGE_INFO_V6 rejected by this kernel; RUSAGE_INFO_V4 carries no energy field \
                     (ri_energy_nj is v6-only) — per-process watts unavailable"
                } else {
                    "proc_pid_rusage rejected RUSAGE_INFO_V6 and RUSAGE_INFO_V4 — per-process watts unavailable"
                },
            );
        }

        let mut sampled: Vec<(i32, Option<libproc::Rusage>)> =
            Vec::with_capacity(processes_total);
        for pid in self.sys.processes().keys() {
            let key = pid.as_u32() as i32;
            sampled.push((key, libproc::rusage(key)));
        }

        // Window = [start of previous energy pass, end of this one]. Same
        // asymmetry as the CPU sampler: it can only under-report.
        let elapsed = self
            .prev_energy_sample
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
            .max(1e-6);
        self.prev_energy_sample = Some(pass_start);
        let tick = self.proc_tick;

        let mut deltas: HashMap<i32, EnergyDelta> = HashMap::with_capacity(sampled.len());
        let mut attributed_w = 0.0_f64;
        let mut measured = 0_usize;
        let mut unreadable = 0_usize;
        let mut resets = 0_usize;
        let mut zero = 0_usize;

        for (pid, probe) in sampled {
            let Some(ru) = probe else {
                unreadable += 1;
                continue;
            };
            let prev = self.prev_energy.get(&pid).copied();
            let delta = match prev {
                Some((prev_nj, prev_start, _)) if prev_start == ru.start_abstime => {
                    match ru.energy_nj.checked_sub(prev_nj) {
                        Some(d) => Some(d),
                        None => {
                            // Counter went backwards: not a negative wattage,
                            // a reset. Never bill it to anyone.
                            resets += 1;
                            None
                        }
                    }
                }
                _ => None,
            };
            self.prev_energy.insert(pid, (ru.energy_nj, ru.start_abstime, tick));

            if let Some(d) = delta {
                let watts_raw = (d as f64 / 1e9) / elapsed;
                measured += 1;
                if d == 0 {
                    zero += 1;
                }
                attributed_w += watts_raw;
                deltas.insert(
                    pid,
                    EnergyDelta {
                        delta_nj: d,
                        watts_raw: watts_raw as f32,
                    },
                );
            }
        }

        // Bound the map exactly like `prev_proc_cpu`: 10-tick eviction.
        self.prev_energy
            .retain(|_, &mut (_, _, seen)| tick.saturating_sub(seen) < 10);

        EnergySample {
            rusage_version: Some(6),
            unavailable: None,
            window_s: elapsed,
            deltas,
            attributed_w,
            processes_total,
            processes_measured: measured,
            unreadable_pids: unreadable,
            counter_resets: resets,
            zero_energy_pids: zero,
        }
    }

    /// Platforms without libproc have no `proc_pid_rusage`: the block is
    /// reported as unavailable rather than silently zeroed.
    #[cfg(not(target_os = "macos"))]
    fn sample_energy(&mut self) -> EnergySample {
        EnergySample::unavailable(
            None,
            self.sys.processes().len(),
            "proc_pid_rusage is macOS-only — per-process energy unavailable on this platform",
        )
    }

    /// Platforms without libproc keep sysinfo's own (interval-based on
    /// Windows) figure — behaviour there is unchanged. `cpu_pct` is always
    /// `Some`, `threads` always `null`.
    #[cfg(not(target_os = "macos"))]
    fn sample_proc_cpu(&mut self) -> HashMap<i32, ProcCpu> {
        self.sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                (
                    pid.as_u32() as i32,
                    ProcCpu {
                        cpu_pct: Some(p.cpu_usage()),
                        perm: true,
                        threads: None,
                        resident: None,
                    },
                )
            })
            .collect()
    }
}

/// One PID's result from a `sample_proc_cpu` pass — what the row builders read.
struct ProcCpu {
    /// Instantaneous share of one core, or `null` (first sighting / unreadable).
    cpu_pct: Option<f32>,
    /// `false` when the OS refused inspection of this PID.
    perm: bool,
    /// `pti_threadnum` when readable.
    threads: Option<i32>,
    /// `pti_resident_size` (bytes) when readable.
    resident: Option<u64>,
}

/// One PID's energy for this tick.
#[derive(Clone)]
struct EnergyDelta {
    /// `ri_energy_nj` delta (nanojoules).
    delta_nj: u64,
    /// Unrounded watts; the payload carries the 3-dp rounding, but shares are
    /// computed from these so they sum to 100 % across measured processes.
    watts_raw: f32,
}

/// One `sample_energy` pass — per-PID deltas plus the counts the `energy`
/// block reports. Shared by both platform implementations so `Snapshot` always
/// carries the block.
#[derive(Clone)]
struct EnergySample {
    rusage_version: Option<u8>,
    /// `Some(reason)` when per-process energy could not be measured at all.
    unavailable: Option<String>,
    window_s: f64,
    deltas: HashMap<i32, EnergyDelta>,
    /// Σ unrounded watts over every measured PID (not just the top 25).
    attributed_w: f64,
    processes_total: usize,
    processes_measured: usize,
    unreadable_pids: usize,
    counter_resets: usize,
    zero_energy_pids: usize,
}

impl EnergySample {
    fn unavailable(version: Option<u8>, processes_total: usize, reason: &str) -> Self {
        Self {
            rusage_version: version,
            unavailable: Some(reason.to_string()),
            window_s: 0.0,
            deltas: HashMap::new(),
            attributed_w: 0.0,
            processes_total,
            processes_measured: 0,
            unreadable_pids: 0,
            counter_resets: 0,
            zero_energy_pids: 0,
        }
    }

    /// Freeze this tick into the serialized `energy` block.
    ///
    /// `package_w` / `unaccounted_w` / `over_attribution_pct` are left `None`
    /// because no unprivileged source for whole-machine package power exists on
    /// this host. They are omitted from the JSON entirely rather than filled
    /// with a made-up number that would make the books "close".
    fn into_block(self) -> Energy {
        let measured = self.unavailable.is_none();
        Energy {
            rusage_version: self.rusage_version,
            attribution: if measured {
                "processes_only".to_string()
            } else {
                "unavailable".to_string()
            },
            source: if measured {
                "proc_pid_rusage(RUSAGE_INFO_V6).ri_energy_nj, diffed per tick".to_string()
            } else {
                "unavailable".to_string()
            },
            window_s: round3_f64(self.window_s),
            attributed_w: round3_f64(self.attributed_w),
            package_w: None,
            unaccounted_w: None,
            over_attribution_pct: None,
            processes_total: self.processes_total,
            processes_measured: self.processes_measured,
            unreadable_pids: self.unreadable_pids,
            counter_resets: self.counter_resets,
            zero_energy_pids: self.zero_energy_pids,
            gpu_time: "unavailable_no_rusage_field".to_string(),
            note: Some(match self.unavailable {
                Some(reason) => reason,
                None => "no unprivileged package-power source on this host (SMC needs root, \
                         IOReport is private API, powermetrics is a root daemon), so no \
                         package_w / unaccounted_w is claimed: attributed_w is a measured \
                         floor on machine spend, not the total"
                    .to_string(),
            }),
            units: EnergyUnits {
                energy: "nJ".to_string(),
                power: "W".to_string(),
                share: "%".to_string(),
            },
        }
    }
}

fn round3_f64(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// One output row, fed by the sampling pass where possible.
fn proc_row(p: &Process, proc_cpu: &HashMap<i32, ProcCpu>, energy: &EnergySample) -> ProcRow {
    let pid = p.pid().as_u32() as i32;
    let c = proc_cpu.get(&pid);
    // Energy fields are `None` unless this exact PID was measured twice with
    // an unchanged start time (see `sample_energy`). A first sighting is null,
    // never 0.
    let (energy_nj_delta, watts, energy_share_pct) = match energy.deltas.get(&pid) {
        Some(d) => {
            let share = if energy.attributed_w > 0.0 {
                Some(round2(d.watts_raw as f64 / energy.attributed_w * 100.0))
            } else {
                None
            };
            (Some(d.delta_nj), Some(round3(d.watts_raw as f64)), share)
        }
        None => (None, None, None),
    };
    ProcRow {
        pid,
        name: p.name().to_string_lossy().chars().take(24).collect(),
        cpu_pct: c.and_then(|c| c.cpu_pct),
        perm: c.map(|c| c.perm).unwrap_or(false),
        threads: c.and_then(|c| c.threads),
        // Prefer the resident size we just read from `proc_taskinfo`
        // (same metric sysinfo reports); fall back to sysinfo for rows we
        // could not inspect.
        mem_mb: c
            .and_then(|c| c.resident)
            .map(|r| r as f64 / MB)
            .unwrap_or_else(|| p.memory() as f64 / MB),
        energy_nj_delta,
        watts,
        energy_share_pct,
    }
}

fn round3(v: f64) -> f32 {
    ((v * 1000.0).round() / 1000.0) as f32
}

fn round2(v: f64) -> f32 {
    ((v * 100.0).round() / 100.0) as f32
}

// ─── macOS: instantaneous per-process CPU via libproc ──────────────────
//
// `sysinfo`'s per-process CPU on macOS is a lifetime average — the `ps`
// "%cpu" lie this task exists to kill. We call `proc_pidinfo(2)` per tick
// and diff `(pti_total_user + pti_total_system)` ourselves.
//
// UNITS (verified on this machine, Apple Silicon M2 — the brief says these
// fields are nanoseconds, they are not): they come back in mach absolute
// time ticks. `mach_timebase_info()` reports numer=125 denom=3 here, i.e.
// 1 tick = 125/3 ns, and `raw * numer / denom` reproduces
// `getrusage(RUSAGE_SELF)` to the microsecond for a 3 s spin:
//   getrusage user+sys = 2.895912 s
//   taskinfo  raw*125/3 = 2.895910 s     (raw/1e9 would be 0.069502 s)
// We therefore convert with the timebase instead of assuming 1 tick = 1 ns,
// which keeps the arithmetic correct on Intel Macs too (timebase 1/1).
#[cfg(target_os = "macos")]
mod libproc {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    /// Flavor value of `PROC_PIDTASKINFO` (<sys/proc_info.h> line 721).
    const PROC_PIDTASKINFO: i32 = 4;

    /// Mirror of `struct proc_taskinfo` from <sys/proc_info.h>:
    /// 6 × u64 + 12 × i32 = 96 bytes. `proc_pidinfo` only fills a buffer of
    /// exactly this size (it returns 96 on success, 0 on failure).
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct ProcTaskInfo {
        pti_virtual_size: u64,
        pti_resident_size: u64,
        pti_total_user: u64,
        pti_total_system: u64,
        pti_threads_user: u64,
        pti_threads_system: u64,
        pti_policy: i32,
        pti_faults: i32,
        pti_pageins: i32,
        pti_cow_faults: i32,
        pti_messages_sent: i32,
        pti_messages_received: i32,
        pti_syscalls_mach: i32,
        pti_syscalls_unix: i32,
        pti_csw: i32,
        pti_threadnum: i32,
        pti_numrunning: i32,
        pti_priority: i32,
    }

    /// Layout guard: if the SDK ever changes the struct, this fails to compile
    /// instead of silently reading garbage.
    const _: () = assert!(std::mem::size_of::<ProcTaskInfo>() == 96);

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut c_void,
            buffersize: i32,
        ) -> i32;
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
        /// <libproc.h:111> — `int proc_pid_rusage(int pid, int flavor, rusage_info_t *buffer)`
        /// where `typedef void *rusage_info_t` (<sys/resource.h:200>), i.e. the
        /// buffer argument points straight at the caller's struct.
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut c_void) -> i32;
    }

    #[repr(C)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    /// What one `proc_pidinfo(PROC_PIDTASKINFO)` probe read out of a PID.
    pub(super) struct TaskTimes {
        /// `pti_total_user + pti_total_system` in **mach ticks**, not ns
        /// (see the module comment for the measured evidence).
        pub(super) total_ticks: u64,
        /// `pti_resident_size` in bytes.
        pub(super) resident: u64,
        /// `pti_threadnum`.
        pub(super) threads: i32,
    }

    /// ticks → nanoseconds factor. Queried once; the timebase is a constant
    /// for the life of the process.
    fn timebase() -> (f64, f64) {
        static TB: OnceLock<(f64, f64)> = OnceLock::new();
        *TB.get_or_init(|| {
            let mut tb = MachTimebaseInfo { numer: 0, denom: 0 };
            unsafe { mach_timebase_info(&mut tb) };
            (tb.numer as f64, tb.denom.max(1) as f64)
        })
    }

    /// Convert a mach-tick delta to nanoseconds (used on the *delta*, where
    /// the numbers are small, so float rounding stays negligible).
    pub(super) fn ticks_to_ns(ticks: u64) -> f64 {
        let (numer, denom) = timebase();
        ticks as f64 * numer / denom
    }

    /// One `proc_pidinfo` probe. `None` = we may not inspect this PID
    /// (root-owned / kernel → errno EPERM, or it exited → ESRCH).
    pub(super) fn task_times(pid: i32) -> Option<TaskTimes> {
        let mut ti = ProcTaskInfo::default();
        let want = std::mem::size_of::<ProcTaskInfo>() as i32;
        let got = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTASKINFO,
                0,
                &mut ti as *mut ProcTaskInfo as *mut c_void,
                want,
            )
        };
        if got != want {
            return None;
        }
        Some(TaskTimes {
            total_ticks: ti.pti_total_user.saturating_add(ti.pti_total_system),
            resident: ti.pti_resident_size,
            threads: ti.pti_threadnum,
        })
    }

    // ─── Per-process energy: proc_pid_rusage(RUSAGE_INFO_V6) ────────────
    //
    // Flavor numbers from <sys/resource.h:190-193>; the struct from :368-418.
    // `ri_energy_nj` lives ONLY in v6 (v0–v4 have no energy member at all, and
    // v5 has the undocumented `ri_billed_energy` / `ri_serviced_energy`), so
    // there is no meaningful "v4 fallback" for energy — only a recorded gap.
    const RUSAGE_INFO_V4: i32 = 4;
    const RUSAGE_INFO_V6: i32 = 6;

    /// Mirror of `struct rusage_info_v6`: `ri_uuid[16]` + 47 × u64 +
    /// `ri_reserved[9]` = 464 bytes. Field order is copied from the header, not
    /// from memory, and the offsets are asserted below.
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct RusageV6 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
        ri_child_user_time: u64,
        ri_child_system_time: u64,
        ri_child_pkg_idle_wkups: u64,
        ri_child_interrupt_wkups: u64,
        ri_child_pageins: u64,
        ri_child_elapsed_abstime: u64,
        ri_diskio_bytesread: u64,
        ri_diskio_byteswritten: u64,
        ri_cpu_time_qos_default: u64,
        ri_cpu_time_qos_maintenance: u64,
        ri_cpu_time_qos_background: u64,
        ri_cpu_time_qos_utility: u64,
        ri_cpu_time_qos_legacy: u64,
        ri_cpu_time_qos_user_initiated: u64,
        ri_cpu_time_qos_user_interactive: u64,
        ri_billed_system_time: u64,
        ri_serviced_system_time: u64,
        ri_logical_writes: u64,
        ri_lifetime_max_phys_footprint: u64,
        ri_instructions: u64,
        ri_cycles: u64,
        ri_billed_energy: u64,
        ri_serviced_energy: u64,
        ri_interval_max_phys_footprint: u64,
        ri_runnable_time: u64,
        ri_flags: u64,
        ri_user_ptime: u64,
        ri_system_ptime: u64,
        ri_pinstructions: u64,
        ri_pcycles: u64,
        ri_energy_nj: u64,
        ri_penergy_nj: u64,
        ri_secure_time_in_system: u64,
        ri_secure_ptime_in_system: u64,
        ri_neural_footprint: u64,
        ri_lifetime_max_neural_footprint: u64,
        ri_interval_max_neural_footprint: u64,
        ri_reserved: [u64; 9],
    }

    /// Layout guards. These are the reason the brief's suggested offsets were
    /// corrected in code: `ri_proc_start_abstime` is byte 80, not 88.
    const _: () = assert!(std::mem::size_of::<RusageV6>() == 464);
    const _: () = assert!(std::mem::offset_of!(RusageV6, ri_proc_start_abstime) == 80);
    const _: () = assert!(std::mem::offset_of!(RusageV6, ri_diskio_bytesread) == 144);
    const _: () = assert!(std::mem::offset_of!(RusageV6, ri_energy_nj) == 336);
    const _: () = assert!(std::mem::offset_of!(RusageV6, ri_penergy_nj) == 344);

    /// What one `proc_pid_rusage(RUSAGE_INFO_V6)` probe read out of a PID.
    pub(super) struct Rusage {
        /// `ri_energy_nj` — lifetime nanojoules billed to this process.
        pub(super) energy_nj: u64,
        /// `ri_proc_start_abstime` — the identity token that distinguishes a
        /// recycled PID from the process that used to own the number.
        pub(super) start_abstime: u64,
    }

    /// One `proc_pid_rusage` probe. `None` = refused (EPERM for root-owned
    /// PIDs) or the process is gone (ESRCH).
    pub(super) fn rusage(pid: i32) -> Option<Rusage> {
        let mut ri = RusageV6::default();
        let rc = unsafe {
            proc_pid_rusage(
                pid,
                RUSAGE_INFO_V6,
                &mut ri as *mut RusageV6 as *mut c_void,
            )
        };
        if rc != 0 {
            return None;
        }
        Some(Rusage {
            energy_nj: ri.ri_energy_nj,
            start_abstime: ri.ri_proc_start_abstime,
        })
    }

    /// Which `proc_pid_rusage` flavor this kernel actually accepts, probed
    /// once against our own PID and cached. `6` = energy available, `4` = the
    /// documented fallback (no energy field), `0` = neither.
    pub(super) fn rusage_version() -> u8 {
        static VERSION: OnceLock<u8> = OnceLock::new();
        *VERSION.get_or_init(|| {
            let self_pid = std::process::id() as i32;
            if rusage(self_pid).is_some() {
                return RUSAGE_INFO_V6 as u8;
            }
            // v4 is the fallback we would record in the JSON; it is only
            // relevant for *recording the gap*, since it has no ri_energy_nj.
            let mut scratch = [0_u64; 64];
            let rc4 = unsafe {
                proc_pid_rusage(
                    self_pid,
                    RUSAGE_INFO_V4,
                    scratch.as_mut_ptr() as *mut c_void,
                )
            };
            if rc4 == 0 { RUSAGE_INFO_V4 as u8 } else { 0 }
        })
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
