//! macOS menu-bar app (T6a, truths fixed in T16B).
//!
//! One `NSStatusItem`, a live title, and a dropdown built iStat-style: a
//! header with RAM / process count / thermal pressure / CPU temperature, then
//! the CPU row with real P/E residency, the battery block, the power rails,
//! the thermal row and finally the top energy consumers.  Built from scratch on
//! AppKit — no iStat code, assets or pixels.
//!
//! THE TITLE LIE THIS FIXES
//!   T6a rendered `⚡ {cpu}% {attributed_w}W`.  `attributed_w` is the sum of the
//!   per-process `ri_energy_nj` deltas we could read (~0.2 W) — a floor on the
//!   machine's spend, not its draw.  The title now shows *machine* watts:
//!   pack voltage × amperage from ioreg (~4–5 W on this laptop), or the root
//!   daemon's measured package power on a machine with no battery.  Attributed
//!   watts still appear in the dropdown, labelled as what they are.
//!
//! Everything runs on the **main thread**: the sample is taken, the title and
//! menu are updated, then the run loop is pumped for one interval. That avoids
//! touching AppKit from a worker thread (the usual source of random crashes in
//! Rust menu-bar apps) and keeps the cost to one sample per interval.

#![cfg(target_os = "macos")]

use cocoa::appkit::{
    NSApplication, NSApplicationActivationPolicyAccessory, NSMenu, NSMenuItem, NSStatusBar,
    NSVariableStatusItemLength,
};
use cocoa::base::{id, nil, NO, YES};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use objc::{class, msg_send, sel, sel_impl};

use crate::machsample::{self, Battery, RootSample};
use crate::stats::{Collector, Snapshot};
use std::time::Instant;

/// How often the expensive full collector refresh (process enumeration +
/// energy sweep) runs in the menu bar.  The title's CPU%/RAM% value is
/// refreshed every tick; the "top energy" list and the process count are
/// browser data and do not need 2 s granularity.  Measured: full refresh
/// ≈50 ms vs light ≈2 ms, so at 2 s ticks this keeps the app under its 1 %
/// budget instead of at ~1.6 %.  30 s: the measured full-refresh tick is
/// ~80 ms, so at 15 s it alone cost 0.5 % of a core.  The top-energy list is
/// browsing data — the title's CPU/RAM value is still fresh every tick.
const MENU_FULL_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);

/// Dropdown slots, created once and reused.  Header + CPU + clocks + cores +
/// gap + battery + power + apps + thermal + GPU + gap + "Top energy" + top-N
/// rows + attribution note — 32 leaves room for a full top-N list.
const MENU_SLOTS: usize = 32;

