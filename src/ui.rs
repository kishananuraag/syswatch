use crate::config::Config;
use crate::stats::{ProcRow, Snapshot};
use std::io::Write;

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
