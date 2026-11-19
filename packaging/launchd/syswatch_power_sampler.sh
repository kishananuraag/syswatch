#!/bin/sh
# syswatch elevated sampler — runs AS ROOT from the LaunchDaemon
# com.barbarian.syswatch.power (system domain).  Installed by the dashboard's
# one-click grant flow (POST /api/permissions/grant) under a single macOS admin
# prompt; the dashboard process NEVER runs as root and never holds a password.
#
# RETIRED IN T19 — DO NOT INSTALL THIS.
#   This whole loop is now the Rust binary `syswatch-powerd`
#   (src/bin/powerd.rs, installed to /usr/local/libexec/syswatch/syswatch-powerd).
#   The port keeps every decision in this file: the verified powermetrics flags,
#   the sampler probe with `smc` first, the awk field mapping, the "absent
#   rather than zero" rule for anything this build does not print, the first
#   sample dumped to powermetrics.raw.log, and the atomic sample.json write.
#   The grant flow no longer copies this file, and removes any installed
#   /usr/local/libexec/syswatch/power_sampler.sh.  Kept as the reference the
#   Rust parser is compared against.
#
# What it does
#   Every INTERVAL_MS it runs powermetrics once (-n 1), parses the real text
#   output, and atomically writes ONE JSON snapshot to
#       <OUT_DIR>/sample.json      (the file the unprivileged dashboard reads)
#   The first real sample is also dumped verbatim to
#       <OUT_DIR>/powermetrics.raw.log
#   as evidence of the exact field names on this macOS build.
#
# T16B — temperatures and per-cluster clocks
#   powermetrics on Apple Silicon (macOS 15.8.3 / 24H241) prints ONLY the
#   thermal pressure level: `-s smc` is rejected ("unrecognized sampler: smc"),
#   so die temperatures are NOT reachable through powermetrics here.  They are
#   read from the AppleSMC IOKit user client by the small helper
#       /usr/local/libexec/syswatch/smc_temp
#   (source: packaging/launchd/smc_temp.c, compiled at grant time).  Its output
#   is merged into sample.json as
#       "temps": [{"key","label","c"}, …],  "cpu_avg_c", "gpu_avg_c",
#       "battery_c", "ssd_c", "smc": {…}
#   If the helper is missing, fails, or finds no sane reading, `temps` is simply
#   ABSENT — the UI keeps its honest "unavailable", never a guess.
#   The helper's full sweep (~75 ms, every key the SMC publishes) runs once at
#   startup into smc_all.json; the 5 s loop reads only the named sensors
#   (~10 ms), which is what keeps the daemon inside its CPU budget.
#
#   Cluster clocks come from the SAME powermetrics text — no extra sampling:
#       "e_cluster": {"freq_mhz","residency_pct"}, "p_cluster": {…},
#       "gpu_freq_mhz", "gpu_residency_pct", "per_cpu": [{…}, …]
#
# REAL FLAGS — verified with `/usr/bin/powermetrics --help` on macOS 15.8.3
# (24H241).  The brief's `-f <file> -i 5` is WRONG on this build:
#     -f | --format <text|plist>   is the OUTPUT FORMAT, not a file
#     -o | --output-file <file>    is the file flag
#     -i | --sample-rate <N>       is the interval in MILLISECONDS (default 5000)
#     -n | --sample-count <N>      -1 (default) = infinite, 1 = one sample, exit
#     -s | --samplers <list>       comma-separated sampler names
#   Samplers supported on this build (from --help): tasks, battery, network,
#   disk, interrupts, cpu_power, thermal, sfi, gpu_power, ane_power.
#   `smc` is NOT listed by --help on 15.8.3 even though `man powermetrics`
#   documents it, so it is probed once at startup and dropped if rejected.
#   `powermetrics` itself lives at /usr/bin/powermetrics (NOT /usr/sbin).
#
# SECURITY
#   * No password is ever read, echoed, stored or logged by anything here: the
#     credential is consumed by macOS in the admin dialog before this script is
#     copied into place, and this file only ever sees stdout/stderr.
#   * Installed root-owned (root:wheel, 0755) so unprivileged code cannot swap
#     it for its own payload between reboots.
#
# Usage: power_sampler.sh [interval_ms] [output_dir]