fn nsstring(s: &str) -> id {
    unsafe { NSString::alloc(nil).init_str(s) }
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn fmt_w(v: Option<f64>) -> Option<String> {
    v.map(|w| format!("{:.2} W", w))
}fn fmt_res(c: &Option<machsample::Cluster>) -> Option<String> {
    c.as_ref().and_then(|c| c.residency_pct).map(|p| format!("{:.0}%", p))
}

fn fmt_freq(c: &Option<machsample::Cluster>) -> Option<String> {
    c.as_ref()
        .and_then(|c| c.freq_mhz)
        .map(|f| format!("{:.0} MHz", f))
}

/// The header line: `RAM 76% · 412 procs · Nominal · 38.6°`.
fn header_line(
    ram_pct: f32,
    procs: usize,
    root: Option<&RootSample>,
    batt: Option<&Battery>,
) -> String {
    let mut parts = vec![format!("RAM {:.0}%", ram_pct), format!("{} procs", procs)];
    if let Some(r) = root.filter(|r| r.is_fresh()) {
        if let Some(p) = r.thermal_pressure.as_deref() {
            parts.push(p.to_string());
        }
        if let Some(t) = r.cpu_temp_c() {
            parts.push(format!("{:.1}°", t));
        }
    } else if let Some(t) = batt.and_then(|b| b.temp_c) {
        // No root tier: the pack temperature is the only real one we have, and
        // it is always labelled as such in the thermal row.
        parts.push(format!("batt {:.1}°", t));
    }
    parts.join("  ·  ")
}

/// The battery block: `4:26 left · 38 % · 4.4 W · 350 cycles · 81 % health`.
fn battery_line(b: &Battery) -> Option<String> {
    let charging = b.charging.unwrap_or(false);
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = b.time_left_s.and_then(machsample::format_minutes) {
        parts.push(format!("{} {}", t, if charging { "to full" } else { "left" }));
    }
    if let Some(p) = b.pct {
        parts.push(format!("{:.0} %", p));
        if let Some(h) = b.health_pct {
            parts.push(format!("{:.0} % health", h));
        }
        if let Some(c) = b.cycles {
            parts.push(format!("{} cycles", c));
        }
        // Only when current actually flows: on AC at 100 % the meter reads
        // 0 mA and "0.00 W draw" next to a 5 W system load is just noise.
        let flowing = b.amperage_ma.map(|a| a.abs() >= 50).unwrap_or(false)
            || b.external_connected == Some(false);
        if let (true, Some(w)) = (flowing, b.watts) {
            parts.push(format!(
                "{:.2} W {}",
                w,
                if charging { "charge" } else { "draw" }
            ));
        }
        // Provenance of that watt figure: the pack rail it came from, or the
        // gauge's whole-system estimate when no current is flowing.
        if let (Some(v), Some(a)) = (b.voltage_v, b.amperage_ma) {
            parts.push(format!("{:.2} V × {} mA", v, a));
        }
        if let Some(w) = b.system_load_w {
            parts.push(format!("system load {:.2} W (gauge)", w));
        }
        if let Some(w) = b.system_in_w {
            parts.push(format!("adapter in {:.2} W", w));
        }
        if let Some(ext) = b.external_connected {
            parts.push(if ext { "on AC" } else { "on battery" }.to_string());
        }
        let line = format!("Battery   {}", parts.join("  ·  "));
        return Some(line);
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("Battery   {}", parts.join("  ·  ")))
    }
}

/// The power rail block: `package 0.49 W · cpu 0.45 · gpu 0.04 · ane 0.00`.
fn power_line(r: &RootSample) -> Option<String> {
    if !r.is_fresh() || r.package_w.is_none() {
        return None;
    }
    let mut parts = vec![format!("package {}", fmt_w(r.package_w).unwrap_or_default())];
    if let Some(v) = r.cpu_w {
        parts.push(format!("cpu {:.2}", v));
    }
    if let Some(v) = r.gpu_w {
        parts.push(format!("gpu {:.2}", v));
    }
    if let Some(v) = r.ane_w {
        parts.push(format!("ane {:.2}", v));
    }
    Some(format!("Power     {}", parts.join("  ·  ")))
}

fn thermal_line(r: Option<&RootSample>) -> Option<String> {
    let r = r.filter(|r| r.is_fresh())?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = r.thermal_pressure.as_deref() {
        parts.push(p.to_string());
    }
    if let Some(t) = r.cpu_avg_c {
        parts.push(format!("CPU {:.1}°", t));
    }
    if let Some(t) = r.gpu_temp_c() {
        parts.push(format!("GPU {:.1}°", t));
    }
    if let Some(t) = r.ssd_c {
        parts.push(format!("SSD {:.1}°", t));
    }
    if let Some(t) = r.battery_c {
        parts.push(format!("batt {:.1}°", t));
    }
    // Hottest single sensor, named by its SMC key: the aggregate above can hide
    // a hot core, and the key makes the reading traceable.
    if let Some((label, key, c)) = r
        .temps
        .iter()
        .filter(|t| t.label.as_deref().map(|l| l.starts_with("CPU")).unwrap_or(false))
        .map(|t| {
            (
                t.label.clone().unwrap_or_else(|| "sensor".into()),
                t.key.clone().unwrap_or_default(),
                t.c,
            )
        })
        .fold(None::<(String, String, f64)>, |acc, row| match acc {
            Some(a) if a.2 >= row.2 => Some(a),
            _ => Some(row),
        })
    {
        // The SMC key already travels in the trailing parens, so the label is
        // shortened without its own "(Tp09)" suffix — no mid-key truncation.
        let bare = label.split(" (").next().unwrap_or(&label).to_string();
        parts.push(format!("max {} {:.1}° ({})", short(&bare, 24), c, key));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("Thermal   {}", parts.join("  ·  ")))
    }
}


