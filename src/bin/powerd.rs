//! syswatch-powerd — the root power/temperature daemon (T19).
//!
//! One binary replaces the two pieces that used to do this job:
//!
//!   * `packaging/launchd/syswatch_power_sampler.sh` — a POSIX shell loop that
//!     spawned `powermetrics`, piped its text through a 150-line `awk` program
//!     and spliced in a helper's JSON with shell string surgery;
//!   * `packaging/launchd/smc_temp.c` — a separate process, forked twice per
//!     cycle, that read the AppleSMC temperature keys through IOKit.
//!
//! Per cycle the old design cost: 1 shell + 1 `date` + 2 `awk`/`rm`/`mv`
//! forks, one `smc_temp` fork (~10 ms), and one JSON splice done in `awk`
//! text.  This binary costs one `powermetrics` spawn — the sample source
//! itself, which cannot be avoided — and one SMC read on a connection it keeps
//! open for the lifetime of the process.
//!
//! ## Contract (unchanged — this is why the dashboard needs no edit)
//!
//! Every `--interval-ms` (default 15000 — T20: powermetrics is the dominant
//! cost, so one sample every 15 s instead of every 5 s drops its spike from
//! 12/min to 4/min and puts the whole agent at ~1.5–2 % CPU; the cycle still
//! takes as long as the sample does and never busy-waits):
//!
//! 1. `powermetrics -n 1 -i <interval> -f text -s <samplers>` is spawned once.
//!    The sampler list is probed at startup: `smc` is offered first and
//!    dropped when this build rejects it (it does on macOS 15.8.3 / 24H241),
//!    with the rejection logged to `powermetrics.err.log`.
//! 2. The text is parsed in-process into the sample schema the shell emitted:
//!    watts, thermal pressure, per-cluster clocks, per-core residency.
//! 3. The AppleSMC temperature keys are read through IOKit and merged in:
//!    `temps: [{key,label,c,type}]`, `cpu_avg_c`, `gpu_avg_c`, `battery_c`,
//!    `ssd_c`, `smc: {…}`.  A failing SMC read contributes nothing — the keys
//!    are *absent*, never 0 °C, so the UI keeps its honest "unavailable".
//! 4. The merged object is written atomically: `sample.json.tmp` → `fsync` →
//!    `rename`.  A reader therefore never sees a half-written snapshot.
//!
//! The first real sample is also dumped verbatim to `powermetrics.raw.log`
//! (evidence of the exact field names on this macOS build), and the full SMC
//! sweep runs once at startup into `smc_all.json` (the named-sensor read in
//! the loop is the cheap ~10 ms half of the same reader).
//!
//! ## Cost (measured on Mac14,15 / M2, macOS 15.8.3)
//!
//! SMC open + discovery + full sweep: ~75 ms, once.  Named-sensor read: ~10 ms,
//! once per cycle, on a connection held open.  Everything else is text
//! parsing over ~4 KB.  `powermetrics` is, and remains, the dominant cost —
//! which is why the interval is a constant of the contract and is not lowered
//! (T20 raised it to 15 s to cut that cost by two thirds).
//!
//! ## Why the IOKit FFI is hand-written
//!
//! Two extern functions and four structs are all that is needed; the `io-kit-sys`
//! crate pulls a bindgen-style tree for the same 40 lines.  The struct layouts
//! are asserted at compile time below, because a silent mismatch against the
//! kernel's `SMCKeyData_t` would produce garbage temperatures rather than an
//! error.
//!
//! Usage: syswatch-powerd [--interval-ms N] [--out-dir DIR] [--once]
//!                        [--replay FILE] [--powermetrics PATH]

use std::collections::HashSet;
use std::ffi::{c_char, c_void};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ───────────────────────────── AppleSMC via IOKit ─────────────────────────────
//
// Ported from packaging/launchd/smc_temp.c, which was itself a fresh C
// implementation of the documented AppleSMC protocol (IOKit plumbing and the
// M2 key inventory sourced from smctemp, MIT, and exelban/stats, MIT).
// The behaviour is deliberate and kept identical:
//
//   * probe, do not assume — enumerate the SMC's own key index ("#KEY" + one
//     read per index) and keep the keys this machine actually publishes, then
//     cache that list (enumerating ~1800 keys costs ~0.4 s; the loop is 5 s);
//   * only sane readings are emitted (−20…120 °C) — a key that fails, is a
//     type we cannot decode, or decodes out of range is *absent*;
//   * a key with no name in the table is labelled "Sensor <key>" — honest
//     naming beats a plausible invention.
mod smc {
    use super::*;

    const SMC_CMD_READ_BYTES: i8 = 5;
    const SMC_CMD_READ_INDEX: i8 = 8;
    const SMC_CMD_READ_KEYINFO: i8 = 9;
    const SMC_KERNEL_INDEX: u32 = 2;

    const TEMP_MIN: f64 = -20.0;
    const TEMP_MAX: f64 = 120.0;

