use crate::config::Config;
use crate::stats::{Collector, Snapshot};
use crate::ui;
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
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
    ///
    /// `processes.by_cpu[].cpu_pct` is the CPU burned during the sample
    /// window as a share of ONE core: 100.0 = one fully-pegged core, so an
    /// 8-thread spinner reads ~800 (this is instantaneous, not the `ps`
    /// lifetime average). `null` means "no previous sample yet" (first
    /// sighting of the PID) or the PID could not be inspected — the latter
    /// carries `"perm": false`. Both are omitted from `by_cpu` but still
    /// counted in `process_count`.
    #[arg(short, long)]
    pub json: bool,

    /// Append each sample as a JSON line to the syswatch log directory.
    #[arg(long)]
    pub log: bool,

    /// Directory for JSONL logs (overrides SYSWATCH_LOG_DIR and platform defaults).
    #[arg(long, value_name = "DIR")]
    pub log_dir: Option<PathBuf>,

    /// Run without a TTY: sample and append JSONL logs until SIGTERM/SIGINT.
    #[arg(long)]
    pub headless: bool,

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

    /// macOS: run as a menu-bar app instead of the terminal UI.
    #[arg(long)]
    pub menubar: bool,

    /// macOS: print the menu-bar title and dropdown once, then exit.
    ///
    /// The same code path the status item renders, so the rows can be verified
    /// (and diffed in a release check) without a GUI or a screenshot.
    #[arg(long)]
    pub menubar_print: bool,

    /// Run one storage-cascade compaction pass over the log directory and exit.
    ///
    /// Walks raw → 15 s → 60 s → 300 s → 1800 s → archive, demoting every
    /// bucket that has aged past its tier's window. Stateless: running it twice
    /// in a row is a no-op the second time.
    #[arg(long)]
    pub compact: bool,

    /// With `--compact`: print what would move without touching the disk.
    #[arg(long)]
    pub dry_run: bool,

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

    if let Some(dir) = &cli.log_dir {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("syswatch: cannot create log dir {}: {}", dir.display(), e);
            std::process::exit(1);
        }
        unsafe { std::env::set_var("SYSWATCH_LOG_DIR", dir.as_os_str()); }
    }

    if let Some(crate::cli::Command::Service { action }) = &cli.command {
        match action {
            ServiceAction::Install => crate::service::install(),
            ServiceAction::Uninstall => crate::service::uninstall(),
            ServiceAction::Run => crate::service::run_service(),
        }
        return;
    }

    if cli.compact {
        let cfg = Config::load(&cli);
        let Some(dir) = crate::logging::log_dir() else {
            eprintln!("syswatch: --compact needs a log directory");
            std::process::exit(1);
        };
        match crate::storage::compact(&dir, &cfg.storage, cli.dry_run) {
            Ok(rep) => {
                print!("{}", crate::storage::format_report(&rep));
                println!("  log dir: {}", dir.display());
            }
            Err(e) => {
                eprintln!("syswatch: compaction failed in {}: {}", dir.display(), e);
                std::process::exit(1);
            }
        }
        return;
    }

    if cli.headless && !cli.json {
        let mut cfg = Config::load(&cli);
        apply_hides(&mut cfg, &cli);
        if cli.temp {
            cfg.temp = true;
        }
        if cfg.interval_ms == 0 {
            eprintln!("syswatch: interval must be > 0");
            std::process::exit(1);
        }
        start_storage(&cfg);
        let mut collector = Collector::new(cfg.top_n);
        run_headless(&mut collector, &cfg);
        return;
    }

    #[cfg(target_os = "macos")]
    if cli.menubar_print {
        let top_n = cli.top_n.unwrap_or(6);
        crate::menubar::print_once(top_n);
        return;
    }

    #[cfg(target_os = "macos")]
    if cli.menubar {
        let interval = cli.interval_ms.unwrap_or(2000);
        let top_n = cli.top_n.unwrap_or(6);
        crate::menubar::run(interval, top_n);
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
        start_storage(&cfg);
    }

    let mut collector = Collector::new(cfg.top_n);
    collector.print_startup_self_test();
    let mut tick: u64 = 0;
    let mut next_compact =
        Instant::now() + std::time::Duration::from_secs(CASCADE_PERIOD_SECS);

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

    // Set up terminal for raw mode (interactive TUI only; JSON/headless are pipe-friendly).
    if !cli.json && !cli.headless {
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
            crate::logging::write(&snap, cfg.storage.cascade);
            compact_if_due(&cfg, &mut next_compact);
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
    if !cli.json && !cli.headless {
        ui::cleanup();
    }
}

