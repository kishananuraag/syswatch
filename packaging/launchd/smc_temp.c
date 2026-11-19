/*
 * smc_temp — AppleSMC temperature reader for the syswatch root daemon (T16B).
 *
 * RETIRED IN T19 — DO NOT INSTALL THIS.
 *   The whole reader now lives inside the Rust daemon as `mod smc` in
 *   src/bin/powerd.rs (installed as /usr/local/libexec/syswatch/syswatch-powerd).
 *   That module is a line-for-line port of this file's IOKit plumbing, data-type
 *   decoding, key inventory and probe order, so the two can be compared field by
 *   field.  This source is kept only as provenance for that port and as the
 *   record of why the behaviour is what it is; the grant flow no longer compiles
 *   it and removes any installed /usr/local/libexec/syswatch/smc_temp.
 *   (Verified equivalent on Mac14,15 / macOS 15.8.3: full sweep 131 rows and
 *   25 read failures, named set 17 rows and 25 read failures — identical.)
 *
 * WHY THIS EXISTS
 *   powermetrics on macOS 15.8.3 / Apple Silicon prints ONLY the thermal
 *   pressure level; `-s smc` is rejected ("unrecognized sampler: smc"), so die
 *   temperatures are NOT reachable through powermetrics on this platform.
 *   They ARE reachable through the AppleSMC IOKit user client, which is the
 *   same mechanism iStat Menus and smctemp use.  The syswatch sampler already
 *   runs as root, so the read happens there.
 *
 * PROVENANCE
 *   IOKit plumbing, SMC key-data structures, data-type decoding and the
 *   Apple-Silicon key inventory are ported from:
 *     - github.com/narugit/smctemp  (MIT)   — AppleSMC open/read/type decode
 *     - github.com/exelban/stats    (MIT)   — sensor key -> human name
 *   Both permissive licences; this is a fresh C implementation of the same
 *   documented protocol, not a copy of their files.
 *
 * DESIGN RULES (same policy as the rest of syswatch)
 *   * Probe, do not assume.  A static key table gives the known names; a
 *     discovery pass reads the SMC's own key index ("#KEY" + per-index key
 *     names + type) and records what this machine actually publishes.  The
 *     result is cached, because enumerating ~1800 keys costs ~0.4 s and the
 *     sample loop runs every 5 s.
 *   * Only sane readings are emitted: −20…120 °C, per the brief.  A key that
 *     fails, is a type we cannot decode, or decodes out of range is simply
 *     absent — never 0 °C, never a guess.
 *   * A key with no name in the table is labelled "Sensor <key>".  Honest
 *     naming beats a plausible-looking invention.
 *
 * COST (measured on Mac14,15 / M2, macOS 15.8.3)
 *   full sweep of every decodable key : ~75 ms  (146 rows)
 *   named set only (`--fast`)         : ~10 ms  (15 rows)
 *   The daemon reads the fast set each cycle and the full sweep once at
 *   startup (dumped to smc_all.json) — see syswatch_power_sampler.sh.
 *
 * OUTPUT (stdout, one line of JSON)
 *   {"ok":true,"mode":"fast","source":"AppleSMC (root)","keys":15,
 *    "discovered":146,"scanned":0,
 *    "temps":[{"key":"Tp01","label":"CPU performance core 1","c":38.01,
 *              "type":"flt "}, …],
 *    "cpu_avg_c":36.25,"gpu_avg_c":34.42,"battery_c":28.0,"ssd_c":30.14,
 *    "emitted":15}
 *   On failure: {"ok":false,"error":"…"} and a non-zero exit — the sampler
 *   then emits no `temps` key at all.
 *
 * BUILD (the grant flow compiles this into /usr/local/libexec/syswatch)
 *   cc -O2 -framework IOKit -framework CoreFoundation -o smc_temp smc_temp.c
 *
 * USAGE
 *   smc_temp [--cache FILE] [--fast] [--all] [--discover] [--max N]
 *     --cache FILE  read/write the discovered-key cache (recommended)
 *     --fast        read only the named sensors (default when a cache exists)
 *     --all         read every decodable temperature key
 *     --discover    ignore the cache and re-enumerate the SMC key index
 *     --max N       cap the emitted rows
 */

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <sys/stat.h>

#include <IOKit/IOKitLib.h>
#include <CoreFoundation/CoreFoundation.h>

/* ── SMC protocol structures (layout must match the kernel's) ─────────── */
#define SMC_CMD_READ_BYTES   5
#define SMC_CMD_READ_INDEX   8
#define SMC_CMD_READ_KEYINFO 9
#define SMC_KERNEL_INDEX     2

