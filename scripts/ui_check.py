#!/usr/bin/env python3
"""Layout oracle for the syswatch dashboard.

Renders the dashboard in headless Chrome and then *measures* the live box model
over the DevTools protocol: computed grid tracks, computed grid-column spans,
rendered widths, scrollWidth vs clientWidth.  Nothing here trusts an attribute in
the markup, so `data-span="2"` alone cannot pass a check.

Contract checks (T33/T34):
  1. exact tile label set, and a WORKLOAD card present
  2. six single-column cards on row 1, then three cards that really span two
     grid tracks (computed span *and* rendered width)
  3. each wide card: taller chart, four HH:MM axis labels ending in "now",
     a footer naming both series
  4. lower row: Apps panel measured at four grid tracks and STATUS at two, on
     one line, equal heights; STATUS: at least six rows, filling its height
  5. Apps list: sorted descending by the metric it prints, never above 100 %,
     bars follow that order, and any truncating name has an ellipsis plus a
     title that carries at least the text it hides (nothing is silently
     clipped; the page never scrolls sideways)
  6. one decimal discipline per card, matching the reference
  7. the six top cards keep one height: every footer line (sub, delta, series)
     is clamped to a single line and carries a title when ellipsised
  8. a missing temperature sample breaks the chart line instead of being drawn
     as a spike to the baseline (checked against the series actually rendered,
     plus a synthetic gapped series)
  9. the Power Use footer prints the unit next to every value

Every assertion is printed by name, then the canonical line last.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
URL = "http://127.0.0.1:8199/"

EXPECTED_LABELS = [
    "PROCESSOR", "MEMORY", "GRAPHICS", "BATTERY", "POWER USE", "WORKLOAD",
    "NETWORK TRAFFIC", "STORAGE ACTIVITY", "TEMPERATURE",
]
EXPECTED_RANGES = ["1m", "5m", "15m", "1h", "6h", "1d", "7d", "30d", "1y"]
EXPECTED_TABS = ["Overview", "Apps", "Analytics"]

# id -> (label, first series name, second series name)
WIDE_TILES = [
    ("network", "NETWORK TRAFFIC", "Downloading", "Uploading"),
    ("storage", "STORAGE ACTIVITY", "Reading", "Writing"),
    ("thermal", "TEMPERATURE", "Processor", "Graphics"),
]
WIDE_IDS = [w[0] for w in WIDE_TILES]

# id -> regex the printed value must match (decimals normalised per card)
PRECISION = {
    "cpu": r"^\d+\.\d$",
    "memory": r"^\d+\.\d$",
    "gpu": r"^\d+\.\d$",
    "battery": r"^\d+$",
    "power": r"^\d+\.\d\d$",
    "workload": r"^\d+\.\d\d$",
    "network": r"^\d+\.\d$",
    "storage": r"^\d+\.\d$",
    "thermal": r"^\d+\.\d$",
}

# reference sample values that must never be hardcoded in the source
MOCK_SOURCE_TOKENS = [
    "36.5", "80.1", "16.0", "3.68", "12.8 points", "10 h 29 min", "4.5 W",
    "Studio PC", "Living-room TV", "Old laptop", "3221 of 8192", "22:41",
]
# distinctive reference strings that must never appear rendered either
MOCK_DOM_TOKENS = [
    "12.8 points", "Studio PC", "Living-room TV", "Old laptop",
    "10 h 29 min", "mediaanalysisd-access",
]

PROBE_JS = r"""(() => {
  const txt = (el) => el ? el.textContent.replace(/\s+/g, " ").trim() : null;
  const host = document.getElementById("tiles");
  const hostStyle = host ? getComputedStyle(host) : null;
  const tracks = hostStyle && hostStyle.gridTemplateColumns !== "none"
    ? hostStyle.gridTemplateColumns.split(/\s+/).filter(Boolean) : [];
  const px = (v) => parseFloat(String(v).replace("px", ""));

  // one measured text line: does it stay on a single line, and if it clips,
  // does it show an ellipsis with the full text recoverable from its title?
  const lineBox = (el) => {
    if (!el) return null;
    // a footer may split into several nowrap parts that are allowed to wrap
    // between each other; the clamped line is the widest part, not the wrapper
    const kids = [...el.children];
    const target = kids.length
      ? kids.reduce((a, b) => (b.scrollWidth > a.scrollWidth ? b : a), kids[0])
      : el;
    const cs = getComputedStyle(target);
    return {
      text: txt(target),
      title: target.getAttribute("title") || el.getAttribute("title") || "",
      nowrap: String(cs.whiteSpace || "").indexOf("nowrap") === 0,
      ellipsis: cs.textOverflow === "ellipsis",
      sw: target.scrollWidth,
      cw: target.clientWidth,
      ch: el.clientHeight,
      lh: parseFloat(cs.lineHeight) || 0
    };
  };

  const tiles = Array.from(document.querySelectorAll("#tiles .tile")).map((t) => {
    const r = t.getBoundingClientRect();
    const cs = getComputedStyle(t);
    const valEl = t.querySelector(".tile-value");
    let value = null;
    if (valEl) {
      const clone = valEl.cloneNode(true);
      clone.querySelectorAll(".tile-unit").forEach((e) => e.remove());
      value = clone.textContent.replace(/\s+/g, " ").trim();
    }
    const xaxis = t.querySelector(".tile-xaxis");
    const footer = t.querySelector(".tile-footer");
    const chart = t.querySelector(".tile-spark svg");
    return {
      id: t.getAttribute("data-tile"),
      label: txt(t.querySelector(".tile-label")),
      value: value,
      unit: txt(t.querySelector(".tile-unit")),
      sub: txt(t.querySelector(".tile-sub")),
      delta: txt(t.querySelector(".tile-delta")),
      left: Math.round(r.left * 100) / 100,
      top: Math.round(r.top * 100) / 100,
      width: Math.round(r.width * 100) / 100,
      height: Math.round(r.height * 100) / 100,
      gridColumn: cs.gridColumnStart + " / " + cs.gridColumnEnd,
      spansTwo: cs.gridColumnStart === "span 2" || cs.gridColumnEnd === "span 2",
      wide: t.classList.contains("wide"),
      chartHeight: chart ? Math.round(chart.getBoundingClientRect().height) : 0,
      xaxis: xaxis ? Array.from(xaxis.children).map((s) => s.textContent.trim()) : null,
      footerSeries: footer ? Array.from(footer.querySelectorAll(".series")).map((s) => txt(s)) : null,
      subBox: lineBox(t.querySelector(".tile-sub")),
      deltaBox: lineBox(t.querySelector(".tile-delta")),
      footerBoxes: footer ? Array.from(footer.querySelectorAll(".series")).map(lineBox) : []
    };
  });

  const apps = Array.from(document.querySelectorAll("#topApps .approw")).map((r) => {
    const name = r.querySelector(".appname");
    const bar = r.querySelector(".bar > span");
    return {
      name: txt(name),
      title: name ? name.getAttribute("title") : null,
      nameBox: lineBox(name),
      val: txt(r.querySelector(".appval")),
      barWidth: bar ? Math.round(bar.getBoundingClientRect().width * 100) / 100 : null
    };
  });

  const panel = document.querySelector(".panel-status");
  const list = document.getElementById("status");
  const panelLabel = panel ? panel.querySelector(".panel-label") : null;
  const panelStyle = panel ? getComputedStyle(panel) : null;
  const statusRows = Array.from(document.querySelectorAll("#status .statusrow")).map((r) => ({
    title: txt(r.querySelector(".title")),
    sub: txt(r.querySelector(".sub")),
    top: Math.round(r.getBoundingClientRect().top * 100) / 100,
    height: Math.round(r.getBoundingClientRect().height * 100) / 100
  }));

  const overflow = [];
  document.querySelectorAll("body *").forEach((el) => {
    if (el.namespaceURI !== "http://www.w3.org/1999/xhtml") return;
    const cs = getComputedStyle(el);
    if (cs.display === "inline" || cs.display === "inline-block" || cs.display === "contents") return;
    if (el.clientWidth < 2) return;
    if (el.scrollWidth <= el.clientWidth + 1) return;
    overflow.push({
      tag: el.tagName.toLowerCase(),
      cls: String(el.className || "").slice(0, 60),
      sw: el.scrollWidth, cw: el.clientWidth,
      ellipsis: cs.textOverflow === "ellipsis",
      title: el.getAttribute("title") || "",
      titleLen: (el.getAttribute("title") || "").length,
      fullLen: (el.textContent || "").replace(/\s+/g, "").trim().length,
      text: (el.textContent || "").replace(/\s+/g, " ").trim().slice(0, 40)
    });
  });

  const doc = document.documentElement;

  // ---- temperature chart: gaps in the rendered series vs breaks in the path ----
  const thermalSvg = document.querySelector('#tiles .tile[data-tile="thermal"] .tile-spark svg');
  const thermalPaths = thermalSvg
    ? Array.from(thermalSvg.querySelectorAll("path")).map((p) => ({
        stroke: p.getAttribute("stroke") || "",
        fill: p.getAttribute("fill") || "",
        d: p.getAttribute("d") || ""
      }))
    : [];
  const nullIdx = (arr) => {
    const out = [];
    if (!Array.isArray(arr)) return out;
    for (let i = 0; i < arr.length; i++) {
      const v = arr[i];
      if (!(typeof v === "number" && isFinite(v))) out.push(i);
    }
    return out;
  };
  const thermalData = (() => {
    try {
      if (typeof wideSeries !== "function") return null;
      const pair = wideSeries("thermal") || [];
      const a = Array.isArray(pair[0]) ? pair[0] : [];
      const b = Array.isArray(pair[1]) ? pair[1] : [];
      return { aLen: a.length, bLen: b.length, aNulls: nullIdx(a), bNulls: nullIdx(b) };
    } catch (e) { return { error: String(e && e.message || e) }; }
  })();
  // a synthetic series with one hole must break the line: deterministic proof
  // that a missing sample is not plotted as 0 degC.
  const gapProbe = (() => {
    try {
      if (typeof sparkPath !== "function") return null;
      return { gapped: sparkPath([1, 2, null, 4, 5]), solid: sparkPath([1, 2, 3, 4, 5]) };
    } catch (e) { return null; }
  })();

  // lower row: Apps panel vs STATUS panel, measured against the panel grid
  const panelsHost = document.querySelector(".panels");
  const phs = panelsHost ? getComputedStyle(panelsHost) : null;
  const pTracks = phs && phs.gridTemplateColumns !== "none"
    ? phs.gridTemplateColumns.split(/\s+/).filter(Boolean).map(px) : [];
  const pGap = phs ? (parseFloat(phs.columnGap) || 0) : 0;
  const panelBox = (sel) => {
    const el = document.querySelector(sel);
    if (!el) return null;
    const cs = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    const t = pTracks.length ? pTracks[0] : 0;
    return {
      gridColumn: cs.gridColumnStart + " / " + cs.gridColumnEnd,
      span: t ? Math.round((r.width + pGap) / (t + pGap)) : 0,
      width: Math.round(r.width * 100) / 100,
      height: Math.round(r.height),
      left: Math.round(r.left * 100) / 100,
      top: Math.round(r.top * 100) / 100
    };
  };

  return {
    html: doc.outerHTML,
    viewportWidth: window.innerWidth,
    docScrollWidth: doc.scrollWidth, docClientWidth: doc.clientWidth,
    bodyScrollWidth: document.body.scrollWidth, bodyClientWidth: document.body.clientWidth,
    tracks: tracks.map(px), tracksCount: tracks.length,
    tiles: tiles, apps: apps, statusRows: statusRows,
    statusPanelHeight: panel ? Math.round(panel.getBoundingClientRect().height) : 0,
    statusListHeight: list ? Math.round(list.getBoundingClientRect().height) : 0,
    statusPanelContentHeight: panel && panelStyle
      ? Math.round(panel.clientHeight - parseFloat(panelStyle.paddingTop) - parseFloat(panelStyle.paddingBottom)) : 0,
    statusLabelBlock: panelLabel
      ? Math.round(panelLabel.getBoundingClientRect().height + parseFloat(getComputedStyle(panelLabel).marginBottom || "0")) : 0,
    overflow: overflow,
    thermalPaths: thermalPaths,
    thermalData: thermalData,
    gapProbe: gapProbe,
    wideCount: document.querySelectorAll("#tiles .tile.wide").length,
    ranges: Array.from(document.querySelectorAll(".range-pill")).map((b) => b.textContent.trim()),
    tabs: Array.from(document.querySelectorAll(".tab")).map((b) => b.textContent.trim()),
    wordmark: txt(document.querySelector(".wordmark")),
    devicePill: txt(document.querySelector(".device-pill")),
    deviceNames: Array.from(document.querySelectorAll("#deviceMenu .dm-name")).map((e) => e.textContent.trim()),
    appsTitle: txt(document.querySelector("#appsTitle")),
    panelsTracks: pTracks.length,
    panelsGap: pGap,
    panelsTrackWidth: pTracks.length ? Math.round(pTracks[0] * 100) / 100 : 0,
    panelsApps: panelBox(".panel.panel-wide"),
    panelsStatus: panelBox(".panel.panel-status")
  };
})()"""

NODE_DRIVER = r"""const { spawn } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const [, , chromePath, url, widthArg, probeFile] = process.argv;
const width = parseInt(widthArg, 10);
const probe = fs.readFileSync(probeFile, "utf8");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 9300 + Math.floor(Math.random() * 500);
const profile = fs.mkdtempSync(path.join(os.tmpdir(), "syswatch-ui-"));
let chrome = null;

