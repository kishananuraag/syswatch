use crate::config::Config;
use crate::stats::{ProcRow, Snapshot};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

/// ANSI color helper; emits nothing when color is disabled.
struct Palette {
    color: bool,
}

impl Palette {
    fn new(on: bool) -> Self {
        Self { color: on }
    }
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{}m{}\x1b[0m", code, text)
        } else {
            text.to_string()
        }
    }
    fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    fn cyan(&self, s: &str) -> String {
        self.paint("36", s)
    }
    fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }

    /// Colorize a percentage string by severity.
    fn sev(&self, p: f64, txt: &str) -> String {
        if !self.color {
            return txt.to_string();
        }
        match p as u64 {
            0..=59 => self.green(txt),
            60..=84 => self.yellow(txt),
            _ => self.red(txt),
        }
    }

    fn pct(&self, p: f64) -> String {
        let txt = format!("{:>5.1}%", p);
        self.sev(p, &txt)
    }

    /// Colorized bar: green -> yellow -> red fill.
    fn bar(&self, p: f64, width: usize) -> String {
        let filled = ((p / 100.0) * width as f64).round() as usize;
        let filled = filled.min(width);
        let glyph = "█".repeat(filled);
        let colored = self.sev(p, &glyph);
        format!("{}{}", colored, "░".repeat(width - filled))
    }
}

fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", b, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

fn human_rate(bps: f64) -> String {
    if bps >= 1024.0 * 1024.0 {
        format!("{:.1} MiB/s", bps / 1024.0 / 1024.0)
    } else {
        format!("{:.1} KiB/s", bps / 1024.0)
    }
}

fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{}d {}h {}m", d, h, m)
    } else {
        format!("{}h {}m", h, m)
    }
}

fn strip_ansi(s: &str) -> usize {
    // Count only visible characters.
    let mut count = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            count += 1;
        }
    }
    count
}

