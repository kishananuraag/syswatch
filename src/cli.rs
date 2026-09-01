use crate::config::Config;
use crate::stats::{Collector, Snapshot};
use crate::ui;
use clap::Parser;
use std::time::Duration;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;

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

    /// Append each sample as a JSON line to the syswatch log directory.
    #[arg(long)]
    pub log: bool,

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

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Manage the Windows service named 'syswatch'.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum ServiceAction {
    /// Register the syswatch service (auto-start).
    Install,
    /// Remove the syswatch service.
    Uninstall,
    /// Run the service entrypoint (sampling + logging loop).
    Run,
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

    if let Some(crate::cli::Command::Service { action }) = &cli.command {
        match action {
            ServiceAction::Install => crate::service::install(),
            ServiceAction::Uninstall => crate::service::uninstall(),
            ServiceAction::Run => crate::service::run_service(),
        }
        return;
    }
    let mut cfg = Config::load(&cli);
    apply_hides(&mut cfg, &cli);

    if cli.temp {
        cfg.temp = true;
    }

    if cfg.interval_ms == 0 {
        eprintln!("syswatch: interval must be > 0");
        std::process::exit(1);
    }

    if cli.log {
        crate::logging::prune(cfg.retention_days);
    }

    let mut collector = Collector::new(cfg.top_n);
    collector.print_startup_self_test();
    let mut tick: u64 = 0;

    // Define view state
    #[derive(Debug, Clone, Copy)]
    enum View {
        Live,
        History,
    }
    let mut view = View::Live;
    let mut history_state = ui::HistoryState {
        selected_date: None,
        range: ui::HistoryRange::TwentyFourHours,
    };

    // Set up terminal for raw mode (interactive TUI only; JSON mode is pipe-friendly).
    if !cli.json {
        if let Err(_) = terminal::enable_raw_mode() {
            eprintln!("Failed to enter raw mode");
            std::process::exit(1);
        }
    }

    loop {
        // Check for events with a timeout of 0 (non-blocking)
        if let Ok(true) = event::poll(Duration::from_millis(0)) {
            if let Ok(Event::Key(key)) = event::read() {
                match view {
                    View::Live => {
                        if key.code == KeyCode::Char('h') {
                            view = View::History;
                        } else if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                            break;
                        }
                    }
                    View::History => {
                        match key.code {
                            KeyCode::Char('q') => {
                                view = View::Live;
                            }
                            KeyCode::Char('2') => {
                                history_state.range = ui::HistoryRange::TwentyFourHours;
                            }
                            KeyCode::Char('7') => {
                                history_state.range = ui::HistoryRange::SevenDays;
                            }
                            KeyCode::Char('d') => {
                                // Cycle through available log dates (then back to all days).
                                let dates = ui::available_dates();
                                history_state.selected_date = match &history_state.selected_date {
                                    None => dates.first().cloned(),
                                    Some(cur) => {
                                        match dates.iter().position(|d| d == cur) {
                                            Some(i) if i + 1 < dates.len() => {
                                                Some(dates[i + 1].clone())
                                            }
                                            _ => None, // wrapped past the last date
                                        }
                                    }
                                };
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        collector.refresh();
        let snap: Snapshot = collector.snapshot(cfg.temp);

        if cli.log {
            crate::logging::append(&snap);
        }

        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&snap).expect("serialize snapshot")
            );
            break; // JSON mode prints one frame and exits (pipe-friendly)
        }

        let frame = match view {
            View::Live => ui::render(&snap, &cfg, tick),
            View::History => {
                // We need to render the history view. We'll need to pass the log directory and state.
                // We'll create a function in ui.rs for rendering history.
                ui::render_history(&snap, &cfg, tick, &history_state)
            }
        };

        ui::flush(&frame);
        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(cfg.interval_ms));
    }

    // Cleanup
    let _ = terminal::disable_raw_mode();
    if !cli.json {
        ui::cleanup();
    }
}