/// The status-item title.
///
/// Machine watts only: the pack rail (V × A) on a laptop, the root daemon's
/// measured package power on a machine without a battery.  **Never**
/// `attributed_w` — that is the sum of what we could pin on processes (~0.1 W
/// here), a floor on the machine's spend, not its draw.  That confusion is
/// what made the T6a title say `0.2W` on a machine pulling ~4 W.
pub fn title(cpu_pct: f32, root: Option<&RootSample>, batt: Option<&Battery>) -> String {
    // The pack rail answers "what is the machine drawing" only while current
    // actually flows.  On AC at 100 % the ammeter reads 0 mA and V × A is 0.00,
    // which is a true reading of the pack and a useless title; there we fall
    // back to the daemon's measured package power and label it in the dropdown.
    let batt_w = batt.and_then(|b| {
        let on_battery = b.external_connected == Some(false);
        let flowing = b.amperage_ma.map(|a| a.abs() >= 50).unwrap_or(false);
        if on_battery || flowing {
            b.watts
        } else {
            None
        }
    });
    // On AC with the pack idle, the gauge's own whole-system load is the only
    // measured number that answers "what is this machine drawing"; the SoC
    // package power (CPU+GPU+ANE) is the last resort and understates the box.
    let draw = batt_w
        .or_else(|| batt.and_then(|b| b.system_load_w))
        .or_else(|| root.filter(|r| r.is_fresh()).and_then(|r| r.package_w));
    match draw {
        Some(w) => format!("⚡ {:.0}%  {:.1}W", cpu_pct, w),
        None => format!("⚡ {:.0}%", cpu_pct),
    }
}

/// Every dropdown row, top to bottom.  An empty string is a separator.
///
/// Pure: it takes the three measured inputs and returns text, so the same rows
/// can be rendered into the status item and printed by `--menubar-print` for
/// verification.  A value we do not have removes its field — never a dash with
/// no reason.
pub fn menu_lines(
    snap: &Snapshot,
    root: Option<&RootSample>,
    batt: Option<&Battery>,
    top_n: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let fresh_root = root.filter(|r| r.is_fresh());

    out.push(header_line(
        snap.memory.pct as f32,
        snap.process_count,
        root,
        batt,
    ));

    // ---- CPU: utilisation + real P/E residency + clocks -----------------
    let mut cpu_parts = vec![format!("{:.0}%", snap.cpu.total_pct)];
    if let Some(p) = fresh_root.and_then(|r| fmt_res(&r.p_cluster)) {
        cpu_parts.push(format!("P {}", p));
    }
    if let Some(e) = fresh_root.and_then(|r| fmt_res(&r.e_cluster)) {
        cpu_parts.push(format!("E {}", e));
    }
    cpu_parts.push(format!("{} MHz", snap.cpu.freq_mhz));
    out.push(format!("CPU       {}", cpu_parts.join("  ·  ")));

    if let Some(r) = fresh_root {
        let mut clocks: Vec<String> = Vec::new();
        if let Some(f) = fmt_freq(&r.p_cluster) {
            clocks.push(format!("P {}", f));
        }
        if let Some(f) = fmt_freq(&r.e_cluster) {
            clocks.push(format!("E {}", f));
        }
        if let Some(f) = r.gpu_freq_mhz {
            clocks.push(format!("GPU {:.0} MHz", f));
        }
        if !clocks.is_empty() {
            out.push(format!("Clocks    {}", clocks.join("  ·  ")));
        }
        // Per-core clocks straight from powermetrics' own per-CPU lines.
        let core_freqs: Vec<String> = r
            .per_cpu
            .iter()
            .filter_map(|c| c.freq_mhz)
            .map(|f| format!("{:.0}", f))
            .collect();
        if !core_freqs.is_empty() {
            out.push(format!(
                "Cores     {} MHz (CPU0…{})",
                core_freqs.join(" · "),
                core_freqs.len() - 1
            ));
        }
    }

    out.push(String::new());

    if let Some(line) = batt.and_then(battery_line) {
        out.push(line);
    }
    if let Some(r) = fresh_root {
        if let Some(line) = power_line(r) {
            out.push(line);
        }
        // Attributed watts stay — labelled as what they are.
        if snap.energy.attributed_w > 0.0 {
            out.push(format!(
                "Apps      {:.2} W measured over {} procs (floor, not total)",
                snap.energy.attributed_w, snap.energy.processes_measured
            ));
        }
        if let Some(line) = thermal_line(Some(r)) {
            out.push(line);
        }
        if let Some(p) = r.gpu_residency_pct {
            out.push(format!("GPU       {:.0}% active", p));
        }
    } else if let Some(t) = batt.and_then(|b| b.temp_c) {
        out.push(format!("Thermal   batt {:.1}° (pack sensor)", t));
    }

    out.push(String::new());
    out.push("Top energy".to_string());
    let mut listed = 0;
    for p in snap.processes.by_energy.iter().take(top_n) {
        let watts = match p.watts {
            Some(w) => w,
            None => continue, // not measured this tick: no row, no dash
        };
        let cpu = p.cpu_pct.unwrap_or(0.0);
        out.push(format!("{}  {:.2} W  {:.0}%", short(&p.name, 24), watts, cpu));
        listed += 1;
    }
    if listed == 0 {
        out.push("  no per-process energy measured yet".to_string());
    }
    if snap.energy.attribution != "processes_only" {
        out.push(format!("attribution: {}", snap.energy.attribution));
    }
    out
}

