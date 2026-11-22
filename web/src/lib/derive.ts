// Everything the dashboard works out from a collector sample: what each tile
// says, how each sparkline is shaped, which apps used the processor, and what
// the status column reports. One place, so a tile and its chart never disagree.
import type {
  Battery,
  BatteryApi,
  Consumers,
  History,
  Range,
  SensorGroup,
  Sensors,
  Snapshot,
  Thermal,
} from "../data/types";
import { clamp, fmt, gb, isNum } from "./format";

export const WIDE_TILES = new Set(["network", "storage", "thermal"]);

export const DEFAULT_TILE_ORDER = [
  "cpu",
  "memory",
  "gpu",
  "battery",
  "power",
  "workload",
  "network",
  "storage",
  "thermal",
] as const;

export type TileId = (typeof DEFAULT_TILE_ORDER)[number];

export const TILE_META: Record<TileId, { label: string; unit: string }> = {
  cpu: { label: "Processor", unit: "%" },
  memory: { label: "Memory", unit: "%" },
  gpu: { label: "Graphics", unit: "%" },
  battery: { label: "Battery", unit: "%" },
  power: { label: "Power use", unit: "W" },
  workload: { label: "Workload", unit: "" },
  network: { label: "Network traffic", unit: "KB/s" },
  storage: { label: "Storage activity", unit: "MB/s" },
  thermal: { label: "Temperature", unit: "°C" },
};

const RANGE_WORDS: Record<Range, string> = {
  "1m": "last minute",
  "5m": "last 5 min",
  "15m": "last 15 min",
  "1h": "last hour",
  "6h": "last 6 hours",
  "1d": "last day",
  "7d": "last 7 days",
  "30d": "last 30 days",
  "1y": "last year",
};

export function rangeWord(range: Range): string {
  return RANGE_WORDS[range] ?? "recent average";
}

function thermalGroup(sensors: Sensors | null): SensorGroup | null {
  if (!sensors || !Array.isArray(sensors.groups)) return null;
  return sensors.groups.find((g) => g.id === "thermal") ?? null;
}

export function maxTemp(c: Snapshot | null, sensors: Sensors | null): number | null {
  if (c && Array.isArray(c.temps_c) && c.temps_c.length) {
    const vals = c.temps_c.map((t) => t.c).filter(isNum);
    if (vals.length) return Math.max(...vals);
  }
  const group = thermalGroup(sensors);
  const rows = group ? group.sensors : [];
  const vals = rows.map((r) => r.value).filter(isNum);
  return vals.length ? Math.max(...vals) : null;
}

export function gpuTemp(c: Snapshot | null, thermal: Thermal | null): number | null {
  if (c && Array.isArray(c.temps_c)) {
    const row = c.temps_c.find((t) => isNum(t.c) && /^GPU/i.test(String(t.label ?? "")));
    if (row && isNum(row.c)) return row.c;
  }
  if (c && c.gpu && isNum(c.gpu.temp_c)) return c.gpu.temp_c;
  const rows = thermal && Array.isArray(thermal.sensors) ? thermal.sensors : [];
  const hit = rows.find((r) => isNum(r.c) && /^GPU/i.test(String(r.label ?? "")));
  return hit && isNum(hit.c) ? hit.c : null;
}

export function storageTotalMB(c: Snapshot | null): number | null {
  const read = c?.disk_io?.read_bps;
  const write = c?.disk_io?.write_bps;
  if (!isNum(read) && !isNum(write)) return null;
  return ((isNum(read) ? read : 0) + (isNum(write) ? write : 0)) / 1048576;
}

export function batteryPct(c: Snapshot | null): number | null {
  const b = c?.battery as (Battery & { charge_pct?: number }) | undefined;
  if (!b) return null;
  return isNum(b.pct) ? b.pct : isNum(b.charge_pct) ? b.charge_pct : null;
}

export function tileValue(id: TileId, c: Snapshot | null, sensors: Sensors | null = null): string {
  switch (id) {
    case "cpu":
      return fmt(c?.cpu?.total_pct, 1);
    case "memory":
      return fmt(c?.ram?.pct, 1);
    case "gpu":
      return fmt(c?.gpu?.util_pct, 1);
    case "battery":
      return fmt(batteryPct(c), 0);
    case "power":
      return fmt(c?.power?.package_w, 2);
    case "workload":
      return fmt(c?.cpu?.load?.one, 2);
    case "network":
      return fmt(isNum(c?.net?.down_bps) ? (c!.net.down_bps as number) / 1024 : null, 1);
    case "storage":
      return fmt(storageTotalMB(c), 1);
    case "thermal":
      return fmt(maxTemp(c, sensors), 1);
  }
}