fn truncate(s: &str, w: usize) -> String {
    match s.char_indices().nth(w) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct HistoryState {
    pub selected_date: Option<String>, // YYYY-MM-DD, None for today
    pub range: HistoryRange,
}

#[derive(Debug, Clone, Copy)]
pub enum HistoryRange {
    TwentyFourHours,
    SevenDays,
}

/// Render one full frame into a single String (double-buffered output:
/// one write per tick means no flicker or tearing).
pub fn render(snap: &Snapshot, cfg: &Config, tick: u64) -> String {
    let p = Palette::new(cfg.color);

    let mut lines: Vec<String> = Vec::new();

    macro_rules! add {
        ($($arg:tt)*) => {
            lines.push(format!($($arg)*))
        };
    }

    // CPU panel
    if cfg.cpu {
        let freq = if snap.cpu.freq_mhz > 0 {
            format!(" @ {:.1} GHz", snap.cpu.freq_mhz as f64 / 1000.0)
        } else {
            String::new()
        };
        add!(
            " CPU   {} {}{}",
            p.bar(snap.cpu.total_pct as f64, cfg.bar_width),
            p.pct(snap.cpu.total_pct as f64),
            p.dim(&freq)
        );
        // Per-core grid, 6 cores per row, globally numbered.
        for (row_i, chunk) in snap.cpu.per_core_pct.chunks(6).enumerate() {
            let cells: Vec<String> = chunk
                .iter()
                .enumerate()
                .map(|(j, c)| format!("c{}{} {:>3.0}", row_i * 6 + j, p.bar(*c as f64, 5), c))
                .collect();
            add!("       {}", cells.join(" "));
        }
    }

    // Memory panel
    if cfg.ram {
        let mem = &snap.memory;
        add!(
            " MEM   {} {}  ({})",
            p.bar(mem.pct, cfg.bar_width),
            p.pct(mem.pct),
            p.dim(&format!(
                "{} / {}",
                human_bytes(mem.used_bytes),
                human_bytes(mem.total_bytes)
            ))
        );
        if mem.swap_total_bytes > 0 {
            let spct = mem.swap_used_bytes as f64 * 100.0 / mem.swap_total_bytes.max(1) as f64;
            add!(
                " SWP   {} {}",
                p.bar(spct, cfg.bar_width),
                p.dim(&format!("{:>7.1}%", spct))
            );
        }
    }

    // Temperature
    if cfg.temp && snap.temperature.is_some() {
        let t = snap.temperature.unwrap();
        let txt = format!("{:.0}°C", t);
        add!(
            " TEMP  {}",
            if t < 70.0 { p.green(&txt) } else { p.red(&txt) }
        );
    }

    // Disk panel
    if cfg.disk {
        for d in snap.disks.iter().take(4) {
            let mount = truncate(d.mount.trim_end_matches(['\\', '/']), 10);
            add!(
                " DSK   {:<10} {} {}  {} free",
                p.dim(&mount),
                p.bar(d.used_pct, cfg.bar_width),
                p.pct(d.used_pct),
                p.dim(&format!("{:.0}G", d.free_gb))
            );
        }
    }

    // Network panel
    if cfg.net {
        if snap.networks.is_empty() {
            add!(" NET   {}", p.dim("(no activity)"));
        } else {
            for n in snap.networks.iter().take(3) {
                add!(
                    " NET   {:<12} ↓ {}  ↑ {}",
                    p.dim(&truncate(&n.name, 12)),
                    p.green(&human_rate(n.rx_bps)),
                    p.yellow(&human_rate(n.tx_bps))
                );
            }
        }
    }

    // Processes panel
    if cfg.proc_panel {
        lines.push(p.dim(&format!(
            "  {:<22}{:>7}{:>7}{:>9}   {:<22}{:>7}{:>7}{:>9}",
            "NAME(CPU)", "PID", "CPU%", "MEM", "NAME(MEM)", "PID", "CPU%", "MEM"
        )));
        let n = cfg.top_n.min(snap.processes.by_cpu.len());
        for i in 0..n {
            add!(
                " {}",
                fmt_proc_pair(snap.processes.by_cpu.get(i), snap.processes.by_mem.get(i),)
            );
        }
    }

    // Footer
    lines.push(format!(
        " up {} · {} procs · {}ms refresh · ctrl+c quit",
        p.dim(&fmt_uptime(snap.uptime_secs)),
        snap.process_count,
        cfg.interval_ms,
    ));

    // Frame assembly: dynamic-width box around content.
    let inner = lines
        .iter()
        .map(|l| strip_ansi(l))
        .max()
        .unwrap_or(20)
        .max("syswatch v".len() + env!("CARGO_PKG_VERSION").len());

    let top_border = p.cyan(&format!("╔{}╗", "═".repeat(inner)));
    let mid_border = p.cyan(&format!("╠{}╣", "═".repeat(inner)));
    let bot_border = p.cyan(&format!("╚{}╝", "═".repeat(inner)));

    let mut out = String::with_capacity(16 * 1024);

    // Move cursor home + clear below; no full-screen clear flash.
    if tick > 0 {
        out.push_str("\x1b[H\x1b[J");
    } else {
        out.push_str("\x1b[?25l"); // hide cursor on first frame
    }

    out.push_str(&top_border);
    out.push('\n');
    out.push_str(&boxed_line(
        &format!(
            "{}{}",
            p.bold(" syswatch"),
            p.dim(&format!(" v{}", env!("CARGO_PKG_VERSION")))
        ),
        inner,
    ));
    out.push_str(&mid_border);
    out.push('\n');
    for l in &lines {
        out.push_str(&boxed_line(l, inner));
        out.push('\n');
    }
    out.push_str(&bot_border);

    out
}

fn boxed_line(content: &str, inner: usize) -> String {
    let visible = strip_ansi(content);
    let pad = inner.saturating_sub(visible);
    format!("║{}{}", content, " ".repeat(pad) + "║")
}

fn fmt_proc_pair(cpu: Option<&ProcRow>, mem: Option<&ProcRow>) -> String {
    let one = |pr: Option<&ProcRow>| match pr {
        None => " ".repeat(47),
        Some(prc) => format!(
            "{:<22}{:>7}{:>7.1}{:>8.0}M",
            truncate(&prc.name, 22),
            prc.pid,
            prc.cpu_pct,
            prc.mem_mb
        ),
    };
    format!("{}   {}", one(cpu), one(mem))
}

// (end of module)

/// Write a frame and flush in one go (single write per tick).
pub fn flush(frame: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(frame.as_bytes());
    let _ = lock.flush();
}

/// Restore terminal state (show cursor).
pub fn cleanup() {
    print!("\x1b[?25h");
    let _ = std::io::stdout().flush();
}

/// List YYYY-MM-DD dates that have log files, sorted ascending.
pub fn available_dates() -> Vec<String> {
    let mut dates: Vec<String> = Vec::new();
    for dir in crate::logging::log_dirs() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(d) = name.strip_suffix(".jsonl") {
                    if d.len() == 10 {
                        dates.push(d.to_string());
                    }
                }
            }
        }
    }
    dates.sort();
    dates.dedup();
    dates
}