set -u

INTERVAL_MS="${1:-5000}"
OUT_DIR="${2:-/tmp}"
PM=/usr/bin/powermetrics
SMC=/usr/local/libexec/syswatch/smc_temp

OUT="$OUT_DIR/sample.json"
RAW="$OUT_DIR/powermetrics.raw.log"
ERR="$OUT_DIR/powermetrics.err.log"
SMC_CACHE="$OUT_DIR/smc_keys.txt"
SMC_OUT="$OUT_DIR/smc_temps.txt"
SMC_ALL="$OUT_DIR/smc_all.json"

if [ ! -d "$OUT_DIR" ]; then
    mkdir -p "$OUT_DIR" 2>/dev/null || {
        echo "$(date) output dir $OUT_DIR is not writable" >>/tmp/syswatch-power.err
        exit 1
    }
fi
umask 022

if [ ! -x "$PM" ]; then
    echo "$(date) $PM is missing" >>"$ERR"
    exit 1
fi

# ---- SMC helper probe (T16B) -------------------------------------------------
# One read at startup: builds the key cache on first run (~0.4 s) and proves the
# helper works.  Nothing else depends on it — a failure only removes `temps`.
SMC_STATE="absent"
if [ -x "$SMC" ]; then
    if "$SMC" --cache "$SMC_CACHE" >/dev/null 2>>"$ERR"; then
        SMC_STATE="ok"
    else
        SMC_STATE="failed"
        echo "$(date) smc_temp present but returned no sane reading; temps omitted" >>"$ERR"
    fi
else
    echo "$(date) $SMC not installed; temperatures omitted (powermetrics cannot supply them on this build)" >>"$ERR"
fi
# A leftover readings file from a previous install must never be merged.
[ "$SMC_STATE" = "ok" ] || rm -f "$SMC_OUT" 2>/dev/null

# ---- sampler support probe (one extra short sample at startup) --------------
SAMPLERS="thermal,smc,gpu_power,cpu_power,ane_power"
if ! "$PM" -n 1 -i 100 -f text -s "$SAMPLERS" >/dev/null 2>>"$ERR"; then
    SAMPLERS="thermal,gpu_power,cpu_power,ane_power"
    echo "$(date) sampler set with smc rejected on this build; using $SAMPLERS" >>"$ERR"
fi

