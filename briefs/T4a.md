# T4a — server lifecycle only (small task, do exactly this)

## Goal
Make `dashboard/server.py` safe to start, safe to start twice, and safe to stop. **Nothing else.**

## Do exactly three things
1. **Single instance**: if port 8123 is already serving (check with a quick GET to `/api/health` or a socket connect), print `ALREADY_RUNNING pid=<n>` and exit with code 0. Do not bind twice, do not kill the existing server.
2. **`GET /api/health`** → 200 JSON `{"ok":true,"pid":<int>,"uptime_s":<float>,"port":8123}`.
3. **Clean SIGTERM**: on SIGTERM, shut the HTTP server down and release the port within 2 seconds.

## Constraints
- Edit **`dashboard/server.py` only**. Do not touch `src/`, `Cargo.toml`, `scripts/`, `briefs/`, `GOAL.md`, `LEDGER.jsonl`.
- Python stdlib only. No sudo. Bind 127.0.0.1.
- **Do not run cargo.** Do not install anything.
- Keep all existing endpoints (`/api/current` and the rest) working exactly as they are.

## Acceptance commands (run them; all must pass)
```
python3 -m py_compile dashboard/server.py
(SYSWATCH_PORT=8123 python3 dashboard/server.py > /tmp/t4a.log 2>&1 &) ; sleep 6
curl -s -m 5 http://127.0.0.1:8123/api/health
SYSWATCH_PORT=8123 python3 dashboard/server.py; echo "second-exit=$?"
pkill -TERM -f "dashboard/server.py"; sleep 2; (lsof -i :8123 | grep -c LISTEN || true)
```

## Report
Paste every command and its real output. **Do not claim success.**
