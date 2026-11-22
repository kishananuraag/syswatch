// The two panels under the cards: who is using the processor, and what the
// machine is saying about itself right now.
import { useState } from "react";
import type { AppRow, StatusRow } from "../lib/derive";
import { fmt, isNum } from "../lib/format";

export function AppList({ rows, interactive = false }: { rows: AppRow[]; interactive?: boolean }) {
  const [open, setOpen] = useState<string | null>(null);
  if (!rows.length) return <div className="muted">not available on this agent yet</div>;
  const max = rows.reduce((m, a) => Math.max(m, isNum(a.cpu_pct) ? a.cpu_pct : 0), 0) || 1;

  const parts = (a: AppRow) => {
    const value = isNum(a.cpu_pct) ? a.cpu_pct.toFixed(1) + " %" : "—";
    const width = isNum(a.cpu_pct) ? Math.max(2, (a.cpu_pct / max) * 100) : 2;
    return (
      <>
        <span className="appname" title={a.name}>
          {a.name}
        </span>
        <span className="bar">
          <span style={{ width: Math.min(100, width).toFixed(1) + "%" }}></span>
        </span>
        <span className="appval">{value}</span>
      </>
    );
  };

  if (!interactive) {
    return (
      <>
        {rows.map((a) => (
          <div className="approw" key={a.name}>
            {parts(a)}
          </div>
        ))}
      </>
    );
  }

  return (
    <>
      {rows.map((a) => {
        const expanded = open === a.name;
        return (
          <div className={expanded ? "approw has-line is-open" : "approw has-line"} key={a.name}>
            <button
              className="approw-line"
              onClick={() => setOpen(expanded ? null : a.name)}
              aria-expanded={expanded}
              title={a.name + " · click for memory and processes"}
            >
              {parts(a)}
            </button>
            {expanded && (
              <div className="appdetail">
                <span>
                  {a.procs} process{a.procs === 1 ? "" : "es"}
                </span>
                <span>{fmt(a.memory_mb, 1)} MB resident</span>
                <span>{isNum(a.cpu_pct) ? fmt(a.cpu_pct / (a.procs || 1), 1) : "—"} % per process</span>
              </div>
            )}
          </div>
        );
      })}
    </>
  );
}

const WARN_ICON = (
  <svg className="icon warn" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
    <path
      d="M8 2.2l6.2 11H1.8z M8 6.6v3 M8 11.6v.1"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinejoin="round"
      strokeLinecap="round"
    />
  </svg>
);

export function StatusList({ rows }: { rows: StatusRow[] }) {
  return (
    <>
      {rows.map((row) => (
        <div className="statusrow" key={row.title}>
          {row.icon === "warn" ? (
            WARN_ICON
          ) : (
            <span className="icon">
              <span className="sdot"></span>
            </span>
          )}
          <div>
            <div className={row.warn ? "title warn" : "title"}>{row.title}</div>
            <div className="sub">{row.sub}</div>
          </div>
        </div>
      ))}
    </>
  );
}

export function AppsPanel({ rows, onOpenApps }: { rows: AppRow[]; onOpenApps: () => void }) {
  return (
    <div className="panel panel-wide">
      <div className="panel-head">
        <span className="panel-label" id="appsTitle">
          APPS USING THE MOST · % OF THE PROCESSOR · LAST HOUR
        </span>
        <button className="linkbtn" onClick={onOpenApps}>
          All apps
        </button>
      </div>
      <div id="topApps" className="applist">
        <AppList rows={rows} />
      </div>
    </div>
  );
}

export function StatusPanel({ rows }: { rows: StatusRow[] }) {
  return (
    <div className="panel panel-status">
      <div className="panel-label">Status</div>
      <div id="status" className="statuslist">
        <StatusList rows={rows} />
      </div>
    </div>
  );
}
