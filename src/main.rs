mod app;
mod data;
mod events;
mod json;
mod ui;

use std::io;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::{
    cursor,
    event,
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use app::AppState;

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

fn main() -> io::Result<()> {
    let args = Args::parse();
    let interval = Duration::from_secs(args.interval);

    // JSON mode: collect once and exit (no TUI)
    if args.json {
        use sysinfo::{Disks, Networks, System};
        let mut sys = System::new_all();
        let mut disks = Disks::new_with_refreshed_list();
        let mut networks = Networks::new_with_refreshed_list();
        std::thread::sleep(interval / 2);
        sys.refresh_all();
        networks.refresh(true);
        disks.refresh(true);
        std::thread::sleep(interval / 2);
        json::print_json(&sys, &disks, &networks);
        return Ok(());
    }

    // TUI mode
    let mut state = AppState::new(interval);

    // Initial data load (two-phase for accurate CPU)
    std::thread::sleep(Duration::from_millis(200));
    data::refresh(&mut state);

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Restore terminal on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
        original_hook(info);
    }));

    let mut last_refresh = Instant::now();
    loop {
        // Non-blocking keyboard poll (100ms timeout)
        if event::poll(Duration::from_millis(100))? {
            if events::handle_events(&mut state)? {
                break;
            }
        }

        // Refresh data on interval
        if last_refresh.elapsed() >= state.interval {
            data::refresh(&mut state);
            last_refresh = Instant::now();
        }

        terminal.draw(|frame| ui::render(frame, &state))?;
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, cursor::Show)?;

    Ok(())
}
