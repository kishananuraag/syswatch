//! T7C step 0 — probe `proc_pid_rusage(RUSAGE_INFO_V6)` on this OS build.
//!
//! This example exists to answer one question before any aggregation code is
//! written: **does `ri_energy_nj` advance for unprivileged PIDs on this
//! machine?** If it is flat, per-process watts do not exist here and the
//! collector must say `attribution: "unavailable"` instead of printing zeros.
//!
//! Run: `cargo build --release --example energy_probe && ./target/release/examples/energy_probe`
//!
//! Layout is mirrored from <sys/resource.h> `struct rusage_info_v6` and
//! guarded at compile time below (size 464, `ri_energy_nj` at byte 336).

use std::ffi::c_void;
use std::io::Error;
use std::time::{Duration, Instant};

/// `RUSAGE_INFO_V6` per <sys/resource.h> line 192.
const RUSAGE_INFO_V6: i32 = 6;

/// Mirror of `struct rusage_info_v6` (sys/resource.h:368-418).
/// 16-byte uuid + 47 × u64 + `ri_reserved[9]` = 464 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
struct RusageV6 {
    ri_uuid: [u8; 16],
    ri_user_time: u64,
    ri_system_time: u64,
    ri_pkg_idle_wkups: u64,
    ri_interrupt_wkups: u64,
    ri_pageins: u64,
    ri_wired_size: u64,
    ri_resident_size: u64,
    ri_phys_footprint: u64,
    ri_proc_start_abstime: u64,
    ri_proc_exit_abstime: u64,
    ri_child_user_time: u64,
    ri_child_system_time: u64,
    ri_child_pkg_idle_wkups: u64,
    ri_child_interrupt_wkups: u64,
    ri_child_pageins: u64,
    ri_child_elapsed_abstime: u64,
    ri_diskio_bytesread: u64,
    ri_diskio_byteswritten: u64,
    ri_cpu_time_qos_default: u64,
    ri_cpu_time_qos_maintenance: u64,
    ri_cpu_time_qos_background: u64,
    ri_cpu_time_qos_utility: u64,
    ri_cpu_time_qos_legacy: u64,
    ri_cpu_time_qos_user_initiated: u64,
    ri_cpu_time_qos_user_interactive: u64,
    ri_billed_system_time: u64,
    ri_serviced_system_time: u64,
    ri_logical_writes: u64,
    ri_lifetime_max_phys_footprint: u64,
    ri_instructions: u64,
    ri_cycles: u64,
    ri_billed_energy: u64,
    ri_serviced_energy: u64,
    ri_interval_max_phys_footprint: u64,
    ri_runnable_time: u64,
    ri_flags: u64,
    ri_user_ptime: u64,
    ri_system_ptime: u64,
    ri_pinstructions: u64,
    ri_pcycles: u64,
    ri_energy_nj: u64,
    ri_penergy_nj: u64,
    ri_secure_time_in_system: u64,
    ri_secure_ptime_in_system: u64,
    ri_neural_footprint: u64,
    ri_lifetime_max_neural_footprint: u64,
    ri_interval_max_neural_footprint: u64,
    ri_reserved: [u64; 9],
}

const _: () = assert!(std::mem::size_of::<RusageV6>() == 464);
const _: () = assert!(std::mem::offset_of!(RusageV6, ri_energy_nj) == 336);
const _: () = assert!(std::mem::offset_of!(RusageV6, ri_penergy_nj) == 344);
const _: () = assert!(std::mem::offset_of!(RusageV6, ri_proc_start_abstime) == 80);
const _: () = assert!(std::mem::offset_of!(RusageV6, ri_diskio_bytesread) == 144);

unsafe extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut c_void) -> i32;
}

fn probe(pid: i32) -> Result<RusageV6, Error> {
    let mut ri = unsafe { std::mem::zeroed::<RusageV6>() };
    let rc = unsafe {
        proc_pid_rusage(
            pid,
            RUSAGE_INFO_V6,
            &mut ri as *mut RusageV6 as *mut c_void,
        )
    };
    if rc != 0 {
        return Err(Error::last_os_error());
    }
    Ok(ri)
}