export function pointsVsRange(current: number | null, values: number[], range: Range): string {
  if (!isNum(current) || !Array.isArray(values)) return "";
  const nums = values.filter(isNum);
  if (nums.length < 3) return "";
  const avg = nums.reduce((a, b) => a + b, 0) / nums.length;
  const delta = current - avg;
  if (Math.abs(delta) < 0.5) return "Same as " + rangeWord(range);
  return fmt(Math.abs(delta), 1) + " points " + (delta > 0 ? "above " : "below ") + rangeWord(range);
}

export function tileSub(id: TileId, c: Snapshot | null, batteryApi: BatteryApi | null): string {
  switch (id) {
    case "cpu":
      return (c?.cpu?.cores ?? "—") + " cores";
    case "memory":
      return (
        fmt(gb(c?.ram?.used_bytes), 1) + " GB of " + fmt(gb(c?.ram?.total_bytes), 1) + " GB in use"
      );
    case "gpu":
      return "drawing the screen: " + fmt(isNum(c?.gpu?.renderer_pct) ? c!.gpu.renderer_pct : null, 0) + " %";
    case "battery": {
      const b = c?.battery ?? ({} as Battery);
      const api = batteryApi ?? {};
      const remaining = isNum(b.time_remaining_min)
        ? b.time_remaining_min
        : isNum(api.time_remaining_min)
          ? api.time_remaining_min
          : null;
      let line =
        remaining != null
          ? Math.floor(remaining / 60) + " h " + Math.round(remaining % 60) + " min left"
          : b.external_connected
            ? "on adapter"
            : "on battery";
      if (isNum(b.draw_w)) line += " · using " + fmt(b.draw_w, 1) + " W";
      else if (isNum(b.adapter_w)) line += " · " + fmt(b.adapter_w, 0) + " W";
      return line;
    }
    case "power":
      return (
        "Processor " +
        fmt(c?.power?.cpu_w, 1) +
        " W · Graphics " +
        fmt(c?.power?.gpu_w, 1) +
        " W"
      );
    case "workload":
      return (
        "5 min " + fmt(c?.cpu?.load?.five, 2) + " · 15 min " + fmt(c?.cpu?.load?.fifteen, 2)
      );
  }
  return "";
}

export function tileDelta(
  id: TileId,
  c: Snapshot | null,
  history: History | null,
  sensors: Sensors | null,
  thermal: Thermal | null,
  range: Range,
): string {
  switch (id) {
    case "cpu":
      return pointsVsRange(c?.cpu?.total_pct ?? null, history?.series?.cpu ?? [], range);
    case "memory":
      return pointsVsRange(c?.ram?.pct ?? null, history?.series?.ram ?? [], range);
    case "gpu":
      return isNum(c?.gpu?.mem_used_mb) ? fmt(c!.gpu.mem_used_mb, 0) + " MB vram" : "vram not reported";
    case "battery":
      return isNum(c?.battery?.health_pct)
        ? "Holds " + fmt(c!.battery.health_pct, 0) + " % of new"
        : "health not reported";
    case "power": {
      const ms = thermal && isNum(thermal.sample_ms) ? thermal.sample_ms : null;
      return ms ? "Updates every " + fmt(ms / 1000, 0) + " seconds" : "root daemon sampling";
    }
    case "workload":
      return isNum(c?.cpu?.cores) ? "across " + c!.cpu.cores + " cores" : "";
    case "network":
      return isNum(c?.net?.up_bps) ? fmt((c!.net.up_bps as number) / 1024, 1) + " KB/s up" : "—";
    case "storage":
      return isNum(c?.disk?.free_gb) ? fmt(c!.disk.free_gb, 1) + " GB free" : "free space unavailable";
    case "thermal":
      return (sensors && sensors.count ? sensors.count : "—") + " sensors";
  }
  return "";
}

export function tileSeries(
  id: TileId,
  history: History | null,
  live: LiveSeries,
): number[] {
  const series = history?.series;
  if (id === "cpu") return series?.cpu ?? [];
  if (id === "memory") return series?.ram ?? [];
  if (id === "network") return series?.down_bps ?? [];
  if (id === "gpu") return live.gpu;
  if (id === "battery") return live.battery;
  if (id === "workload") return live.load;
  if (id === "thermal" && history && Array.isArray(history.temp_series) && history.temp_series.length) {
    const rows = history.temp_series;
    let len = 0;
    rows.forEach((a) => {
      if (Array.isArray(a) && a.length > len) len = a.length;
    });
    const out: number[] = [];
    for (let i = 0; i < len; i++) {
      const vals: number[] = [];
      rows.forEach((a) => {
        if (Array.isArray(a) && isNum(a[i]) && a[i] > 10 && a[i] < 120) vals.push(a[i]);
      });
      out.push(vals.length ? Math.max(...vals) : NaN);
    }
    return out;
  }
  return [];
}

