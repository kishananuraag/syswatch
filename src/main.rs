use sysinfo::{Disks, Networks, System};
use std::{thread, time::Duration};
use crossterm::{execute, terminal::{Clear, ClearType}, cursor};
use clap::Parser;

#[derive(Parser)]
#[command(name = "syswatch", about = "Terminal system monitor")]
struct Args {
    /// Refresh interval in seconds
    #[arg(short, long, default_value_t = 1)]
    interval: u64,

    /// Output as JSON instead of terminal UI
    #[arg(short, long)]
    json: bool,
}

fn bar(pct: f64, width: usize) -> String {
    let filled = (pct / 100.0 * width as f64).round() as usize;
    let empty   = width.saturating_sub(filled);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

fn print_pretty(sys: &System, disks: &Disks, networks: &Networks, interval: u64) {
    let stdout = &mut std::io::stdout();
    execute!(stdout, Clear(ClearType::All), cursor::MoveTo(0, 0)).unwrap();

    let cpu_usage: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
        / sys.cpus().len() as f32;

    let total_ram = sys.total_memory();
    let used_ram  = sys.used_memory();
    let ram_pct   = used_ram as f64 / total_ram as f64 * 100.0;

    println!("╔══════════════════════════════════════╗");
    println!("║             syswatch                 ║");
    println!("╠══════════════════════════════════════╣");
    println!("║ CPU  {} {:>5.1}%", bar(cpu_usage as f64, 20), cpu_usage);
    println!("║ RAM  {} {:>5.1}%  ({} / {} MB)",
        bar(ram_pct, 20), ram_pct,
        used_ram  / 1024 / 1024,
        total_ram / 1024 / 1024,
    );
    println!("╠══════════════════════════════════════╣");

    for disk in disks {
        let total = disk.total_space();
        if total == 0 { continue; }
        let free     = disk.available_space();
        let used_pct = (total - free) as f64 / total as f64 * 100.0;
        println!("║ DSK  {} {:>5.1}%  ({} GB free)",
            bar(used_pct, 20), used_pct,
            free / 1024 / 1024 / 1024,
        );
    }

    println!("╠══════════════════════════════════════╣");

    let mut printed = false;
    for (name, data) in networks {
        let rx = data.received();
        let tx = data.transmitted();
        if rx == 0 && tx == 0 { continue; }
        println!("║ NET  {:<10} ↓{:>6} KB  ↑{:>6} KB",
            name, rx / 1024, tx / 1024,
        );
        printed = true;
    }
    if !printed { println!("║ NET  (no activity)"); }

    println!("╠══════════════════════════════════════╣");
    println!("║  interval: {}s  [ctrl+c to quit]      ║", interval);
    println!("╚══════════════════════════════════════╝");
}

fn print_json(sys: &System, disks: &Disks, networks: &Networks) {
    let cpu_usage: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
        / sys.cpus().len() as f32;

    let total_ram = sys.total_memory();
    let used_ram  = sys.used_memory();

    let disk_info: Vec<String> = disks.iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| {
            let total = d.total_space();
            let free  = d.available_space();
            let used_pct = (total - free) as f64 / total as f64 * 100.0;
            format!(r#"{{"free_gb":{},"used_pct":{:.1}}}"#,
                free / 1024 / 1024 / 1024, used_pct)
        })
        .collect();

    let net_info: Vec<String> = networks.iter()
        .filter(|(_, d)| d.received() > 0 || d.transmitted() > 0)
        .map(|(name, d)| {
            format!(r#"{{"name":"{}","rx_kb":{},"tx_kb":{}}}"#,
                name, d.received() / 1024, d.transmitted() / 1024)
        })
        .collect();

    println!(
        r#"{{"cpu_pct":{:.1},"ram_used_mb":{},"ram_total_mb":{},"disks":[{}],"networks":[{}]}}"#,
        cpu_usage,
        used_ram  / 1024 / 1024,
        total_ram / 1024 / 1024,
        disk_info.join(","),
        net_info.join(","),
    );
}

fn main() {
    let args = Args::parse();
    let interval = Duration::from_secs(args.interval);

    let mut sys      = System::new_all();
    let mut disks    = Disks::new_with_refreshed_list();
    let mut networks = Networks::new_with_refreshed_list();

    loop {
        thread::sleep(interval / 2);
        sys.refresh_all();
        networks.refresh(true);
        disks.refresh(true);
        thread::sleep(interval / 2);

        if args.json {
            print_json(&sys, &disks, &networks);
        } else {
            print_pretty(&sys, &disks, &networks, args.interval);
        }

        if args.json { break; } // JSON mode: print once and exit (for piping)
    }
}
