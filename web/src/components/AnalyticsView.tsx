// Analytics view: the measured averages for the chosen window, then the
// findings the analyser wrote about it. A measure opens its own graph for the
// window it was computed over.
import { useState } from "react";
import type { Analytics, Measure, Range } from "../data/types";
import { RANGE_WORD } from "../collector/engine";
import { fmt } from "../lib/format";
import { Spark } from "./Sparkline";

function MeasureRow({
  measure,
  values,
  open,
  onToggle,
}: {
  measure: Measure;
  values: number[];
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <div className={open ? "measure-block open" : "measure-block"}>
      <button className="measure" aria-expanded={open} onClick={onToggle}>
        <span>{measure.label}</span>
        <span>
          {fmt(measure.avg, 2)} {measure.unit} avg · max {fmt(measure.max, 2)}
        </span>
      </button>
      {open && (
        <div className="measure-graph">
          <Spark values={values} height={64} />
          <div className="measure-stats">
            <span>p95 {fmt(measure.p95, 1)}</span>
            <span>median {fmt(measure.median, 1)}</span>
            <span>{measure.samples ?? values.length} samples</span>
            <span>window {Math.round((measure.window_s ?? 0) / 60) || 1} min</span>
          </div>
        </div>
      )}
    </div>
  );
}

export function AnalyticsView({
  analytics,
  range,
  mode,
  metricSeries,
  hidden,
}: {
  analytics: Analytics;
  range: Range;
  mode: "sim" | "live";
  metricSeries: (id: string) => number[];
  hidden?: boolean;
}) {
  const [open, setOpen] = useState<string | null>(null);
  const measures = Array.isArray(analytics.measures) ? analytics.measures.filter((m) => m.available) : [];
  const findings = Array.isArray(analytics.findings) ? analytics.findings : [];

  return (
    <section id="tab-analytics" className="tabview" hidden={hidden}>
      <div className="panel panel-full">
        <div className="panel-head analytics-head">
          <span className="panel-label">ANALYTICS · {RANGE_WORD[range].toUpperCase()}</span>
          <span className="analytics-meta">
            {mode === "live" ? "from the collector" : "recomputed every second"} ·{" "}
            {measures.length} measures
          </span>
        </div>
        <div id="analyticsTab" className="analytics">
          {measures.map((m) => (
            <MeasureRow
              key={m.id}
              measure={m}
              values={metricSeries(m.id)}
              open={open === m.id}
              onToggle={() => setOpen(open === m.id ? null : m.id)}
            />
          ))}
          {findings.map((f, i) => (
            <div className="finding" key={f + i}>
              {f}
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