/// `--menubar-print`: the exact title and rows the status item would show,
/// printed once.  Read-only, no AppKit, no status item — the honest way to
/// verify a GUI without a screenshot.
pub fn print_once(top_n: usize) {
    let mut c = Collector::new(top_n.max(3));
    c.refresh();
    let _ = c.snapshot(false);
    std::thread::sleep(std::time::Duration::from_millis(500));
    c.refresh_light();
    let snap = c.snapshot(false);
    let root = machsample::read_root_sample();
    let batt = machsample::read_battery();

    let root_ref = root.as_ref().filter(|r| r.is_fresh());
    println!("TITLE  {}", title(snap.cpu.total_pct, root.as_ref(), batt.as_ref()));
    println!(
        "ROOT   {}",
        match root_ref {
            Some(r) => format!(
                "fresh, {:.1} s old, {} temp sensors",
                r.age_s,
                r.temps.len()
            ),
            None => match &root {
                Some(r) => format!("STALE, {:.0} s old — withheld", r.age_s),
                None => "absent (root daemon not installed or file missing)".to_string(),
            },
        }
    );
    println!(
        "BATT   {}",
        match &batt {
            Some(b) => format!(
                "V×A {:.2} W, {:.0} %, {} cycles",
                b.watts.unwrap_or(0.0),
                b.pct.unwrap_or(0.0),
                b.cycles.unwrap_or(0)
            ),
            None => "no battery".to_string(),
        }
    );
    println!("---- dropdown ----");
    for line in menu_lines(&snap, root.as_ref(), batt.as_ref(), top_n) {
        if line.is_empty() {
            println!("    ─────────────");
        } else {
            println!("  {}", line);
        }
    }
}