    const MAX_KEYS: u32 = 4096;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Vers {
        major: c_char,
        minor: c_char,
        build: c_char,
        reserved: c_char,
        release: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PLimit {
        version: u16,
        length: u16,
        cpu_p_limit: u32,
        gpu_p_limit: u32,
        mem_p_limit: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeyInfo {
        data_size: u32,
        data_type: u32,
        data_attributes: c_char,
    }

    /// Mirrors `SMCKeyData_t` in the kernel and in smc_temp.c.  `size_of` is
    /// asserted below: 80 bytes on every macOS SDK since 10.6.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeyData {
        key: u32,
        vers: Vers,
        p_limit_data: PLimit,
        key_info: KeyInfo,
        result: c_char,
        status: c_char,
        data8: c_char,
        data32: u32,
        bytes: [u8; 32],
    }

    const _: () = assert!(std::mem::size_of::<KeyData>() == 80);
    const _: () = assert!(std::mem::size_of::<KeyInfo>() == 12);
    const _: () = assert!(std::mem::size_of::<PLimit>() == 16);
    const _: () = assert!(std::mem::size_of::<Vers>() == 6);

    impl KeyData {
        fn zeroed() -> Self {
            // Every field is a plain integer/array; all-zero is a valid value.
            unsafe { std::mem::zeroed() }
        }
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IOServiceOpen(service: u32, owning_task: u32, kind: u32, connection: *mut u32) -> i32;
        fn IOObjectRelease(object: u32) -> i32;
        fn IOConnectCallStructMethod(
            connection: u32,
            selector: u32,
            input_struct: *const c_void,
            input_struct_cnt: usize,
            output_struct: *mut c_void,
            output_struct_cnt: *mut usize,
        ) -> i32;
        /// `mach_task_self()` is a macro over this global on macOS.
        static mach_task_self_: u32;
    }

    /// One decoded SMC key read.  `ty` is the raw 4-byte data type, kept
    /// verbatim (trailing space and all) because that is what the old helper
    /// printed and what the evidence file shows.
    pub struct Val {
        pub key: String,
        pub size: u32,
        pub ty: String,
        pub bytes: [u8; 32],
    }

    pub struct Smc {
        conn: u32,
    }

    impl Smc {
        pub fn open() -> Result<Smc, &'static str> {
            unsafe {
                let matching = IOServiceMatching(c"AppleSMC".as_ptr());
                if matching.is_null() {
                    return Err("IOServiceMatching(AppleSMC) returned null");
                }
                // kIOMainPortDefault == 0.
                let service = IOServiceGetMatchingService(0, matching);
                if service == 0 {
                    return Err("AppleSMC service not found");
                }
                let mut conn: u32 = 0;
                let kr = IOServiceOpen(service, mach_task_self_, 0, &mut conn);
                IOObjectRelease(service);
                if kr != 0 {
                    return Err("IOServiceOpen(AppleSMC) failed");
                }
                Ok(Smc { conn })
            }
        }

        fn call(&self, index: u32, input: &KeyData) -> Option<KeyData> {
            let mut out = KeyData::zeroed();
            let mut out_size = std::mem::size_of::<KeyData>();
            let kr = unsafe {
                IOConnectCallStructMethod(
                    self.conn,
                    index,
                    input as *const KeyData as *const c_void,
                    std::mem::size_of::<KeyData>(),
                    &mut out as *mut KeyData as *mut c_void,
                    &mut out_size,
                )
            };
            (kr == 0).then_some(out)
        }

        fn key_info(&self, key: &str) -> Option<KeyInfo> {
            let mut input = KeyData::zeroed();
            input.key = key_code(key);
            input.data8 = SMC_CMD_READ_KEYINFO;
            self.call(SMC_KERNEL_INDEX, &input).map(|o| o.key_info)
        }

        pub fn read(&self, key: &str) -> Option<Val> {
            let info = self.key_info(key)?;
            if info.data_size == 0 || info.data_size > 32 {
                return None;
            }
            let mut input = KeyData::zeroed();
            input.key = key_code(key);
            input.key_info.data_size = info.data_size;
            input.data8 = SMC_CMD_READ_BYTES;
            let out = self.call(SMC_KERNEL_INDEX, &input)?;
            Some(Val {
                key: key.to_string(),
                size: info.data_size,
                ty: type_string(info.data_type),
                bytes: out.bytes,
            })
        }

        /// The SMC publishes its key count at "#KEY".
        fn key_count(&self) -> u32 {
            let Some(v) = self.read("#KEY") else { return 0 };
            let mut n: u32 = 0;
            for i in 0..(v.size as usize).min(4) {
                n = (n << 8) | v.bytes[i] as u32;
            }
            if n == 0 || n > MAX_KEYS { 0 } else { n }
        }

        fn key_at(&self, index: u32) -> Option<String> {
            let mut input = KeyData::zeroed();
            input.data8 = SMC_CMD_READ_INDEX;
            input.data32 = index;
            let out = self.call(SMC_KERNEL_INDEX, &input)?;
            let k = out.key;
            Some(String::from_utf8_lossy(&[
                (k >> 24) as u8,
                (k >> 16) as u8,
                (k >> 8) as u8,
                k as u8,
            ])
            .into_owned())
        }

        /// Enumerate the SMC key index and keep the keys this machine
        /// publishes: temperatures live in `T*`, and a key is kept when it has
        /// a sourced name or its second character is alphabetic (the family
        /// prefixes `family_label` understands).
        ///
        /// Returns the keys in the order the SMC publishes them, plus the
        /// number of index slots scanned (0 when a cache was used).
        pub fn discover(&self, scanned: &mut u32) -> Vec<String> {
            let count = self.key_count();
            *scanned = count;
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            if count == 0 {
                return out;
            }
            for i in 0..count {
                let Some(k) = self.key_at(i) else { continue };
                let b = k.as_bytes();
                if b.len() != 4 || b[0] != b'T' {
                    continue;
                }
                if lookup_name(&k).is_none() && !b[1].is_ascii_alphabetic() {
                    continue;
                }
                if seen.insert(k.clone()) {
                    out.push(k);
                }
            }
            out
        }
    }

    fn key_code(key: &str) -> u32 {
        let b = key.as_bytes();
        ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | b[3] as u32
    }

    fn type_string(t: u32) -> String {
        let bytes = [(t >> 24) as u8, (t >> 16) as u8, (t >> 8) as u8, t as u8];
        bytes.iter().map(|&b| b as char).collect()
    }

