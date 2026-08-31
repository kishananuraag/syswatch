"""API test battery for the syswatch dashboard (run while server is on 8123)."""
import json
import urllib.request

BASE = "http://127.0.0.1:8123"


def get(path):
    with urllib.request.urlopen(BASE + path, timeout=30) as r:
        return r.status, json.loads(r.read().decode())


st, cur = get("/api/current")
print("=== /api/current [%d] ===" % st)
print(json.dumps({
    "ts": cur["ts"], "source": cur.get("source"),
    "cpu_total_pct": cur["cpu"]["total_pct"],
    "per_core_n": len(cur["cpu"]["per_core_pct"]),
    "freq_mhz": cur["cpu"].get("freq_mhz"),
    "ram_pct": cur["ram"]["pct"],
    "ram_used_gb": round(cur["ram"]["used_bytes"] / 2 ** 30, 2),
    "ram_total_gb": round(cur["ram"]["total_bytes"] / 2 ** 30, 2),
    "net_down_bps": cur["net"]["down_bps"],
    "net_up_bps": cur["net"]["up_bps"],
    "top_cpu_proc": cur["processes"]["by_cpu"][0]["name"] if cur["processes"]["by_cpu"] else None,
    "top_mem_proc_mb": cur["processes"]["by_mem"][0]["mem_mb"] if cur["processes"]["by_mem"] else None,
    "temps_c": cur.get("temps_c"),
}, indent=1))

for rng in ("10m", "15m", "1h", "2d", "5d", "7d"):
    st, h = get("/api/history?range=" + rng)
    pts = h["points"]
    cpu_vals = [p["cpu"] for p in pts if p["cpu"] is not None]
    ram_vals = [p["ram"] for p in pts if p["ram"] is not None]
    print("=== history %-3s [%d] points=%-4d first_ts=%s last_ts=%s "
          "cpu[min/avg/max]=%.0f/%.0f/%.0f ram_avg=%.0f ==="
          % (rng, st, len(pts),
             (pts[0]["ts"][11:16] if pts else "-"),
             (pts[-1]["ts"][11:16] if pts else "-"),
             min(cpu_vals), sum(cpu_vals) / max(len(cpu_vals), 1), max(cpu_vals),
             sum(ram_vals) / max(len(ram_vals), 1)) if pts else
          "=== history %s [%d] EMPTY ===" % (rng, st))

st, logs = get("/api/logs?limit=50")
print("=== /api/logs [%d] count=%d buffer=%d ===" % (st, len(logs["events"]), logs["count"]))
for e in logs["events"][:5]:
    print("  ", e["ts"][11:19], e["src"], e["msg"][:70])

with urllib.request.urlopen(BASE + "/", timeout=10) as r:
    html = r.read().decode()
print("=== / [%d] len=%d has_chartjs_ref=%s ==="
      % (200, len(html), "/static/app.js" in html))
with urllib.request.urlopen(BASE + "/static/chart.umd.min.js", timeout=10) as r:
    print("=== chart.js [%d] bytes=%d ===" % (r.status, len(r.read())))
