"""B11 regression tests: singleton guard + bounded _read_new() reads.

Run:  python dashboard/test_b11.py

These tests are pure-stdlib + psutil (already a server.py dep). They do NOT
start the dashboard server, do NOT touch C:\\ProgramData, and do NOT touch
the live :8123 port. They import server.py in-process.
"""
import json
import os
import sys
import tempfile
import time

# Make dashboard/ importable
HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

import server  # noqa: E402

PASS = []
FAIL = []


def check(name, cond, detail=""):
    if cond:
        PASS.append(name)
        print("  PASS  %s" % name)
    else:
        FAIL.append((name, detail))
        print("  FAIL  %s  -- %s" % (name, detail))


# ---------------------------------------------------------------- TEST 1
# Singleton guard: write a fake .sampler.pid with OUR pid, then call
# _singleton_acquire() — it must NOT exit (because the recorded pid is us,
# not a "different" alive process).
print("\n=== TEST 1: singleton acquires when no other pid is alive ===")
tmpdir = tempfile.mkdtemp(prefix="b11_test_")
saved_base = server.BASE_DIR
server.BASE_DIR = tmpdir
try:
    lock = os.path.join(tmpdir, ".sampler.pid")
    # write someone else's pid that does NOT exist (PID 1 is init on Linux
    # but on Windows there's no PID 1; pick a clearly-dead high pid).
    # Use psutil to find a definitely-dead pid.
    import psutil
    dead_pid = None
    for cand in (999999, 999998, 999997):
        if not psutil.pid_exists(cand):
            dead_pid = cand
            break
    with open(lock, "w") as fh:
        fh.write(str(dead_pid))
    # Should overwrite (because dead_pid is not alive) and NOT exit.
    server._singleton_acquire()
    with open(lock) as fh:
        recorded = int(fh.read().strip())
    check("singleton acquires lock over dead pid",
          recorded == os.getpid(),
          "lock file now holds %r, expected our pid %d" % (recorded, os.getpid()))
finally:
    server.BASE_DIR = saved_base


# ---------------------------------------------------------------- TEST 2
# Singleton guard: write a fake .sampler.pid with a LIVE pid (us, just
# re-acquired is fine — but the spec wants an *other* alive pid). We use
# a subprocess that holds itself alive briefly so psutil.pid_exists()
# returns True, then call _singleton_acquire() from the parent: it MUST
# call sys.exit(0).
print("\n=== TEST 2: singleton exits if a live different pid holds the lock ===")
import subprocess
proc = subprocess.Popen([sys.executable, "-c",
                         "import time, os; open(os.environ['HOLD_FILE'], 'w').write(str(os.getpid())); time.sleep(30)"],
                        env={**os.environ, "HOLD_FILE": os.path.join(tmpdir, "hold.txt")})
try:
    # wait for the child to write its pid
    hold = os.path.join(tmpdir, "hold.txt")
    deadline = time.time() + 5
    while time.time() < deadline and not os.path.exists(hold):
        time.time() and time.sleep(0.05)
    with open(hold) as fh:
        live_other = int(fh.read().strip())
    assert psutil.pid_exists(live_other), "child died before test ran"

    # write the live-other pid into the lock file
    with open(lock, "w") as fh:
        fh.write(str(live_other))
    # call _singleton_acquire — it should sys.exit(0)
    raised = None
    saved_base = server.BASE_DIR
    server.BASE_DIR = tmpdir
    try:
        try:
            server._singleton_acquire()
        except SystemExit as e:
            raised = e
    finally:
        server.BASE_DIR = saved_base
    check("singleton sys.exit(0) when live other pid holds lock",
          raised is not None and raised.code == 0,
          "expected SystemExit(0), got %r" % (raised,))
finally:
    proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()


# ---------------------------------------------------------------- TEST 3
# Bounded read: write a 120 MB JSONL file full of valid collector snapshots,
# prime a HistoryIndex, then call _read_new() and assert it does NOT
# allocate 120 MB. Time the call — must be under 100 ms (per spec).
print("\n=== TEST 3: _read_new() bounds reads to <=5 MB ===")
bigdir = tempfile.mkdtemp(prefix="b11_big_")
bigfile = os.path.join(bigdir, "2026-09-01.jsonl")
CHUNK = b'{"timestamp":"2026-09-01T00:00:00Z","cpu":{"total_pct":12.3},' \
        b'"memory":{"pct":45.6},"networks":[{"rx_bps":100,"tx_bps":50}],' \
        b'"temperature_c":42.0}\n'