typedef struct {
    char major, minor, build, reserved[1];
    uint16_t release;
} smc_vers_t;

typedef struct {
    uint16_t version, length;
    uint32_t cpuPLimit, gpuPLimit, memPLimit;
} smc_plimit_t;

typedef struct {
    uint32_t dataSize;
    uint32_t dataType;
    char dataAttributes;
} smc_keyinfo_t;

typedef unsigned char smc_bytes_t[32];

typedef struct {
    uint32_t key;
    smc_vers_t vers;
    smc_plimit_t pLimitData;
    smc_keyinfo_t keyInfo;
    char result, status, data8;
    uint32_t data32;
    smc_bytes_t bytes;
} smc_keydata_t;

typedef struct {
    char key[5];
    uint32_t dataSize;
    char dataType[5];
    smc_bytes_t bytes;
} smc_val_t;

/* A sane die/skin temperature.  Anything outside is noise, a counter, or a
   different data type entirely — dropped rather than rendered. */
#define TEMP_MIN (-20.0)
#define TEMP_MAX (120.0)

#define MAX_KEYS 4096

/* ── static name table ────────────────────────────────────────────────── *
 * Sourced from exelban/stats values.swift (M2 generation block) plus the
 * generic key names, and smctemp's ARM64 key list.  Only names we can source
 * appear here; `family_label()` covers the rest, and anything it does not
 * recognise stays "Sensor <key>".                                          */
typedef struct {
    const char *key;
    const char *label;
    const char *group;   /* cpu | gpu | battery | ssd | other */
} name_row;

static const name_row NAMES[] = {
    /* Apple M2 (this machine: Mac14,15) — CPU */
    {"Tp1h", "CPU efficiency core 1", "cpu"},
    {"Tp1t", "CPU efficiency core 2", "cpu"},
    {"Tp1p", "CPU efficiency core 3", "cpu"},
    {"Tp1l", "CPU efficiency core 4", "cpu"},
    {"Tp01", "CPU performance core 1", "cpu"},
    {"Tp05", "CPU performance core 2", "cpu"},
    {"Tp09", "CPU performance core 3", "cpu"},
    {"Tp0D", "CPU performance core 4", "cpu"},
    {"Tp0X", "CPU performance core 5", "cpu"},
    {"Tp0b", "CPU performance core 6", "cpu"},
    {"Tp0f", "CPU performance core 7", "cpu"},
    {"Tp0j", "CPU performance core 8", "cpu"},
    {"Tp0n", "CPU performance core 9", "cpu"},
    {"Tp0r", "CPU performance core 10", "cpu"},
    /* Apple M2 — GPU */
    {"Tg0f", "GPU 1", "gpu"},
    {"Tg0j", "GPU 2", "gpu"},
    /* M1 (the same helper ships to M1 machines) */
    {"Tp0T", "CPU efficiency core 2 (M1)", "cpu"},
    {"Tp0H", "CPU performance core 4 (M1)", "cpu"},
    {"Tp0L", "CPU performance core 5 (M1)", "cpu"},
    {"Tp0P", "CPU performance core 6 (M1)", "cpu"},
    {"Tp0h", "CPU performance core 7 (M1)", "cpu"},
    {"Tg05", "GPU 1 (M1)", "gpu"},
    {"Tg0D", "GPU 2 (M1)", "gpu"},
    {"Tg0L", "GPU 3 (M1)", "gpu"},
    {"Tg0T", "GPU 4 (M1)", "gpu"},
    /* CPU die auxiliary sensors (smctemp ARM64 list) */
    {"Tc0a", "CPU die aux Tc0a", "cpu"},
    {"Tc0b", "CPU die aux Tc0b", "cpu"},
    {"Tc0x", "CPU die aux Tc0x", "cpu"},
    {"Tc0z", "CPU die aux Tc0z", "cpu"},
    /* Memory (stats: Tm* on Apple Silicon) */
    {"Tm02", "Memory 1", "other"},
    {"Tm06", "Memory 2", "other"},
    {"Tm08", "Memory 3", "other"},
    {"Tm09", "Memory 4", "other"},
    /* Battery pack sensors (Intel names; present on some Apple packs) */
    {"TB0T", "Battery 1", "battery"},
    {"TB1T", "Battery 2", "battery"},
    {"TB2T", "Battery 3", "battery"},
    /* Palm rest / skin */
    {"Ts0P", "Palm rest", "other"},
    {"Ts1P", "Palm rest 2", "other"},
    /* SSD / NAND */
    {"TH0x", "SSD", "ssd"},
    {"TH0A", "SSD 2", "ssd"},
    {"TH0B", "SSD 3", "ssd"},
    {"TH0T", "SSD 4", "ssd"},
};

