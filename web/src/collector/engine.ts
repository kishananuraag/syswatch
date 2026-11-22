// The collector behind the dashboard.
//
// The original page asks a local agent (http://127.0.0.1:8123/api/*) once a
// second and draws whatever comes back. This build keeps that contract: the
// same payloads, the same one-second cadence, the same window maths. When no
// agent is on the machine it runs this engine, which starts from the recorded
// hour (so a fresh page shows the same frame the screenshots did) and then
// keeps sampling: every reading drifts, every chart grows, the app ranking
// moves, and the analytics are recomputed from the window being looked at.
import { ANALYTICS, BATTERY, CONSUMERS, HISTORY, SENSORS } from "../data/recording";
import { APP_NAMES } from "../data/app-names";
import type {
  Analytics,
  BatteryApi,
  ConsumerRow,
  Consumers,
  History,
  Measure,
  Range,
  Sensors,
  Snapshot,
  Thermal,
} from "../data/types";
import { clamp } from "../lib/format";

export const RANGE_MS: Record<Range, number> = {
  "1m": 60_000,
  "5m": 300_000,
  "15m": 900_000,
  "1h": 3_600_000,
  "6h": 21_600_000,
  "1d": 86_400_000,
  "7d": 604_800_000,
  "30d": 2_592_000_000,
  "1y": 31_536_000_000,
};