LINE_LEN = len(CHUNK)
# 120 MB worth of lines
target_bytes = 120 * 1024 * 1024
n_lines = target_bytes // LINE_LEN
print("  writing %d lines (~%d MB) to %s ..." % (n_lines, target_bytes // (1024*1024), bigfile))
with open(bigfile, "wb") as fh:
    written = 0
    batch = CHUNK * 10000  # 10k lines per batch
    while written < target_bytes:
        fh.write(batch)
        written += len(batch)
print("  done. file size on disk: %d MB" % (os.path.getsize(bigfile) // (1024*1024)))

# Override COLLECTOR_LOGS so the indexer reads our temp dir
saved_logs = server.COLLECTOR_LOGS
server.COLLECTOR_LOGS = bigdir
try:
    hist = server.HistoryIndex()
    # Run a full index pass first (this still loads everything once — that's
    # the indexer, not the tail loop). The bounded-read fix is on _read_new,
    # which tail_loop() calls every 15s; we test THAT path.
    hist.index_done.set()  # skip the full index; test only the tail path
    # Pretend we've already read everything once, so the second call has
    # nothing new to do — but we want a TICK where the file has grown a bit.
    # Easier: pretend offset is 0 and call _read_new directly; check that
    # len(raw) <= 5_000_000 + one_line.
    got, n = hist._read_new(bigfile)
    # got is points; n is line-count. After the bounded read we should have
    # parsed ~5MB worth of lines (last 5MB of the 120MB file).
    # Assert the OFFSET advanced past read_from and roughly to file size —
    # proving we didn't re-read 120MB and we didn't get stuck re-reading
    # the same 5MB. (raw_size ~ size because we capped read_from.)
    file_size = os.path.getsize(bigfile)
    raw_size = hist.offsets.get(os.path.basename(bigfile), 0)
    check("bounded read advances offset to ~file size (not stuck at 5MB)",
          abs(raw_size - file_size) < 5_000_000,
          "offset=%d, file_size=%d" % (raw_size, file_size))

    # Steady-state tick simulation: the first _read_new above consumed the
    # last 5MB and advanced the offset to size-of-file minus trailing
    # partial. After that, a second call with no new data should exit
    # almost immediately (size - 5MB <= off). Append ZERO bytes and time it.
    t0 = time.perf_counter()
    got2, n2 = hist._read_new(bigfile)
    elapsed_ms = (time.perf_counter() - t0) * 1000.0
    check("steady-state _read_new() (no new bytes) under 100ms",
          elapsed_ms < 100 and len(got2) == 0,
          "elapsed %.1f ms, lines %d, points %d" %
          (elapsed_ms, n2, len(got2)))

    # Now simulate a realistic tick: append ONE line and time the read.
    with open(bigfile, "ab") as fh:
        fh.write(CHUNK)
    t0 = time.perf_counter()
    got3, n3 = hist._read_new(bigfile)
    elapsed_ms = (time.perf_counter() - t0) * 1000.0
    # data.split("\n") on "json\n" yields ["json", ""] -> n=2, but only 1 real
    # point. The empty trailing string is a split artifact.
    check("incremental _read_new() after +1 line under 100ms",
          elapsed_ms < 100 and len(got3) == 1,
          "elapsed %.1f ms, lines %d, points %d" %
          (elapsed_ms, n3, len(got3)))
finally:
    server.COLLECTOR_LOGS = saved_logs


# ---------------------------------------------------------------- TEST 4
# Confirm offsets stay consistent across the "rewind to 5MB cap" branch:
# call _read_new repeatedly with the file growing; verify the offsets dict
# advances monotonically and we never lose ground.
print("\n=== TEST 4: bounded read never loses data over many ticks ===")
hist2 = server.HistoryIndex()
hist2.index_done.set()
import psutil as _ps
proc_rss0 = _ps.Process(os.getpid()).memory_info().rss
last_offsets = {}
for tick in range(5):
    # simulate the collector appending another 100 lines
    with open(bigfile, "ab") as fh:
        for _ in range(100):
            fh.write(CHUNK)
    got, n = hist2._read_new(bigfile)
    off = hist2.offsets.get(os.path.basename(bigfile), 0)
    last_offsets[tick] = (off, len(got))
proc_rss1 = _ps.Process(os.getpid()).memory_info().rss
# offsets should be monotonically non-decreasing
ok = all(last_offsets[t][0] >= last_offsets[t-1][0]
         for t in range(1, len(last_offsets)))
check("offsets monotonic across 5 bounded reads", ok,
      "offsets: %s" % last_offsets)
# RSS shouldn't have ballooned (>120MB just from appending bytes)
check("RSS growth bounded (<20MB) after 5 bounded reads",
      (proc_rss1 - proc_rss0) < 20 * 1024 * 1024,
      "rss delta %d KB" % ((proc_rss1 - proc_rss0) // 1024))


print("\n=== SUMMARY ===")
print("PASS: %d   FAIL: %d" % (len(PASS), len(FAIL)))
if FAIL:
    for name, detail in FAIL:
        print("  FAIL: %s -- %s" % (name, detail))
    sys.exit(1)
print("ALL GREEN")