// The KPI grid: six single cards, then three double-width cards that carry a
// dual-series chart. Clicking a card isolates it; the rest fade back. Charts
// can be dragged to zoom (double-click resets) and a value can be hovered for
// its recent graph, as the hint under the grid promises.
import { useRef, useState } from "react";
import type { BatteryApi, History, Range, Sensors, Snapshot, Thermal } from "../data/types";
import {
  TILE_META,
  WIDE_TILES,
  gpuTemp,
  maxTemp,
  tileDelta,
  tileSeries,
  tileSub,
  tileValue,
  timeLabels,
  wideSeries,
  type LiveSeries,
  type TileId,
} from "../lib/derive";
import { fmt, isNum } from "../lib/format";
import { Spark, WideSpark } from "./Sparkline";

interface Props {
  current: Snapshot;
  history: History;
  sensors: Sensors | null;
  thermal: Thermal | null;
  batteryApi: BatteryApi;
  range: Range;
  live: LiveSeries;
  focused: TileId | null;
  onFocus: (id: TileId) => void;
  tiles: TileId[];
}

const CHEVRON = (
  <svg className="chev" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
    <path
      d="M6 3.5L10.5 8 6 12.5"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);

interface Win {
  from: number;
  to: number;
}

function slice<T>(arr: T[], win: Win | null): T[] {
  if (!win || !arr.length) return arr;
  const a = Math.max(0, Math.min(arr.length - 1, Math.floor(win.from * arr.length)));
  const b = Math.max(a + 1, Math.min(arr.length, Math.ceil(win.to * arr.length)));
  return arr.slice(a, b);
}

function fraction(e: React.PointerEvent<HTMLElement>): number {
  const box = e.currentTarget.getBoundingClientRect();
  if (!box.width) return 0;
  return Math.max(0, Math.min(1, (e.clientX - box.left) / box.width));
}

function ValueTip({ values, unit }: { values: number[]; unit: string }) {
  const nums = values.filter(isNum);
  const last = nums.length ? nums[nums.length - 1] : NaN;
  return (
    <span className="value-graph" role="presentation">
      <span className="vg-head">
        <span>recent {unit}</span>
        <span className="vg-last">{isNum(last) ? fmt(last, 1) : "—"}</span>
      </span>
      <Spark values={values.slice(-60)} height={34} />
    </span>
  );
}

function WideFooter({
  id,
  current,
  sensors,
  thermal,
}: {
  id: TileId;
  current: Snapshot;
  sensors: Sensors | null;
  thermal: Thermal | null;
}) {
  if (id === "network") {
    const down = isNum(current.net?.down_bps) ? current.net.down_bps / 1024 : null;
    const up = isNum(current.net?.up_bps) ? current.net.up_bps / 1024 : null;
    return (
      <span className="tile-footer">
        <span className="series" title={"Downloading " + fmt(down, 1) + " KB/s"}>
          <span className="swatch acc"></span>Downloading {fmt(down, 1)} KB/s
        </span>
        <span className="series" title={"Uploading " + fmt(up, 1) + " KB/s"}>
          <span className="swatch l2"></span>Uploading {fmt(up, 1)} KB/s
        </span>
      </span>
    );
  }
  if (id === "storage") {
    const readMB = isNum(current.disk_io?.read_bps) ? (current.disk_io.read_bps as number) / 1048576 : null;
    const writeMB = isNum(current.disk_io?.write_bps) ? (current.disk_io.write_bps as number) / 1048576 : null;
    const show = (v: number | null) =>
      isNum(v) && v >= 1 ? fmt(v, 1) + " MB/s" : fmt(isNum(v) ? v * 1024 : null, 0) + " KB/s";
    return (
      <span className="tile-footer">
        <span className="series" title={"Reading " + show(readMB)}>
          <span className="swatch acc"></span>Reading {show(readMB)}
        </span>
        <span className="series" title={"Writing " + show(writeMB)}>
          <span className="swatch l2"></span>Writing {show(writeMB)}
        </span>
      </span>
    );
  }
  if (id === "thermal") {
    const cpu = maxTemp(current, sensors);
    const gpu = gpuTemp(current, thermal);
    const heat =
      (current as { thermal?: { thermal_pressure?: string } }).thermal?.thermal_pressure ??
      (current.power?.thermal_pressure || "normal");
    return (
      <span className="tile-footer">
        <span className="series" title={"Processor " + fmt(cpu, 1) + " °C"}>
          <span className="swatch acc"></span>Processor {fmt(cpu, 1)} °C
        </span>
        <span
          className="series"
          title={"Graphics " + fmt(gpu, 1) + " °C · heat level " + String(heat).toLowerCase()}
        >
          <span className="swatch l2"></span>Graphics {fmt(gpu, 1)} °C · heat level {String(heat).toLowerCase()}
        </span>
      </span>
    );
  }
  return null;
}

