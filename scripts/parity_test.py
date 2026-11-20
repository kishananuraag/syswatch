#!/usr/bin/env python3
"""API parity test: compare Rust agent (8199) vs Python server (8123) responses."""
import json, urllib.request, sys

RUST = "http://127.0.0.1:8199"
PYTHON = "http://127.0.0.1:8123"

ENDPOINTS = [
    "/api/health",
    "/api/current",
    "/api/sensors",
    "/api/consumers?limit=3",
    "/api/apps?limit=3",
    "/api/power",
    "/api/analytics",
    "/api/battery",
    "/api/thermal",
]

def fetch(base, path):
    try:
        with urllib.request.urlopen(base + path, timeout=10) as r:
            return r.status, json.loads(r.read())
    except Exception as e:
        return 0, {"error": str(e)}

def shape(obj, depth=0):
    """Return a sorted list of top-level keys (and nested for dicts)."""
    if isinstance(obj, dict):
        return {k: shape(v, depth+1) for k, v in sorted(obj.items())}
    elif isinstance(obj, list):
        if len(obj) == 0:
            return []
        return [shape(obj[0], depth+1)]
    else:
        return type(obj).__name__

ok = True
for ep in ENDPOINTS:
    rs, rj = fetch(RUST, ep)
    ps, pj = fetch(PYTHON, ep)
    r_shape = shape(rj)
    p_shape = shape(pj)
    match = (rs == ps) and (r_shape == p_shape)
    status = "PASS" if match else "FAIL"
    if not match:
        ok = False
    print(f"{status}  {ep}  rust={rs} python={ps}  shape_match={r_shape == p_shape}")
    if r_shape != p_shape:
        print(f"  rust keys: {list(r_shape.keys()) if isinstance(r_shape, dict) else r_shape}")
        print(f"  py   keys: {list(p_shape.keys()) if isinstance(p_shape, dict) else p_shape}")

# Series and graph need a metric param
for ep in ["/api/series?metric=cpu.total&from=0&to=9999999999&max_points=3",
           "/api/graph?metric=cpu.total&from=0&to=9999999999",
           "/api/history?metric=cpu.total&range=1h"]:
    rs, rj = fetch(RUST, ep)
    ps, pj = fetch(PYTHON, ep)
    r_shape = shape(rj)
    p_shape = shape(pj)
    match = (rs == ps) and (r_shape == p_shape)
    if not match:
        ok = False
    print(f"{'PASS' if match else 'FAIL'}  {ep}  rust={rs} python={ps}  shape_match={r_shape == p_shape}")

print()
print("ALL PASS" if ok else "FAILURES DETECTED")
sys.exit(0 if ok else 1)
