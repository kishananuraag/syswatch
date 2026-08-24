#!/usr/bin/env python3
"""syswatch alerts — threshold rules + Telegram ping. Stdlib only.

Reads dashboard/alerts.json (created with defaults if missing). A background
thread samples the latest snapshot every 30s; when a rule's condition holds
continuously for duration_secs, it fires once (ALERT line to stdout + Telegram
POST if SYSWATCH_TG_TOKEN / SYSWATCH_TG_CHAT are set). Cooldown: at most one
fire per rule per hour.
"""
import json
import os
import threading
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone

RULES_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "alerts.json")
LOG_DIR = os.environ.get("SYSWATCH_LOGS", r"C:\ProgramData\syswatch\logs")
CHECK_INTERVAL = 30  # seconds between samples
COOLDOWN_SECS = 3600  # at most one fire per rule per hour

DEFAULT_RULES = [
    {"metric": "cpu_total", "op": ">", "value": 90, "duration_secs": 300},
    {"metric": "ram_pct", "op": ">", "value": 90, "duration_secs": 600},
]

# metric name -> extractor from a snapshot dict (same shape as /api/latest)
_METRIC_GETTERS = {
    "cpu_total": lambda s: (s.get("cpu") or {}).get("total_pct"),
    "ram_pct": lambda s: (s.get("memory") or {}).get("pct"),
}

_alerter = None  # module singleton, set by start_alerter()


def load_rules(path=RULES_PATH):
    """Read rules from path; create with defaults (validated) if missing/bad."""
    rules = None
    if os.path.exists(path):
        try:
            with open(path, encoding="utf-8") as f:
                raw = json.load(f)
            if isinstance(raw, list):
                rules = [r for r in raw if _valid_rule(r)]
        except (OSError, ValueError):
            rules = None
    if not rules:
        rules = [dict(r) for r in DEFAULT_RULES]
        try:
            with open(path, "w", encoding="utf-8") as f:
                json.dump(rules, f, indent=2)
        except OSError:
            pass
    return rules


def _valid_rule(r):
    return (isinstance(r, dict) and isinstance(r.get("metric"), str)
            and r.get("op") in (">", "<")
            and isinstance(r.get("value"), (int, float))
            and isinstance(r.get("duration_secs"), (int, float)))


def get_value(snap, metric):
    return _METRIC_GETTERS.get(metric, lambda s: None)(snap)


def _holds(rule, value):
    if value is None:
        return False
    try:
        if rule["op"] == ">":
            return value > rule["value"]
        if rule["op"] == "<":
            return value < rule["value"]
    except TypeError:
        return False
    return False


class Alerter:
    def __init__(self, source, rules_path=RULES_PATH, check_interval=CHECK_INTERVAL):
        self.source = source  # callable -> latest snapshot dict or None
        self.rules_path = rules_path
        self.check_interval = check_interval
        self.rules = load_rules(rules_path)
        self._since = {}        # rule index -> monotonic start of continuous hold
        self._last_fired = {}   # rule index -> epoch seconds
        self._lock = threading.Lock()
        self._stop = threading.Event()

    def check_once(self, now=None):
        """Evaluate all rules against one snapshot. Returns fired (idx, rule, value)."""
        now = time.time() if now is None else now
        snap = self.source()
        if snap is None:
            return []
        fired = []
        with self._lock:
            for i, rule in enumerate(self.rules):
                value = get_value(snap, rule.get("metric"))
                if not _holds(rule, value):
                    self._since.pop(i, None)
                    continue
                since = self._since.setdefault(i, now)
                if now - since >= float(rule.get("duration_secs", 0)):
                    last = self._last_fired.get(i)
                    if last is not None and now - last < COOLDOWN_SECS:
                        continue  # cooldown: at most once/hour
                    self._last_fired[i] = now
                    self._since[i] = now  # fresh continuous window after firing
                    fired.append((i, rule, value))
        for i, rule, value in fired:
            self._fire(rule, value)
        return fired

    def _fire(self, rule, value):
        ts = datetime.now().strftime("%H:%M:%S")
        line = ("ALERT %s %s %s %s current=%s duration_secs=%s"
                % (ts, rule["metric"], rule["op"], rule["value"],
                   round(value, 1), rule["duration_secs"]))
        print(line, flush=True)
        token = os.environ.get("SYSWATCH_TG_TOKEN")
        chat = os.environ.get("SYSWATCH_TG_CHAT")
        if token and chat:
            try:
                self._tg(token, chat, line)
            except Exception as e:  # never let telegram break the loop
                print("ALERT telegram failed: %s" % e, flush=True)

    @staticmethod
    def _tg(token, chat, text):
        url = "https://api.telegram.org/bot%s/sendMessage" % token
        data = urllib.parse.urlencode({"chat_id": chat, "text": text}).encode()
        with urllib.request.urlopen(urllib.request.Request(url, data=data), timeout=10) as r:
            r.read()

    def run(self):
        while not self._stop.is_set():
            try:
                self.check_once()
            except Exception as e:
                print("ALERT check error: %s" % e, flush=True)
            self._stop.wait(self.check_interval)

    def stop(self):
        self._stop.set()

    def state(self):
        """Current rules + last-fired timestamps (UTC ISO or null)."""
        with self._lock:
            rules = []
            for i, r in enumerate(self.rules):
                out = dict(r)
                last = self._last_fired.get(i)
                out["last_fired"] = (datetime.fromtimestamp(last, timezone.utc)
                                     .isoformat() if last else None)
                rules.append(out)
            return {"rules": rules}


def default_source():
    """Latest snapshot from JSONL files in LOG_DIR (same data as /api/latest)."""
    try:
        names = sorted(n for n in os.listdir(LOG_DIR) if n.endswith(".jsonl"))
    except OSError:
        return None
    latest = None
    for name in names:
        try:
            with open(os.path.join(LOG_DIR, name), encoding="utf-8",
                      errors="replace") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        latest = json.loads(line)
                    except ValueError:
                        continue
        except OSError:
            continue
    return latest


def start_alerter(source=None):
    """Spawn the background alerter thread; returns the module singleton."""
    global _alerter
    if _alerter is None:
        _alerter = Alerter(source or default_source)
        t = threading.Thread(target=_alerter.run, daemon=True,
                             name="syswatch-alerts")
        t.start()
    return _alerter


def state():
    if _alerter is not None:
        return _alerter.state()
    return {"rules": load_rules(), "error": "alerts not started"}


def _self_test():
    """Fake snapshot source: CPU pinned at 95 (> 90). Expect exactly 1 fire."""
    fake = [{"cpu": {"total_pct": 95.0}, "memory": {"pct": 50.0}}]
    a = Alerter(lambda: fake[0], check_interval=0.05)
    a.rules = [{"metric": "cpu_total", "op": ">", "value": 90,
                "duration_secs": 0.1}]
    fired = 0
    t0 = time.time()
    while time.time() - t0 < 0.5:
        fired += len(a.check_once())
        time.sleep(0.05)
    print("SELFTEST fired=%d (expect 1)" % fired, flush=True)
    return 0 if fired == 1 else 1


if __name__ == "__main__":
    import sys
    if len(sys.argv) > 1 and sys.argv[1] == "--selftest":
        sys.exit(_self_test())
    # no-op when run directly without --selftest; server.py drives start_alerter()