export function wideSeries(
  id: TileId,
  history: History | null,
  live: LiveSeries,
): [number[], number[]] {
  const series = history?.series;
  if (id === "network") return [series?.down_bps ?? [], series?.up_bps ?? []];
  if (id === "storage") return [live.readMB, live.writeMB];
  if (id === "thermal" && history && Array.isArray(history.temp_series) && history.temp_series.length) {
    const rows = history.temp_series;
    const gpuIdx = rows.length > 5 ? 5 : 0;
    return [tileSeries("thermal", history, live), Array.isArray(rows[gpuIdx]) ? rows[gpuIdx] : []];
  }
  return [[], []];
}

export interface LiveSeries {
  readMB: number[];
  writeMB: number[];
  gpu: number[];
  battery: number[];
  load: number[];
}

export function timeLabels(history: History | null): string[] {
  const labels = history && Array.isArray(history.labels) ? history.labels : [];
  if (!labels.length) return ["", "", "", "now"];
  const clock = (iso: string) => {
    const m = String(iso).match(/T(\d{2}):(\d{2})/);
    return m ? m[1] + ":" + m[2] : String(iso);
  };
  return [
    clock(labels[0]),
    clock(labels[Math.floor(labels.length / 3)]),
    clock(labels[Math.floor((2 * labels.length) / 3)]),
    "now",
  ];
}

export interface Bounds {
  lo: number;
  hi: number;
}

export function seriesBounds(arrays: number[][]): Bounds | null {
  const nums: number[] = [];
  arrays.forEach((a) => {
    if (Array.isArray(a)) a.forEach((v) => {
      if (isNum(v)) nums.push(v);
    });
  });
  if (!nums.length) return null;
  // Robust bounds: one freak sample must not squash the whole line onto the floor.
  const sorted = nums.slice().sort((x, y) => x - y);
  const at = (p: number) => sorted[clamp(Math.round((sorted.length - 1) * p), 0, sorted.length - 1)];
  const lo = at(0.02);
  const hi = at(0.98);
  if (!(hi > lo)) return { lo: Math.min(...nums), hi: Math.max(...nums) };
  const pad = (hi - lo) * 0.06;
  return { lo: lo - pad, hi: hi + pad };
}

function yFor(v: number, min: number, span: number): number {
  return clamp(27 - ((v - min) / span) * 24, 0.8, 29.2);
}

export function sparkPath(arr: number[], bounds: Bounds | null): string {
  if (!Array.isArray(arr)) return "M0 15 L100 15";
  const nums = arr.filter(isNum);
  if (nums.length < 2) return "M0 15 L100 15";
  const min = bounds ? bounds.lo : Math.min(...nums);
  const max = bounds ? bounds.hi : Math.max(...nums);
  const span = max - min;
  if (!(span > 0.0001)) return "M0 15 L100 15";
  let d = "";
  let penDown = false;
  for (let i = 0; i < arr.length; i++) {
    const v = arr[i];
    if (!isNum(v)) {
      penDown = false;
      continue;
    }
    const x = (i / (arr.length - 1)) * 100;
    d += (penDown ? "L" : "M") + x.toFixed(2) + " " + yFor(v, min, span).toFixed(2) + " ";
    penDown = true;
  }
  return d.trim() || "M0 15 L100 15";
}

export function areaPath(arr: number[], bounds: Bounds | null): string {
  if (!Array.isArray(arr)) return "";
  const nums = arr.filter(isNum);
  if (nums.length < 2) return "";
  const min = bounds ? bounds.lo : Math.min(...nums);
  const max = bounds ? bounds.hi : Math.max(...nums);
  const span = max - min;
  if (!(span > 0.0001)) return "";
  let d = "";
  let run: Array<{ x: number; y: number }> = [];
  const flush = () => {
    if (run.length < 1) return;
    let seg = "M" + run[0].x.toFixed(2) + " " + run[0].y.toFixed(2);
    for (let k = 1; k < run.length; k++) seg += " L" + run[k].x.toFixed(2) + " " + run[k].y.toFixed(2);
    const lastX = run[run.length - 1].x;
    const firstX = run[0].x;
    seg += " L" + lastX.toFixed(2) + " 30 L" + firstX.toFixed(2) + " 30 Z";
    d += seg + " ";
    run = [];
  };
  for (let i = 0; i < arr.length; i++) {
    const v = arr[i];
    if (!isNum(v)) {
      flush();
      continue;
    }
    const x = (i / (arr.length - 1)) * 100;
    run.push({ x, y: yFor(v, min, span) });
  }
  flush();
  return d.trim();
}

