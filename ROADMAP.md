# ROADMAP — syswatch (parked future scope)

Status: FUTURE. Current focus remains the single-machine dashboard.
Captured from Kishan's stated final goal (2026-11-19): monitor many
devices (PCs, phones, TVs) from one dashboard, reachable anywhere.

## Layer 1 — per-platform agents (build these first, in this order)
- macOS: done (collector + menubar + dashboard). Windows + Linux: one
  adapter each, reusing dashboard/sensors.py contract. Android + iOS:
  lightweight collectors (battery, thermal, CPU, network). tvOS: last.
- Rule: every platform speaks the SAME JSON contract (sensors, power, apps)
  so the dashboard never learns a new shape.

## Layer 2 — sync + identity
- Each device runs an unprivileged agent that pushes samples to a sync
  endpoint (user-chosen: self-hosted or cloud). Login (email + device
  pairing code). One dashboard view lists all devices; drill into one.
- Security: device pairing, revocable tokens, no raw shell from the cloud,
  read-only aggregates by default.

## Layer 3 — anywhere access
- Web dashboard (this codebase, hosted) + mobile app. Offline-first: a
  device's own dashboard still works with no network.

## Non-goals / guardrails
- Do NOT cloud-ify the current single-machine build. Keep it local-first.
- Do NOT add accounts/auth to the local dashboard now.
- The local dashboard remains the reference implementation of the contract.

## Rust migration track (max efficiency + scalability)

Kishan's directive (2026-11-19): pick Rust for max efficiency, not because
anything is broken. Migrate incrementally — never a big-bang rewrite.

Order (each is its own brief, dispatched when the dashboard feature set
for that layer stabilises):
1. **Collector** — DONE (Rust, 1 s, <1 % CPU).
2. **Menubar** — DONE (Rust, 2 s tick).
3. **Power daemon** — T19 (Rust, consolidates shell+C). IN PROGRESS.
4. **Dashboard server** — when Layer 1 (multi-device) starts. Single
   self-contained binary per platform, no Python runtime.
5. **Frontend** — only if Layer 3 (web dashboard) needs a shared
   component library. Keep HTML/JS until the design stabilises.
6. **Storage cascade** — T18 (Rust, collector-side).

Rule: a component moves to Rust only when it is (a) always-on or
(b) shipped per-platform. Interactive, low-frequency parts stay in the
language that iterates fastest.