    // ── key → name table ───────────────────────────────────────────────────
    // Sourced from exelban/stats values.swift (M2 generation block) plus the
    // generic key names, and smctemp's ARM64 key list.  `group` drives the
    // per-category averages (cpu / gpu / battery / ssd).
    const NAMES: &[(&str, &str, &str)] = &[
        // Apple M2 (this machine: Mac14,15) — CPU
        ("Tp1h", "CPU efficiency core 1", "cpu"),
        ("Tp1t", "CPU efficiency core 2", "cpu"),
        ("Tp1p", "CPU efficiency core 3", "cpu"),
        ("Tp1l", "CPU efficiency core 4", "cpu"),
        ("Tp01", "CPU performance core 1", "cpu"),
        ("Tp05", "CPU performance core 2", "cpu"),
        ("Tp09", "CPU performance core 3", "cpu"),
        ("Tp0D", "CPU performance core 4", "cpu"),
        ("Tp0X", "CPU performance core 5", "cpu"),
        ("Tp0b", "CPU performance core 6", "cpu"),
        ("Tp0f", "CPU performance core 7", "cpu"),
        ("Tp0j", "CPU performance core 8", "cpu"),
        ("Tp0n", "CPU performance core 9", "cpu"),
        ("Tp0r", "CPU performance core 10", "cpu"),
        // Apple M2 — GPU
        ("Tg0f", "GPU 1", "gpu"),
        ("Tg0j", "GPU 2", "gpu"),
        // M1 (the same reader ships to M1 machines)
        ("Tp0T", "CPU efficiency core 2 (M1)", "cpu"),
        ("Tp0H", "CPU performance core 4 (M1)", "cpu"),
        ("Tp0L", "CPU performance core 5 (M1)", "cpu"),
        ("Tp0P", "CPU performance core 6 (M1)", "cpu"),
        ("Tp0h", "CPU performance core 7 (M1)", "cpu"),
        ("Tg05", "GPU 1 (M1)", "gpu"),
        ("Tg0D", "GPU 2 (M1)", "gpu"),
        ("Tg0L", "GPU 3 (M1)", "gpu"),
        ("Tg0T", "GPU 4 (M1)", "gpu"),
        // CPU die auxiliary sensors (smctemp ARM64 list)
        ("Tc0a", "CPU die aux Tc0a", "cpu"),
        ("Tc0b", "CPU die aux Tc0b", "cpu"),
        ("Tc0x", "CPU die aux Tc0x", "cpu"),
        ("Tc0z", "CPU die aux Tc0z", "cpu"),
        // Memory (stats: Tm* on Apple Silicon)
        ("Tm02", "Memory 1", "other"),
        ("Tm06", "Memory 2", "other"),
        ("Tm08", "Memory 3", "other"),
        ("Tm09", "Memory 4", "other"),
        // Battery pack sensors (Intel names; present on some Apple packs)
        ("TB0T", "Battery 1", "battery"),
        ("TB1T", "Battery 2", "battery"),
        ("TB2T", "Battery 3", "battery"),
        // Palm rest / skin
        ("Ts0P", "Palm rest", "other"),
        ("Ts1P", "Palm rest 2", "other"),
        // SSD / NAND
        ("TH0x", "SSD", "ssd"),
        ("TH0A", "SSD 2", "ssd"),
        ("TH0B", "SSD 3", "ssd"),
        ("TH0T", "SSD 4", "ssd"),
    ];

    fn lookup_name(key: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
        NAMES.iter().find(|r| r.0 == key)
    }

    /// Fallback naming by key family — only unambiguous Apple Silicon prefixes.
    fn family_label(key: &str) -> Option<(String, &'static str)> {
        let b = key.as_bytes();
        if b.len() != 4 || b[0] != b'T' {
            return None;
        }
        let (prefix, group) = match b[1] {
            b'p' => ("CPU core", "cpu"),
            b'g' => ("GPU", "gpu"),
            b'c' => ("CPU die aux", "cpu"),
            b'm' => ("Memory", "other"),
            b'B' => ("Battery", "battery"),
            b's' => ("Palm rest", "other"),
            b'H' => ("SSD", "ssd"),
            _ => return None,
        };
        Some((format!("{} {}", prefix, key), group))
    }

    // ── data-type decoding (ported verbatim from smc_temp.c) ───────────────
    //
    // An unknown type ("ioft", "hex_", …) is NOT 0 °C — it is "no reading".
    pub fn decode(v: &Val) -> Option<f64> {
        let b = &v.bytes;
        let n = v.size;
        let s16 = || -> i16 { i16::from_be_bytes([b[0], b[1]]) };
        let u16v = || -> u16 { u16::from_be_bytes([b[0], b[1]]) };
        let need2 = |n: u32| n >= 2;
        let val = match v.ty.as_str() {
            "flt " if n >= 4 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            "sp78" if need2(n) => s16() as f64 / 256.0,
            "sp87" if need2(n) => s16() as f64 / 128.0,
            "sp96" if need2(n) => s16() as f64 / 64.0,
            "spb4" if need2(n) => s16() as f64 / 16.0,
            "spf0" if need2(n) => s16() as f64,
            "fpe2" if need2(n) => u16v() as f64 / 4.0,
            "fp88" if need2(n) => u16v() as f64 / 256.0,
            "fp4c" if need2(n) => u16v() as f64 / 4096.0,
            "fp5b" if need2(n) => u16v() as f64 / 2048.0,
            "fp6a" if need2(n) => u16v() as f64 / 1024.0,
            "fp79" if need2(n) => u16v() as f64 / 512.0,
            "fp1f" if need2(n) => u16v() as f64 / 32768.0,
            "fpa6" if need2(n) => u16v() as f64 / 64.0,
            "fpc4" if need2(n) => u16v() as f64 / 16.0,
            "ui8 " if n >= 1 => b[0] as f64,
            "ui16" if need2(n) => u16v() as f64,
            "ui32" if n >= 4 => u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
            "si8 " if n >= 1 => b[0] as i8 as f64,
            "si16" if need2(n) => s16() as f64,
            _ => return None,
        };
        Some(val)
    }

    pub fn is_sane(c: f64) -> bool {
        c > TEMP_MIN && c < TEMP_MAX
    }

