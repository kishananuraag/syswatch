# Sensor Backend Research — syswatch-core (Task B7)

**Scope:** research only. No code in `src/` or `dashboard/` was modified.
**Machine:** HP Laptop 15s-fq5xxx, Windows 11, Intel Iris Xe Graphics (integrated GPU), NVMe SSD.

---

## 1. Current temperature source in syswatch-core

- `src/stats.rs` uses the **`sysinfo` crate v0.33** (`Components::new_with_refreshed_list()`).
- On refresh it iterates components, preferring labels containing `cpu` / `package` / `tctl`, otherwise falling back to the hottest reading. Result is collapsed into a single `Option<f32>` (`Snapshot.temperature`).
- `src/config.rs` gates this behind `temp` (default `false`; CLI/config `temperature = true` to enable).
- On Windows, sysinfo's component backend queries WMI thermal zones
  (`MSAcpi_ThermalZoneTemperature` under `root/wmi`). That class is
  **restricted**: it requires the process to run elevated (Administrator),
  otherwise the query is denied and no components are returned.

### LibreHardwareMonitor (LHM) comparison

| Aspect | sysinfo (current) | LibreHardwareMonitor |
|---|---|---|
| CPU package temp | Only via ACPI thermal zone; often missing/denied on laptops | Yes — reads Intel MSR `DTS`/`IA32_THERM_STATUS` via ring-0 kernel driver |
| Per-core temps | No | Yes |
| GPU temp | No (integrated Xe partially supported) | Yes for most discrete GPUs; Iris Xe support is partial |
| NVMe/SSD temp | No | Yes — reads SMART health data directly |
| Motherboard/super-I/O | No | Yes — probes LPC/eSIO chips (ITE, Nuvoton, etc.) |
| Elevation | Needed for WMI thermal zones | Needed to load its kernel driver (`WinRing0`/`WinRing0x64.sys`) |
| Rust integration | Native crate | Options: (a) run LHM.exe and consume WMI namespace `root/LibreHardwareMonitor`, (b) `lhm` Rust bindings crate (wraps .NET DLL — heavy), (c) reimplement in Rust (large effort) |

## 2. What is actually detectable on THIS machine right now

Probed via PowerShell CIM (2026-02):

- `Get-CimInstance Win32_TemperatureProbe` → **0 instances returned** (typical on modern laptops; SMBIOS temperature probe entries are usually absent).
- `Get-CimInstance -Namespace root/wmi MSAcpi_ThermalZoneTemperature` → **Access denied (HRESULT 0x80041003)** from a non-elevated shell. This confirms why sysinfo currently yields no usable temperature here.

**Honest conclusion:** with the current unprivileged process, **no sensors are detectable** — see `docs/sensors.json` (empty schema). Expected results if re-probed:

| Sensor | Elevated shell (ACPI/LHM) | Notes |
|---|---|---|
| CPU package | Likely yes via thermal zone; reliable via LHM | Intel Core i5/i7 12th-gen (Alder Lake) — LHM supports MSR read |
| GPU | Not separately exposed (iGPU shares package) | LHM partial support for Iris Xe |
| NVMe | Only via LHM / SMART (`Get-Disk`-level SMART not exposed by default) | Composite temp available via `StorageReliabilityData` or LHM |
| Board | Usually one ACPI zone at best | Depends on HP EC/SIO; LHM may find an ITE chip |

## 3. Proposed sensors.json emission

```json
{
  "timestamp": "2026-02-14T12:00:00Z",
  "sensors": [
    {
      "id": "cpu_package",
      "label": "CPU Package",
      "type": "temperature",
      "value": 52.0,
      "unit": "\u00b0C",
      "source": "lhm"
    }
  ]
}
```

Field rules:

- `id`: stable slug (`cpu_package`, `gpu_core`, `nvme_ssd0`, `board_acpi0`).
- `type`: one of `temperature` (extensible later: `fan_rpm`, `voltage`).
- `value`: number, or omit sensor entry entirely when unavailable.
- `source`: `sysinfo` \| `lhm` \| `wmi`.
- **Show-only-detected:** dashboard should render only entries present in the array; an empty array means hide the panel entirely (matches existing `temp: false` gating).

Recommended implementation path: keep `sysinfo` as fallback, add optional LHM bridge (spawn `LibreHardwareMonitor.exe` with WMI server enabled + query `root/LibreHardwareMonitor` via the `wmi` crate). Avoids unsafe driver code in Rust and keeps LGPL dependency out of the binary (LHM runs as a separate process).

## 4. Effort & risk

- **Effort: medium.** Schema + collector abstraction ~small; the LHM process/WMI bridge + elevation handling + tests push it to medium (~1–2 days).
- **Risks:**
  - **Licensing:** LHM is **LGPL-2.1 (Mozilla Public License for newer versions)** — fine if used as an external process; linking the .NET DLL into a Rust binary via `lhm` bindings is murkier and adds heavy .NET runtime dependency.
  - **Elevation:** both current sysinfo path and LLM require Administrator; syswatch would need either elevation, a privileged helper, or graceful degradation (hide panel).
  - **Driver weight:** LHM loads a signed kernel driver (WinRing0); some AV/EDR flags it.
  - **HP consumer laptops** sometimes expose few/no Super-I/O sensors even under LHM — must verify empirically before committing to the design.
