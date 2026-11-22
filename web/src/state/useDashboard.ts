// The dashboard's one piece of state: which view, which range, which card is
// being looked at, which of the collector's endpoints answers, and the live
// tail of samples it keeps producing.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { DEFAULT_TILE_ORDER, type LiveSeries, type TileId } from "../lib/derive";
import { loadBase, poll, probe, probeSlow, saveBase, type RemotePayload } from "../collector/api";
import { LiveEngine } from "../collector/engine";
import type { ConsumerRow, Range } from "../data/types";

export type TabName = "overview" | "apps" | "analytics";

const TILE_KEY = "syswatch-tile-config";

function loadTiles(): TileId[] {
  try {
    const saved = window.localStorage.getItem(TILE_KEY);
    if (saved) {
      const parsed = JSON.parse(saved) as { hidden?: string[] } | string[];
      const hidden = Array.isArray(parsed) ? [] : (parsed.hidden ?? []);
      return DEFAULT_TILE_ORDER.filter((id) => !hidden.includes(id)) as TileId[];
    }
  } catch {
    /* fall through to every card */
  }
  return [...DEFAULT_TILE_ORDER] as TileId[];
}

function saveTiles(tiles: TileId[]): void {
  try {
    const hidden = DEFAULT_TILE_ORDER.filter((id) => !tiles.includes(id));
    window.localStorage.setItem(TILE_KEY, JSON.stringify({ hidden }));
  } catch {
    /* ignore */
  }
}

/** The tablet and phone layouts were photographed against the narrow frame. */
const NARROW = "(max-width: 900px)";

function narrowLayout(): boolean {
  return typeof window !== "undefined" && window.matchMedia(NARROW).matches;
}

/** The phone frame is its own photograph again below this width. */
const PHONE = "(max-width: 640px)";

function phoneLayout(): boolean {
  return typeof window !== "undefined" && window.matchMedia(PHONE).matches;
}

const byOrder = (a: TileId, b: TileId) =>
  DEFAULT_TILE_ORDER.indexOf(a) - DEFAULT_TILE_ORDER.indexOf(b);

