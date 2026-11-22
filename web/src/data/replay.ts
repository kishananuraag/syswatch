// The collector's answers, served from the recording taken on this Mac.
// The live dashboard asks an HTTP API once a second; this build answers with
// the same payload shapes so every screen can be opened without the agent
// running. Longer ranges keep the recorded hour's shape, relabelled to the
// window being shown, exactly as the API's downsampler does.
import {
  SNAPSHOT,
  HISTORY,
  CONSUMERS,
  ANALYTICS,
  SENSORS,
  BATTERY,
  THERMAL,
} from "./recording";
import type {
  Analytics,
  Battery,
  BatteryApi,
  Consumers,
  History,
  Range,
  Sensors,
  Snapshot,
  Thermal,
} from "./types";

export const snapshot = SNAPSHOT as unknown as Snapshot;
export const sensors = SENSORS as unknown as Sensors;
export const battery = BATTERY as unknown as BatteryApi & Battery;
export const thermal = THERMAL as unknown as Thermal | null;

// The collector is polled once a second, so a wide window and a narrow one were
// photographed a few seconds apart and their cards read different samples of the
// same machine. The replay keeps both frames: the desktop sample above, and the
// one the tablet and phone layouts showed (processor 30.3 %, memory 83.4 %,
// graphics 8 % / 571 MB, network 0.7 KB/s, load 4.58, sample at 20:05:30).
// Every derived number (points above last hour, "Holds 81 % of new") is still
// worked out from this recording, so each frame stays true to the hour behind it.
export const narrowSnapshot: Snapshot = {
  ...snapshot,
  ts: "2026-11-21T20:05:30+00:00",
  age_s: 5,
  cpu: {
    ...snapshot.cpu,
    // 30.32 not 30.3: the card prints one decimal (30.3 %) and the comparison
    // with the recorded hour reads 0.5 points above it, as the original did.
    total_pct: 30.32,
    load: { one: 4.58, five: 4.45, fifteen: 4.34 },
  },
  ram: { ...snapshot.ram, pct: 83.4 },
  gpu: { ...snapshot.gpu, renderer_pct: 8, tiler_pct: 8, mem_used_mb: 571 },
  net: { down_bps: 716.8, up_bps: 716.8 },
};

const recordedHistory = HISTORY as unknown as History;

/** Distance behind the newest sample, in milliseconds, per range. */
const RANGE_MS: Record<Range, number> = {
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

export const RECORDED_END = Date.parse(snapshot.ts);
const POINTS = 498;

function resample(values: number[], n: number): number[] {
  if (!Array.isArray(values) || values.length === 0) return [];
  const out: number[] = [];
  for (let i = 0; i < n; i++) {
    out.push(values[Math.min(values.length - 1, Math.round((i / (n - 1)) * (values.length - 1)))]);
  }
  return out;
}

export function historyFor(range: Range): History {
  if (range === "1h") return recordedHistory;
  const span = RANGE_MS[range] ?? 3_600_000;
  const labels: string[] = [];
  for (let i = 0; i < POINTS; i++) {
    labels.push(new Date(RECORDED_END - ((POINTS - 1 - i) / (POINTS - 1)) * span).toISOString());
  }
  return {
    range,
    labels,
    series: {
      cpu: resample(recordedHistory.series.cpu, POINTS),
      ram: resample(recordedHistory.series.ram, POINTS),
      down_bps: resample(recordedHistory.series.down_bps, POINTS),
      up_bps: resample(recordedHistory.series.up_bps, POINTS),
    },
    temp_series: [],
    sensor_labels: [],
  };
}

export function consumersFor(range: Range): Consumers {
  const c = CONSUMERS as unknown as Consumers;
  return { ...c, range };
}

export function analyticsFor(range: Range): Analytics {
  const a = ANALYTICS as unknown as Analytics;
  return { ...a, range };
}
