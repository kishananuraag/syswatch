// Apps view: every process group the collector ranked for the chosen window,
// with the same bar as the overview panel, and a search and sort so the long
// list can be read. Clicking a row opens its memory and process count.
import { useMemo, useState } from "react";
import type { Range } from "../data/types";
import { RANGE_WORD } from "../collector/engine";
import type { AppRow } from "../lib/derive";
import { AppList } from "./Panels";

type Sort = "cpu" | "mem" | "name";

const SORTS: Array<{ id: Sort; label: string }> = [
  { id: "cpu", label: "CPU" },
  { id: "mem", label: "Memory" },
  { id: "name", label: "Name" },
];

export function AppsView({
  rows,
  range,
  mode,
  hidden,
}: {
  rows: AppRow[];
  range: Range;
  mode: "sim" | "live";
  hidden?: boolean;
}) {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<Sort>("cpu");

  const listed = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const filtered = needle ? rows.filter((r) => r.name.toLowerCase().includes(needle)) : rows.slice();
    filtered.sort((a, b) => {
      if (sort === "name") return a.name.localeCompare(b.name);
      if (sort === "mem") return (b.memory_mb ?? 0) - (a.memory_mb ?? 0);
      return (b.cpu_pct ?? 0) - (a.cpu_pct ?? 0);
    });
    return filtered;
  }, [rows, query, sort]);

  return (
    <section id="tab-apps" className="tabview" hidden={hidden}>
      <div className="panel panel-full">
        <div className="panel-head apps-head">
          <span className="panel-label" id="appsTitle">
            APPS USING THE MOST · % OF THE PROCESSOR · {RANGE_WORD[range].toUpperCase()}
          </span>
          <div className="app-tools">
            <input
              className="app-search"
              type="search"
              value={query}
              placeholder="Filter apps"
              aria-label="Filter apps"
              onChange={(e) => setQuery(e.target.value)}
            />
            <div className="app-sort" role="group" aria-label="Sort apps">
              {SORTS.map((s) => (
                <button
                  key={s.id}
                  className={sort === s.id ? "sort-pill active" : "sort-pill"}
                  aria-pressed={sort === s.id}
                  onClick={() => setSort(s.id)}
                >
                  {s.label}
                </button>
              ))}
            </div>
          </div>
        </div>
        <div className="apps-meta">
          {listed.length} of {rows.length} apps · {mode === "live" ? "asked of the collector" : "sampled"} once a
          second · ranked by {sort === "mem" ? "memory" : sort === "name" ? "name" : "share of the processor"}
        </div>
        <div id="appsTab" className="applist applist-full">
          {listed.length ? (
            <AppList rows={listed} interactive />
          ) : (
            <div className="muted">no app matches “{query}”</div>
          )}
        </div>
      </div>
    </section>
  );
}