    // ── key cache ──────────────────────────────────────────────────────────
    // Same file the C helper wrote and read: `<out_dir>/smc_keys.txt`.  Kept
    // because it is the evidence of what this machine publishes, and because
    // an upgrade must not pay for a 0.4 s enumeration on every restart.
    pub fn load_cache(path: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let Ok(text) = fs::read_to_string(path) else {
            return out;
        };
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let Some(tok) = line.split_whitespace().next() else {
                continue;
            };
            if tok.len() == 4 && seen.insert(tok.to_string()) {
                out.push(tok.to_string());
            }
        }
        out
    }

    pub fn save_cache(path: &Path, keys: &[String]) {
        let tmp = with_suffix(path, ".tmp");
        {
            let Ok(mut f) = fs::File::create(&tmp) else { return };
            let _ = writeln!(
                f,
                "# syswatch-powerd SMC key cache — {} decodable temperature keys",
                keys.len()
            );
            let _ = writeln!(f, "# regenerated by syswatch-powerd on startup");
            for k in keys {
                let _ = writeln!(f, "{}", k);
            }
            let _ = f.sync_all();
        }
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644));
        let _ = fs::rename(&tmp, path);
    }

    /// Everything the loop needs from the SMC, resolved once at startup.
    pub struct Reader {
        pub smc: Smc,
        keys: Vec<String>,
        pub discovered: usize,
        pub scanned: u32,
    }

    impl Reader {
        /// Open the SMC, resolve the key list (cache first, discovery as the
        /// fallback) and append the sourced names that this machine did not
        /// publish — the C helper did the same, so `keys` counted 170 on a
        /// machine that discovered 145.
        pub fn new(cache_path: &Path) -> Result<Reader, String> {
            let smc = Smc::open().map_err(|e| e.to_string())?;
            let mut scanned = 0u32;
            let cached = load_cache(cache_path);
            let (mut keys, cache_used) = if cached.is_empty() {
                (smc.discover(&mut scanned), false)
            } else {
                (cached, true)
            };
            let discovered = keys.len();
            // A fresh discovery is worth caching; a loaded cache is already
            // current by definition.
            if !cache_used && !keys.is_empty() {
                save_cache(cache_path, &keys);
            }
            let mut seen: HashSet<String> = keys.iter().cloned().collect();
            for row in NAMES {
                if seen.insert(row.0.to_string()) {
                    keys.push(row.0.to_string());
                }
            }
            Ok(Reader {
                smc,
                keys,
                discovered,
                scanned,
            })
        }

        pub fn key_count(&self) -> usize {
            self.keys.len()
        }

        /// Read one key into a temperature row.
        ///
        /// Three outcomes, and the distinction matters for the `smc` block's
        /// `read_failures` counter: only an IOKit read failure is a *failure*.
        /// A key whose type we cannot decode, or which decodes outside
        /// −20…120 °C, is silently skipped — exactly as the C helper did
        /// (`failures++` happened before `decode()`).
        fn temp_row(&self, key: &str) -> Outcome {
            let Some(v) = self.smc.read(key) else {
                return Outcome::ReadFailed;
            };
            let Some(c) = decode(&v) else {
                return Outcome::Rejected;
            };
            if !is_sane(c) {
                return Outcome::Rejected;
            }
            let (label, group) = match lookup_name(&v.key) {
                Some(row) => {
                    // A sourced name is kept, but the SMC key travels with it:
                    // name tables are written for a chip generation and a
                    // variant die can publish different keys.  The reader can
                    // always see which key the number came from.  (Only CPU
                    // rows carry the suffix, exactly as the C helper did.)
                    let label = if row.2 == "cpu" {
                        format!("{} ({})", row.1, v.key)
                    } else {
                        row.1.to_string()
                    };
                    (label, row.2)
                }
                None => match family_label(&v.key) {
                    Some((l, g)) => (l, g),
                    None => (format!("Sensor {}", v.key), "other"),
                },
            };
            Outcome::Row(Row {
                key: v.key,
                label,
                c,
                ty: v.ty,
                group,
            })
        }

        /// The 5 s cycle: named sensors only (~10 ms, 15–17 rows).
        pub fn read_named(&self) -> Sweep {
            let mut sweep = Sweep::default();
            for key in &self.keys {
                if lookup_name(key).is_none() {
                    continue;
                }
                sweep.record(self.temp_row(key));
            }
            sweep
        }

        /// The startup evidence sweep: every key in the list (~75 ms).
        pub fn read_all(&self) -> Sweep {
            let mut sweep = Sweep::default();
            for key in &self.keys {
                sweep.record(self.temp_row(key));
            }
            sweep
        }
    }

    /// What one key read produced.
    pub enum Outcome {
        Row(Row),
        /// IOKit refused the read — this is what `read_failures` counts.
        ReadFailed,
        /// Read fine, but undecodable or outside the sane temperature band.
        Rejected,
    }

    pub struct Row {
        pub key: String,
        pub label: String,
        pub c: f64,
        pub ty: String,
        pub group: &'static str,
    }

    #[derive(Default)]
    pub struct Sweep {
        pub rows: Vec<Row>,
        pub failures: usize,
        cpu: (f64, usize),
        gpu: (f64, usize),
        battery: (f64, usize),
        ssd: (f64, usize),
    }

    impl Sweep {
        fn record(&mut self, outcome: Outcome) {
            match outcome {
                Outcome::Row(row) => self.push(row),
                Outcome::ReadFailed => self.failures += 1,
                Outcome::Rejected => {}
            }
        }

        fn push(&mut self, row: Row) {
            match row.group {
                "cpu" => {
                    self.cpu.0 += row.c;
                    self.cpu.1 += 1;
                }
                "gpu" => {
                    self.gpu.0 += row.c;
                    self.gpu.1 += 1;
                }
                "battery" => {
                    self.battery.0 += row.c;
                    self.battery.1 += 1;
                }
                "ssd" => {
                    self.ssd.0 += row.c;
                    self.ssd.1 += 1;
                }
                _ => {}
            }
            self.rows.push(row);
        }

        pub fn emitted(&self) -> usize {
            self.rows.len()
        }

        pub fn avg_cpu(&self) -> Option<f64> {
            mean(self.cpu)
        }
        pub fn avg_gpu(&self) -> Option<f64> {
            mean(self.gpu)
        }
        pub fn avg_battery(&self) -> Option<f64> {
            mean(self.battery)
        }
        pub fn avg_ssd(&self) -> Option<f64> {
            mean(self.ssd)
        }
    }

    fn mean((sum, n): (f64, usize)) -> Option<f64> {
        (n > 0).then(|| sum / n as f64)
    }
}

// ──────────────────────────────── powermetrics ────────────────────────────────
//
// The text parser is a port of the `awk` program that used to live inside
// syswatch_power_sampler.sh — same line tests, same field names, same
// "absent rather than zero" rule for anything the build does not print.

const DEFAULT_SOURCE: &str = "powermetrics (root LaunchDaemon com.barbarian.syswatch.power)";

