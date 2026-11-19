# What a computer has — capability inventory (source of truth for the registry)

Rule: every row is a *capability*, probed at runtime, never assumed. A row
that cannot be read on a platform reports `unavailable` + reason. Platforms
are adapters; the registry is the contract.

Columns: **macOS** (this machine, verified = ✔ probed live during the build,
u = unprivileged, r = root), **Windows** (source family), **Linux** (source).

## 1. CPU

| Capability | Unit | macOS source | Win | Linux |
|---|---|---|---|---|
| Total utilisation | % | host_processor_info (✔ u) | PDH `\Processor(_Total)\% Processor Time` | `/proc/stat` |
| Per-core utilisation | % per core | host_processor_info (✔ u) | PDH per-instance | `/proc/stat` per cpu |
| Per-core discovery (count) | n | `hw.perflevel0/1.logicalcpu` (✔ u) | `NumberOfLogicalProcessors` | `/sys/devices/system/cpu` |
| Cluster split (P/E) | % each | perflevel + per-core aggregation (✔ u) | hybrid info (12th gen+) | `cpu_capacity` |
| Frequency (per cluster) | GHz | `sysctl hw.cpufrequency` / IOReport (r for accurate) | `% Processor Performance` × base | `scaling_cur_freq` |
| CPU pressure / stall | % | dispatch gauge proxy + load (u, derived) | `System\Processor Queue Length` | PSI `cpu.pressure` |
| Load average 1/5/15 | n | `getloadavg` (✔ u) | PDH queue length | `/proc/loadavg` |
| Die temperature | °C | powermetrics thermal (r) | WMI thermal zone / LHM | hwmon `coretemp` |
| Per-cluster temperature | °C | powermetrics thermal (r) | vendor EC | hwmon |
| Thermal pressure | level | `pmset -g therm` / powermetrics (u/r) | `ThermalZoneInformation` | `/sys/class/thermal` |
| Fan RPM | rpm | SMC via powermetrics (r) | WMI `Win32_Fan` | hwmon `fan*_input` |

## 2. GPU

| Capability | Unit | macOS source | Win | Linux |
|---|---|---|---|---|
| Utilisation | % | IOAccelerator `Device Utilization %` (✔ u) | PDH GPU Engine | `/sys/class/drm` busy |
| Renderer / Tiler utilisation | % | `PerformanceStatistics` (✔ u) | PDH GPU Engine per engine | — |
| Dedicated/shared memory used | MB | IOAccelerator (✔ u) | `GPU Adapter Memory` | `amdgpu`/`nvidia-smi` |
| GPU temperature | °C | powermetrics (r) | vendor | hwmon |
| GPU frequency | GHz | ioreg PerformanceStatistics (✔ u) | PDH | `pp_dpm_sclk` |
| Framerate (compositor) | fps | CoreVideo/WD (u, low value) | — | — |
| Neural Engine / NPU util | % | powermetrics `ANEMetrics` (r) | Intel NPU PDH / AMD XDNA | `accel` sysfs |
| NPU power | mW | powermetrics (r) | — | — |

## 3. Memory

| Capability | Unit | macOS source | Win | Linux |
|---|---|---|---|---|
| Used / total | MB | vm_statistics64 (✔ u) | `Win32_OperatingSystem` | `/proc/meminfo` |
| Wired / active / inactive / compressed | MB | vm_statistics64 (✔ u) | performance counters | meminfo |
| **Memory pressure level** | Normal/Warn/Critical (+%) | `kern.memorystatus_vm_pressure_level` (✔ u, value 1 today) | `Memory\% Committed Bytes` | PSI `memory.pressure` |
| Swap used / out rate | MB, MB/s | `sysctl vm.swapusage` (✔ u) | pagefile counters | `swap` in meminfo |
| Memory temperature | °C | powder/SMC (r) | — | hwmon |

## 4. Storage

| Capability | Unit | macOS source | Win | Linux |
|---|---|---|---|---|
| Per-volume free / used / % | GB, % | `statfs` (✔ u) | `Win32_LogicalDisk` | `statvfs` |
| **Read / write throughput** | B/s | IOBlockStorageDriver cumulative bytes, differentiated (✔ u) | PDH PhysicalDisk | `/proc/diskstats` |
| IOPS (read/write) | ops/s | same counters, differentiated (✔ u) | PDH | diskstats |
| Latency | ms | `Statistics Latency Time` deltas (u) | PDH Avg. Disk sec | iostat |
| Per-drive (not just mount) | rows | ioreg per IOBlockStorageDriver (✔ u) | `Win32_DiskDrive` | `/sys/block` |
| SSD/NVMe temperature | °C | `smartctl`/SMC (r) | NVMe WMI | `nvme smart-log` |
| SMART health | status | smartctl (r) | `Win32_DiskDrive.Status` | smartctl |
| **Storage pressure** | % used + free trend | derived from free-ratio (✔ u) | same | same |

