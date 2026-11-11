# Environment facts (measured)

| Fact | Value |
|---|---|
| Mac | Apple Silicon, 8 cores, 8 GB (`Barbarian`) |
| Working python for the dashboard server | `/Users/barbarian/.hermes/tools/python-3.14.9+*/bin/python3` — **psutil 7.2.2 present** |
| Broken python for the server | system `python3` 3.15 — **no psutil**, `/api/current` 503s |
| opencode | `/opt/homebrew/bin/opencode`, authed: Anthropic, DeepSeek, Google, Groq, OpenAI, OpenRouter |
| Free lane | opencode zen `*-free` only (OpenRouter free tier is daily-capped) |
| Port | 8123, bound 127.0.0.1 |

**Rule:** every start script, LaunchAgent (T8) and agent brief must use the psutil-enabled interpreter explicitly.
Never bare `python3` — it resolves differently depending on the shell.