#[derive(Default)]
struct PmSample {
    cpu_w: Option<f64>,
    gpu_w: Option<f64>,
    ane_w: Option<f64>,
    package_w: Option<f64>,
    package_w_source: Option<String>,
    cpu_temp_c: Option<f64>,
    gpu_temp_c: Option<f64>,
    cluster_temps: Vec<(String, f64)>,
    thermal_pressure: Option<String>,
    fans_rpm: Vec<u64>,
    ane_util_pct: Option<f64>,
    e_cluster: (Option<f64>, Option<f64>),
    p_cluster: (Option<f64>, Option<f64>),
    gpu_freq_mhz: Option<f64>,
    gpu_residency_pct: Option<f64>,
    core_freq: Vec<Option<f64>>,
    core_res: Vec<Option<f64>>,
}

impl PmSample {
    fn push_core_freq(&mut self, v: Option<f64>) {
        self.core_freq.push(v);
        self.core_res.push(None);
    }

    fn set_core_res(&mut self, v: Option<f64>) {
        if self.core_res.is_empty() {
            self.core_freq.push(None);
            self.core_res.push(v);
        } else {
            let last = self.core_res.len() - 1;
            self.core_res[last] = v;
        }
    }
}

fn parse_powermetrics(text: &str) -> PmSample {
    let mut s = PmSample::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Some((label, val)) = split_label_value(line) else {
            continue;
        };

        match label {
            "E-Cluster HW active frequency" => {
                s.e_cluster.0 = first_num(val);
                continue;
            }
            "E-Cluster HW active residency" => {
                s.e_cluster.1 = first_num(val);
                continue;
            }
            "P-Cluster HW active frequency" => {
                s.p_cluster.0 = first_num(val);
                continue;
            }
            "P-Cluster HW active residency" => {
                s.p_cluster.1 = first_num(val);
                continue;
            }
            "GPU HW active frequency" => {
                s.gpu_freq_mhz = first_num(val);
                continue;
            }
            "GPU HW active residency" => {
                s.gpu_residency_pct = first_num(val);
                continue;
            }
            "CPU Power" => {
                s.cpu_w = watts(val);
                continue;
            }
            "GPU Power" => {
                s.gpu_w = watts(val);
                continue;
            }
            "ANE Power" => {
                s.ane_w = watts(val);
                continue;
            }
            "Package Power" => {
                s.package_w = watts(val);
                s.package_w_source = Some(label.to_string());
                continue;
            }
            "Current pressure level" => {
                s.thermal_pressure = Some(val.to_string());
                continue;
            }
            _ => {}
        }

        if label.starts_with("Combined Power") {
            // Only fills in when the build printed no "Package Power" line,
            // which is the case on Apple Silicon.
            if s.package_w_source.as_deref() != Some("Package Power") {
                s.package_w = watts(val);
                s.package_w_source = Some(label.to_string());
            }
            continue;
        }

        if label.contains("die temperature") {
            let v = strip_num(val);
            if label.starts_with("CPU die") {
                s.cpu_temp_c = Some(v);
            } else if label.starts_with("GPU die") {
                s.gpu_temp_c = Some(v);
            } else {
                s.cluster_temps.push((label.to_string(), v));
            }
            continue;
        }

        if cpu_core_field(label, "frequency").is_some() {
            s.push_core_freq(first_num(val));
            continue;
        }
        if cpu_core_field(label, "active residency").is_some() {
            s.set_core_res(first_num(val));
            continue;
        }

        if (line.contains("fan") || line.contains("Fan")) && line.contains("rpm") {
            s.fans_rpm.push(strip_num(val) as u64);
            continue;
        }

        if label.contains("ANE") && label.contains("tiliz") {
            s.ane_util_pct = Some(strip_num(val));
        }
    }
    s
}

/// `CPU <n> frequency` → Some(n).  The counter the old awk kept is the
/// *position* in the output, so the number is only used as a shape check.
fn cpu_core_field(label: &str, suffix: &str) -> Option<usize> {
    let rest = label.strip_prefix("CPU ")?;
    let mid = rest.strip_suffix(suffix)?.trim();
    if mid.is_empty() || !mid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    mid.parse().ok()
}

fn split_label_value(line: &str) -> Option<(&str, &str)> {
    let i = line.find(':')?;
    Some((line[..i].trim(), line[i + 1..].trim()))
}

/// First number in `s`, matching awk's `-?[0-9]+(\.[0-9]+)?`.
fn first_num(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'-' || c.is_ascii_digit() {
            let start = i;
            if c == b'-' {
                i += 1;
            }
            let mut digits = false;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
                digits = true;
            }
            if !digits {
                i = start + 1;
                continue;
            }
            if i < b.len() && b[i] == b'.' {
                let mut j = i + 1;
                let mut frac = false;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                    frac = true;
                }
                if frac {
                    i = j;
                }
            }
            return s[start..i].parse::<f64>().ok();
        }
        i += 1;
    }
    None
}

/// `286 mW` → 0.286, `1.42 W` → 1.42, anything else → None.
fn watts(val: &str) -> Option<f64> {
    let n = first_num(val)?;
    if val.contains("mW") {
        Some(n / 1000.0)
    } else if val.contains('W') {
        Some(n)
    } else {
        None
    }
}

/// awk's `number()`: drop everything that is not `0-9 . -`, then add 0.
fn strip_num(s: &str) -> f64 {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    cleaned.parse::<f64>().unwrap_or(0.0)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

// ──────────────────────────────── the daemon ─────────────────────────────────

struct Opts {
    interval_ms: u64,
    out_dir: PathBuf,
    once: bool,
    replay: Option<PathBuf>,
    powermetrics: PathBuf,
}

/// powermetrics' own sample rate is the cycle time: a cycle lasts as long as
/// the sample does.  This floor exists only to make "lower the interval"
/// impossible to pass on the command line — the contract is 15 s (T20).
const MIN_INTERVAL_MS: u64 = 1000;

/// T20: the shipped cadence.  powermetrics' spike is the whole cost of this
/// daemon, so one sample every 15 s instead of every 5 s cuts it to a third
/// (measured target: the agent total lands at ~1.5–2 % CPU).
const DEFAULT_INTERVAL_MS: u64 = 15000;

fn usage() -> &'static str {
    "syswatch-powerd — root power/temperature daemon (T19)

usage: syswatch-powerd [options]

  --interval-ms N     sample interval in milliseconds (default 15000, min 1000)
  --out-dir DIR       where sample.json / logs are written
                      (default ~/Library/Application Support/syswatch/root)
  --once              run exactly one cycle, then exit (debug/verification)
  --replay FILE       parse FILE instead of spawning powermetrics
                      (verification only: replays a captured raw dump)
  --powermetrics PATH powermetrics binary (default /usr/bin/powermetrics)
  -h, --help          this text
  -V, --version       version
"
}

