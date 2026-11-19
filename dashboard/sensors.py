#!/usr/bin/env python3
"""Sensor registry (T14 addendum A/B) — the shape, not a Mac-only assumption.

    Sensor = {id, group, label, value, unit, available, reason, source, precision,
              root_gated}

Every row is a capability probed at runtime.  A row that cannot be read on this
platform reports `available: false` + `reason` — never a placeholder number and
never 0.  Groups follow docs/machine-capability-inventory.md:

    cpu · gpu · memory · storage · network · battery · thermal · power_rails · clocks

Two tiers of source:

  * **unprivileged** — psutil / sysctl / ioreg, read by the dashboard itself.
  * **root** — the T15 privileged daemon's JSON file (powermetrics).  The
    dashboard only READS it; when it is absent the row is `root_gated: true`
    and the group renders as an amber "needs root" row instead of a number.

The registry never spawns a probe itself: the caller passes the live values it
already sampled (the 1 s snapshot, the 5 s GPU/disk rings, the 5 s power ring,
the root file), so /api/sensors costs a dict walk and cannot become a second
sampling loop.

Adding a platform later = one adapter class with the same probe() contract.
"""
import sys

GROUP_ORDER = ("cpu", "gpu", "memory", "storage", "network", "battery",
               "thermal", "power_rails", "clocks")
GROUP_LABELS = {
    "cpu": "CPU",
    "gpu": "GPU",
    "memory": "Memory",
    "storage": "Storage",
    "network": "Network",
    "battery": "Battery / power",
    "thermal": "Temperatures",
    "power_rails": "Power rails",
    "clocks": "Clocks",
}
# Cluster label prefixes: the platform name is kept, but a 1-character badge
# makes a per-core row readable ("P3" / "E1") without inventing anything.
CLUSTER_BADGE = {"performance": "P", "efficiency": "E"}


def sensor(sid, group, label, value=None, unit=None, source=None,
           available=True, reason=None, precision=None, root_gated=False):
    return {"id": sid, "group": group, "label": label, "value": value,
            "unit": unit, "available": bool(available), "reason": reason,
            "source": source, "precision": precision,
            "root_gated": bool(root_gated)}