## 5. Network

| Capability | Unit | macOS source | Win | Linux |
|---|---|---|---|---|
| Per-interface rx / tx rate | B/s | `if_msghdr2` / netstat (✔ u) | PDH Network Interface | `/proc/net/dev` |
| Packets, errors, drops | n/s | same (✔ u) | PDH | netdev |
| Wi-Fi signal (RSSI), noise, tx rate, channel, band | dBm, Mbps | `airport -I` (deprecated) / CoreWLAN (u) | `netsh wlan show interfaces` | `iw dev link` |
| Wi-Fi temperature | °C | SMC (r) | — | — |
| Active interface + IP + link speed | text | SystemConfiguration (u) | WMI | ethtool |

## 6. Battery / power

| Capability | Unit | macOS source | Win | Linux |
|---|---|---|---|---|
| Charge % | % | ioreg AppleSmartBattery (✔ u) | `Win32_Battery` | `/sys/class/power_supply` |
| **Time remaining / to full** | min | `AvgTimeToEmpty` / `AvgTimeToFull` (✔ u, 357 min live) | EstimatedChargeRemaining + runtime | `energy_now/power_now` |
| Battery draw | W | V × A (✔ u — 4.37 W live) | rate counter | power_now |
| Voltage / amperage | V, mA | ioreg (✔ u) | — | power_supply |
| Cycles / health / condition | n, %, text | ioreg (✔ u, 350 cycles / 81.2 %) | `Win32_Battery` design vs full | `cycle_count` |
| Cell voltages | mV each | `BatteryData.CellVoltage` (✔ u) | — | per-cell sysfs |
| Battery temperature | °C | ioreg `Temperature` (✔ u when key present) | — | temp sysfs |
| Charging state / adapter W | text, W | ioreg AdapterDetails (✔ u) | WMI | power_supply |
| Package / CPU / GPU / ANE power | mW | powermetrics (r) | vendor | RAPL `energy_uj` |
| Power rails (V, A) | V, mA | SMC (r for full set) | — | — |

## 7. Processes / apps

| Capability | Unit | macOS source | Win | Linux |
|---|---|---|---|---|
| Per-process CPU | % | proc_pidinfo (✔ u) | PDH / ETW | `/proc/<pid>/stat` |
| Per-process memory (RSS) | MB | proc_pidinfo (✔ u) | `Win32_Process` | `/proc/<pid>/status` |
| **Per-process energy attribution** | mW | rusage_info_v6 (✔ u) | ETW energy estimation | RAPL + cgroup (approx) |
| Process count, threads | n | sysinfo / proc_listallpids (✔ u) | WMI | /proc |
| Disk I/O per process | B/s | proc_pid_rusage disk fields (✔ u) | ETW | `io` in /proc/<pid> |

## 8. System

| Capability | Unit | macOS source |
|---|---|---|
| Uptime, boot time | s | `kern.boottime` (✔ u) |
| Model / chip / core config | text | `sysctl machdep.cpu.brand_string`, `hw.model` (✔ u) |
| OS version, build | text | `sw_vers` (✔ u) |
| Kernel / metal versions | text | sysctl |
| Backing store / disk sleep / power nap | flags | pmset -g (u) |

---

## How this becomes the dashboard (Power BI semantics)

**Model:** dimensions × measures.
- Dimensions: `time` (range + bucket), `group` (cpu/gpu/…), `app`, `core`,
  `interface`, `volume`, `battery cell`.
- Measures: `avg`, `max`, `min`, `p95`, `sum`, `count`, `delta vs prev window`,
  `coverage %`.

**Slicers (filters):** one global time-range slicer in the shell header; a
group slicer per view; an app slicer on Apps/Analytics.

**Cross-filtering:** clicking a chart segment (a spike, an app row, a core)
filters the other visuals on that view — exactly the Power BI interaction
model. Clicking empty space clears the filter. Filter state shows as chips
under the header so it is never invisible.

**Drill-through:** every overview card → its section; every section row
(app, core, volume) → a detail drawer with that entity's own time series.

**Pressure first:** CPU pressure, memory pressure, and storage pressure get
first-class gauges on Overview (iStat parity) because pressure, not raw
utilisation, is what predicts a slow machine.