fn parse_args() -> Result<Opts, String> {
    let mut interval_ms = DEFAULT_INTERVAL_MS;
    // Resolved lazily: under launchd (UserName root) HOME usually exists, but a
    // daemon must not refuse to start over a default it never needs once
    // --out-dir is given.
    let mut out_dir: Option<PathBuf> = None;
    let mut once = false;
    let mut replay = None;
    let mut powermetrics = PathBuf::from("/usr/bin/powermetrics");

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--interval-ms" => {
                let v = it.next().ok_or("--interval-ms needs a value")?;
                interval_ms = v.parse().map_err(|_| format!("bad --interval-ms: {v}"))?;
            }
            "--out-dir" => {
                out_dir = Some(PathBuf::from(it.next().ok_or("--out-dir needs a value")?));
            }
            "--once" => once = true,
            "--replay" => {
                replay = Some(PathBuf::from(it.next().ok_or("--replay needs a value")?));
            }
            "--powermetrics" => {
                powermetrics = PathBuf::from(it.next().ok_or("--powermetrics needs a value")?);
            }
            "-h" | "--help" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("syswatch-powerd {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if interval_ms < MIN_INTERVAL_MS {
        return Err(format!(
            "--interval-ms {interval_ms} is below the {MIN_INTERVAL_MS} ms floor — \
             the 15 s contract is what keeps this daemon under 0.3 % CPU"
        ));
    }
    let out_dir = match out_dir {
        Some(p) => p,
        None => default_out_dir()?,
    };
    Ok(Opts {
        interval_ms,
        out_dir,
        once,
        replay,
        powermetrics,
    })
}

fn default_out_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join("Library/Application Support/syswatch/root"))
}

struct Daemon {
    opts: Opts,
    smc: Option<smc::Reader>,
    smc_state: &'static str,
    samplers: String,
    err_log: PathBuf,
}

impl Daemon {
    fn log(&self, msg: &str) {
        append_log(&self.err_log, msg);
    }

    /// Probe the sampler list once: `smc` is offered first and dropped when
    /// this build rejects it.  Same probe, same log line, as the shell script.
    fn probe_samplers(&mut self) {
        self.samplers = "thermal,smc,gpu_power,cpu_power,ane_power".to_string();
        if self.opts.replay.is_some() {
            // A replay carries a fixed sampler set in its own header; no probe.
            self.samplers = "thermal,gpu_power,cpu_power,ane_power".to_string();
            return;
        }
        let ok = Command::new(&self.opts.powermetrics)
            .args(["-n", "1", "-i", "100", "-f", "text", "-s", &self.samplers])
            .stdout(Stdio::null())
            .stderr(self.append_stderr())
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        if !ok {
            self.samplers = "thermal,gpu_power,cpu_power,ane_power".to_string();
            let msg = format!(
                "sampler set with smc rejected on this build; using {}",
                self.samplers
            );
            self.log(&msg);
        }
    }

    /// Open the SMC and resolve the key list.  A failure only removes `temps`
    /// (and the temperature-derived keys) — everything powermetrics supplies
    /// still gets written.
    fn open_smc(&mut self) {
        let cache = self.opts.out_dir.join("smc_keys.txt");
        match smc::Reader::new(&cache) {
            Ok(reader) => {
                self.smc_state = "ok";
                // Evidence sweep, once: every temperature key this machine
                // publishes.  ~75 ms at startup, never repeated.
                let sweep = reader.read_all();
                let body = render_smc_object(&sweep, "all", &reader, true);
                let path = self.opts.out_dir.join("smc_all.json");
                let _ = write_atomic(&path, body.as_bytes());
                self.smc = Some(reader);
            }
            Err(e) => {
                self.smc_state = "failed";
                let msg = format!("AppleSMC reader unavailable ({e}); temps omitted");
                self.log(&msg);
            }
        }
    }

