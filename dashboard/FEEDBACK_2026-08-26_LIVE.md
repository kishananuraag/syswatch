

# SYSWATCH FEEDBACK — LIVE REVIEW (Kishan voice, Aug 26 ~17:15)
## CRITICAL BUG (P0)
Opening the dashboard link SPIKES his CPU + RAM (~5GB in Chrome). Graph grows unbounded downward
("expanding to bottom forever"), page becomes unusable. Suspected: chart re-render loop / history
fetch growing without cap / 5s refresh redrawing full canvas stack. The dashboard itself was also
seen eating ~50MB idle and much more when opened. FIX FIRST before any features.

## PRODUCT DIRECTION (the real ask): iStat Menus MODEL, not a webpage
- A persistent TILE/BAR (menu-bar style app) showing mini CPU/RAM/GPU/temp/battery indicators
- Clicking tile opens a MINI POPUP with small graphs; click-away closes it
- Popup graphs are COMPACT: whole timeline squeezed into small window
- Time-range selection inside popup: 1h / 3h / etc. — graph rescales to selection
- Segmented sections: CPU | RAM | GPU | Temperatures | Battery
- Each section shows basic stats inline; HOVERING a stat opens a separate popover with deep detail:
  * CPU popover: efficiency vs performance cores, per-core load, uptime, top processes (5-10,
    scrollable, sorted highest-first)
  * RAM popover: swap usage, memory pressure, which apps use what (scrollable, sortable)
- ALL hardware sensors shown WITH UNITS
- SETTINGS: toggle what's visible, how bars align, everything user-configurable
- Battery section: health, voltage(?), charge graph over time
- Efficiency doctrine: minimal resource footprint ALWAYS (it's a monitor, it must not become the load)
- Presentation: readable, human, graphical — "not throwing bytes", real stats humans understand

## VERDICT ON CURRENT WORK
"What you built is really good BUT bugs first." Design direction approved, implementation has P0.