#[cfg(unix)]
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// T18: how often the hourly cascade pass runs out of the sampling loop.
const CASCADE_PERIOD_SECS: u64 = 3600;

/// One-time storage startup: create the tier layout and run the first
/// compaction pass (which also migrates pre-T18 day files). Falls back to the
/// old "prune by mtime" behaviour when `storage.cascade` is off.
fn start_storage(cfg: &Config) {
    if !cfg.storage.cascade {
        crate::logging::prune(cfg.retention_days);
        return;
    }
    let Some(dir) = crate::logging::log_dir() else {
        return;
    };
    match crate::storage::init(&dir, &cfg.storage) {
        Ok(()) => eprintln!(
            "syswatch: storage cascade on (raw {}h / {} MB cap, log {})",
            cfg.storage.raw_hours,
            cfg.storage.raw_max_bytes / (1024 * 1024),
            dir.display()
        ),
        Err(e) => eprintln!(
            "syswatch: storage cascade unavailable in {} ({}), falling back to raw JSONL",
            dir.display(),
            e
        ),
    }
}

/// Run the cascade pass when the hour is up. Cheap on every other tick.
fn compact_if_due(cfg: &Config, next: &mut Instant) {
    if !cfg.storage.cascade || Instant::now() < *next {
        return;
    }
    *next = Instant::now() + std::time::Duration::from_secs(CASCADE_PERIOD_SECS);
    if let Some(dir) = crate::logging::log_dir() {
        match crate::storage::compact(&dir, &cfg.storage, false) {
            Ok(rep) => {
                if !rep.actions.is_empty() {
                    eprintln!(
                        "syswatch: cascade pass — {} action(s), {} bucket(s) demoted",
                        rep.actions.len(),
                        rep.buckets_moved
                    );
                }
            }
            Err(e) => eprintln!("syswatch: cascade pass failed: {}", e),
        }
    }
}

#[cfg(unix)]
unsafe extern "C" fn handle_signal(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
fn register_shutdown_signals() {
    unsafe {
        libc::signal(libc::SIGTERM, handle_signal as libc::sighandler_t);
        libc::signal(libc::SIGINT, handle_signal as libc::sighandler_t);
    }
}

#[cfg(not(unix))]
fn register_shutdown_signals() {}

#[cfg(unix)]
fn should_shutdown() -> bool {
    SHUTDOWN.load(Ordering::Relaxed)
}

#[cfg(not(unix))]
fn should_shutdown() -> bool {
    false
}

fn run_headless(collector: &mut Collector, cfg: &Config) {
    register_shutdown_signals();

    let Some(log_path) = crate::logging::log_dir() else {
        eprintln!("syswatch: headless mode requires a log directory");
        std::process::exit(1);
    };

    eprintln!(
        "syswatch: headless collector started (interval {} ms, log {})",
        cfg.interval_ms,
        log_path.display()
    );

    let interval = Duration::from_millis(cfg.interval_ms);
    let start = Instant::now();
    let mut next_tick = start;
    let mut next_compact = start + Duration::from_secs(CASCADE_PERIOD_SECS);

    loop {
        if should_shutdown() {
            break;
        }

        let now = Instant::now();
        if now >= next_tick {
            collector.refresh();
            let snap = collector.snapshot(cfg.temp);
            crate::logging::write(&snap, cfg.storage.cascade);
            compact_if_due(cfg, &mut next_compact);
            next_tick += interval;
        }

        if should_shutdown() {
            break;
        }

        let delay = next_tick.saturating_duration_since(Instant::now());
        std::thread::sleep(delay.min(Duration::from_millis(50)));
    }

    crate::storage::flush_hot();
    crate::logging::flush();
}