/** One row of the app ranking, after summing processes that share a name. */
export interface AppRow {
  name: string;
  cpu_pct: number | null;
  procs: number;
  memory_mb?: number;
  mem_avg_mb?: number;
}

export interface StatusRow {
  title: string;
  sub: string;
  warn?: boolean;
  icon: "warn" | "dot";
}

export function statusRows(args: {
  current: Snapshot | null;
  consumers: Consumers | null;
  sensors: Sensors | null;
  batteryApi: BatteryApi | null;
  thermal: Thermal | null;
  ageSeconds: number | null;
}): StatusRow[] {
  const { current: c, consumers, sensors, batteryApi, thermal: thermalApi, ageSeconds } = args;
  const ram = c?.ram;
  const memPressure = c?.pressure?.memory;
  const memLevel = memPressure?.level ?? ((c?.ram?.pct ?? 0) > 85 ? "warning" : "normal");
  const tight = memLevel === "warning" || memLevel === "critical";

  const agentProc = c?.processes?.by_cpu?.find((p) => /syswatch|agent/i.test(String(p.name ?? "")));
  let agentCpu: number | null = agentProc && isNum(agentProc.cpu_pct) ? agentProc.cpu_pct : null;
  let agentMem: number | null = agentProc && isNum(agentProc.mem_mb) ? agentProc.mem_mb : null;
  if (agentCpu == null) {
    const rows = consumers && Array.isArray(consumers.rows) ? consumers.rows : [];
    const hit = rows.find((r) => /syswatch|monitor/i.test(String(r.name ?? "")));
    if (hit) {
      agentCpu = isNum(hit.cpu_pct) ? hit.cpu_pct : null;
      agentMem = isNum(hit.memory_mb) ? hit.memory_mb : isNum(hit.mem_avg_mb) ? hit.mem_avg_mb : null;
    }
  }

  const deepLocked = !(c?.power && c.power.available);
  const disk = c?.disk;
  const batt = { ...(c?.battery ?? {}), ...(batteryApi ?? {}) };
  const health = isNum(batt.health_pct) ? batt.health_pct : null;
  const cycles = isNum(batt.cycles) ? batt.cycles : null;

  return [
    {
      title: tight ? "Memory is getting tight" : "Memory is healthy",
      sub: fmt(gb(ram?.used_bytes), 1) + " of " + fmt(gb(ram?.total_bytes), 1) + " GB in use",
      warn: tight,
      icon: "warn",
    },
    {
      title: "Monitoring is running",
      sub:
        "uses " +
        fmt(agentCpu, 1) +
        " % of the processor" +
        (agentMem != null ? " · " + fmt(agentMem, 0) + " MB" : ""),
      icon: "dot",
    },
    {
      title: (sensors && sensors.count ? sensors.count : "—") + " sensors found",
      sub: "Deep power readings " + (deepLocked ? "are locked." : "available."),
      icon: "dot",
    },
    {
      title: "Collector log age",
      sub: ageSeconds != null ? ageSeconds + " s since the last collector sample" : "no collector sample yet",
      icon: "dot",
    },
    { title: "Last sample time", sub: c?.ts ? String(c.ts) : "not available", icon: "dot" },
    {
      title: "Disk free",
      sub: isNum(disk?.free_gb)
        ? fmt(disk!.free_gb, 1) + " GB free of " + fmt(disk!.total_gb, 1) + " GB"
        : "disk info unavailable",
      icon: "dot",
    },
    {
      title: "Battery health",
      sub:
        health != null
          ? "Holds " + fmt(health, 1) + " % of its original charge" + (cycles != null ? " · " + fmt(cycles, 0) + " cycles" : "")
          : "health not reported",
      icon: "dot",
    },
    {
      title: "Sensor source",
      sub:
        thermalApi && thermalApi.source
          ? String(thermalApi.source)
          : sensors && sensors.platform
            ? "platform: " + sensors.platform
            : "source not reported",
      icon: "dot",
    },
  ];
}