#define NAME_COUNT (sizeof(NAMES) / sizeof(NAMES[0]))

static const name_row *lookup_name(const char *key) {
    for (size_t i = 0; i < NAME_COUNT; i++) {
        if (strcmp(NAMES[i].key, key) == 0) return &NAMES[i];
    }
    return NULL;
}

/* Fallback naming by key family — only unambiguous Apple Silicon prefixes. */
static const char *family_label(const char *key, char *buf, size_t buflen,
                                const char **group) {
    *group = "other";
    if (key[0] != 'T') return NULL;
    switch (key[1]) {
    case 'p': *group = "cpu";     snprintf(buf, buflen, "CPU core %s", key);    return buf;
    case 'g': *group = "gpu";     snprintf(buf, buflen, "GPU %s", key);         return buf;
    case 'c': *group = "cpu";     snprintf(buf, buflen, "CPU die aux %s", key); return buf;
    case 'm': *group = "other";   snprintf(buf, buflen, "Memory %s", key);      return buf;
    case 'B': *group = "battery"; snprintf(buf, buflen, "Battery %s", key);     return buf;
    case 's': *group = "other";   snprintf(buf, buflen, "Palm rest %s", key);   return buf;
    case 'H': *group = "ssd";     snprintf(buf, buflen, "SSD %s", key);         return buf;
    default: return NULL;
    }
}

/* ── IOKit plumbing ───────────────────────────────────────────────────── */
static io_connect_t g_conn = 0;

static kern_return_t smc_call(int index, smc_keydata_t *in, smc_keydata_t *out) {
    size_t in_size = sizeof(smc_keydata_t);
    size_t out_size = sizeof(smc_keydata_t);
    return IOConnectCallStructMethod(g_conn, (uint32_t)index, in, in_size,
                                     out, &out_size);
}

static int smc_open(void) {
    io_service_t device = IOServiceGetMatchingService(kIOMainPortDefault,
                                                      IOServiceMatching("AppleSMC"));
    if (!device) return -1;
    kern_return_t r = IOServiceOpen(device, mach_task_self(), 0, &g_conn);
    IOObjectRelease(device);
    return (r == kIOReturnSuccess) ? 0 : -2;
}

static uint32_t smc_key_code(const char *key) {
    return ((uint32_t)(unsigned char)key[0] << 24) |
           ((uint32_t)(unsigned char)key[1] << 16) |
           ((uint32_t)(unsigned char)key[2] << 8)  |
           ((uint32_t)(unsigned char)key[3]);
}

static int smc_key_info(const char *key, smc_keyinfo_t *info) {
    smc_keydata_t in, out;
    memset(&in, 0, sizeof in);
    memset(&out, 0, sizeof out);
    in.key = smc_key_code(key);
    in.data8 = SMC_CMD_READ_KEYINFO;
    if (smc_call(SMC_KERNEL_INDEX, &in, &out) != kIOReturnSuccess) return -1;
    *info = out.keyInfo;
    return 0;
}

static int smc_read(const char *key, smc_val_t *val) {
    smc_keydata_t in, out;
    smc_keyinfo_t info;

    memset(val, 0, sizeof *val);
    snprintf(val->key, sizeof val->key, "%s", key);

    if (smc_key_info(key, &info) != 0) return -1;
    val->dataSize = info.dataSize;
    if (val->dataSize == 0 || val->dataSize > sizeof val->bytes) return -1;
    val->dataType[0] = (char)((info.dataType >> 24) & 0xff);
    val->dataType[1] = (char)((info.dataType >> 16) & 0xff);
    val->dataType[2] = (char)((info.dataType >> 8) & 0xff);
    val->dataType[3] = (char)(info.dataType & 0xff);
    val->dataType[4] = '\0';

    memset(&in, 0, sizeof in);
    memset(&out, 0, sizeof out);
    in.key = smc_key_code(key);
    in.keyInfo.dataSize = val->dataSize;
    in.data8 = SMC_CMD_READ_BYTES;
    if (smc_call(SMC_KERNEL_INDEX, &in, &out) != kIOReturnSuccess) return -1;
    memcpy(val->bytes, out.bytes, sizeof val->bytes);
    return 0;
}