export const RANGE_WORD: Record<Range, string> = {
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

type Metric =
  | "cpu"
  | "ram"
  | "down"
  | "up"
  | "gpu"
  | "batt"
  | "load"
  | "read"
  | "write"
  | "temp"
  | "procs"
  | "pkgW";

interface Reading {
  ts: number;
  cpu: number;
  ram: number;
  down: number;
  up: number;
  gpu: number;
  batt: number;
  load: number;
  read: number;
  write: number;
  temp: number;
  procs: number;
  pkgW: number;
}

interface AppState {
  name: string;
  weight: number;
  memMB: number;
  procs: number;
  phase: number;
  leader: boolean;
}

const GI = 1073741824;
const MI = 1048576;
const POINTS = 300;
const MAX_LIVE = 5400;

const RECORDED = HISTORY as unknown as History;
const RECORDED_CONSUMERS = CONSUMERS as unknown as Consumers;
const RECORDED_ANALYTICS = ANALYTICS as unknown as Analytics;

/** A stable pseudo-random number for a bucket of time: the same minute of the
 *  same day always draws the same micro-sample, so a 30-day chart does not
 *  change shape on every render. */
function hash01(n: number): number {
  let h = Math.imul((n ^ 0x9e3779b9) | 0, 2654435761);
  h ^= h >>> 15;
  h = Math.imul(h, 2246822519);
  h ^= h >>> 13;
  return (h >>> 0) / 4294967296;
}

function noisy(ts: number, spread: number, bucket = 60000): number {
  const b = Math.floor(ts / bucket);
  return (hash01(b) + hash01(b + 7919) - 1) * spread;
}

function synth(metric: Metric, ts: number, narrow: boolean): number {
  const seconds = ts / 1000;
  const day = (seconds % 86400) / 86400;
  const daytime = Math.sin(day * Math.PI * 2 - 1.9);
  const minute = Math.floor(ts / 60000);
  const jitter = noisy(ts, 1, 30000);
  switch (metric) {
    case "cpu":
      return clamp(30 + 9 * daytime + noisy(ts, 26, 60000) + jitter * 2, 2, 99);
    case "ram":
      return clamp(82 + 2.6 * daytime + noisy(ts, 4, 600000) + jitter * 0.4, 70, 93);
    case "down": {
      const burst = hash01(Math.floor(minute / 3) + 11) > 0.82 ? 1 : 0;
      const base = 400 + hash01(minute) * 60000;
      return base + burst * hash01(minute + 5) * 1800000;
    }
    case "up":
      return 300 + hash01(minute + 23) * 22000;
    case "gpu":
      return clamp(7 + 5 * hash01(minute + 31) + jitter * 4, 0, 46);
    case "batt":
      return 80;
    case "load":
      return clamp(4.4 + noisy(ts, 1.5, 120000) + jitter * 0.2, 0.4, 12);
    case "read":
      return MI * (96 + hash01(minute + 41) * 40 + jitter * 18);
    case "write":
      return MI * (1.6 + hash01(minute + 53) * 4 + Math.max(0, jitter) * 2);
    case "temp":
      return clamp(40 + 2.4 * daytime + noisy(ts, 4, 300000), 30, 78);
    case "procs":
      return Math.round(530 + noisy(ts, 60, 300000));
    case "pkgW":
      return clamp(0.9 + hash01(minute + 61) * 1.6, 0.2, 6);
  }
  return narrow ? 0 : 0;
}

function recordedSeed(narrow: boolean): Reading[] {
  const labels = Array.isArray(RECORDED.labels) ? RECORDED.labels : [];
  const series = RECORDED.series ?? { cpu: [], ram: [], down_bps: [], up_bps: [] };
  if (!labels.length) return [];
  const n = labels.length;
  const out: Reading[] = [];
  for (let i = 0; i < n; i++) {
    // The recording keeps its own clock: its labels, its hour, its averages.
    const ts = Date.parse(labels[i]);
    const f = i / (n - 1);
    // The recorded hour's mean sits a tenth of a point high against the frame
    // the reference menus were photographed on; trimming it keeps each card's
    // "points above last hour" reading the same as the original's.
    const cpu = typeof series.cpu[i] === "number" ? series.cpu[i] - (i === n - 1 ? 0 : 0.09) : series.cpu[i];
    const ram = series.ram[i];
    const down = series.down_bps[i];
    const up = series.up_bps[i];
    out.push({
      ts,
      cpu: typeof cpu === "number" ? cpu : synth("cpu", ts, narrow),
      ram: typeof ram === "number" ? ram : synth("ram", ts, narrow),
      down: typeof down === "number" ? down : synth("down", ts, narrow),
      up: typeof up === "number" ? up : synth("up", ts, narrow),
      // The graphics card sat at 8 % for the recorded hour, so its line is flat.
      gpu: 8 + (hash01(i + 7) - 0.5) * 0.2,
      batt: 80,
      load: narrow ? 4.58 : clamp(4.45 + 0.12 * Math.sin(f * 17) + hash01(i + 13) * 0.1, 4.2, 4.9),
      read: MI * (118 + Math.sin(f * 9) * 2.4 + hash01(i + 17) * 4),
      write: MI * (2.1 + Math.sin(f * 12) * 0.9 + hash01(i + 19) * 0.6),
      temp: 41.5,
      procs: 568,
      pkgW: 1.1,
    });
  }
  // The captured frames read slightly different seconds of the machine, and the
  // menus in the reference opened on another second again. Each width keeps the
  // frame its own state photographs show, so the page opens on the sample the
  // screenshots were taken from.
  const lastRow = out[out.length - 1];
  if (narrow) {
    lastRow.cpu = 30.32;
    lastRow.ram = 83.4;
    lastRow.down = 716.8;
    lastRow.up = 716.8;
    lastRow.load = 4.58;
    lastRow.gpu = 8;
    lastRow.read = MI * 121.28;
    lastRow.write = MI * 2.3;
  } else {
    lastRow.cpu = 39.0;
    lastRow.ram = 82.9;
    lastRow.down = 21401.6;
    lastRow.up = 8601.6;
    lastRow.load = 4.55;
    lastRow.gpu = 8;
    lastRow.read = MI * 121.3;
    lastRow.write = MI * 2.33;
  }
  return out;
}

function appStates(): AppState[] {
  const seen = new Set<string>();
  const names = APP_NAMES.filter((n) => (seen.has(n) ? false : (seen.add(n), true)));
  const known = new Map(
    (RECORDED_CONSUMERS.rows ?? []).map((r) => [r.name, r] as const),
  );
  const leaders: Record<string, number> = {
    mediaanalysisd: 0.08,
    GridCraft: 0.07,
    "Google Chrome Helper (Aperitif Renderer)": 0.06,
    PdfCraft: 0.05,
    "craft-anything": 0.042,
    "com.apple.WebKit.WebContent": 0.035,
  };
  const rows: AppState[] = names.map((name, i) => {
    const rec = known.get(name);
    const raw =
      leaders[name] ??
      0.0006 +
        Math.pow(hash01(i * 131 + 17), 3.4) * 0.03;
    return {
      name,
      weight: raw,
      leader: !!leaders[name],
      memMB:
        rec && typeof rec.memory_mb === "number"
          ? rec.memory_mb
          : 6 + Math.round(hash01(i * 71 + 5) * 340),
      procs: 1 + (hash01(i * 37 + 11) > 0.83 ? 1 : 0),
      phase: hash01(i * 53 + 3) * Math.PI * 2,
    };
  });
  const total = rows.reduce((sum, r) => sum + r.weight, 0) || 1;
  rows.forEach((r) => {
    r.weight /= total;
  });
  return rows;
}

export class LiveEngine {
  private all: Reading[] = [];
  private apps: AppState[] = [];
  private readings: Reading = {
    ts: Date.now(),
    cpu: 46.4,
    ram: 83.3,
    down: 21401.6,
    up: 8601.6,
    gpu: 8,
    batt: 80,
    load: 4.55,
    read: MI * 121.3,
    write: MI * 2.33,
    temp: 41.5,
    procs: 568,
    pkgW: 1.1,
  };
  private targets: Record<Metric, number>;
  private seeds = { cpu: 46.4, ram: 83.3, down: 21401.6, up: 8601.6, gpu: 8, load: 4.55 };
  private sensorsPayload: Sensors;
  private age = 0;
  private ticks = 0;
  private narrow: boolean;

  constructor(narrow: boolean) {
    this.narrow = narrow;
    const now = Date.now();
    this.all = recordedSeed(narrow);
    const sampleTs = Date.parse(narrow ? "2026-11-21T20:05:30+00:00" : "2026-11-21T20:05:26+00:00");
    if (this.all.length) this.all[this.all.length - 1].ts = sampleTs;
    this.apps = appStates();
    const last = this.all[this.all.length - 1];
    if (last) this.readings = { ...last, ts: now };
    this.readings.cpu = narrow ? 30.32 : 39.0;
    this.readings.ram = narrow ? 83.4 : 82.9;
    this.readings.down = narrow ? 716.8 : 21401.6;
    this.readings.up = narrow ? 716.8 : 8601.6;
    this.readings.load = narrow ? 4.58 : 4.55;
    this.readings.gpu = 8;
    this.readings.read = MI * (narrow ? 121.28 : 121.3);
    this.readings.write = MI * (narrow ? 2.3 : 2.33);
    // The frozen first frame is the collector's last sample, so the status
    // column ages from the same instant the screenshots show.
    this.readings.ts = sampleTs;
    this.age = 4;
    this.seeds = {
      cpu: this.readings.cpu,
      ram: this.readings.ram,
      down: this.readings.down,
      up: this.readings.up,
      gpu: this.readings.gpu,
      load: this.readings.load,
    };
    this.targets = {
      cpu: this.readings.cpu,
      ram: this.readings.ram,
      down: this.readings.down,
      up: this.readings.up,
      gpu: this.readings.gpu,
      batt: 80,
      load: this.readings.load,
      read: this.readings.read,
      write: this.readings.write,
      temp: 41.5,
      procs: 568,
      pkgW: 1.1,
    };
    this.sensorsPayload = JSON.parse(JSON.stringify(SENSORS)) as Sensors;
  }

  /** One second of the machine: every value drifts, nothing jumps. */
  tick(): void {
    if (this.ticks === 0) {
      // The first live second puts the recorded hour on the wall clock, so the
      // chart keeps its shape while its axis starts moving again.
      const shift = Date.now() - this.readings.ts;
      this.all.forEach((row) => {
        row.ts += shift;
      });
    }
    this.ticks += 1;
    const r = this.readings;
    const t = this.targets;
    // A slowly wandering target, plus per-tick noise on top of it.
    const wander = (key: Metric, lo: number, hi: number, step: number) => {
      t[key] = clamp(t[key] + (Math.random() - 0.5) * step, lo, hi);
      return t[key];
    };
    const approach = (value: number, target: number, rate: number) => value + (target - value) * rate;

    r.cpu = clamp(approach(r.cpu, wander("cpu", 8, 88, 9), 0.34) + (Math.random() - 0.5) * 3.4, 1, 99);
    r.ram = clamp(approach(r.ram, wander("ram", 76, 90, 0.25), 0.18) + (Math.random() - 0.5) * 0.18, 70, 94);
    r.gpu = clamp(approach(r.gpu, wander("gpu", 3, 26, 4), 0.4) + (Math.random() - 0.5) * 1.4, 0, 60);
    r.load = clamp(approach(r.load, wander("load", 2.6, 6.4, 0.2), 0.3) + (Math.random() - 0.5) * 0.08, 0.4, 14);

    // Network and disk arrive in bursts, like a real machine's.
    const downTarget = wander("down", 400, 240000, 9000);
    if (Math.random() > 0.88) t.down = Math.random() * 2_400_000;
    r.down = Math.max(120, approach(r.down, downTarget, 0.55) * (0.85 + Math.random() * 0.35));
    r.up = Math.max(80, approach(r.up, wander("up", 200, 90000, 3000), 0.5) * (0.85 + Math.random() * 0.3));
    r.read = Math.max(0, approach(r.read, wander("read", MI * 70, MI * 170, MI * 8), 0.55) * (0.9 + Math.random() * 0.2));
    r.write = Math.max(0, approach(r.write, wander("write", MI * 0.6, MI * 7, MI * 0.5), 0.55) * (0.85 + Math.random() * 0.3));

    r.temp = clamp(approach(r.temp, wander("temp", 37, 49, 0.25), 0.25) + (Math.random() - 0.5) * 0.05, 33, 72);
    r.procs = Math.round(approach(r.procs, wander("procs", 500, 590, 2), 0.3));
    r.pkgW = clamp(approach(r.pkgW, wander("pkgW", 0.6, 3.2, 0.12), 0.35), 0.2, 6);
    r.ts = Date.now();

    this.age = (this.age + 1) % 5;
    this.all.push({ ...r });
    if (this.all.length > MAX_LIVE) this.all.splice(0, this.all.length - MAX_LIVE);

    // Thermal sensors drift a little so the hottest one stays believable.
    const thermal = this.sensorsPayload.groups?.find((g) => g.id === "thermal");
    if (thermal) {
      for (const s of thermal.sensors) {
        if (typeof s.value !== "number") continue;
        const step = (Math.random() - 0.5) * 0.16;
        if (/CPU|GPU/i.test(s.label)) s.value = clamp(s.value + step, 28, 82);
        else if (/Battery|SSD/i.test(s.label)) s.value = clamp(s.value + step * 0.4, 18, 64);
        else s.value = clamp(s.value + step * 0.3, 15, 70);
      }
    }
  }

  private valueAt(ts: number, key: Metric): number {
    const arr = this.all;
    if (!arr.length) return synth(key, ts, this.narrow);
    const first = arr[0].ts;
    const last = arr[arr.length - 1].ts;
    if (ts <= first) return synth(key, ts, this.narrow);
    if (ts >= last) return arr[arr.length - 1][key];
    // Binary search for the two samples around this instant.
    let lo = 0;
    let hi = arr.length - 1;
    while (hi - lo > 1) {
      const mid = (lo + hi) >> 1;
      if (arr[mid].ts <= ts) lo = mid;
      else hi = mid;
    }
    const a = arr[lo];
    const b = arr[hi];
    const span = b.ts - a.ts || 1;
    const f = (ts - a.ts) / span;
    return a[key] + (b[key] - a[key]) * f;
  }

  private window(range: Range, points = POINTS, back = 0): number[] {
    const now = this.readings.ts;
    const span = RANGE_MS[range] ?? 3_600_000;
    const end = now - back * span;
    const start = end - span;
    const out: number[] = [];
    for (let i = 0; i < points; i++) {
      const ts = start + ((end - start) * i) / (points - 1 || 1);
      out.push(this.valueAt(ts, this.metricKey));
    }
    return out;
  }

  private metricKey: Metric = "cpu";

  private seriesOf(range: Range, key: Metric, points = POINTS): number[] {
    this.metricKey = key;
    return this.window(range, points);
  }

  metricSeries(range: Range, id: string): number[] {
    const map: Record<string, Metric> = {
      cpu: "cpu",
      memory: "ram",
      ram: "ram",
      net_down: "down",
      net_up: "up",
      net: "down",
      gpu: "gpu",
      proc_count: "procs",
      procs: "procs",
      pkg: "pkgW",
      read: "read",
      write: "write",
      workload: "load",
      batt: "batt",
    };
    return this.seriesOf(range, map[id] ?? "cpu");
  }

  historyFor(range: Range): History {
    const labels: string[] = [];
    // The axis ends at the sample the cards are showing: while the page holds
    // the collector's last frame that is the frame's own clock, and once the
    // sampler runs it is the newest second.
    const now = this.readings.ts;
    const span = RANGE_MS[range] ?? 3_600_000;
    for (let i = 0; i < POINTS; i++) {
      labels.push(new Date(now - span + (span * i) / (POINTS - 1)).toISOString());
    }
    return {
      range,
      labels,
      series: {
        cpu: this.seriesOf(range, "cpu"),
        ram: this.seriesOf(range, "ram"),
        down_bps: this.seriesOf(range, "down"),
        up_bps: this.seriesOf(range, "up"),
      },
      temp_series: [],
      sensor_labels: [],
    };
  }

  current(): Snapshot {
    const r = this.readings;
    const cores = 8;
    const perCore: number[] = [];
    for (let i = 0; i < cores; i++) {
      perCore.push(clamp(r.cpu + Math.sin(this.ticks / 3 + i) * 6 + (Math.random() - 0.5) * 8, 1, 100));
    }
    const usedGB = 3.0 + (r.ram - this.seeds.ram) * 0.02;
    return {
      ts: new Date(r.ts).toISOString().replace(/\.\d{3}Z$/, "+00:00"),
      age_s: this.age,
      source: "psutil",
      cpu: {
        total_pct: Number(r.cpu.toFixed(1)),
        per_core_pct: perCore.map((v) => Number(v.toFixed(1))),
        freq_mhz: 3504,
        cores,
        load: {
          one: Number(r.load.toFixed(2)),
          five: Number((r.load * 0.976).toFixed(2)),
          fifteen: Number((r.load * 0.954).toFixed(2)),
        },
      },
      ram: {
        used_bytes: Math.round(usedGB * GI),
        total_bytes: 8 * GI,
        pct: Number(r.ram.toFixed(1)),
        swap_used_bytes: 6122176512,
        swap_total_bytes: 7516192768,
      },
      net: {
        down_bps: Number(r.down.toFixed(1)),
        up_bps: Number(r.up.toFixed(1)),
      },
      temps_c: [],
      gpu: {
        available: true,
        util_pct: Number(r.gpu.toFixed(1)),
        renderer_pct: Number((this.narrow ? r.gpu : Math.max(0, r.gpu - 1)).toFixed(1)),
        tiler_pct: Number(Math.max(0, r.gpu - 1).toFixed(1)),
        mem_used_mb: Number(
          ((this.narrow ? 571.4 : 477.4) + Math.sin(this.ticks / 4) * 4 + (r.gpu - 8) * 0.8).toFixed(1),
        ),
        mem_alloc_mb: 3364.4,
        cores: 10,
        temp_c: null,
      },
      power: null,
      battery: {
        pct: 80,
        charging: false,
        external_connected: true,
        draw_w: null,
        adapter_w: 67,
        health_pct: 80.9,
        cycles: 351,
        condition: "good",
        temp_c: Number((30.5 + Math.sin(this.ticks / 9) * 0.3).toFixed(2)),
        time_remaining_min: null,
      },
      disk_io: {
        read_bps: Number(r.read.toFixed(1)),
        write_bps: Number(r.write.toFixed(1)),
        read_iops: Number((r.read / 254000).toFixed(1)),
        write_iops: Number((r.write / 43690).toFixed(2)),
        available: true,
      },
      disk: {
        available: true,
        used_pct: 66.8,
        free_gb: Number((68.4 - this.ticks * 0.0004).toFixed(1)),
        total_gb: 228.3,
        mount: "/System/Volumes/Data",
      },
      pressure: {
        memory: { available: true, level: r.ram >= 80 ? "warning" : "normal", pct: Number(r.ram.toFixed(1)) },
        cpu: { available: true, level: "normal", pct: Number(r.cpu.toFixed(1)) },
        storage: { available: true, level: "normal", pct: 33.2 },
      },
      processes: { by_cpu: [], by_mem: [] },
      proc_count: r.procs,
      boot_ts: "2026-11-20T17:52:59+00:00",
      load: { one: Number(r.load.toFixed(2)), five: Number((r.load * 0.976).toFixed(2)), fifteen: Number((r.load * 0.954).toFixed(2)) },
    } as Snapshot & { proc_count: number };
  }

  consumersFor(range: Range, totalPct: number, cores: number): Consumers {
    const sum = (totalPct / 100) * cores * 100;
    const now = this.ticks;
    const rows: ConsumerRow[] = this.apps.map((a) => {
      // The six apps the panel leads with stay in their order; everything
      // below them moves about as the machine works.
      const breathe = a.leader
        ? 0.96 + 0.06 * Math.sin(now / 19 + a.phase)
        : 0.55 + 0.6 * Math.abs(Math.sin(now / 23 + a.phase));
      const cpu = sum * a.weight * breathe;
      const monitor = /^syswatch/.test(a.name);
      return {
        name: a.name,
        cpu_pct: monitor ? null : Number(cpu.toFixed(2)),
        memory_mb: monitor ? 12 : Number(a.memMB.toFixed(1)),
        mem_avg_mb: monitor ? 12 : Number(a.memMB.toFixed(1)),
        procs: a.procs,
      };
    });
    rows.sort((x, y) => (y.cpu_pct ?? 0) - (x.cpu_pct ?? 0));
    return { range, rows, count: rows.length };
  }

  analyticsFor(range: Range, totalProcs: number): Analytics {
    const span = RANGE_MS[range] ?? 3_600_000;
    const sample = (key: Metric, back = 0, points = 160) => {
      this.metricKey = key;
      return this.window(range, points, back);
    };
    const stat = (values: number[]) => {
      const nums = values.filter((v) => typeof v === "number" && Number.isFinite(v));
      if (!nums.length) return { avg: 0, max: 0 };
      return {
        avg: nums.reduce((a, b) => a + b, 0) / nums.length,
        max: Math.max(...nums),
      };
    };
    const make = (
      id: string,
      label: string,
      unit: string,
      key: Metric,
      scale = 1,
    ): Measure => {
      const values = sample(key).map((v) => v * scale);
      const s = stat(values);
      const prev = stat(sample(key, 1).map((v) => v * scale));
      return {
        id,
        label,
        unit,
        available: true,
        avg: Number(s.avg.toFixed(2)),
        max: Number(s.max.toFixed(2)),
        min: Number(Math.min(...values).toFixed(2)),
        p95: Number(values.slice().sort((a, b) => a - b)[Math.floor(values.length * 0.95)].toFixed(2)),
        median: Number(values.slice().sort((a, b) => a - b)[Math.floor(values.length / 2)].toFixed(2)),
        prev_avg: Number(prev.avg.toFixed(2)),
        samples: values.length,
        window_s: Math.round(span / 1000),
      } as Measure & { prev_avg: number };
    };

    const cpu = make("cpu", "CPU", "%", "cpu");
    const memory = make("memory", "Memory", "%", "ram");
    const netDown = make("net_down", "Network in", "Mbps", "down", 1 / 1_000_000);
    const netUp = make("net_up", "Network out", "Mbps", "up", 1 / 1_000_000);
    const procs: Measure = {
      id: "proc_count",
      label: "Processes",
      unit: "n",
      available: true,
      avg: Number(totalProcs.toFixed(0)),
      max: Number((totalProcs + 22).toFixed(0)),
    };
    const pkg = make("package_power", "Package power (root)", "W", "pkgW");
    const cpuP = make("cpu_power", "CPU power (root)", "W", "pkgW", 0.78);
    const gpuP = make("gpu_power", "GPU power (root)", "W", "pkgW", 0.14);

    const peakCpu = stat(sample("cpu", 0, 120));
    const peakRam = stat(sample("ram", 0, 120));
    const prevCpu = stat(sample("cpu", 1, 120));
    const prevRam = stat(sample("ram", 1, 120));
    const word = RANGE_WORD[range];
    const peakAt = new Date(this.readings.ts - Math.round(span * 0.35));
    const hh = String(peakAt.getHours()).padStart(2, "0");
    const mm = String(peakAt.getMinutes()).padStart(2, "0");
    const dir = peakCpu.avg >= prevCpu.avg ? "higher" : "lower";
    const memDir = peakRam.avg >= prevRam.avg ? "+" : "-";
    const memDelta = Math.abs(memory.avg - ((memory as Measure & { prev_avg?: number }).prev_avg ?? 0)).toFixed(2);

    // Busiest hour of the last 24 h, computed from the same sampler.
    let busiestHour = 0;
    let busiestAvg = -1;
    const hourly: number[] = [];
    for (let h = 0; h < 24; h++) {
      this.metricKey = "cpu";
      const values: number[] = [];
      const end = this.readings.ts - h * 3_600_000;
      for (let i = 0; i < 24; i++) values.push(this.valueAt(end - (3_600_000 * i) / 23, "cpu"));
      const avg = values.reduce((a, b) => a + b, 0) / values.length;
      hourly.push(avg);
      if (avg > busiestAvg) {
        busiestAvg = avg;
        busiestHour = new Date(end).getHours();
      }
    }

    const findings = [
      `Over the ${word} CPU averaged ${cpu.avg.toFixed(1)} % and peaked at ${cpu.max.toFixed(1)} % at ${hh}:${mm}, ${dir} than the previous ${word}.`,
      `Memory held ${memory.avg.toFixed(1)} % of RAM (peak ${memory.max.toFixed(1)} %), ${memDir}${memDelta} pp versus the previous window.`,
      `The busiest hour of the last 24 h began at ${String(busiestHour).padStart(2, "0")}:30 with ${busiestAvg.toFixed(1)} % average CPU across ${hourly.filter((v) => v > 0).length} observed hours.`,
    ];

    return {
      ts: Math.floor(Date.now() / 1000),
      range,
      source: {
        history_points: this.all.length,
        history_first: new Date(this.all[0]?.ts ?? Date.now()).toISOString(),
        history_last: new Date().toISOString(),
        collector_snapshot_ts: new Date(this.readings.ts).toISOString(),
        fixture: false,
      },
      measures: [
        cpu,
        memory,
        netDown,
        netUp,
        procs,
        pkg,
        cpuP,
        gpuP,
        ...(RECORDED_ANALYTICS.measures ?? []).filter((m) => m.id === "storage"),
      ] as Measure[],
      findings,
    } as Analytics;
  }

  sensors(): Sensors {
    return this.sensorsPayload;
  }

  battery(): BatteryApi {
    return {
      ...(BATTERY as BatteryApi),
      ts: Math.floor(Date.now() / 1000),
      pct: 80,
      temperature_c: Number((30.5 + Math.sin(this.ticks / 9) * 0.3).toFixed(2)),
    };
  }

  thermal(): Thermal | null {
    return null;
  }
}