    fn append_stderr(&self) -> Stdio {
        match fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.err_log)
        {
            Ok(f) => Stdio::from(f),
            Err(_) => Stdio::null(),
        }
    }

    /// One cycle: sample, parse, merge, write atomically.
    ///
    /// Returns the number of bytes written, or None when the sample source
    /// produced nothing (in which case nothing is written at all — the old
    /// snapshot keeps its mtime and the dashboard's 30 s staleness rule
    /// decides, exactly as before).
    fn cycle(&self, first: bool) -> Option<usize> {
        let text = self.sample_text()?;
        if text.trim().is_empty() {
            return None;
        }
        let ts = unix_now();

        if first {
            let raw = self.opts.out_dir.join("powermetrics.raw.log");
            let mut body = format!(
                "# first real powermetrics sample on this build — {}Z\n",
                utc_stamp_inner()
            );
            body.push_str(&format!(
                "# flags: -n 1 -i {} -f text -s {}\n",
                self.opts.interval_ms, self.samplers
            ));
            body.push_str(&text);
            let _ = write_atomic(&raw, body.as_bytes());
            let msg = format!(
                "first sample captured; samplers={} smc={}",
                self.samplers, self.smc_state
            );
            self.log(&msg);
        }

        let pm = parse_powermetrics(&text);
        let mut body = render_sample(&pm, ts, self.opts.interval_ms, &self.samplers);

        if let Some(reader) = &self.smc {
            let sweep = reader.read_named();
            if sweep.emitted() > 0 {
                body = merge_smc(body, &sweep, reader);
            } else {
                // A failed read must not leave a previous cycle's numbers
                // lying around: nothing is merged, so nothing is claimed.
                let msg = format!(
                    "AppleSMC returned no sane reading this cycle ({} read failures); temps omitted",
                    sweep.failures
                );
                self.log(&msg);
            }
        }

        let path = self.opts.out_dir.join("sample.json");
        match write_atomic(&path, body.as_bytes()) {
            Ok(()) => Some(body.len()),
            Err(e) => {
                let msg = format!("could not write {}: {e}", path.display());
                self.log(&msg);
                None
            }
        }
    }

    fn sample_text(&self) -> Option<String> {
        if let Some(file) = &self.opts.replay {
            return fs::read_to_string(file).ok();
        }
        let out = Command::new(&self.opts.powermetrics)
            .args([
                "-n",
                "1",
                "-i",
                &self.opts.interval_ms.to_string(),
                "-f",
                "text",
                "-s",
                &self.samplers,
            ])
            .stdout(Stdio::piped())
            .stderr(self.append_stderr())
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// The sample object, field for field what the shell's awk printed.
fn render_sample(pm: &PmSample, ts: u64, interval_ms: u64, samplers: &str) -> String {
    let mut o = Obj::new();
    o.kv_raw("ts", &ts.to_string());
    o.kv_raw("sample_ms", &interval_ms.to_string());
    o.kv_str("samplers", samplers);
    o.kv_str("source", DEFAULT_SOURCE);

    if let Some(v) = pm.cpu_w {
        o.kv_raw("cpu_w", &fmt(round3(v)));
    }
    if let Some(v) = pm.gpu_w {
        o.kv_raw("gpu_w", &fmt(round3(v)));
    }
    if let Some(v) = pm.ane_w {
        o.kv_raw("ane_w", &fmt(round3(v)));
    }
    if let Some(v) = pm.package_w {
        o.kv_raw("package_w", &fmt(round3(v)));
        if let Some(src) = &pm.package_w_source {
            o.kv_str("package_w_source", src);
        }
    }
    if let Some(v) = pm.cpu_temp_c {
        o.kv_raw("cpu_temp_c", &fmt(round2(v)));
    }
    if let Some(v) = pm.gpu_temp_c {
        o.kv_raw("gpu_temp_c", &fmt(round2(v)));
    }
    if !pm.cluster_temps.is_empty() {
        let rows: Vec<String> = pm
            .cluster_temps
            .iter()
            .map(|(label, c)| {
                let mut r = Obj::new();
                r.kv_str("label", label);
                r.kv_raw("c", &fmt(round2(*c)));
                r.close()
            })
            .collect();
        o.frag(&format!("\"cpu_cluster_temps_c\":[{}]", rows.join(",")));
    }
    if let Some(p) = &pm.thermal_pressure {
        o.kv_str("thermal_pressure", p);
    }
    if !pm.fans_rpm.is_empty() {
        let fans: Vec<String> = pm.fans_rpm.iter().map(|f| f.to_string()).collect();
        o.frag(&format!("\"fans_rpm\":[{}]", fans.join(",")));
    }
    if let Some(v) = pm.ane_util_pct {
        o.kv_raw("ane_util_pct", &fmt(round2(v)));
    }

    if let Some(text) = render_cluster("e_cluster", pm.e_cluster) {
        o.frag(&text);
    }
    if let Some(text) = render_cluster("p_cluster", pm.p_cluster) {
        o.frag(&text);
    }

    if let Some(v) = pm.gpu_freq_mhz {
        o.kv_raw("gpu_freq_mhz", &fmt_int(v));
    }
    if let Some(v) = pm.gpu_residency_pct {
        o.kv_raw("gpu_residency_pct", &fmt(round2(v)));
    }
    if !pm.core_freq.is_empty() {
        let mut rows = Vec::with_capacity(pm.core_freq.len());
        for i in 0..pm.core_freq.len() {
            let mut r = Obj::new();
            if let Some(f) = pm.core_freq[i] {
                r.kv_raw("freq_mhz", &fmt_int(f));
            }
            if let Some(res) = pm.core_res[i] {
                r.kv_raw("residency_pct", &fmt(round2(res)));
            }
            rows.push(r.close());
        }
        o.frag(&format!("\"per_cpu\":[{}]", rows.join(",")));
    }
    o.close()
}

/// `{"freq_mhz":…,"residency_pct":…}` as `"name":{…}`, or None when the build
/// printed neither line (the shell emitted no key at all in that case).
fn render_cluster(name: &str, (freq, res): (Option<f64>, Option<f64>)) -> Option<String> {
    if freq.is_none() && res.is_none() {
        return None;
    }
    let mut o = Obj::new();
    if let Some(f) = freq {
        o.kv_raw("freq_mhz", &fmt_int(f));
    }
    if let Some(r) = res {
        o.kv_raw("residency_pct", &fmt(round2(r)));
    }
    Some(format!("\"{}\":{}", name, o.close()))
}

/// Merge the SMC half in: `temps`, the four aggregates and the `smc` block.
/// Called only when the sweep emitted at least one row, so a broken reader can
/// never render as 0 °C.
fn merge_smc(body: String, sweep: &smc::Sweep, reader: &smc::Reader) -> String {
    // The powermetrics object is complete and ends with '}': reopen it so the
    // SMC fields land inside the same object.
    let mut buf = body;
    buf.pop();
    let mut o = Obj::resume(buf);

    let rows: Vec<String> = sweep
        .rows
        .iter()
        .map(|row| {
            let mut r = Obj::new();
            r.kv_str("key", &row.key);
            r.kv_str("label", &row.label);
            r.kv_raw("c", &fmt(round2(row.c)));
            r.kv_str("type", &row.ty);
            r.close()
        })
        .collect();
    o.frag(&format!("\"temps\":[{}]", rows.join(",")));

    o.kv_raw("cpu_avg_c", &opt_num(sweep.avg_cpu()));
    o.kv_raw("gpu_avg_c", &opt_num(sweep.avg_gpu()));
    o.kv_raw("battery_c", &opt_num(sweep.avg_battery()));
    o.kv_raw("ssd_c", &opt_num(sweep.avg_ssd()));

    o.frag(&format!(
        "\"smc\":{}",
        render_smc_object(sweep, "fast", reader, false)
    ));
    o.close()
}

fn opt_num(v: Option<f64>) -> String {
    match v {
        Some(x) => fmt(round2(x)),
        None => "null".to_string(),
    }
}

/// The `smc` block.  `full` is the standalone `smc_all.json` evidence dump: it
/// carries the rows and the source string.  The block spliced into
/// `sample.json` keeps exactly the keys the T16B helper's meta line produced
/// (the rows are already in `temps`).
fn render_smc_object(
    sweep: &smc::Sweep,
    mode: &str,
    reader: &smc::Reader,
    full: bool,
) -> String {
    let mut o = Obj::new();
    o.kv_raw("ok", "true");
    o.kv_str("mode", mode);
    if full {
        o.kv_str("source", "AppleSMC (root)");
    }
    o.kv_raw("keys", &reader.key_count().to_string());
    o.kv_raw("discovered", &reader.discovered.to_string());
    if full {
        // The T16B helper printed `scanned` only in its standalone JSON; the
        // block spliced into sample.json never carried it.
        o.kv_raw("scanned", &reader.scanned.to_string());
    }
    if full {
        let rows: Vec<String> = sweep
            .rows
            .iter()
            .map(|row| {
                let mut r = Obj::new();
                r.kv_str("key", &row.key);
                r.kv_str("label", &row.label);
                r.kv_raw("c", &fmt(round2(row.c)));
                r.kv_str("type", &row.ty);
                r.close()
            })
            .collect();
        o.frag(&format!("\"temps\":[{}]", rows.join(",")));
    }
    o.kv_raw("emitted", &sweep.emitted().to_string());
    o.kv_raw("read_failures", &sweep.failures.to_string());
    o.close()
}

// ──────────────────────────────── tiny JSON ─────────────────────────────────
//
// The object is built by hand rather than through `serde_json::Value` because
// every value is a number or a short string: formatting them directly keeps
// the snapshot byte-for-byte close to what the awk splice produced, and keeps
// this binary at ~5 % of the 2 MB budget.
//
// `Obj` owns the comma bookkeeping — a JSON object built by concatenation
// whose separators are maintained at every call site is exactly how the first
// version of this file emitted `{"ts":1"sample_ms":2…`.  One place decides.

struct Obj {
    buf: String,
    /// True while no field has been written yet (so no leading comma).
    empty: bool,
}

impl Obj {
    fn new() -> Obj {
        Obj {
            buf: String::from("{"),
            empty: true,
        }
    }

    /// Continue an object whose opening fields already exist (a body that was
    /// cut back to its last complete field).  The next field gets a comma.
    fn resume(buf: String) -> Obj {
        Obj { buf, empty: false }
    }

    fn sep(&mut self) {
        if self.empty {
            self.empty = false;
        } else {
            self.buf.push(',');
        }
    }

    fn kv_raw(&mut self, key: &str, raw: &str) {
        self.sep();
        self.buf.push('"');
        self.buf.push_str(key);
        self.buf.push_str("\":");
        self.buf.push_str(raw);
    }

    fn kv_str(&mut self, key: &str, value: &str) {
        self.sep();
        self.buf.push('"');
        self.buf.push_str(key);
        self.buf.push_str("\":\"");
        push_escaped(&mut self.buf, value);
        self.buf.push('"');
    }

    /// Append a field that was rendered by another `Obj` (`"name":{…}`).
    fn frag(&mut self, text: &str) {
        self.sep();
        self.buf.push_str(text);
    }

    fn close(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

fn push_escaped(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

/// `%.3f` / `%.2f` without the trailing zeros being dropped in a way that
/// changes the shape: an integer-valued float still prints as a plain integer.
fn fmt(v: f64) -> String {
    if !v.is_finite() {
        return "null".to_string();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v}");
    s
}

fn fmt_int(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    format!("{}", v.round() as i64)
}

// ────────────────────────────────── files ────────────────────────────────────

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// `sample.json.tmp` → fsync → `rename`.  A reader either sees the previous
/// complete snapshot or the new complete one, never a half-written body.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = with_suffix(path, ".tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644));
    fs::rename(&tmp, path)?;
    // Make the rename itself durable (the directory entry, not the data).
    if let Some(dir) = path.parent()
        && let Ok(d) = fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(())
}

fn append_log(path: &Path, msg: &str) {
    let fresh = !path.exists();
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {}", utc_stamp(), msg);
    }
    if fresh {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o644));
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn utc_stamp() -> String {
    utc_stamp_inner()
}

/// ISO-8601 UTC, no dependency on the system timezone: the log is evidence,
/// and a timestamp that is unambiguous without a tz database is worth more
/// than one that reads in local time.
fn utc_stamp_inner() -> String {
    let secs = unix_now();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's civil-from-days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ─────────────────────────────────── main ────────────────────────────────────

fn main() {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("syswatch-powerd: {e}");
            eprintln!("{}", usage());
            std::process::exit(2);
        }
    };
    if let Err(e) = fs::create_dir_all(&opts.out_dir) {
        eprintln!(
            "syswatch-powerd: output dir {} is not writable: {e}",
            opts.out_dir.display()
        );
        std::process::exit(1);
    }
    if opts.replay.is_none() && !opts.powermetrics.exists() {
        eprintln!(
            "syswatch-powerd: {} is missing",
            opts.powermetrics.display()
        );
        std::process::exit(1);
    }

    let err_log = opts.out_dir.join("powermetrics.err.log");
    let mut d = Daemon {
        opts,
        smc: None,
        smc_state: "absent",
        samplers: String::new(),
        err_log,
    };

    d.open_smc();
    d.probe_samplers();

    let mut first = true;
    loop {
        let started = Instant::now();
        let wrote = d.cycle(first);
        if wrote.is_some() {
            first = false;
        }
        if d.opts.once {
            std::process::exit(if wrote.is_some() { 0 } else { 3 });
        }
        if wrote.is_none() {
            // The source produced nothing.  Nothing was written: the previous
            // snapshot keeps its age and the dashboard withholds it.  Same
            // back-off as the shell script.
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        // powermetrics blocks for the whole sample window, so this normally
        // sleeps ~0 ms — it exists so the loop can never spin.
        let elapsed = started.elapsed();
        let interval = Duration::from_millis(d.opts.interval_ms);
        if elapsed < interval {
            std::thread::sleep(interval - elapsed);
        }
    }
}