export function Tiles({
  current,
  history,
  sensors,
  thermal,
  batteryApi,
  range,
  live,
  focused,
  onFocus,
  tiles,
}: Props) {
  const [zoom, setZoom] = useState<{ id: TileId; win: Win } | null>(null);
  const [preview, setPreview] = useState<{ id: TileId; win: Win } | null>(null);
  const [hovered, setHovered] = useState<TileId | null>(null);
  const drag = useRef<{ id: TileId; from: number } | null>(null);

  const zoomOf = (id: TileId) => (zoom && zoom.id === id ? zoom.win : null);
  const baseLabels = timeLabels(history);

  const start = (id: TileId) => (e: React.PointerEvent<HTMLElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    drag.current = { id, from: fraction(e) };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const move = (id: TileId) => (e: React.PointerEvent<HTMLElement>) => {
    if (!drag.current || drag.current.id !== id) return;
    const to = fraction(e);
    setPreview({ id, win: { from: Math.min(drag.current.from, to), to: Math.max(drag.current.from, to) } });
  };
  const finish = (id: TileId) => (e: React.PointerEvent<HTMLElement>) => {
    if (!drag.current || drag.current.id !== id) return;
    const to = fraction(e);
    const win = { from: Math.min(drag.current.from, to), to: Math.max(drag.current.from, to) };
    drag.current = null;
    setPreview(null);
    if (win.to - win.from > 0.04) setZoom({ id, win });
  };
  const reset = (id: TileId) => () => {
    setZoom((prev) => (prev && prev.id === id ? null : prev));
    setPreview(null);
  };

  return (
    <section id="tiles" className="tiles">
      {tiles.map((id) => {
        const meta = TILE_META[id];
        const wide = WIDE_TILES.has(id);
        const value = tileValue(id, current, sensors);
        const showUnit = value !== "—" && meta.unit !== "";
        const dimmed = !!focused && focused !== id;
        const classes = ["tile", wide ? "wide" : "", focused === id ? "is-focused" : "", dimmed ? "is-dimmed" : ""]
          .filter(Boolean)
          .join(" ");
        const win = preview && preview.id === id ? preview.win : zoomOf(id);

        if (wide) {
          const [firstFull, secondFull] = wideSeries(id, history, live);
          const first = slice(firstFull, win);
          const second = slice(secondFull, win);
          const xLabels = win ? slice(baseLabels, win) : baseLabels;
          return (
            <div
              key={id}
              className={classes}
              data-tile={id}
              tabIndex={0}
              role="button"
              aria-expanded={focused === id}
              onClick={() => onFocus(id)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onFocus(id);
                }
              }}
            >
              <div className="tile-head">
                <span className="tile-label">{meta.label}</span>
                {CHEVRON}
              </div>
              <button
                className="tile-help"
                aria-label={"What is " + meta.label + "?"}
                onClick={(e) => e.stopPropagation()}
              >
                <span>?</span>
              </button>
              <span
                className="tile-value"
                onMouseEnter={() => setHovered(id)}
                onMouseLeave={() => setHovered(null)}
              >
                {value}
                {showUnit && <span className="tile-unit">{meta.unit}</span>}
                {hovered === id && <ValueTip values={firstFull} unit={meta.unit} />}
              </span>
              <span
                className="tile-spark wide"
                onPointerDown={start(id)}
                onPointerMove={move(id)}
                onPointerUp={finish(id)}
                onPointerCancel={finish(id)}
                onDoubleClick={reset(id)}
              >
                <WideSpark first={first} second={second} />
                {win && (
                  <span
                    className="zoom-box"
                    style={{ left: win.from * 100 + "%", width: Math.max(0.6, (win.to - win.from) * 100) + "%" }}
                  ></span>
                )}
                <span className="tile-xaxis">
                  {xLabels.map((l, i) => (
                    <span key={i}>{l}</span>
                  ))}
                </span>
              </span>
              <WideFooter id={id} current={current} sensors={sensors} thermal={thermal} />
              {zoomOf(id) && <span className="zoom-hint">double-click resets</span>}
            </div>
          );
        }

        const series = tileSeries(id, history, live);
        const sub = tileSub(id, current, batteryApi);
        const delta = tileDelta(id, current, history, sensors, thermal, range);
        return (
          <div
            key={id}
            className={classes}
            data-tile={id}
            tabIndex={0}
            role="button"
            aria-expanded={focused === id}
            onClick={() => onFocus(id)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onFocus(id);
              }
            }}
          >
            <div className="tile-head">
              <span className="tile-label">{meta.label}</span>
            </div>
            <button
              className="tile-help"
              aria-label={"What is " + meta.label + "?"}
              onClick={(e) => e.stopPropagation()}
            >
              <span>?</span>
            </button>
            <span
              className="tile-value"
              onMouseEnter={() => setHovered(id)}
              onMouseLeave={() => setHovered(null)}
            >
              {value}
              {showUnit && <span className="tile-unit">{meta.unit}</span>}
              {hovered === id && <ValueTip values={tileSeries(id, history, live)} unit={meta.unit} />}
            </span>
            <span
              className="tile-spark"
              onPointerDown={start(id)}
              onPointerMove={move(id)}
              onPointerUp={finish(id)}
              onPointerCancel={finish(id)}
              onDoubleClick={reset(id)}
            >
              <Spark values={slice(series, win)} />
              {win && (
                <span
                  className="zoom-box"
                  style={{ left: win.from * 100 + "%", width: Math.max(0.6, (win.to - win.from) * 100) + "%" }}
                ></span>
              )}
            </span>
            {sub && (
              <span className="tile-sub">
                {sub.split(" · ").map((part, i) => (
                  <span key={part + i} title={part}>
                    {i ? "· " : ""}
                    {part}
                  </span>
                ))}
              </span>
            )}
            {delta && (
              <span className="tile-delta" title={delta}>
                {delta}
              </span>
            )}
          </div>
        );
      })}
    </section>
  );
}
