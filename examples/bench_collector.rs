//! Headless benchmark for the syswatch collector.
//!
//! Drives `Collector::refresh()` + `Collector::snapshot()` in a tight loop
//! at a fixed interval (default 1000 ms) without any TUI so we can measure
//! the *pure* collector CPU and RSS cost. With `--log` it also runs the
//! JSONL writer (`logging::append`) so the measurement includes the disk
//! I/O the production Windows service does every tick.
//!
//! Run via:
//!     cargo run --release --example bench_collector -- --duration 60 --interval 1000
//!
//! Then in another shell:
//!     python scripts/measure_collector.py --name bench_collector.exe --duration 60 --interval 2
//!
//! This binary never touches the global state, terminal, or filesystem
//! (unless `--log` is passed).

use std::time::Duration;

use syswatch::logging;
use syswatch::stats::{Collector, Snapshot};

fn main() {
    let mut duration_secs: u64 = 60;
    let mut interval_ms: u64 = 1000;
    let mut top_n: usize = 5;
    let mut with_temp = false;
    let mut with_log = false;

    let mut i = 1;
    while i < std::env::args().len() {
        match std::env::args().nth(i).unwrap().as_str() {
            "--duration" => {
                duration_secs = std::env::args().nth(i + 1).unwrap().parse().unwrap();
                i += 2;
            }
            "--interval" => {
                interval_ms = std::env::args().nth(i + 1).unwrap().parse().unwrap();
                i += 2;
            }
            "--top-n" => {
                top_n = std::env::args().nth(i + 1).unwrap().parse().unwrap();
                i += 2;
            }
            "--temp" => {
                with_temp = true;
                i += 1;
            }
            "--log" => {
                with_log = true;
                i += 1;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }

    eprintln!(
        "bench_collector: duration={duration_secs}s interval={interval_ms}ms top_n={top_n} temp={with_temp} log={with_log}"
    );

    // Snapshot the *cost of building a snapshot* (the bit that runs every
    // tick) — we don't render or write anything for the baseline run.
    let mut collector = Collector::new(top_n);
    collector.print_startup_self_test();
    let end = std::time::Instant::now() + Duration::from_secs(duration_secs);
    let mut ticks: u64 = 0;
    let mut refresh_total = Duration::ZERO;
    let mut snapshot_total = Duration::ZERO;
    let mut log_total = Duration::ZERO;
    while std::time::Instant::now() < end {
        let tick_start = std::time::Instant::now();
        let t0 = std::time::Instant::now();
        collector.refresh();
        let t1 = std::time::Instant::now();
        let snap: Snapshot = collector.snapshot(with_temp);
        let t2 = std::time::Instant::now();
        if with_log {
            logging::append(&snap);
        }
        let t3 = std::time::Instant::now();
        refresh_total += t1 - t0;
        snapshot_total += t2 - t1;
        log_total += t3 - t2;
        ticks += 1;
        let elapsed = tick_start.elapsed();
        if elapsed < Duration::from_millis(interval_ms) {
            std::thread::sleep(Duration::from_millis(interval_ms) - elapsed);
        }
    }
    eprintln!(
        "bench_collector: ran {ticks} ticks over {duration_secs}s | \
         refresh={:.2}us/tick snapshot={:.2}us/tick log={:.2}us/tick",
        refresh_total.as_micros() as f64 / ticks.max(1) as f64,
        snapshot_total.as_micros() as f64 / ticks.max(1) as f64,
        log_total.as_micros() as f64 / ticks.max(1) as f64,
    );
}