/// Run the menu-bar app forever. `interval_ms` is the refresh cadence,
/// `top_n` how many processes the dropdown lists.
pub fn run(interval_ms: u64, top_n: usize) -> ! {
    unsafe {
        let app = NSApplication::sharedApplication(nil);
        app.setActivationPolicy_(NSApplicationActivationPolicyAccessory);
        // Without finishLaunching the app never joins the status-bar session
        // and the item is created but never registered — it silently does not
        // appear. Costs nothing when already launched.
        let _: () = msg_send![app, finishLaunching];

        let bar = NSStatusBar::systemStatusBar(nil);
        let item: id = bar.statusItemWithLength_(NSVariableStatusItemLength);
        // Outlive this frame: the status bar holds only a weak reference.
        let _: id = msg_send![item, retain];

        let mut c = Collector::new(top_n.max(3));
        c.refresh();
        let _ = c.snapshot(false); // prime the CPU/energy windows
        let mut last_full = Instant::now();

        // ---- one menu, reused forever -------------------------------------
        // Rebuilding NSMenu + N NSMenuItems every tick made AppKit do layout
        // and validation work *inside the run-loop sleep*, which the tick
        // timings never saw: the process measured 1.5 % of a core while its own
        // accounted work was ~0.3 %.  The rows now live in fixed disabled slots
        // that get a new title (only when it changed) or are hidden.
        let menu: id = NSMenu::new(nil);
        let _: () = msg_send![menu, setAutoenablesItems: NO];
        let mut slots: Vec<id> = Vec::with_capacity(MENU_SLOTS);
        for _ in 0..MENU_SLOTS {
            let mi: id = NSMenuItem::new(nil);
            let _: () = msg_send![mi, setEnabled: NO];
            let _: () = msg_send![menu, addItem: mi];
            slots.push(mi);
        }
        let _: () = msg_send![item, setMenu: menu];
        let mut last_titles: Vec<String> = vec![String::new(); MENU_SLOTS];

        let seconds = (interval_ms.max(500) as f64) / 1000.0;
        loop {
            let pool = NSAutoreleasePool::new(nil);

            // T16B: the T6a loop read `snapshot()` without ever calling
            // `refresh()`, so the CPU percentage in the title was frozen at the
            // priming sample for the life of the process.  This loop needs both
            // a live CPU/RAM value every tick (light refresh, ~0.1 ms) and the
            // process/energy lists on a slower cadence (full refresh, ~40 ms).
            let t_refresh = Instant::now();
            let full = last_full.elapsed() >= MENU_FULL_REFRESH;
            if full {
                c.refresh();
                last_full = Instant::now();
            } else {
                c.refresh_light();
            }
            let t_snap = Instant::now();
            let snap = c.snapshot(false);
            let t_read = Instant::now();
            // Both reads are cheap: sample.json is a 2 KB file read, ioreg is
            // cached for 15 s inside machsample.
            let root = machsample::read_root_sample();
            let batt = machsample::read_battery();

            let title = title(snap.cpu.total_pct, root.as_ref(), batt.as_ref());
            let _: () = msg_send![item, setTitle: nsstring(&title)];

            let lines = menu_lines(&snap, root.as_ref(), batt.as_ref(), top_n);
            for i in 0..MENU_SLOTS {
                let mi = slots[i];
                match lines.get(i) {
                    // An empty line in `menu_lines` is a gap; a single space
                    // renders as a blank disabled row without a line to draw.
                    Some(text) => {
                        let text = if text.is_empty() { " ".to_string() } else { text.clone() };
                        if last_titles[i] != text {
                            let _: () = msg_send![mi, setTitle: nsstring(&text)];
                            last_titles[i] = text;
                        }
                        let _: () = msg_send![mi, setHidden: NO];
                    }
                    None => {
                        let _: () = msg_send![mi, setHidden: YES];
                    }
                }
            }
            let t_menu = Instant::now();

            if std::env::var_os("SYSWATCH_MENUBAR_DEBUG").is_some() {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/syswatch-menubar.log")
                {
                    let _ = writeln!(
                        f,
                        "{} | full={} refresh={}us snap={}us read={}us rows={}us total={}us",
                        title,
                        full,
                        t_snap.duration_since(t_refresh).as_micros(),
                        t_read.duration_since(t_snap).as_micros(),
                        t_menu.duration_since(t_read).as_micros(),
                        lines.len(),
                        t_menu.duration_since(t_refresh).as_micros()
                    );
                }
            }

            // Pump the run loop for one interval so clicks still work; the
            // date is built by hand because cocoa's NSDate/NSRunLoop helpers
            // are trait methods with no usable Self here.
            let runloop: id = msg_send![class!(NSRunLoop), currentRunLoop];
            let until: id = msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: seconds];
            let _: () = msg_send![runloop, runUntilDate: until];
            drop(pool);
        }
    }
}
