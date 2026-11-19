# T4b — agent event stream (do only after T4a is committed)

## Goal
Two read-only endpoints so the cockpit can show live agent work.

1. `GET /api/agent-events?limit=50` → JSON array of the last N lines of `.hermes/run/events.jsonl` (tail read; missing file → `[]`, never an error).
2. `GET /api/agent-stream` → `text/event-stream`. On connect send `data: {"kind":"hello"}`. Then push one `data: <line>` per new line appended to the events file. Must not block other requests; must survive the file being deleted or rotated.

## Constraints
Edit `dashboard/server.py` only. Stdlib only. No sudo. No cargo. Do not break `/api/current` or `/api/health`.

## Acceptance commands (hang up after 2 seconds is expected — use timeout)
```
python3 -m py_compile dashboard/server.py
(SYSWATCH_PORT=8124 python3 dashboard/server.py > /tmp/t4b.log 2>&1 &) ; sleep 5
curl -s -m 5 "http://127.0.0.1:8124/api/agent-events?limit=5"
curl -s -N -m 3 http://127.0.0.1:8124/api/agent-stream | head -2
pkill -TERM -f "dashboard/server.py"
```

## Report
Paste commands and real output. **Do not claim success.**