def _num(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


class Adapter:
    """One adapter per platform. probe() returns a flat list of Sensor dicts."""

    platform = "unknown"

    def probe(self):
        raise NotImplementedError

    # -- helpers shared by every adapter --------------------------------
    def rows(self, group, pairs):
        """pairs = [(id, label, value, unit, source, precision, reason)]."""
        out = []
        for item in pairs:
            sid, label, value, unit, source, precision, reason = item
            ok = _num(value) or (isinstance(value, str) and value != "")
            out.append(sensor(sid, group, label, value if ok else None, unit,
                              source, ok, None if ok else reason, precision))
        return out

    def root_rows(self, group, pairs, missing_reason):
        """Rows that only exist with the root daemon; all-or-nothing per group."""
        out = []
        for (sid, label, value, unit, source, precision) in pairs:
            ok = _num(value)
            out.append(sensor(sid, group, label, value if ok else None, unit,
                              source, ok, None if ok else missing_reason,
                              precision, root_gated=not ok))
        return out


class MacOSAdapter(Adapter):
    """macOS: ioreg + sysctl + psutil (unprivileged) + the T15 root file."""

    platform = "macos"

    def __init__(self, ctx):
        self.ctx = ctx or {}

    # -- root tier ------------------------------------------------------
    def _root(self):
        r = self.ctx.get("root") or {}
        return (r.get("data") or {}) if r.get("available") else {}

    def _root_reason(self):
        r = self.ctx.get("root") or {}
        return r.get("reason") or "needs root — privileged daemon sample absent"

    def probe(self):
        out = []
        for fn in (self._cpu, self._gpu, self._memory, self._storage,
                   self._network, self._battery, self._thermal,
                   self._power_rails, self._clocks):
            try:
                out.extend(fn())
            except Exception as e:  # a broken probe never kills the catalogue
                out.append(sensor(fn.__name__.lstrip("_") + ".error",
                                  fn.__name__.lstrip("_"), "probe failed",
                                  available=False, reason=repr(e),
                                  source="MacOSAdapter"))
        return out

    # -- groups ---------------------------------------------------------
    def _cpu(self):
        snap = self.ctx.get("snap") or {}
        cpu = snap.get("cpu") or {}
        rows = [
            ("cpu.total", "Total utilisation", cpu.get("total_pct"), "%",
             "host_processor_info via psutil", 1, "cpu sample not ready"),
            ("cpu.load1", "Load average 1 m", (snap.get("load") or {}).get("one"),
             "n", "getloadavg", 2, "load average unavailable"),
            ("cpu.cores", "Logical cores", cpu.get("cores"), "n",
             "hw.ncpu", 0, "core count unavailable"),
        ]
        out = self.rows("cpu", rows)
        per_core = cpu.get("per_core_pct") or []
        clusters = self.ctx.get("clusters") or []
        root = self._root()
        cluster_temps = {}
        for i, c in enumerate(root.get("cpu_cluster_temps_c") or []):
            if isinstance(c, dict) and _num(c.get("c")):
                cluster_temps[i] = c.get("c")
        if clusters and per_core:
            i = 0
            for ci, cl in enumerate(clusters):
                name = cl.get("name") or "cluster %d" % (ci + 1)
                badge = CLUSTER_BADGE.get(name, name[:1].upper())
                # T14C #1: clusters carry the FIRST logical CPU index
                # (efficiency cluster first, performance after). Fall back to the
                # running offset only when a platform omits `index`.
                base = cl.get("index")
                if not isinstance(base, int):
                    base = i
                slice_ = per_core[base:base + cl["cores"]]
                if slice_:
                    avg = round(sum(slice_) / len(slice_), 1)
                    label = "%s cluster utilisation" % name.capitalize()
                    out.append(sensor("cpu.%s.util" % name, "cpu", label, avg,
                                      "%", "mean of this cluster's per-core delta",
                                      1))
                    if _num(cluster_temps.get(ci)):
                        out.append(sensor("cpu.%s.temp" % name, "cpu",
                                          "%s cluster temperature" % name.capitalize(),
                                          cluster_temps[ci], "°C",
                                          "powermetrics thermal (root)", 1,
                                          root_gated=False))
                for n in range(cl["cores"]):
                    idx = base + n
                    if idx >= len(per_core):
                        break
                    out.append(sensor(
                        "cpu.%s.core%d" % (name, n + 1), "cpu",
                        "%s (%s%d)" % ("%s core %d" % (name.capitalize(), n + 1),
                                       badge, n + 1),
                        per_core[idx], "%", "per-core delta, logical CPU order", 1))
                i = base + cl["cores"]
        elif per_core:
            for n, v in enumerate(per_core):
                out.append(sensor("cpu.core%d" % (n + 1), "cpu",
                                  "CPU core %d" % (n + 1), v, "%",
                                  "per-core delta", 1))
        return out

    def _gpu(self):
        gpu = self.ctx.get("gpu") or {}
        rows = [
            ("gpu.util", "Device utilisation", gpu.get("util_pct"), "%",
             "ioreg IOAccelerator Device Utilization %", 0,
             gpu.get("reason") or "GPU probe not ready"),
            ("gpu.renderer", "Renderer utilisation", gpu.get("renderer_pct"), "%",
             "IOAccelerator Renderer Utilization %", 0, "renderer % absent"),
            ("gpu.tiler", "Tiler utilisation", gpu.get("tiler_pct"), "%",
             "IOAccelerator Tiler Utilization %", 0, "tiler % absent"),
            ("gpu.mem_used", "Graphics memory in use", gpu.get("mem_used_mb"),
             "MB", "IOAccelerator In use system memory", 1, "not exposed"),
            ("gpu.cores", "GPU cores", gpu.get("cores"), "n",
             "ioreg gpu-core-count", 0, "core count absent"),
            ("gpu.freq", "GPU frequency", gpu.get("freq_mhz"), "MHz",
             "powermetrics / IOReport (root)", 0,
             gpu.get("freq_reason") or "no GPU clock in PerformanceStatistics"),
        ]
        out = self.rows("gpu", rows)
        root = self._root()
        ane = root.get("ane_util_pct")
        out.append(sensor(
            "gpu.neural_engine", "gpu", "Neural Engine utilisation",
            ane if _num(ane) else None, "%",
            "powermetrics ANEMetrics (root)",
            available=_num(ane),
            reason=None if _num(ane) else self._root_reason(),
            precision=1, root_gated=not _num(ane)))
        ane_w = root.get("ane_w")
        out.append(sensor(
            "gpu.neural_engine_w", "gpu", "Neural Engine power",
            ane_w if _num(ane_w) else None, "W", "powermetrics (root)",
            available=_num(ane_w),
            reason=None if _num(ane_w) else self._root_reason(),
            precision=2, root_gated=not _num(ane_w)))
        return out

    def _memory(self):
        snap = self.ctx.get("snap") or {}
        ram = snap.get("ram") or {}
        pressure = (snap.get("pressure") or {}).get("memory") or {}
        total = ram.get("total_bytes")
        used = ram.get("used_bytes")
        rows = [
            ("memory.pct", "Used", ram.get("pct"), "%",
             "vm_statistics64 via psutil", 1, "memory sample not ready"),
            ("memory.used", "Used (RAM)",
             round(used / 1048576.0, 1) if _num(used) else None,
             "MB", "vm_statistics64 via psutil", 1, "not exposed"),
            ("memory.total", "Installed",
             round(total / 1048576.0, 1) if _num(total) else None,
             "MB", "vm_statistics64 via psutil", 1, "not exposed"),
            ("memory.pressure", "Pressure level", pressure.get("level"), None,
             "kern.memorystatus_vm_pressure_level", None,
             pressure.get("reason") or "pressure level unreadable"),
            ("memory.swap", "Swap used",
             round((ram.get("swap_used_bytes") or 0) / 1048576.0, 1), "MB",
             "sysctl vm.swapusage via psutil", 1, "swap unavailable"),
        ]
        return self.rows("memory", rows)

    def _storage(self):
        disk = self.ctx.get("disk") or {}
        io = self.ctx.get("disk_io") or {}
        rows = [
            ("storage.free", "Free space", disk.get("free_gb"), "GB",
             "statfs", 2, disk.get("reason") or "free space unavailable"),
            ("storage.used_pct", "Used", disk.get("used_pct"), "%",
             "statfs", 2, "used % unavailable"),
            ("storage.read", "Read throughput", io.get("read_bps"), "B/s",
             "IOBlockStorageDriver Bytes (Read) delta, 5 s", 1,
             io.get("reason") or "no sample yet"),
            ("storage.write", "Write throughput", io.get("write_bps"), "B/s",
             "IOBlockStorageDriver Bytes (Write) delta, 5 s", 1,
             io.get("reason") or "no sample yet"),
            ("storage.read_iops", "Read IOPS", io.get("read_iops"), "ops/s",
             "IOBlockStorageDriver Operations (Read) delta", 2,
             io.get("reason") or "no sample yet"),
            ("storage.write_iops", "Write IOPS", io.get("write_iops"), "ops/s",
             "IOBlockStorageDriver Operations (Write) delta", 2,
             io.get("reason") or "no sample yet"),
        ]
        out = self.rows("storage", rows)
        for vol in self.ctx.get("volumes") or []:
            out.append(sensor("storage.vol.%s" % vol.get("mount"), "storage",
                              str(vol.get("mount")),
                              vol.get("used_pct"), "%",
                              source="statfs per mount", precision=1,
                              available=_num(vol.get("used_pct")),
                              reason=vol.get("reason")))
        root = self._root()
        ssd = root.get("ssd_temp_c")
        out.append(sensor("storage.ssd_temp", "storage", "SSD temperature",
                          ssd if _num(ssd) else None, "°C",
                          "powermetrics / SMC (root)", available=_num(ssd),
                          reason=None if _num(ssd) else self._root_reason(),
                          precision=1, root_gated=not _num(ssd)))
        out.append(sensor(
            "storage.smart", "storage", "SMART health", None, None,
            "smartctl (root)", available=False,
            reason="needs root — smartctl is not permitted unprivileged "
                   "on the internal NVMe", root_gated=True))
        return out

    def _network(self):
        snap = self.ctx.get("snap") or {}
        net = snap.get("net") or {}
        out = self.rows("network", [
            ("network.down", "Total down", net.get("down_bps"), "B/s",
             "if_msghdr2 byte delta via psutil", 1, "network sample not ready"),
            ("network.up", "Total up", net.get("up_bps"), "B/s",
             "if_msghdr2 byte delta via psutil", 1, "network sample not ready"),
        ])
        for iface in net.get("ifaces") or []:
            out.append(sensor("network.%s.down" % iface.get("name"), "network",
                              "%s down" % iface.get("name"),
                              iface.get("down_bps"), "B/s",
                              "per-interface delta", 1))
            out.append(sensor("network.%s.up" % iface.get("name"), "network",
                              "%s up" % iface.get("name"),
                              iface.get("up_bps"), "B/s",
                              "per-interface delta", 1))
        return out

    def _battery(self):
        batt = self.ctx.get("battery") or (self.ctx.get("snap") or {}).get("battery") or {}
        if not batt or (batt.get("pct") is None and batt.get("unavailable_reason")):
            return [sensor("battery.absent", "battery", "Battery", None, None,
                           "AppleSmartBattery", available=False,
                           reason=batt.get("unavailable_reason") or "no battery")]
        secs = batt.get("time_remaining_s")
        rows = [
            ("battery.pct", "Charge", batt.get("pct"), "%",
             "ioreg AppleSmartBattery", 1, "charge unavailable"),
            ("battery.time_remaining",
             "Time to full" if batt.get("charging") else "Time remaining",
             round(secs / 60.0, 0) if _num(secs) else None, "min",
             "AvgTimeToFull / AvgTimeToEmpty", 0,
             "estimating — gauge has no projection yet"),
            ("battery.watts", "Draw", batt.get("battery_w"), "W",
             "V x A from ioreg AppleSmartBattery", 2,
             "voltage/current not exposed"),
            ("battery.voltage", "Voltage", batt.get("voltage_v"), "V",
             "AppleRawBatteryVoltage", 3, "not exposed"),
            ("battery.amperage", "Amperage", batt.get("amperage_ma"), "mA",
             "ioreg Amperage (signed)", 0, "not exposed"),
            ("battery.adapter", "Adapter input", batt.get("adapter_w"), "W",
             batt.get("adapter_source") or "ioreg AppleRawAdapterDetails",
             2, "on battery — AppleRawAdapterDetails is empty while "
                "ExternalConnected is false"),
            ("battery.external", "External power",
             "connected" if batt.get("external_connected") else "on battery",
             None, "ioreg ExternalConnected", None, "not exposed"),
            ("battery.cycles", "Cycles", batt.get("cycles"), "n",
             "ioreg CycleCount", 0, "not exposed"),
            ("battery.health", "Health", batt.get("health_pct"), "%",
             "MaxCapacity / DesignCapacity", 1, "not exposed"),
            ("battery.temp", "Battery temperature", batt.get("temp_c"), "°C",
             "ioreg AppleSmartBattery Temperature (centi-°C, unprivileged)", 2,
             "Temperature key absent on this pack"),
        ]
        return self.rows("battery", rows)

    def _thermal(self):
        """Root-only group.  Unprivileged macOS exposes NO named die sensors.

        Verified on macOS 15.8.3 / Apple Silicon: `powermetrics -s thermal`
        prints ONLY the pressure level (`smc` is rejected — "unrecognized
        sampler: smc"), so temperatures come from the T16B SMC reader that the
        root daemon runs, emitted as `temps: [{key,label,c}]`.  Battery
        temperature is real and unprivileged, but it belongs to the battery
        group.  Without the daemon the group renders ONE amber "needs root"
        row, never 0 °C.
        """
        root = self._root()
        data = root or {}
        rows = []
        # T16B: every sensor the SMC reader actually produced, each one named
        # and traceable to its SMC key.
        seen_labels = set()
        for t in (data.get("temps") or []):
            if not isinstance(t, dict) or not _num(t.get("c")):
                continue
            key = str(t.get("key") or "")
            label = str(t.get("label") or ("Sensor %s" % key if key else "sensor"))
            seen_labels.add(label)
            sid = "thermal.smc.%s" % (key or label.replace(" ", "_").lower())
            shown = label if (not key or key in label) else "%s [%s]" % (label, key)
            rows.append((sid, shown, t.get("c"), "°C",
                         "AppleSMC via the root daemon (syswatch-powerd)",
                         2, "value missing"))
        # Aggregates: only for a family whose per-sensor rows are NOT already
        # on the page — never two rows for the same measurement.
        def _has_family(prefix):
            return any(lbl.startswith(prefix) for lbl in seen_labels)
        for sid, label, value, src, family in (
                ("thermal.smc.cpu_avg", "CPU average (SMC cores)",
                 data.get("cpu_avg_c"), "AppleSMC (root), mean of the CPU core sensors",
                 "CPU"),
                ("thermal.smc.gpu_avg", "GPU average",
                 data.get("gpu_avg_c"), "AppleSMC (root), mean of the GPU sensors",
                 "GPU"),
                ("thermal.smc.battery", "Battery pack",
                 data.get("battery_c"), "AppleSMC (root), pack sensors",
                 "Battery"),
                ("thermal.smc.ssd", "SSD",
                 data.get("ssd_c"), "AppleSMC (root), NAND sensors",
                 "SSD"),
        ):
            if _num(value) and not _has_family(family):
                rows.append((sid, label, value, "°C", src, 2, "value missing"))
        cpu_t = data.get("cpu_temp_c")
        if _num(cpu_t):
            rows.append(("thermal.cpu_die", "CPU die", cpu_t, "°C",
                         "powermetrics thermal (root)", 1, "value missing"))
        for i, c in enumerate(data.get("cpu_cluster_temps_c") or []):
            if isinstance(c, dict) and _num(c.get("c")):
                rows.append(("thermal.cluster%d" % (i + 1),
                             c.get("label") or "CPU cluster %d" % (i + 1),
                             c.get("c"), "°C", "powermetrics thermal (root)", 1,
                             "value missing"))
            elif _num(c):
                rows.append(("thermal.cluster%d" % (i + 1),
                             "CPU cluster %d" % (i + 1), c, "°C",
                             "powermetrics thermal (root)", 1, "value missing"))
        gpu_t = data.get("gpu_temp_c")
        if _num(gpu_t):
            rows.append(("thermal.gpu", "GPU", gpu_t, "°C",
                         "powermetrics thermal (root)", 1, "value missing"))
        ssd_t = data.get("ssd_temp_c")
        if _num(ssd_t):
            rows.append(("thermal.ssd", "SSD", ssd_t, "°C",
                         "powermetrics / SMC (root)", 1, "value missing"))
        batt_t = data.get("battery_temp_c")
        if _num(batt_t):
            rows.append(("thermal.battery", "Battery", batt_t, "°C",
                         "powermetrics / SMC (root)", 1, "value missing"))
        pressure = data.get("thermal_pressure")
        if isinstance(pressure, str) and pressure:
            rows.append(("thermal.pressure", "Thermal pressure", pressure, None,
                         "powermetrics thermal (root)", None, "value missing"))
        fans = data.get("fans_rpm") or []
        for i, rpm in enumerate(fans):
            rows.append(("thermal.fan%d" % (i + 1),
                         "Fan %d" % (i + 1), rpm, "rpm",
                         "SMC via powermetrics (root)", 0, "value missing"))
        if rows:
            return self.rows("thermal", rows)
        smc = data.get("smc") if isinstance(data.get("smc"), dict) else {}
        if smc and not smc.get("ok", True):
            reason = ("the SMC reader ran but returned no sane reading on this "
                      "machine (%s)" % (smc.get("error") or "no keys in range"))
            return [sensor("thermal.smc_needs_reader", "thermal", "Temperatures",
                           None, "°C", "AppleSMC (root, syswatch-powerd)",
                           available=False, reason=reason, root_gated=True)]
        return [sensor("thermal.needs_root", "thermal", "Temperatures", None,
                       "°C", "AppleSMC via the root daemon (syswatch-powerd)",
                       available=False, reason=self._root_reason(),
                       root_gated=True)]

    def _power_rails(self):
        root = self._root()
        rails = [
            ("power_rails.package", "CPU/GPU/ANE package power", root.get("package_w"),
             "W", "powermetrics Package Power (root)", 2),
            ("power_rails.cpu", "CPU power", root.get("cpu_w"), "W",
             "powermetrics CPU Power (root)", 2),
            ("power_rails.gpu", "GPU power", root.get("gpu_w"), "W",
             "powermetrics GPU Power (root)", 2),
            ("power_rails.ane", "Neural Engine power", root.get("ane_w"), "W",
             "powermetrics ANE Power (root)", 2),
        ]
        rows = self.root_rows("power_rails", rails, self._root_reason())
        rails_avail = any(r["available"] for r in rows)
        if not rails_avail:
            # One honest row beats four identical "unavailable" lines.
            return [sensor("power_rails.needs_root", "power_rails", "Power rails",
                           None, "W", "powermetrics (root)", available=False,
                           reason=self._root_reason(), root_gated=True)]
        return rows

    def _clocks(self):
        snap = self.ctx.get("snap") or {}
        cpu = snap.get("cpu") or {}
        root = self._root()
        rows = [
            ("clocks.cpu", "CPU frequency", cpu.get("freq_mhz"), "MHz",
             "psutil cpu_freq() — nominal; per-cluster clocks need IOReport (root)",
             0, "CPU clock not exposed"),
        ]
        # T16B: powermetrics already reports the real per-cluster clocks and
        # residency.  These replace the nominal psutil figure as soon as the
        # root daemon is running.
        for cid, label, key in (("clocks.e_cluster", "E-cluster frequency",
                                 "e_cluster"),
                                ("clocks.p_cluster", "P-cluster frequency",
                                 "p_cluster")):
            cl = root.get(key) if isinstance(root.get(key), dict) else {}
            rows.append((cid, label, cl.get("freq_mhz"), "MHz",
                         "powermetrics, root daemon (5 s)", 0,
                         "cluster clock not reported on this build"))
            rows.append((cid + ".residency",
                         label.replace("frequency", "active residency"),
                         cl.get("residency_pct"), "%",
                         "powermetrics, root daemon (5 s)", 1,
                         "cluster residency not reported on this build"))
        rows.append(("clocks.gpu", "GPU frequency", root.get("gpu_freq_mhz"), "MHz",
                     "powermetrics GPU sampler, root daemon (5 s)", 0,
                     "GPU clock not reported on this build"))
        per_cpu = root.get("per_cpu") or []
        out = self.rows("clocks", rows)
        for i, c in enumerate(per_cpu):
            if not isinstance(c, dict):
                continue
            out.append(sensor("clocks.cpu%d" % i,
                              "clocks", "CPU %d frequency" % i,
                              c.get("freq_mhz"), "MHz",
                              "powermetrics per-CPU line, root daemon (5 s)", 0))
        # A psutil value is a nominal clock, not a measurement of this instant:
        # say so rather than hiding the weaker source now that we have a better
        # one.
        for r in out:
            if r["id"] == "clocks.cpu" and not _num(cpu.get("freq_mhz")):
                r["available"] = False
                r["reason"] = r.get("reason") or "CPU clock not exposed"
        return out


class WindowsAdapter(Adapter):
    """Windows stub: same interface, honest capability row, no Mac assumptions.

    Real implementation (documented in docs/machine-capability-inventory.md):
    PDH counters, WMI thermal zones / LibreHardwareMonitor, ETW energy.  Until
    then every probe path is `unsupported yet` rather than silently empty.
    """

    platform = "unsupported yet"

    def __init__(self, ctx=None):
        self.ctx = ctx or {}

    def probe(self):
        return [
            sensor("windows.platform", "cpu", "Windows adapter", None, None,
                   "sensor registry", available=False,
                   reason="platform: unsupported yet — PDH/WMI/ETW adapter "
                          "not implemented (one adapter file adds it)"),
            sensor("windows.package_w", "power_rails", "Package power", None, "W",
                   "vendor WMI (unsupported yet)", available=False,
                   reason="platform: unsupported yet"),
        ]


class LinuxAdapter(Adapter):
    """/proc · /sys stub with the same contract (see the inventory doc)."""

    platform = "unsupported yet"

    def __init__(self, ctx=None):
        self.ctx = ctx or {}

    def probe(self):
        return [
            sensor("linux.platform", "cpu", "Linux adapter", None, None,
                   "sensor registry", available=False,
                   reason="platform: unsupported yet — /proc and /sys adapters "
                          "not implemented (one adapter file adds it)"),
            sensor("linux.hwmon", "thermal", "hwmon temperatures", None, "°C",
                   "hwmon (unsupported yet)", available=False,
                   reason="platform: unsupported yet"),
        ]


def adapter_for(ctx):
    if sys.platform == "darwin":
        return MacOSAdapter(ctx)
    if sys.platform.startswith("win"):
        return WindowsAdapter(ctx)
    if sys.platform.startswith("linux"):
        return LinuxAdapter(ctx)
    return WindowsAdapter(ctx)


def registry(ctx=None):
    """Build the full catalogue: ordered groups + a flat sensor list."""
    ctx = ctx or {}
    adapter = adapter_for(ctx)
    sensors = adapter.probe()
    groups = []
    for gid in GROUP_ORDER:
        rows = [s for s in sensors if s["group"] == gid]
        if not rows:
            continue  # a group with nothing on this device simply does not render
        available = any(s["available"] for s in rows)
        locked = (not available) and all(s["root_gated"] for s in rows)
        groups.append({
            "id": gid,
            "label": GROUP_LABELS.get(gid, gid),
            "available": available,
            "needs_root": locked,
            "sensors": rows,
            "note": ("needs root — this group is unlocked by the privileged "
                     "daemon (T15)" if locked else
                     (None if available else
                      "no sensor in this group is readable on this platform")),
        })
    root = ctx.get("root") or {}
    diagnostics = [
        {"what": "platform", "value": adapter.platform,
         "detail": "adapter interface: probe() -> list[Sensor]"},
        {"what": "unreadable rows",
         "value": sum(1 for s in sensors if not s["available"]),
         "detail": "each one carries its own reason; none is rendered as 0"},
        {"what": "root sample",
         "value": "present" if root.get("available") else "absent",
         "detail": root.get("reason") or (root.get("path") or "")},
    ]
    return {"platform": adapter.platform, "groups": groups,
            "sensors": sensors, "diagnostics": diagnostics,
            "root": {"available": bool(root.get("available")),
                     "age_s": root.get("age_s"),
                     "reason": root.get("reason"),
                     "path": root.get("path")},
            "grant": ctx.get("grant"),
            "count": len(sensors)}