/* SMC publishes the key count at "#KEY". */
static uint32_t smc_key_count(void) {
    smc_val_t v;
    if (smc_read("#KEY", &v) != 0) return 0;
    uint32_t n = 0;
    for (uint32_t i = 0; i < v.dataSize; i++) n = (n << 8) | v.bytes[i];
    if (n == 0 || n > MAX_KEYS) return 0;
    return n;
}

static int smc_key_at(uint32_t index, char out[5]) {
    smc_keydata_t in, out_s;
    memset(&in, 0, sizeof in);
    memset(&out_s, 0, sizeof out_s);
    in.data8 = SMC_CMD_READ_INDEX;
    in.data32 = index;
    if (smc_call(SMC_KERNEL_INDEX, &in, &out_s) != kIOReturnSuccess) return -1;
    out[0] = (char)((out_s.key >> 24) & 0xff);
    out[1] = (char)((out_s.key >> 16) & 0xff);
    out[2] = (char)((out_s.key >> 8) & 0xff);
    out[3] = (char)(out_s.key & 0xff);
    out[4] = '\0';
    return 0;
}

/* ── data-type decoding (ported from smctemp's ReadValue) ─────────────────
 * Returns 1 and writes *out when the type is one we understand, 0 otherwise.
 * An unknown type ("ioft", "hex_", …) is NOT 0 °C — it is "no reading".      */
