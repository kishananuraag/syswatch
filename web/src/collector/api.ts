// The dashboard can talk to a running syswatch agent, exactly as the original
// page does: /api/current, /api/history, /api/analytics, /api/consumers,
// /api/sensors, /api/thermal and /api/battery on 127.0.0.1:8123. When nothing
// answers (the usual case for a built copy opened on its own) the engine in
// ./engine.ts keeps the same contract alive from recorded samples.
import type {
  Analytics,
  BatteryApi,
  Consumers,
  History,
  Range,
  Sensors,
  Snapshot,
  Thermal,
} from "../data/types";

export const DEFAULT_BASE =
  typeof location !== "undefined" && location.protocol.indexOf("http") === 0
    ? location.origin
    : "http://127.0.0.1:8123";
const KEY = "syswatch-api";

export function loadBase(): string {
  try {
    const saved = window.localStorage.getItem(KEY);
    if (saved) return saved.replace(/\/$/, "");
  } catch {
    /* storage can be blocked; the default still works */
  }
  return DEFAULT_BASE;
}

export function saveBase(base: string): void {
  try {
    window.localStorage.setItem(KEY, base.replace(/\/$/, ""));
  } catch {
    /* ignore */
  }
}

async function getJson<T>(base: string, path: string, timeout = 1500): Promise<T | null> {
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), timeout);
  try {
    const res = await fetch(base + path, { signal: controller.signal, cache: "no-store" });
    if (!res.ok) return null;
    return (await res.json()) as T;
  } catch {
    return null;
  } finally {
    window.clearTimeout(timer);
  }
}

/** One quiet knock: is a collector answering on this address? */
export async function probe(base: string): Promise<boolean> {
  const snap = await getJson<Snapshot>(base, "/api/current", 1200);
  return !!snap && !!snap.cpu && typeof snap.cpu.total_pct === "number";
}

export interface RemotePayload {
  current: Snapshot;
  history: History | null;
  consumers: Consumers | null;
  analytics: Analytics | null;
  sensors: Sensors | null;
  battery: BatteryApi | null;
  thermal: Thermal | null;
}

export async function poll(base: string, range: Range, heavy: boolean): Promise<Partial<RemotePayload> | null> {
  const current = await getJson<Snapshot>(base, "/api/current", 1400);
  if (!current) return null;
  const out: Partial<RemotePayload> = { current };
  if (heavy) {
    const [history, consumers, analytics] = await Promise.all([
      getJson<History>(base, `/api/history?range=${range}`, 2500),
      getJson<Consumers>(base, `/api/consumers?range=${range}`, 2500),
      getJson<Analytics>(base, `/api/analytics?range=${range}`, 2500),
    ]);
    if (history) out.history = history;
    if (consumers) out.consumers = consumers;
    if (analytics) out.analytics = analytics;
  }
  return out;
}

export async function probeSlow(base: string): Promise<Partial<RemotePayload>> {
  const [sensors, battery, thermal] = await Promise.all([
    getJson<Sensors>(base, "/api/sensors", 3000),
    getJson<BatteryApi>(base, "/api/battery", 2000),
    getJson<Thermal>(base, "/api/thermal", 2000),
  ]);
  const out: Partial<RemotePayload> = {};
  if (sensors) out.sensors = sensors;
  if (battery) out.battery = battery;
  if (thermal) out.thermal = thermal;
  return out;
}