first=1
while :; do
    # Full SMC sweep once at startup: every temperature key this machine
    # publishes, kept as an evidence file.  Not repeated — it costs ~75 ms.
    if [ "$first" -eq 1 ] && [ "$SMC_STATE" = "ok" ]; then
        "$SMC" --all --cache "$SMC_CACHE" >"$SMC_ALL.tmp.$$" 2>>"$ERR" &&
            mv -f "$SMC_ALL.tmp.$$" "$SMC_ALL"
        chmod 644 "$SMC_ALL" 2>/dev/null
    fi

    SAMPLE=$("$PM" -n 1 -i "$INTERVAL_MS" -f text -s "$SAMPLERS" 2>>"$ERR")
    if [ -z "$SAMPLE" ]; then
        sleep 5
        continue
    fi
    TS=$(date +%s)
    if [ "$first" -eq 1 ]; then
        {
            echo "# first real powermetrics sample on this build — $(date)"
            echo "# flags: -n 1 -i $INTERVAL_MS -f text -s $SAMPLERS"
            printf '%s\n' "$SAMPLE"
        } >"$RAW"
        chmod 644 "$RAW" 2>/dev/null
        echo "$(date) first sample captured; samplers=$SAMPLERS smc=$SMC_STATE" >>"$ERR"
        first=0
    fi

    # Per-cycle SMC read: named sensors only, ~10 ms.
    rm -f "$SMC_OUT.tmp.$$" 2>/dev/null
    if [ "$SMC_STATE" = "ok" ]; then
        if ! "$SMC" --split --cache "$SMC_CACHE" >"$SMC_OUT.tmp.$$" 2>>"$ERR"; then
            # A failed read must not leave the PREVIOUS cycle's temperatures
            # lying around to be merged as if they were current.
            rm -f "$SMC_OUT.tmp.$$" "$SMC_OUT" 2>/dev/null
        elif [ -s "$SMC_OUT.tmp.$$" ]; then
            mv -f "$SMC_OUT.tmp.$$" "$SMC_OUT"
        else
            rm -f "$SMC_OUT.tmp.$$" "$SMC_OUT" 2>/dev/null
        fi
    fi

    printf '%s\n' "$SAMPLE" | awk \
        -v ts="$TS" \
        -v interval="$INTERVAL_MS" \
        -v samplers="$SAMPLERS" \
        -v smc_state="$SMC_STATE" \
        -v smc_file="$SMC_OUT" '
    function trim(s) { gsub(/^[ \t\r]+/, "", s); gsub(/[ \t\r]+$/, "", s); return s }
    function val(line) { return trim(substr(line, index(line, ":") + 1)) }
    function number(s) { gsub(/[^0-9.\-]/, "", s); return s + 0 }
    function firstnum(s,  m) {
        if (match(s, /-?[0-9]+(\.[0-9]+)?/)) return substr(s, RSTART, RLENGTH) + 0
        return ""
    }
    function label_of(line) { return trim(substr(line, 1, index(line, ":") - 1)) }
    function watts(line,  v) {
        v = val(line)
        if (v ~ /mW/) return number(v) / 1000.0
        if (v ~ /W/)  return number(v)
        return ""
    }
    BEGIN {
        ncl = 0; nfan = 0; ncore = 0; nsmc = 0
        pkg = ""; pkgsrc = ""; cpu = ""; gpu = ""; ane = ""
        # 3-line SMC block (temps array / aggregates / meta), read once.
        if (smc_state == "ok") {
            while ((getline sl < smc_file) > 0) {
                nsmc++
                smc_line[nsmc] = sl
            }
            close(smc_file)
            if (nsmc < 2) nsmc = 0
        }
    }
    {
        line = trim($0)
        if (line ~ /^E-Cluster HW active frequency[ \t]*:/) {
            e_freq = firstnum(val(line)); next
        }
        if (line ~ /^E-Cluster HW active residency[ \t]*:/) {
            e_res = firstnum(val(line)); next
        }
        if (line ~ /^P-Cluster HW active frequency[ \t]*:/) {
            p_freq = firstnum(val(line)); next
        }
        if (line ~ /^P-Cluster HW active residency[ \t]*:/) {
            p_res = firstnum(val(line)); next
        }
        if (line ~ /^GPU HW active frequency[ \t]*:/) {
            gpu_freq = firstnum(val(line)); next
        }
        if (line ~ /^GPU HW active residency[ \t]*:/) {
            gpu_res = firstnum(val(line)); next
        }
        if (line ~ /^CPU [0-9]+ frequency[ \t]*:/) {
            ncore++; core_freq[ncore] = firstnum(val(line)); core_res[ncore] = ""; next
        }
        if (line ~ /^CPU [0-9]+ active residency[ \t]*:/) {
            if (ncore == 0) { ncore = 1; core_freq[1] = "" }
            core_res[ncore] = firstnum(val(line)); next
        }
        if (line ~ /^CPU Power[ \t]*:/)    { cpu = watts(line); next }
        if (line ~ /^GPU Power[ \t]*:/)    { gpu = watts(line); next }
        if (line ~ /^ANE Power[ \t]*:/)    { ane = watts(line); next }
        if (line ~ /^Package Power[ \t]*:/) {
            pkg = watts(line); pkgsrc = label_of(line); next
        }
        if (line ~ /^Combined Power/) {
            if (pkgsrc != "Package Power") { pkg = watts(line); pkgsrc = label_of(line) }
            next
        }
        if (line ~ /die temperature[ \t]*:/) {
            l = label_of(line); v = number(val(line))
            if (l ~ /^CPU die/)      cpu_t = v
            else if (l ~ /^GPU die/) gpu_t = v
            else { ncl++; cl_l[ncl] = l; cl_v[ncl] = v }
            next
        }
        if (line ~ /^Current pressure level[ \t]*:/) { pressure = val(line); next }
        if (line ~ /[Ff]an/ && line ~ /rpm/) { nfan++; fan[nfan] = number(val(line)); next }
        if (line ~ /ANE/ && line ~ /[Uu]tiliz/ && line ~ /:/) {
            ane_util = number(val(line)); next
        }
    }
    END {
        printf "{"
        printf "\"ts\": %d, \"sample_ms\": %d", ts, interval
        printf ", \"samplers\": \"%s\"", samplers
        printf ", \"source\": \"powermetrics (root LaunchDaemon com.barbarian.syswatch.power)\""
        if (cpu != "") printf ", \"cpu_w\": %.3f", cpu
        if (gpu != "") printf ", \"gpu_w\": %.3f", gpu
        if (ane != "") printf ", \"ane_w\": %.3f", ane
        if (pkg != "") printf ", \"package_w\": %.3f, \"package_w_source\": \"%s\"", pkg, pkgsrc
        if (cpu_t != "") printf ", \"cpu_temp_c\": %.2f", cpu_t
        if (gpu_t != "") printf ", \"gpu_temp_c\": %.2f", gpu_t
        if (ncl > 0) {
            printf ", \"cpu_cluster_temps_c\": ["
            for (i = 1; i <= ncl; i++) {
                if (i > 1) printf ", "
                printf "{\"label\": \"%s\", \"c\": %.2f}", cl_l[i], cl_v[i]
            }
            printf "]"
        }
        if (pressure != "") printf ", \"thermal_pressure\": \"%s\"", pressure
        if (nfan > 0) {
            printf ", \"fans_rpm\": ["
            for (i = 1; i <= nfan; i++) { if (i > 1) printf ", "; printf "%d", fan[i] }
            printf "]"
        }
        if (ane_util != "") printf ", \"ane_util_pct\": %.2f", ane_util
        # ---- T16B: cluster clocks straight from the same text --------------
        if (e_freq != "" || e_res != "") {
            printf ", \"e_cluster\": {"
            if (e_freq != "") printf "\"freq_mhz\": %d", e_freq
            if (e_freq != "" && e_res != "") printf ", "
            if (e_res != "") printf "\"residency_pct\": %.2f", e_res
            printf "}"
        }
        if (p_freq != "" || p_res != "") {
            printf ", \"p_cluster\": {"
            if (p_freq != "") printf "\"freq_mhz\": %d", p_freq
            if (p_freq != "" && p_res != "") printf ", "
            if (p_res != "") printf "\"residency_pct\": %.2f", p_res
            printf "}"
        }
        if (gpu_freq != "") printf ", \"gpu_freq_mhz\": %d", gpu_freq
        if (gpu_res != "") printf ", \"gpu_residency_pct\": %.2f", gpu_res
        if (ncore > 0) {
            printf ", \"per_cpu\": ["
            for (i = 1; i <= ncore; i++) {
                if (i > 1) printf ", "
                printf "{"
                if (core_freq[i] != "") printf "\"freq_mhz\": %d", core_freq[i]
                if (core_freq[i] != "" && core_res[i] != "") printf ", "
                if (core_res[i] != "") printf "\"residency_pct\": %.2f", core_res[i]
                printf "}"
            }
            printf "]"
        }
        # ---- T16B: SMC temperatures --------------------------------------
        # smc_line[1] = temps array, [2] = aggregates, [3] = meta block.
        if (nsmc >= 1) printf ", \"temps\": %s", smc_line[1]
        if (nsmc >= 2) printf ", %s", smc_line[2]
        if (nsmc >= 3) printf ", %s", smc_line[3]
        printf "}\n"
    }' >"$OUT.tmp.$$" && mv -f "$OUT.tmp.$$" "$OUT"
    chmod 644 "$OUT" 2>/dev/null
    rm -f "$OUT.tmp.$$" 2>/dev/null
done