static int decode(const smc_val_t *v, double *out) {
    const char *t = v->dataType;
    const unsigned char *b = v->bytes;
    uint32_t n = v->dataSize;

    if (strcmp(t, "flt ") == 0 && n >= 4) {
        float f;
        memcpy(&f, b, sizeof f);
        *out = (double)f;
        return 1;
    }
    if (strcmp(t, "sp78") == 0 && n >= 2) { *out = ((int16_t)((b[0] << 8) | b[1])) / 256.0; return 1; }
    if (strcmp(t, "sp87") == 0 && n >= 2) { *out = ((int16_t)((b[0] << 8) | b[1])) / 128.0; return 1; }
    if (strcmp(t, "sp96") == 0 && n >= 2) { *out = ((int16_t)((b[0] << 8) | b[1])) / 64.0;  return 1; }
    if (strcmp(t, "spb4") == 0 && n >= 2) { *out = ((int16_t)((b[0] << 8) | b[1])) / 16.0;  return 1; }
    if (strcmp(t, "spf0") == 0 && n >= 2) { *out = (double)(int16_t)((b[0] << 8) | b[1]);    return 1; }
    if (strcmp(t, "fpe2") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 4.0;   return 1; }
    if (strcmp(t, "fp88") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 256.0; return 1; }
    if (strcmp(t, "fp4c") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 4096.0; return 1; }
    if (strcmp(t, "fp5b") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 2048.0; return 1; }
    if (strcmp(t, "fp6a") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 1024.0; return 1; }
    if (strcmp(t, "fp79") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 512.0;  return 1; }
    if (strcmp(t, "fp1f") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 32768.0; return 1; }
    if (strcmp(t, "fpa6") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 64.0;   return 1; }
    if (strcmp(t, "fpc4") == 0 && n >= 2) { *out = ((uint16_t)((b[0] << 8) | b[1])) / 16.0;   return 1; }
    if (strcmp(t, "ui8 ") == 0 && n >= 1) { *out = (double)b[0]; return 1; }
    if (strcmp(t, "ui16") == 0 && n >= 2) { *out = (double)(uint16_t)((b[0] << 8) | b[1]); return 1; }
    if (strcmp(t, "ui32") == 0 && n >= 4) {
        *out = (double)(((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) |
                        ((uint32_t)b[2] << 8) | b[3]);
        return 1;
    }
    if (strcmp(t, "si8 ") == 0 && n >= 1) { *out = (double)(int8_t)b[0]; return 1; }
    if (strcmp(t, "si16") == 0 && n >= 2) { *out = (double)(int16_t)((b[0] << 8) | b[1]); return 1; }
    return 0;
}

static int is_sane(double c) {
    return c > TEMP_MIN && c < TEMP_MAX;
}

/* ── key set ──────────────────────────────────────────────────────────── */
typedef struct { char key[5]; char type[5]; } keyrow;

static keyrow g_keys[MAX_KEYS];
static int g_nkeys = 0;

static int have_key(const char *k) {
    for (int i = 0; i < g_nkeys; i++) if (strcmp(g_keys[i].key, k) == 0) return 1;
    return 0;
}

static void add_key(const char *k, const char *type) {
    if (g_nkeys >= MAX_KEYS || have_key(k)) return;
    snprintf(g_keys[g_nkeys].key, 5, "%s", k);
    snprintf(g_keys[g_nkeys].type, 5, "%s", type ? type : "");
    g_nkeys++;
}

static int load_cache(const char *path) {
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    char line[64];
    while (fgets(line, sizeof line, f)) {
        char k[5] = {0}, t[5] = {0};
        if (line[0] == '#') continue;
        int got = sscanf(line, "%4s %4s", k, t);
        if (got >= 1 && strlen(k) == 4) add_key(k, got >= 2 ? t : "");
    }
    fclose(f);
    return g_nkeys;
}

static void save_cache(const char *path) {
    char tmp[512];
    snprintf(tmp, sizeof tmp, "%s.tmp", path);
    FILE *f = fopen(tmp, "w");
    if (!f) return;
    fprintf(f, "# smc_temp key cache — %d decodable temperature keys\n", g_nkeys);
    fprintf(f, "# regenerated by: smc_temp --discover --cache <this file>\n");
    for (int i = 0; i < g_nkeys; i++)
        fprintf(f, "%s %s\n", g_keys[i].key, g_keys[i].type);
    fclose(f);
    chmod(tmp, 0644);
    rename(tmp, path);
}

/* Enumerate the SMC key index and keep every key whose type we can decode.
   This is the "probe, do not assume names" half of the design. */
static int discover_keys(unsigned *scanned) {
    uint32_t count = smc_key_count();
    *scanned = count;
    if (!count) return -1;
    int found = 0;
    for (uint32_t i = 0; i < count; i++) {
        char k[5];
        if (smc_key_at(i, k) != 0) continue;
        if (k[0] != 'T') continue;              /* temperatures live in T*  */
        smc_keyinfo_t info;
        if (smc_key_info(k, &info) != 0) continue;
        char type[5];
        type[0] = (char)((info.dataType >> 24) & 0xff);
        type[1] = (char)((info.dataType >> 16) & 0xff);
        type[2] = (char)((info.dataType >> 8) & 0xff);
        type[3] = (char)(info.dataType & 0xff);
        type[4] = '\0';
        if (lookup_name(k) == NULL && !isalpha((unsigned char)k[1])) continue;
        int before = g_nkeys;
        add_key(k, type);
        if (g_nkeys > before) found++;
    }
    return found;
}

/* ── main ─────────────────────────────────────────────────────────────── */
static void emit_fail(const char *why) {
    printf("{\"ok\":false,\"error\":\"%s\"}\n", why);
    fflush(stdout);
}

int main(int argc, char **argv) {
    const char *cache = NULL;
    int force_discover = 0;
    int want_all = 0;
    int max_rows = 0;
    int split = 0;

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--cache") == 0 && i + 1 < argc) {
            cache = argv[++i];
        } else if (strcmp(argv[i], "--discover") == 0) {
            force_discover = 1;
        } else if (strcmp(argv[i], "--all") == 0) {
            want_all = 1;
        } else if (strcmp(argv[i], "--fast") == 0) {
            want_all = 0;
        } else if (strcmp(argv[i], "--split") == 0) {
            split = 1;
        } else if (strcmp(argv[i], "--max") == 0 && i + 1 < argc) {
            max_rows = atoi(argv[++i]);
        } else if (strcmp(argv[i], "--help") == 0 || strcmp(argv[i], "-h") == 0) {
            printf("usage: smc_temp [--cache FILE] [--fast|--all] "
                   "[--discover] [--split] [--max N]\n");
            return 0;
        }
    }

    if (smc_open() != 0) {
        emit_fail("AppleSMC IOServiceOpen failed");
        return 2;
    }

    /* 1. cache = what this machine actually publishes (type included). */
    unsigned scanned = 0;
    int discovered = 0;
    if (cache && !force_discover) {
        int from_cache = load_cache(cache);
        if (from_cache > 0) {
            discovered = from_cache;
        } else {
            discovered = discover_keys(&scanned);
            if (discovered > 0) save_cache(cache);
        }
    }
    if (discovered <= 0 || force_discover) {
        /* no usable cache: fall back to the static names and probe the index */
        discovered = discover_keys(&scanned);
        if (discovered > 0 && cache) save_cache(cache);
        if (discovered < 0) discovered = 0;
    }
    for (size_t i = 0; i < NAME_COUNT; i++)
        if (!have_key(NAMES[i].key)) add_key(NAMES[i].key, "");

    /* 2. read: --all = every discovered key, fast = named sensors only. */
    if (!split)
        printf("{\"ok\":true,\"mode\":\"%s\",\"source\":\"AppleSMC (root)\"",
               want_all ? "all" : "fast");
    if (!split)
        printf(",\"keys\":%d,\"discovered\":%d,\"scanned\":%u,\"temps\":[",
               g_nkeys, discovered, scanned);
    else
        printf("[");

    char label[64];
    const char *group = "other";
    int emitted = 0;
    double cpu_sum = 0.0, gpu_sum = 0.0, batt_sum = 0.0, ssd_sum = 0.0;
    int cpu_n = 0, gpu_n = 0, batt_n = 0, ssd_n = 0;
    int failures = 0;

    for (int i = 0; i < g_nkeys; i++) {
        if (max_rows > 0 && emitted >= max_rows) break;
        const name_row *row = lookup_name(g_keys[i].key);
        if (!want_all && row == NULL) continue;   /* fast mode: named only */

        smc_val_t v;
        if (smc_read(g_keys[i].key, &v) != 0) { failures++; continue; }
        double c = 0.0;
        if (!decode(&v, &c)) continue;
        if (!is_sane(c)) continue;

        if (row) {
            /* A sourced name is kept, but the SMC key travels with it: the
               name tables are written for a chip generation, and a variant die
               can publish different keys.  The reader can always see which
               key the number came from. */
            if (strcmp(row->group, "cpu") == 0)
                snprintf(label, sizeof label, "%s (%s)", row->label, row->key);
            else
                snprintf(label, sizeof label, "%s", row->label);
            group = row->group;
        } else {
            const char *fam = family_label(v.key, label, sizeof label, &group);
            if (!fam) {
                snprintf(label, sizeof label, "Sensor %s", v.key);
                group = "other";
            }
        }

        if (emitted > 0) printf(",");
        printf("{\"key\":\"%s\",\"label\":\"%s\",\"c\":%.2f,\"type\":\"%s\"}",
               v.key, label, c, v.dataType);
        emitted++;

        if (strcmp(group, "cpu") == 0)          { cpu_sum  += c; cpu_n++;  }
        else if (strcmp(group, "gpu") == 0)     { gpu_sum  += c; gpu_n++;  }
        else if (strcmp(group, "battery") == 0) { batt_sum += c; batt_n++; }
        else if (strcmp(group, "ssd") == 0)     { ssd_sum  += c; ssd_n++;  }
    }
    printf("]");
    if (split) {
        /* line 1: the temps array.  line 2: aggregate keys as a ready-to-
           splice JSON fragment.  line 3: the meta block.  The sampler reads
           these three lines with awk — no JSON parser in the 5 s loop. */
        printf("\n");
        printf("\"cpu_avg_c\":");
        if (cpu_n) printf("%.2f", cpu_sum / cpu_n); else printf("null");
        printf(",\"gpu_avg_c\":");
        if (gpu_n) printf("%.2f", gpu_sum / gpu_n); else printf("null");
        printf(",\"battery_c\":");
        if (batt_n) printf("%.2f", batt_sum / batt_n); else printf("null");
        printf(",\"ssd_c\":");
        if (ssd_n) printf("%.2f", ssd_sum / ssd_n); else printf("null");
        printf("\n");
        printf("\"smc\":{\"ok\":true,\"mode\":\"%s\",\"keys\":%d,"
               "\"discovered\":%d,\"emitted\":%d,\"read_failures\":%d}\n",
               want_all ? "all" : "fast", g_nkeys, discovered, emitted,
               failures);
        fflush(stdout);
        return emitted > 0 ? 0 : 3;
    }
    printf(",\"cpu_avg_c\":");
    if (cpu_n) printf("%.2f", cpu_sum / cpu_n); else printf("null");
    printf(",\"gpu_avg_c\":");
    if (gpu_n) printf("%.2f", gpu_sum / gpu_n); else printf("null");
    printf(",\"battery_c\":");
    if (batt_n) printf("%.2f", batt_sum / batt_n); else printf("null");
    printf(",\"ssd_c\":");
    if (ssd_n) printf("%.2f", ssd_sum / ssd_n); else printf("null");
    printf(",\"emitted\":%d,\"read_failures\":%d}\n", emitted, failures);
    fflush(stdout);
    return emitted > 0 ? 0 : 3;
}