/// Median time delta (seconds) between consecutive snapshots in the log data.
/// Used to bucket history accurately regardless of which process wrote the logs.
fn median_interval_secs(snaps: &[Snapshot]) -> Option<f64> {
    if snaps.len() < 2 {
        return None;
    }
    fn secs_of_day(ts: &str) -> Option<u64> {
        let t = ts.get(11..19)?; // "HH:MM:SS"
        let h: u64 = t.get(0..2)?.parse().ok()?;
        let m: u64 = t.get(3..5)?.parse().ok()?;
        let s: u64 = t.get(6..8)?.parse().ok()?;
        Some(h * 3600 + m * 60 + s)
    }
    let mut deltas: Vec<u64> = Vec::with_capacity(snaps.len() - 1);
    for w in snaps.windows(2) {
        let (a, b) = (secs_of_day(&w[0].timestamp)?, secs_of_day(&w[1].timestamp)?);
        let d = if b >= a { b - a } else { b + 86_400 - a }; // day rollover
        if d > 0 {
            deltas.push(d);
        }
    }
    if deltas.is_empty() {
        return None;
    }
    deltas.sort_unstable();
    Some(deltas[deltas.len() / 2] as f64)
}

pub fn render_history(_snap: &Snapshot, cfg: &Config, _tick: u64, history_state: &HistoryState) -> String {
    // Load + merge ALL log files across every candidate log dir (service +
    // interactive; pruned to 30 days upstream), sorted by timestamp so the
    // most recent window is the tail.
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in crate::logging::log_dirs() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }
    }
    files.sort(); // Sort by name (which is date)

    if files.is_empty() {
        return "No log files found".to_string();
    }

    let mut snaps: Vec<Snapshot> = Vec::new();
    for path in &files {
        if let Ok(file) = File::open(path) {
            for line in BufReader::new(file).lines().flatten() {
                if let Ok(snap) = serde_json::from_str::<Snapshot>(&line) {
                    snaps.push(snap);
                }
            }
        }
    }
    // RFC 3339 timestamps sort lexicographically == chronologically.
    snaps.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    if snaps.is_empty() {
        return "No data in log files".to_string();
    }

    // Optional per-day filter ('d' key). None = all days.
    if let Some(date) = &history_state.selected_date {
        snaps.retain(|s| s.timestamp.starts_with(date.as_str()));
        if snaps.is_empty() {
            return format!("No data for {}", date);
        }
    }

    // Determine bucketing parameters based on range
    let (bucket_duration_secs, num_buckets) = match history_state.range {
        HistoryRange::TwentyFourHours => (300.0, 288), // 5 minutes = 300 seconds, 24*60/5 = 288 buckets
        HistoryRange::SevenDays => (3600.0, 168), // 1 hour = 3600 seconds, 7*24 = 168 buckets
    };
    // Derive the real sampling interval from the data (median delta between
    // consecutive snapshots) — the service may log at a different rate than
    // the interactive config.
    let snapshot_interval_secs = median_interval_secs(&snaps)
        .unwrap_or_else(|| cfg.interval_ms as f64 / 1000.0)
        .max(0.001);
    let snapshots_per_bucket = f64::round(bucket_duration_secs / snapshot_interval_secs) as usize;
    if snapshots_per_bucket == 0 {
        return format!("Snapshot interval too large for bucketing: {} ms", cfg.interval_ms);
    }

    // We want to show the most recent `num_buckets` buckets.
    // So we need at least `num_buckets * snapshots_per_bucket` snapshots.
    let needed_snaps = num_buckets * snapshots_per_bucket;
    let start_idx = if snaps.len() >= needed_snaps {
        snaps.len() - needed_snaps
    } else {
        0
    };
    let relevant_snaps = &snaps[start_idx..];

    // If we have fewer snapshots than needed, we'll adjust the number of buckets to fit the data.
    let actual_buckets = relevant_snaps.len() / snapshots_per_bucket;
    if actual_buckets == 0 {
        return format!(
            "Not enough data for {} buckets: need at least {} snapshots, have {}",
            num_buckets,
            needed_snaps,
            snaps.len()
        );
    }

    // Bucket the data: compute average cpu.total_pct and memory.pct for each bucket,
    // plus the timestamp of the bucket's first sample for axis labelling.
    let mut cpu_buckets = Vec::with_capacity(actual_buckets);
    let mut mem_buckets = Vec::with_capacity(actual_buckets);
    let mut bucket_times: Vec<&str> = Vec::with_capacity(actual_buckets);
    for chunk in relevant_snaps.chunks(snapshots_per_bucket) {
        let cpu_sum: f32 = chunk.iter().map(|s| s.cpu.total_pct).sum();
        let mem_sum: f64 = chunk.iter().map(|s| s.memory.pct).sum();
        cpu_buckets.push(cpu_sum / chunk.len() as f32);
        mem_buckets.push(mem_sum / chunk.len() as f64);
        bucket_times.push(&chunk[0].timestamp);
    }

    // Downsample to a display width that fits a typical terminal (80-120 cols).
    let max_width = 100usize;
    let width = actual_buckets.min(max_width);
    let group = actual_buckets.div_ceil(width);

    fn spark_char(v: f64) -> char {
        let index = ((v.clamp(0.0, 100.0) / 100.0) * 8.0).round() as usize;
        ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'][index.min(7)]
    }

    let mut cpu_spark = String::with_capacity(width);
    let mut mem_spark = String::with_capacity(width);
    let mut tick_times: Vec<&str> = Vec::with_capacity(width);
    for g in 0..width {
        let lo = g * group;
        let hi = ((g + 1) * group).min(actual_buckets);
        let cpu_avg: f32 = cpu_buckets[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
        let mem_avg: f64 = mem_buckets[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
        cpu_spark.push(spark_char(cpu_avg as f64));
        mem_spark.push(spark_char(mem_avg));
        tick_times.push(bucket_times[lo]);
    }

    // Time axis: HH:MM labels spread evenly across the width (5 chars each,
    // so at most width/6 labels) + day-boundary '|' markers.
    let mut axis: Vec<char> = vec![' '; width];
    let n_ticks = (width / 6).max(1).min(tick_times.len());
    for k in 0..n_ticks {
        let i = if n_ticks == 1 {
            tick_times.len() - 1
        } else {
            (tick_times.len() - 1) * k / (n_ticks - 1)
        };
        let hhmm = tick_times[i].get(11..16).unwrap_or("");
        let pos = if n_ticks == 1 {
            width.saturating_sub(5) // right-align the single label
        } else {
            (width - 5) * k / (n_ticks - 1)
        };
        for (j, ch) in hhmm.chars().enumerate() {
            if pos + j < width {
                axis[pos + j] = ch;
            }
        }
    }
    // Day boundary markers.
    for i in 1..width {
        if tick_times[i].get(..10) != tick_times[i - 1].get(..10) && axis[i - 1] == ' ' {
            axis[i - 1] = '|';
        }
    }
    // Trim trailing blanks.
    while axis.last() == Some(&' ') {
        axis.pop();
    }
    let axis_line: String = axis.into_iter().collect();

    // Date legend: distinct dates present in the window.
    let mut dates: Vec<&str> = Vec::new();
    for tt in &bucket_times {
        let d = &tt[..10];
        if dates.last() != Some(&d) {
            dates.push(d);
        }
    }

    // Build the output string.
    let mut out = String::new();
    out.push_str(&format!(
        " History: {} \u{b7} {} ({} buckets of {}s) | 2=24h 7=7d d=day q=live
",
        match history_state.range {
            HistoryRange::TwentyFourHours => "24h",
            HistoryRange::SevenDays => "7d",
        },
        match &history_state.selected_date {
            Some(date) => format!("date {}", date),
            None => "all days".to_string(),
        },
        actual_buckets,
        bucket_duration_secs as u64
    ));
    out.push_str(&format!(" CPU {}
", cpu_spark));
    out.push_str(&format!(" MEM {}
", mem_spark));
    out.push_str(&format!("     {}
", axis_line));
    out.push_str(&format!("     dates: {}
", dates.join(" ")));
    out
}