fn main() {
    // One in-process spin thread so at least one PID is provably burning CPU.
    std::thread::spawn(|| {
        let mut x: u64 = 1;
        loop {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            if x == 0 {
                break;
            }
        }
    });

    println!("self pid = {}", std::process::id());
    println!(
        "sizeof(rusage_info_v6) = {}, ri_energy_nj offset = {}, ri_penergy_nj offset = {}",
        std::mem::size_of::<RusageV6>(),
        std::mem::offset_of!(RusageV6, ri_energy_nj),
        std::mem::offset_of!(RusageV6, ri_penergy_nj),
    );

    let pids: Vec<i32> = unsafe {
        let mut v = Vec::new();
        let mut buf = vec![0i32; 4096];
        let n = proc_listallpids(buf.as_mut_ptr() as *mut c_void, buf.len() as i32);
        if n > 0 {
            v.extend_from_slice(&buf[..n as usize]);
        }
        v
    };

    println!("proc_listallpids -> {} pids", pids.len());

    let first: Vec<(i32, Result<RusageV6, Error>)> =
        pids.iter().map(|&p| (p, probe(p))).collect();

    let ok = first.iter().filter(|(_, r)| r.is_ok()).count();
    let denied = first
        .iter()
        .filter(|(_, r)| matches!(r, Err(e) if e.raw_os_error() == Some(1)))
        .count();
    println!(
        "rusage ok = {ok}, EPERM = {denied}, other/failed = {}",
        pids.len() - ok
    );

    let nz_energy = first
        .iter()
        .filter(|(_, r)| matches!(r, Ok(v) if v.ri_energy_nj != 0))
        .count();
    let nz_penergy = first
        .iter()
        .filter(|(_, r)| matches!(r, Ok(v) if v.ri_penergy_nj != 0))
        .count();
    let nz_billed = first
        .iter()
        .filter(|(_, r)| matches!(r, Ok(v) if v.ri_billed_energy != 0))
        .count();
    println!(
        "nonzero at t0: ri_energy_nj={nz_energy}  ri_penergy_nj={nz_penergy}  ri_billed_energy={nz_billed}"
    );

    // Raw rows for the first few readable PIDs so the counter values are visible.
    println!("\n-- t0 raw rows (first 6 readable) --");
    for (pid, r) in first.iter().filter(|(_, r)| r.is_ok()).take(6) {
        let v = r.as_ref().unwrap();
        println!(
            "pid {pid:6} energy_nj={:12} penergy_nj={:12} billed_energy={:12} user={} start_abstime={}",
            v.ri_energy_nj, v.ri_penergy_nj, v.ri_billed_energy, v.ri_user_time, v.ri_proc_start_abstime
        );
    }

    let t0 = Instant::now();
    std::thread::sleep(Duration::from_secs(6));
    let elapsed = t0.elapsed().as_secs_f64();

    let mut moved = 0usize;
    let mut rows: Vec<(i32, u64, u64, u64, f64)> = Vec::new();
    for (pid, r0) in first.iter() {
        let (Ok(a), Ok(b)) = (r0, probe(*pid)) else {
            continue;
        };
        let d_energy = b.ri_energy_nj.wrapping_sub(a.ri_energy_nj);
        let d_penergy = b.ri_penergy_nj.wrapping_sub(a.ri_penergy_nj);
        let d_billed = b.ri_billed_energy.wrapping_sub(a.ri_billed_energy);
        if d_energy != 0 || d_penergy != 0 || d_billed != 0 {
            moved += 1;
        }
        rows.push((
            *pid,
            d_energy,
            d_penergy,
            d_billed,
            (d_energy as f64 / 1e9) / elapsed,
        ));
    }

    println!("\n-- deltas over {elapsed:.3} s ({} readable pids) --", rows.len());
    println!("counters that moved: {moved}");
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (pid, d_e, d_p, d_b, w) in rows.iter().take(12) {
        println!(
            "pid {pid:6} d_energy_nj={d_e:12} d_penergy_nj={d_p:12} d_billed={d_b:12} watts={w:.6}"
        );
    }

    // The decisive line.
    if moved == 0 {
        println!("\nRESULT: FLAT — ri_energy_nj/penergy/billed did not advance for any readable pid.");
        println!("Per-process energy is NOT available to this unprivileged build.");
    } else {
        println!("\nRESULT: MOVING — per-process energy counters advance on this build.");
    }
}

unsafe extern "C" {
    fn proc_listallpids(buffer: *mut c_void, buffersize: i32) -> i32;
}
