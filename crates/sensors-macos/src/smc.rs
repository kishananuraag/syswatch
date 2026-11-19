//! AppleSMC reader over IOKit — hand-written FFI, no bindgen.
//!
//! This is the same reader T19 put inside `syswatch-powerd`, lifted into the
//! sensors crate so `ThermalSensor` can read the die/pack keys in-process.
//! Two extern functions and four structs are all that is needed; the struct
//! layouts are asserted at compile time because a silent mismatch against the
//! kernel's `SMCKeyData_t` produces garbage temperatures rather than an error.
//!
//! Privilege: opening the AppleSMC user client needs root on Apple Silicon.
//! An unprivileged process gets `kIOReturnNotPrivileged` from
//! `IOServiceOpen`, which is reported as `Err` — never as 0 °C.  Callers fall
//! back to the root daemon's `sample.json` in that case.

use std::os::raw::{c_char, c_void};

const SMC_CMD_READ_BYTES: i8 = 5;
const SMC_CMD_READ_KEYINFO: i8 = 9;
const SMC_KERNEL_INDEX: u32 = 2;

/// Anything outside this band is a decoding artefact, not a temperature.
const TEMP_MIN: f64 = -20.0;
const TEMP_MAX: f64 = 120.0;

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

/// Mirrors `SMCKeyData_t`. 80 bytes on every macOS SDK since 10.6.
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
    fn IOServiceClose(connection: u32) -> i32;
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

/// One decoded SMC key read. `ty` is the raw 4-byte data type, kept verbatim
/// (trailing space and all) because that is what the kernel returns.
pub struct Val {
    pub key: String,
    pub size: u32,
    pub ty: String,
    pub bytes: [u8; 32],
}

/// An open AppleSMC user client. Dropping it closes the connection.
pub struct Smc {
    conn: u32,
}

// The connection handle is an integer the kernel owns; the struct holds no
// pointer into process memory, so moving it between threads is sound.
unsafe impl Send for Smc {}

impl Drop for Smc {
    fn drop(&mut self) {
        if self.conn != 0 {
            unsafe {
                IOServiceClose(self.conn);
            }
        }
    }
}

impl Smc {
    pub fn open() -> Result<Smc, String> {
        unsafe {
            let matching = IOServiceMatching(c"AppleSMC".as_ptr());
            if matching.is_null() {
                return Err("IOServiceMatching(AppleSMC) returned null".into());
            }
            // kIOMainPortDefault == 0.
            let service = IOServiceGetMatchingService(0, matching);
            if service == 0 {
                return Err("AppleSMC service not found".into());
            }
            let mut conn: u32 = 0;
            let kr = IOServiceOpen(service, mach_task_self_, 0, &mut conn);
            IOObjectRelease(service);
            if kr != 0 {
                // 0xe00002c1 = kIOReturnNotPrivileged — the usual answer when
                // this process is not root.
                return Err(format!(
                    "IOServiceOpen(AppleSMC) failed (0x{:08x})",
                    kr as u32
                ));
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

    /// Read one key. `None` when the key is absent or the read fails.
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

    /// Read one key and decode it as a temperature in °C. Rejects undecodable
    /// types and out-of-band values — an unknown key is "no reading", not 0 °C.
    pub fn temp(&self, key: &str) -> Option<f64> {
        let val = self.read(key)?;
        let c = decode(&val)?;
        is_sane(c).then_some(c)
    }

    /// Mean of the keys that decode to a sane temperature. `None` when no key
    /// in the set produced a reading.
    pub fn temp_avg(&self, keys: &[&str]) -> Option<f64> {
        let mut sum = 0.0;
        let mut n = 0usize;
        for k in keys {
            if let Some(c) = self.temp(k) {
                sum += c;
                n += 1;
            }
        }
        (n > 0).then(|| sum / n as f64)
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

/// Data-type decoding, ported verbatim from the T19 daemon (itself a port of
/// `smc_temp.c`). An unknown type ("ioft", "hex_", …) is "no reading".
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

// ── key sets ────────────────────────────────────────────────────────────
// The four rows `ThermalSensor` publishes. Each set is read in order and
// averaged: a key a given chip generation does not publish simply does not
// contribute.

/// CPU die. `Tc0*` are the die sensors on Intel; Apple Silicon publishes
/// per-core `Tp*` keys instead, so the same row falls back to the core mean.
pub const CPU_DIE_KEYS: &[&str] = &[
    "Tc0a", "Tc0b", "Tc0c", "Tc0d", "Tc0e", "Tc0f", "Tc0g", "Tc0h", "Tc0j", "Tc0l", "Tc0x", "Tc0z",
    "Tc1a",
];
pub const CPU_CORE_KEYS: &[&str] = &[
    "Tp01", "Tp05", "Tp09", "Tp0D", "Tp0b", "Tp0f", "Tp0j", "Tp0n", "Tp0r", "Tp0T", "Tp0H", "Tp0L",
    "Tp0P", "Tp0h", "Tp1h", "Tp1t", "Tp1p", "Tp1l",
];
pub const GPU_KEYS: &[&str] = &["Tg0f", "Tg0j", "Tg05", "Tg0D", "Tg0L", "Tg0T"];
pub const SSD_KEYS: &[&str] = &["TH0x", "TH0A", "TH0B", "TH0T"];
pub const BATTERY_KEYS: &[&str] = &["TB0T", "TB1T", "TB2T"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_code_is_big_endian_ascii() {
        assert_eq!(key_code("Tp01"), 0x54703031);
        assert_eq!(key_code("TB0T"), 0x54423054);
    }

    #[test]
    fn decodes_flt_temperatures() {
        // "flt " keys are little-endian f32 — the type this machine's SMC
        // returns for every T* key in sample.json.
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&42.5f32.to_le_bytes());
        let v = Val {
            key: "Tp01".into(),
            size: 4,
            ty: "flt ".into(),
            bytes,
        };
        assert!((decode(&v).unwrap() - 42.5).abs() < 1e-6);
    }

    #[test]
    fn decodes_sp78_and_rejects_unknown() {
        // sp78 is signed fixed-point with 8 fraction bits, big-endian.
        let raw = (42.5f64 * 256.0) as i16;
        let mut bytes = [0u8; 32];
        bytes[..2].copy_from_slice(&raw.to_be_bytes());
        let v = Val {
            key: "TB0T".into(),
            size: 2,
            ty: "sp78".into(),
            bytes,
        };
        assert!((decode(&v).unwrap() - 42.5).abs() < 1e-3);

        let unknown = Val {
            key: "XXXX".into(),
            size: 2,
            ty: "hex_".into(),
            bytes,
        };
        // An unknown type is "no reading", never 0.
        assert!(decode(&unknown).is_none());
    }

    #[test]
    fn sane_band_excludes_artifacts() {
        // Same −20…120 °C band the T19 daemon uses. 0 °C is *inside* the band
        // on purpose: a sensor can genuinely idle near it, and the daemon has
        // always accepted it.
        assert!(is_sane(41.5));
        assert!(is_sane(0.0));
        assert!(!is_sane(-20.0));
        assert!(!is_sane(-40.0));
        assert!(!is_sane(200.0));
    }

    /// On an unprivileged run this must be an honest Err, not a panic. Run as
    /// root it must open. Either way the process survives.
    #[test]
    fn open_is_graceful() {
        match Smc::open() {
            Ok(smc) => {
                let t = smc.temp("TB0T");
                println!("SMC open OK; TB0T = {t:?}");
            }
            Err(e) => println!("SMC open refused (expected when not root): {e}"),
        }
    }
}
