#!/bin/sh
# Measure CPU of the root power daemon's whole process subtree, in % of ONE
# core, averaged over a window.  Sampled every 0.5 s because powermetrics
# children are short-lived: their CPU has to be caught while they exist.
#
# Usage: measure_daemon_cpu.sh [seconds]
SECS="${1:-60}"

/usr/bin/python3 - "$SECS" <<'PY'
import subprocess, sys, time, collections
secs = float(sys.argv[1])
samples = 0
total = 0.0
per_cmd = collections.Counter()
t_end = time.time() + secs
while time.time() < t_end:
    out = subprocess.run(["/bin/ps", "-A", "-o", "pcpu=,comm="],
                         capture_output=True, text=True).stdout
    now = 0.0
    for line in out.splitlines():
        parts = line.split(None, 1)
        if len(parts) != 2:
            continue
        cpu, cmd = parts
        base = cmd.rsplit("/", 1)[-1]
        if base in ("powermetrics", "syswatch-powerd", "power_sampler.sh",
                    "smc_temp"):
            v = float(cpu)
            now += v
            per_cmd[base] += v
    total += now
    samples += 1
    time.sleep(0.5)
print("window_s            %.1f" % secs)
print("samples             %d" % samples)
print("avg_cpu_pct_one_core %.3f" % (total / max(1, samples)))
for k, v in per_cmd.most_common():
    print("  share %-18s %.3f" % (k, v / max(1, samples)))
PY
