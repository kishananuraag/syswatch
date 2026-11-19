//! Evidence run for T23: probe every sensor, print the manifest, sample twice
//! and print the values. Run with `cargo run --example sweep`.
//!
//! The second sample is the one that matters for disk and network: a rate is a
//! counter delta over a window, so the first sample of a fresh sensor can only
//! report usage.

use std::thread::sleep;
use std::time::Duration;

use syswatch_core::SampleBuf;
use syswatch_sensors_macos::all_sensors;

fn dump(sensors: &mut [Box<dyn syswatch_core::Sensor>], label: &str) {
    println!("\n── {label} ──");
    for sensor in sensors.iter_mut() {
        let mut buf = SampleBuf::default();
        match sensor.sample(&mut buf) {
            Ok(()) => {
                println!("{}: {} values", sensor.id(), buf.values.len());
                for (id, v) in &buf.values {
                    println!("  {id:<34} {v:>18.3}");
                }
            }
            Err(e) => println!("{}: sample error — {e}", sensor.id()),
        }
    }
}

fn main() {
    let mut sensors = all_sensors();

    println!("── manifest (probe) ──");
    let mut total_defs = 0usize;
    for sensor in sensors.iter_mut() {
        let defs = sensor.probe();
        total_defs += defs.len();
        println!(
            "{}: {} MetricDefs (tier {:?})",
            sensor.id(),
            defs.len(),
            sensor.tier()
        );
        for d in &defs {
            println!(
                "  {:<34} {:<26} {:?} {:?} @{}ms",
                d.id, d.label, d.unit, d.group, d.default_interval_ms
            );
        }
    }
    println!("\ntotal MetricDefs: {total_defs}");

    dump(&mut sensors, "sample 1 (cold)");
    sleep(Duration::from_millis(1200));
    dump(
        &mut sensors,
        "sample 2 (1.2 s later — disk/network rates are real)",
    );
    sleep(Duration::from_millis(5000));
    dump(
        &mut sensors,
        "sample 3 (+5 s — disk rates from a full throttle window)",
    );
}