export function useDashboard() {
  const [tab, setTab] = useState<TabName>("overview");
  const [range, setRange] = useState<Range>("1h");
  const [focused, setFocused] = useState<TileId | null>(null);
  const [deviceMenuOpen, setDeviceMenuOpen] = useState(false);
  const [pairHint, setPairHint] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [tiles, setTiles] = useState<TileId[]>(loadTiles);
  const [beat, setBeat] = useState(0);
  const [narrow, setNarrow] = useState(narrowLayout);
  const [phone, setPhone] = useState(phoneLayout);
  const [mode, setMode] = useState<"sim" | "live">("sim");
  const [apiBase, setApiBase] = useState<string>(loadBase);
  const [remote, setRemote] = useState<RemotePayload | null>(null);
  const [sampling, setSampling] = useState(false);
  const [shouldProbe, setShouldProbe] = useState(false);
  const beatRef = useRef(0);

  // Each frame keeps its own first sample: the desktop frame, the tablet
  // frame, and the phone frame were photographed on different seconds.
  const engine = useMemo(() => new LiveEngine(narrow), [narrow, phone]);

  // The page opens on the collector's last sample (the frame the screenshots
  // were taken from) and starts sampling shortly after, or as soon as a screen
  // or a range is picked. From then on it is one sample a second.
  useEffect(() => {
    setSampling(false);
    const id = window.setTimeout(() => setSampling(true), 1000);
    return () => window.clearTimeout(id);
  }, [engine]);

  useEffect(() => {
    if (!sampling) return;
    const id = window.setInterval(() => {
      engine.tick();
      beatRef.current += 1;
      setBeat(beatRef.current);
    }, 1000);
    return () => window.clearInterval(id);
  }, [engine, sampling]);

  // Knock once for a running agent only when an address was saved before, so a
  // plain static copy never fires a request the page cannot make.
  useEffect(() => {
    if (!shouldProbe) return;
    let cancelled = false;
    probe(apiBase).then((ok) => {
      if (cancelled) return;
      setMode(ok ? "live" : "sim");
    });
    return () => {
      cancelled = true;
    };
  }, [apiBase, shouldProbe]);

  // Live mode: the same seven endpoints the original asks, once a second, with
  // the heavy three refreshed every third tick and sensors/battery once.
  useEffect(() => {
    if (mode !== "live") return;
    let tick = 0;
    let stopped = false;
    const run = async () => {
      const payload = await poll(apiBase, range, tick % 3 === 0);
      if (stopped) return;
      if (!payload) {
        setRemote(null);
        setMode("sim");
        return;
      }
      tick += 1;
      setRemote((prev) => ({ ...(prev ?? {}), ...payload }) as RemotePayload);
      if (tick === 1) {
        const slow = await probeSlow(apiBase);
        if (!stopped) setRemote((prev) => ({ ...(prev ?? {}), ...slow }) as RemotePayload);
      }
    };
    run();
    const id = window.setInterval(run, 1000);
    return () => {
      stopped = true;
      window.clearInterval(id);
    };
  }, [mode, apiBase, range]);

  // Resizing across a breakpoint switches to that width's frame.
  useEffect(() => {
    const wide = window.matchMedia(NARROW);
    const small = window.matchMedia(PHONE);
    const onNarrow = (e: MediaQueryListEvent) => setNarrow(e.matches);
    const onPhone = (e: MediaQueryListEvent) => setPhone(e.matches);
    wide.addEventListener("change", onNarrow);
    small.addEventListener("change", onPhone);
    return () => {
      wide.removeEventListener("change", onNarrow);
      small.removeEventListener("change", onPhone);
    };
  }, []);

  const sim = useMemo(() => {
    const current = engine.current();
    return { current };
  }, [engine, beat]);

  const current = mode === "live" && remote?.current ? remote.current : sim.current;

  const history = useMemo(
    () => (mode === "live" && remote?.history ? remote.history : engine.historyFor(range)),
    [mode, remote?.history, engine, range, beat],
  );

  const consumers = useMemo(() => {
    if (mode === "live" && remote?.consumers) return remote.consumers;
    return engine.consumersFor(range, current.cpu?.total_pct ?? 0, current.cpu?.cores ?? 8);
  }, [mode, remote?.consumers, engine, range, beat, current.cpu?.total_pct, current.cpu?.cores]);

  const analytics = useMemo(() => {
    if (mode === "live" && remote?.analytics) return remote.analytics;
    return engine.analyticsFor(range, current.proc_count ?? 568);
  }, [mode, remote?.analytics, engine, range, beat, current]);

  const sensors = (mode === "live" && remote?.sensors) || engine.sensors();
  const battery = (mode === "live" && remote?.battery) || engine.battery();
  const thermal = mode === "live" ? (remote?.thermal ?? null) : engine.thermal();

  const live = useMemo<LiveSeries>(
    () => ({
      readMB: engine.metricSeries("1m", "read").map((v) => v / 1048576),
      writeMB: engine.metricSeries("1m", "write").map((v) => v / 1048576),
      gpu: engine.metricSeries("1m", "gpu"),
      battery: engine.metricSeries("1m", "batt"),
      load: engine.metricSeries("1m", "workload"),
    }),
    [engine, beat],
  );

  const metricSeries = useCallback(
    (id: string): number[] => {
      if (mode === "live" && remote?.history) {
        const s = remote.history.series;
        if (id === "cpu") return s.cpu;
        if (id === "memory") return s.ram;
        if (id === "net_down") return s.down_bps;
        if (id === "net_up") return s.up_bps;
      }
      return engine.metricSeries(range, id);
    },
    [mode, remote?.history, engine, range, beat],
  );

  const pickRange = useCallback((next: Range) => {
    setSampling(true);
    setRange(next);
  }, []);

  const selectTab = useCallback((next: TabName) => {
    setSampling(true);
    setTab(next);
  }, []);

  const focusTile = useCallback((id: TileId) => {
    // Clicking the isolated card again lets it go; clicking another one moves
    // the isolation to it. Escape clears it either way.
    setFocused((prev) => (prev === id ? null : id));
  }, []);

  const clearFocus = useCallback(() => setFocused(null), []);

  const openDeviceMenu = useCallback(() => {
    setSampling(true);
    setDeviceMenuOpen((open) => {
      if (open) setPairHint(false);
      return !open;
    });
  }, []);

  const closeDeviceMenu = useCallback(() => {
    setDeviceMenuOpen(false);
    setPairHint(false);
  }, []);

  const showPairing = useCallback(() => setPairHint(true), []);

  const toggleSettings = useCallback(() => setSettingsOpen((open) => !open), []);
  const closeSettings = useCallback(() => setSettingsOpen(false), []);

  const toggleTile = useCallback((id: TileId) => {
    setTiles((prev) => {
      const next = prev.includes(id)
        ? prev.filter((t) => t !== id)
        : ([...prev, id] as TileId[]).sort(byOrder);
      if (!next.length) return prev;
      saveTiles(next);
      return next;
    });
  }, []);

  const resetTiles = useCallback(() => {
    const all = [...DEFAULT_TILE_ORDER] as TileId[];
    saveTiles(all);
    setTiles(all);
  }, []);

  const connect = useCallback((base: string) => {
    const clean = base.trim().replace(/\/$/, "") || "http://127.0.0.1:8123";
    saveBase(clean);
    setApiBase(clean);
    setRemote(null);
    setMode("sim");
    setSampling(true);
    setShouldProbe(true);
  }, []);

  const openApps = useCallback(() => setTab("apps"), []);

  const openAnalytics = useCallback(() => setTab("analytics"), []);

  /** The busiest apps as the panels rank them: share of the whole machine. */
  const rankedApps = useMemo<ConsumerRow[]>(() => {
    const rows = Array.isArray(consumers.rows) ? consumers.rows : [];
    const cores = current.cpu?.cores && current.cpu.cores > 0 ? current.cpu.cores : 1;
    const byName = new Map<string, ConsumerRow>();
    rows.forEach((r) => {
      const name = r && r.name ? String(r.name) : "unknown";
      const row =
        byName.get(name) ??
        ({
          name,
          cpu_pct: 0,
          memory_mb: r.memory_mb ?? 0,
          mem_avg_mb: r.mem_avg_mb ?? 0,
          procs: 0,
        } as ConsumerRow);
      row.cpu_pct = (row.cpu_pct ?? 0) + (typeof r.cpu_pct === "number" ? r.cpu_pct : 0);
      row.procs += 1;
      byName.set(name, row);
    });
    return Array.from(byName.values())
      .map((r) => ({ ...r, cpu_pct: Math.min(100, (r.cpu_pct ?? 0) / cores) }))
      .sort((a, b) => (b.cpu_pct ?? 0) - (a.cpu_pct ?? 0));
  }, [consumers, current.cpu?.cores]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setFocused(null);
        setSettingsOpen(false);
        setDeviceMenuOpen(false);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  return {
    tab,
    setTab,
    selectTab,
    sampling,
    range,
    pickRange,
    focused,
    focusTile,
    clearFocus,
    deviceMenuOpen,
    openDeviceMenu,
    closeDeviceMenu,
    pairHint,
    showPairing,
    settingsOpen,
    toggleSettings,
    closeSettings,
    tiles,
    toggleTile,
    resetTiles,
    current,
    history,
    consumers,
    analytics,
    sensors,
    battery,
    thermal,
    live,
    rankedApps,
    metricSeries,
    mode,
    apiBase,
    connect,
    openApps,
    openAnalytics,
    ageSeconds: typeof current.age_s === "number" ? current.age_s : 4,
  };
}

export type Dashboard = ReturnType<typeof useDashboard>;