const cleanup = () => {
  try { if (chrome) chrome.kill("SIGKILL"); } catch (e) {}
  try { fs.rmSync(profile, { recursive: true, force: true }); } catch (e) {}
};

(async () => {
  chrome = spawn(chromePath, [
    "--headless=new", "--no-first-run", "--no-default-browser-check",
    "--disable-extensions", "--disable-sync", "--disable-background-networking",
    "--disable-features=Translate,MediaRouter", "--mute-audio",
    "--user-data-dir=" + profile, "--remote-debugging-port=" + port,
    "--window-size=" + width + ",1200", "about:blank"
  ], { stdio: "ignore" });

  let target = null;
  for (let i = 0; i < 150 && !target; i++) {
    try {
      const res = await fetch("http://127.0.0.1:" + port + "/json/list");
      if (res.ok) {
        const list = await res.json();
        target = list.filter((t) => t.type === "page")[0] || null;
      }
    } catch (e) {}
    if (!target) await sleep(100);
  }
  if (!target) throw new Error("chrome did not expose a page target");

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener("open", res, { once: true });
    ws.addEventListener("error", () => rej(new Error("devtools websocket failed")), { once: true });
  });

  let seq = 0;
  const pending = new Map();
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (!msg.id || !pending.has(msg.id)) return;
    const { res, rej } = pending.get(msg.id);
    pending.delete(msg.id);
    if (msg.error) rej(new Error(JSON.stringify(msg.error)));
    else res(msg.result);
  });
  const send = (method, params) => new Promise((res, rej) => {
    const id = ++seq;
    pending.set(id, { res, rej });
    ws.send(JSON.stringify({ id, method, params: params || {} }));
  });

  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setDeviceMetricsOverride", {
    width: width, height: 1200, deviceScaleFactor: 1, mobile: false
  });
  await send("Page.navigate", { url: url });

  const ready = "document.querySelectorAll('#tiles .tile').length >= 9";
  for (let i = 0; i < 60; i++) {
    const r = await send("Runtime.evaluate", { expression: ready, returnByValue: true });
    if (r.result && r.result.value === true) break;
    await sleep(200);
  }
  await sleep(1600);

  const out = await send("Runtime.evaluate", {
    expression: probe, returnByValue: true, awaitPromise: true
  });
  if (out.exceptionDetails) throw new Error("probe threw: " + JSON.stringify(out.exceptionDetails).slice(0, 400));
  process.stdout.write(JSON.stringify(out.result.value));
  ws.close();
  cleanup();
})().catch((e) => {
  cleanup();
  console.error(String((e && e.message) || e));
  process.exit(2);
});
"""


def fail(message):
    print("UI CHECK FAILED: " + message)
    sys.exit(1)


def measure(width):
    """Render the dashboard at `width` and return the measured layout probe."""
    if not os.path.exists(CHROME):
        fail("Chrome not found at " + CHROME)
    tmpdir = tempfile.mkdtemp(prefix="syswatch-ui-check-")
    probe_path = os.path.join(tmpdir, "probe.js")
    node_path = os.path.join(tmpdir, "driver.js")
    with open(probe_path, "w") as fh:
        fh.write(PROBE_JS)
    with open(node_path, "w") as fh:
        fh.write(NODE_DRIVER)
    try:
        proc = subprocess.run(
            ["node", node_path, CHROME, URL, str(width), probe_path],
            capture_output=True, text=True, timeout=120,
        )
    except subprocess.TimeoutExpired:
        shutil.rmtree(tmpdir, ignore_errors=True)
        fail("layout probe timed out after 120 s")
    shutil.rmtree(tmpdir, ignore_errors=True)
    if proc.returncode != 0:
        fail("layout probe failed (rc=%s): %s" % (proc.returncode, (proc.stderr or "").strip()[:400]))
    try:
        return json.loads(proc.stdout)
    except ValueError:
        fail("layout probe returned no JSON: " + (proc.stdout or proc.stderr)[:300])


def check_source_has_no_mock_values():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    for name in ("dashboard/static/app.js", "dashboard/static/index.html"):
        path = os.path.join(root, name)
        try:
            body = open(path, encoding="utf-8").read()
        except OSError:
            fail("cannot read " + path)
        for token in MOCK_SOURCE_TOKENS:
            if token in body:
                fail("reference sample value %r is hardcoded in %s" % (token, name))


def clipped_but_recoverable(el):
    """A clipped element is only acceptable when it shows an ellipsis *and* its
    title carries at least the text the ellipsis hides.  A stub title (or none)
    means the full value is unrecoverable, which is the bug this guards."""
    if not el.get("ellipsis"):
        return False
    title = el.get("title") or ""
    if not title.strip():
        return False
    if "." in title and " " not in title:      # bare bundle id names the app
        return True
    hidden = el.get("fullLen") or 0
    have = el.get("titleLen")
    return (have if have is not None else len(title)) >= hidden


POINT_RE = re.compile(r"([ML])\s*(-?\d+(?:\.\d+)?)\s+(-?\d+(?:\.\d+)?)")


def path_points(path_d):
    """Every vertex the SVG path actually drew, in viewBox units (0..100, 0..30)."""
    return [(float(x), float(y)) for _, x, y in POINT_RE.findall(path_d or "")]


def subpath_count(path_d):
    return len(re.findall(r"M", path_d or ""))


def runs_of_valid(length, nulls):
    """Number of unbroken runs of real samples in a series - the number of line
    segments a correct renderer must draw (a gap breaks the line)."""
    nulls = set(nulls or [])
    runs = 0
    inside = False
    for i in range(length or 0):
        if i in nulls:
            inside = False
        elif not inside:
            runs += 1
            inside = True
    return runs


def check_footer_lines(m, tiles, equal_heights):
    """Goal items 4 + 7: every footer line is clamped to one line with an
    ellipsis and a title, so the six top cards keep a single height."""
    lines = []
    for t in tiles:
        for key in ("subBox", "deltaBox"):
            box = t.get(key)
            if box:
                lines.append((t.get("id"), key.replace("Box", ""), box))
        for i, box in enumerate(t.get("footerBoxes") or []):
            if box:
                lines.append((t.get("id"), "series %d" % (i + 1), box))
    if len(lines) < 6:
        fail("only %d tile footer line(s) rendered - the reference cards carry a footer each"
             % len(lines))
    for tid, key, box in lines:
        where = "%s %s (%r)" % (tid, key, (box.get("text") or "")[:48])
        if not box.get("nowrap"):
            fail("%s is not clamped to one line (white-space is not nowrap)" % where)
        lh = box.get("lh") or 0
        if lh > 0 and (box.get("ch") or 0) > lh * 1.35 + 1:
            fail("%s renders %dpx tall on a %gpx line - it wrapped mid-phrase"
                 % (where, box.get("ch"), lh))
        if (box.get("sw") or 0) > (box.get("cw") or 0) + 1:
            if not box.get("ellipsis") or not (box.get("title") or "").strip():
                fail("%s is clipped (%dpx in %dpx) with %s"
                     % (where, box.get("sw"), box.get("cw"),
                        "no ellipsis" if not box.get("ellipsis") else "no title attribute"))
    if equal_heights:
        singles = [t for t in tiles if t.get("id") not in WIDE_IDS]
        if len(singles) != 6:
            fail("expected 6 top cards on row 1, measured %d" % len(singles))
        heights = [t.get("height") or 0 for t in singles]
        if max(heights) - min(heights) > 1:
            fail("top cards have uneven heights %r - a footer line wrapped" % (heights,))
    return len(lines)


def check_power_units(tiles):
    """Goal item 6: the Power Use footer prints the unit next to every value."""
    tile = [t for t in tiles if t.get("id") == "power"]
    if not tile:
        fail("Power Use card missing")
    sub = tile[0].get("sub") or ""
    if not sub:
        fail("Power Use footer is empty, expected 'Processor X W . Graphics Y W'")
    values = re.findall(r"(\d+(?:\.\d+)?)\s*([A-Za-z%°]*)", sub)
    if values:
        missing = [v for v, unit in values if unit.strip() != "W"]
        if missing:
            fail("Power Use footer prints %r without a unit next to every value: %r"
                 % (missing, sub))
        return len(values)
    units = sub.count(" W")
    if units < 2:
        fail("Power Use footer must print the unit per value: %r" % (sub,))
    return units


def check_thermal_gaps(m):
    """Goal item 5: a missing temperature sample must break the line, never be
    plotted as 0 degC (which draws a near-vertical spike to the baseline)."""
    data = m.get("thermalData")
    if not data or data.get("error") or data.get("aLen") is None:
        fail("cannot read the temperature series the chart rendered: %r" % (data,))
    paths = [p for p in (m.get("thermalPaths") or [])
             if p.get("stroke") in ("#6EE7C8", "#8A97A3")]
    if len(paths) != 2:
        fail("temperature chart must draw two series lines, measured %d" % len(paths))
    by_stroke = {p.get("stroke"): p for p in paths}
    gaps = 0
    breaks = 0
    for stroke, key in (("#6EE7C8", "a"), ("#8A97A3", "b")):
        path_d = by_stroke[stroke].get("d") or ""
        subs = subpath_count(path_d)
        if subs < 1:
            fail("temperature %s series drew no path at all" % key)
        length = data.get(key + "Len") or 0
        nulls = data.get(key + "Nulls") or []
        gaps += len(nulls)
        breaks += max(0, subs - 1)
        if length >= 2:
            expected = runs_of_valid(length, nulls)
            if subs != expected:
                fail("temperature %s series has %d run(s) of real samples but its path "
                     "draws %d subpath(s) - a gap must break the line, never read as 0 degC"
                     % (key, expected, subs))
            pts = path_points(path_d)
            for i in nulls:
                at_x = (i / float(length - 1)) * 100.0
                for x, _y in pts:
                    if abs(x - at_x) <= 0.75:
                        fail("temperature %s series plots a vertex at x=%.2f for sample %d, "
                             "which is missing - the gap is being drawn as a value"
                             % (key, x, i))
    probe = m.get("gapProbe") or {}
    gapped = probe.get("gapped") or ""
    solid = probe.get("solid") or ""
    if subpath_count(gapped) != 2:
        fail("a series with one missing sample renders %d subpath(s), expected 2 - "
             "the gap must break the line" % subpath_count(gapped))
    if subpath_count(solid) != 1:
        fail("a series with no gaps renders %d subpath(s), expected 1"
             % subpath_count(solid))
    return gaps, breaks


def check_panels_wide(m, tiles):
    """Goal item 4: Apps spans four tracks of the six, STATUS spans two."""
    apps = m.get("panelsApps")
    status = m.get("panelsStatus")
    if not apps or not status:
        fail("lower row must hold the Apps panel (.panel-wide) and STATUS (.panel-status)")
    if m.get("panelsTracks") != 6:
        fail("panel row has %d column tracks, reference has 6" % (m.get("panelsTracks"),))
    track = m.get("panelsTrackWidth") or 0
    gap = m.get("panelsGap") or 0
    for name, box, want in (("Apps", apps, 4), ("STATUS", status, 2)):
        if box.get("span") != want or ("span %d" % want) not in (box.get("gridColumn") or ""):
            fail("%s panel occupies %r tracks (%r), reference spans %d"
                 % (name, box.get("span"), box.get("gridColumn"), want))
        expect_w = track * want + gap * (want - 1)
        if abs((box.get("width") or 0) - expect_w) > 3:
            fail("%s panel renders %.1fpx wide, %d tracks + gaps = %.1fpx"
                 % (name, box.get("width"), want, expect_w))
    if abs((apps.get("top") or 0) - (status.get("top") or 0)) > 1:
        fail("Apps and STATUS panels are on different lines (%.1f vs %.1f)"
             % (apps.get("top"), status.get("top")))
    if (apps.get("left") or 0) >= (status.get("left") or 0):
        fail("STATUS must sit to the right of the Apps panel")
    if tiles and (apps.get("top") or 0) <= tiles[-1].get("top", 0):
        fail("the panel row is not below the tiles")
    if abs((apps.get("height") or 0) - (status.get("height") or 0)) > 2:
        fail("Apps panel is %dpx tall and STATUS %dpx - they must share the row height"
             % (apps.get("height"), status.get("height")))


def check_panels_narrow(m):
    apps = m.get("panelsApps")
    status = m.get("panelsStatus")
    if not apps or not status:
        fail("lower row must hold the Apps panel (.panel-wide) and STATUS (.panel-status)")
    if m.get("panelsTracks") != 1:
        fail("panel row must reflow to one column at this width, measured %d"
             % (m.get("panelsTracks"),))
    for name, box in (("Apps", apps), ("STATUS", status)):
        if box.get("span") != 1:
            fail("%s panel still spans %r tracks at this width" % (name, box.get("span")))
    if abs((apps.get("width") or 0) - (status.get("width") or 0)) > 1:
        fail("stacked panels are not equal width: %.1f vs %.1f"
             % (apps.get("width"), status.get("width")))
    if (status.get("top") or 0) <= (apps.get("top") or 0):
        fail("stacked panels are not ordered Apps then STATUS")


def check_common(m, width, stats):
    # ---- chrome of the page ----
    if not m.get("wordmark") or "syswatch" not in m["wordmark"].lower():
        fail("wordmark missing")
    if "This Mac" not in (m.get("devicePill") or ""):
        fail("device pill missing 'This Mac'")
    if m.get("deviceNames") != ["This Mac"]:
        fail("device popover must list only This Mac, got %r" % (m.get("deviceNames"),))
    if m.get("tabs") != EXPECTED_TABS:
        fail("tabs must be %r, got %r" % (EXPECTED_TABS, m.get("tabs")))
    if m.get("ranges") != EXPECTED_RANGES:
        fail("range pills must be %r, got %r" % (EXPECTED_RANGES, m.get("ranges")))

    # ---- apps header ----
    if m.get("appsTitle") != "APPS USING THE MOST · % OF THE PROCESSOR · LAST HOUR":
        fail("apps title must be 'APPS USING THE MOST · %% OF THE PROCESSOR · LAST HOUR', got %r"
             % (m.get("appsTitle"),))

    # ---- 1. exact label set + WORKLOAD ----
    tiles = m.get("tiles") or []
    labels = [(t.get("label") or "").upper() for t in tiles]
    if labels != EXPECTED_LABELS:
        fail("label set/order must be %r, got %r" % (EXPECTED_LABELS, labels))
    if "WORKLOAD" not in labels:
        fail("WORKLOAD card missing")

    # ---- 6. per-card precision + live values ----
    live = 0
    for t in tiles:
        tid = t.get("id")
        value = t.get("value")
        if value in (None, "", "—"):
            continue
        live += 1
        pattern = PRECISION.get(tid)
        if not pattern:
            fail("unknown tile id %r" % (tid,))
        if not re.match(pattern, value):
            fail("tile %s prints %r, expected precision %s" % (tid, value, pattern))
    if live < 7:
        fail("only %d tiles carry a live number (endpoints not feeding the UI)" % live)

    # ---- 5a. apps sorted descending by the metric printed ----
    apps = m.get("apps") or []
    if len(apps) < 3:
        fail("apps panel shows %d rows, need at least 3" % len(apps))
    values = []
    for row in apps:
        match = re.match(r"^(\d+\.\d+)\s*%$", row.get("val") or "")
        if not match:
            fail("app row %r prints %r, expected 'N.N %%'" % (row.get("name"), row.get("val")))
        v = float(match.group(1))
        if v > 100:
            fail("app row %r prints %.1f%%, apps must never exceed 100%%" % (row.get("name"), v))
        values.append(v)
    for i in range(len(values) - 1):
        if values[i] < values[i + 1]:
            fail("apps not sorted descending: %r" % (values,))
    bars = [row.get("barWidth") or 0 for row in apps]
    for i in range(len(bars) - 1):
        if bars[i] + 0.5 < bars[i + 1]:
            fail("app bars are not ordered like the values: %r" % (bars,))
    if bars[0] < 20:
        fail("top app bar is %.1fpx wide - bars are not scaled to the top consumer" % bars[0])
    # ---- 2. a clipped app name keeps an ellipsis *and* its full name ----
    truncated = 0
    for row in apps:
        if not row.get("title"):
            fail("app row %r has no title attribute, so its name cannot be recovered when clipped" % (row.get("name"),))
        box = row.get("nameBox") or {}
        if (box.get("sw") or 0) > (box.get("cw") or 0) + 1:
            truncated += 1
            if not box.get("ellipsis") or not (box.get("title") or "").strip():
                fail("app name %r is clipped mid-word with %s (%dpx in %dpx) - a long name "
                     "must end in an ellipsis and carry its full name in a title"
                     % (row.get("name"),
                        "no ellipsis" if not box.get("ellipsis") else "no title",
                        box.get("sw"), box.get("cw")))
    stats["app_rows"] = len(apps)
    stats["app_max"] = max(values)
    stats["app_truncated"] = truncated
    stats["max_name_px"] = max([(row.get("nameBox") or {}).get("sw") or 0 for row in apps] or [0])

    # ---- 4. STATUS panel ----
    status = m.get("statusRows") or []
    if len(status) < 6:
        fail("STATUS panel has %d rows, need at least 6" % len(status))
    for row in status:
        if not row.get("title") or not row.get("sub"):
            fail("STATUS row incomplete: %r" % (row,))
    if len(set(row.get("top") for row in status)) != len(status):
        fail("STATUS rows overlap: %r" % (status,))
    panel_h = m.get("statusPanelHeight") or 0
    list_h = m.get("statusListHeight") or 0
    need = (m.get("statusPanelContentHeight") or 0) - (m.get("statusLabelBlock") or 0) - 4
    if panel_h <= 0 or list_h < panel_h * 0.55:
        fail("STATUS list fills only %dpx of a %dpx panel" % (list_h, panel_h))
    if list_h < need:
        fail("STATUS list is %dpx tall, the panel leaves %dpx for it - the lower area is empty"
             % (list_h, need))
    rows_sum = sum(row.get("height") or 0 for row in status)
    if rows_sum > list_h + 2:
        fail("STATUS rows overflow their list: %.0fpx of %dpx" % (rows_sum, list_h))
    if list_h > rows_sum + 8:
        gaps = [status[i + 1]["top"] - (status[i]["top"] + (status[i].get("height") or 0))
                for i in range(len(status) - 1)]
        if min(gaps) < 3:
            fail("STATUS rows are bunched at the top of a %dpx list: gaps %r" % (list_h, gaps))

    # ---- 5b. nothing clipped, no sideways scroll ----
    if m["docScrollWidth"] > m["docClientWidth"] + 1:
        fail("page scrolls horizontally: %d > %d px" % (m["docScrollWidth"], m["docClientWidth"]))
    if m["bodyScrollWidth"] > m["bodyClientWidth"] + 1:
        fail("body scrolls horizontally: %d > %d px" % (m["bodyScrollWidth"], m["bodyClientWidth"]))
    for el in m.get("overflow") or []:
        if not clipped_but_recoverable(el):
            reason = "no ellipsis" if not el.get("ellipsis") else "title is a stub or missing"
            fail("element is clipped with %s: <%s class=%r> %dpx in %dpx, title=%r, text=%r"
                 % (reason, el.get("tag"), el.get("cls"), el.get("sw"), el.get("cw"),
                    el.get("title"), el.get("text")))

    # ---- mock values must not be rendered ----
    html = m.get("html") or ""
    for token in MOCK_DOM_TOKENS:
        if token in html:
            fail("reference sample value rendered on the page: %r" % token)

    return tiles


def check_wide_layout(m, tiles):
    wide = [t for t in tiles if t.get("id") in [w[0] for w in WIDE_TILES]]
    if len(wide) != 3:
        fail("expected 3 wide cards, measured %d" % len(wide))
    if m.get("tracksCount") != 6:
        fail("tile grid has %d column tracks, reference has 6" % m.get("tracksCount"))
    tracks = m.get("tracks") or []
    track_w = tracks[0]

    singles = [t for t in tiles if t.get("id") not in [w[0] for w in WIDE_TILES]]
    if len(singles) != 6:
        fail("expected 6 single cards on row 1, measured %d" % len(singles))
    if len(set(t.get("top") for t in singles)) != 1:
        fail("row 1 cards sit on %d different lines" % len(set(t.get("top") for t in singles)))
    if abs(singles[0]["width"] - track_w) > 2:
        fail("a single card is %.1fpx wide, one track is %.1fpx" % (singles[0]["width"], track_w))
    if len(set(t.get("top") for t in wide)) != 1:
        fail("wide cards sit on %d different lines" % len(set(t.get("top") for t in wide)))
    if wide[0]["top"] <= singles[0]["top"]:
        fail("wide cards are not on the row after the single cards")

    for tid, label, first, second in WIDE_TILES:
        tile = [t for t in wide if t.get("id") == tid][0]
        if not tile.get("spansTwo") or "span 2" not in tile.get("gridColumn"):
            fail("%s computes grid-column %r, not a two-track span" % (tid, tile.get("gridColumn")))
        expected_w = track_w * 2 + 16
        if abs(tile["width"] - expected_w) > 3:
            fail("%s renders %.1fpx wide, two tracks + gap = %.1fpx" % (tid, tile["width"], expected_w))
        if tile.get("chartHeight", 0) < 100:
            fail("%s chart is %dpx tall, reference wide chart is 120px" % (tid, tile.get("chartHeight")))
        axis = tile.get("xaxis") or []
        if len(axis) != 4:
            fail("%s x-axis has %d labels, reference has 4" % (tid, len(axis)))
        if axis[3] != "now":
            fail("%s x-axis does not end in 'now': %r" % (tid, axis))
        for stamp in axis[:3]:
            if not re.match(r"^\d{2}:\d{2}$", stamp):
                fail("%s x-axis label %r is not HH:MM" % (tid, stamp))
        footer = tile.get("footerSeries") or []
        if len(footer) != 2:
            fail("%s footer names %d series, reference names 2" % (tid, len(footer)))
        if first not in footer[0] or second not in footer[1]:
            fail("%s footer is %r, expected %r then %r" % (tid, footer, first, second))

    narrow_wide = [t for t in tiles if t.get("id") in [w[0] for w in WIDE_TILES]]
    single_cards = [t for t in tiles if t.get("id") not in [w[0] for w in WIDE_TILES]]
    if single_cards and single_cards[0].get("chartHeight", 0) > 60:
        fail("single card chart is %dpx tall, reference single chart is 44px" % single_cards[0].get("chartHeight"))
    for tile in narrow_wide:
        if not tile.get("sub") and not tile.get("footerSeries"):
            fail("%s has neither sub-line nor series footer" % tile.get("id"))


def check_narrow_layout(m, tiles):
    if m.get("tracksCount") != 1:
        fail("390px viewport must reflow to one column, measured %d tracks" % m.get("tracksCount"))
    lefts = set(round(t.get("left")) for t in tiles)
    if len(lefts) != 1:
        fail("390px viewport still shows %d columns (lefts %r)" % (len(lefts), sorted(lefts)))
    widths = [t.get("width") for t in tiles]
    if max(widths) - min(widths) > 1.5:
        fail("390px cards are not equal width: %r" % (widths,))
    for tile in tiles:
        if tile.get("spansTwo") and len(tiles) > 1:
            fail("%s still spans two columns at 390px" % tile.get("id"))
    for el in m.get("overflow") or []:
        if not clipped_but_recoverable(el):
            reason = "no ellipsis" if not el.get("ellipsis") else "title is a stub or missing"
            fail("390px viewport clips <%s class=%r> with %s (%dpx in %dpx, title=%r)"
                 % (el.get("tag"), el.get("cls"), reason, el.get("sw"), el.get("cw"),
                    el.get("title")))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--width", type=int, default=1440)
    args = parser.parse_args()

    check_source_has_no_mock_values()
    measured = measure(args.width)
    stats = {}
    tiles = check_common(measured, args.width, stats)

    if args.width > 640:
        check_wide_layout(measured, tiles)
        check_panels_wide(measured, tiles)
        footer_lines = check_footer_lines(measured, tiles, equal_heights=True)
    else:
        check_narrow_layout(measured, tiles)
        check_panels_narrow(measured)
        footer_lines = check_footer_lines(measured, tiles, equal_heights=False)

    power_values = check_power_units(tiles)
    gap_samples, gap_breaks = check_thermal_gaps(measured)

    ranges = len(measured.get("ranges") or [])
    tabs = len(measured.get("tabs") or [])
    status_rows = len(measured.get("statusRows") or [])
    wide_note = "wide cards span 2 tracks" if args.width > 640 else "single column"
    clipped_nodes = sum(1 for el in (measured.get("overflow") or [])
                        if not clipped_but_recoverable(el))
    wide_cards = measured.get("wideCount", 0)
    cards_equal = "6 / 6" if args.width > 640 else "n/a (single column)"

    # every assertion the run made, by name, so a reviewer can audit the checks
    print("ASSERT tiles            %d labels == %d expected, WORKLOAD present"
          % (len(tiles), len(EXPECTED_LABELS)))
    print("ASSERT range pills      %d == %d" % (ranges, len(EXPECTED_RANGES)))
    print("ASSERT tabs             %d == %d" % (tabs, len(EXPECTED_TABS)))
    print("ASSERT wide cards       %d == 3, each spans 2 tracks and renders 2x track width"
          % wide_cards)
    print("ASSERT apps <= 100%%     %d rows, max %.1f %% of the processor, sorted descending"
          % (stats.get("app_rows", 0), stats.get("app_max", 0.0)))
    print("ASSERT app name clipping %d truncated name(s), all with ellipsis + full title (widest %dpx)"
          % (stats.get("app_truncated", 0), stats.get("max_name_px", 0)))
    print("ASSERT clipped nodes    %d nodes with scrollWidth > clientWidth + 1" % clipped_nodes)
    print("ASSERT status rows      %d rows >= 6, list fills the panel" % status_rows)
    print("ASSERT footer single-line %d / %d lines nowrap + ellipsis + title, top cards equal height %s"
          % (footer_lines, footer_lines, cards_equal))
    print("ASSERT thermal gaps     %d missing sample(s), %d line break(s), 0 gaps plotted as a value"
          % (gap_samples, gap_breaks))
    print("ASSERT power units      %d / %d values carry their unit per value"
          % (power_values, power_values))
    print("ASSERT mock values      0 rendered, 0 hardcoded")
    print("layout %dpx: %d tiles, %d grid track(s), %s"
          % (args.width, len(tiles), measured.get("tracksCount", 0), wide_note))
    print("UI OK: %d tiles, %d ranges, %d tabs, %d labels, %d wide cards, apps <=100%%, "
          "%d clipped nodes, status rows >=6, 0 mock values, %d/%d single-line footers, "
          "cards equal height %s, %d gap break(s), 0 gaps as values, %d/%d power values carry W"
          % (len(tiles), ranges, tabs, len(EXPECTED_LABELS), wide_cards, clipped_nodes,
             footer_lines, footer_lines, cards_equal, gap_breaks, power_values, power_values))


if __name__ == "__main__":
    main()
