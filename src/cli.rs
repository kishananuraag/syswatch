use crate::config::Config;
use crate::stats::{Collector, Snapshot};
use crate::ui;
use clap::Parser;

/// CLI surface. Values here override the config file; config file overrides defaults.
#[derive(clap::Parser, Debug)]
#[command(
    name = "syswatch",
    version,
    about = "iStat-style terminal system monitor"
)]
pub struct Cli {
    /// Refresh interval in milliseconds (overrides config).
    #[arg(short, long, value_name = "MS")]
    pub interval_ms: Option<u64>,

    /// Print one JSON snapshot and exit.
    #[arg(short, long)]
    pub json: bool,

    /// Number of processes to show per table (top by CPU / top by memory).
    #[arg(short, long, value_name = "N")]
    pub top_n: Option<usize>,

    /// Bar width in characters (default 20).
    #[arg(long, value_name = "W")]
    pub bar_width: Option<usize>,

    /// Show temperature sensors when available.
    #[arg(long)]
    pub temp: bool,

    /// Disable colored output.
    #[arg(long)]
    pub no_color: bool,

    /// Hide specific panels (repeatable): cpu, ram, disk, net, proc.
    #[arg(long = "hide", value_name = "PANEL")]
    pub hide: Vec<String>,
}

/// Apply --hide flags on top of a loaded config.
pub fn apply_hides(cfg: &mut Config, cli: &Cli) {
    for panel in &cli.hide {
        match panel.as_str() {
            "cpu" => cfg.cpu = false,
            "ram" | "mem" => cfg.ram = false,
            "disk" | "disks" => cfg.disk = false,
            "net" => cfg.net = false,
            "proc" => cfg.proc_panel = false,
            other => eprintln!(
                "syswatch: unknown panel '{}' (--hide cpu|ram|disk|net|proc)",
                other
            ),
        }
    }
}

pub fn run() {
    let cli = Cli::parse();
    let mut cfg = Config::load(&cli);
    apply_hides(&mut cfg, &cli);

    if cli.temp {
        cfg.temp = true;
    }

    if cfg.interval_ms == 0 {
        eprintln!("syswatch: interval must be > 0");
        std::process::exit(1);
    }

    let mut collector = Collector::new(cfg.top_n);
    let mut tick: u64 = 0;

    loop {
        collector.refresh();
        let snap: Snapshot = collector.snapshot(cfg.temp);

        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&snap).expect("serialize snapshot")
            );
            break; // JSON mode prints one frame and exits (pipe-friendly)
        }

        ui::flush(&ui::render(&snap, &cfg, tick));
        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(cfg.interval_ms));
    }

    ui::cleanup();
}
