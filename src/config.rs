use serde::Deserialize;
use std::path::PathBuf;

use crate::cli::Cli;

/// Runtime configuration. CLI flags override values from the config file,
/// which override built-in defaults.
#[derive(Debug, Clone)]
pub struct Config {
    /// Refresh interval in milliseconds.
    pub interval_ms: u64,
    /// Which panels to render.
    pub cpu: bool,
    pub ram: bool,
    pub disk: bool,
    pub net: bool,
    pub proc_panel: bool,
    /// Bar width in characters for gauges.
    pub bar_width: usize,
    /// How many processes to list in each of CPU / memory tables.
    pub top_n: usize,
    /// Show temperature sensors when available.
    pub temp: bool,
    /// Use color output.
    pub color: bool,
    /// Days to keep --log JSONL files before pruning.
    pub retention_days: u64,
    /// T20: `[powermetrics]` — the root daemon's sample cadence.
    pub powermetrics: PowermetricsConfig,
    /// T18 disk-side storage cascade.
    pub storage: crate::storage::StorageConfig,
}

/// T20: powermetrics cadence, in milliseconds.
///
/// powermetrics is the single most expensive probe in the product: it is a
/// subprocess that samples the machine's own counters.  At 5 s it accounted for
/// 12 spikes a minute; at 15 s it is 4, and the agent as a whole lands inside
/// the 1.5–2 % CPU budget.  The dashboard's power/temperature ring runs on the
/// same cadence, so a timeline never implies more resolution than exists.
pub const DEFAULT_POWERMETRICS_INTERVAL_MS: u64 = 15000;

#[derive(Debug, Clone)]
pub struct PowermetricsConfig {
    /// Milliseconds between samples (passed to `syswatch-powerd --interval-ms`).
    pub interval_ms: u64,
}

impl Default for PowermetricsConfig {
    fn default() -> Self {
        Self { interval_ms: DEFAULT_POWERMETRICS_INTERVAL_MS }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            cpu: true,
            ram: true,
            disk: true,
            net: true,
            proc_panel: true,
            bar_width: 20,
            top_n: 5,
            temp: false,
            color: true,
            retention_days: 30,
            powermetrics: PowermetricsConfig::default(),
            storage: crate::storage::StorageConfig::default(),
        }
    }
}

/// Shape of `~/.config/syswatch/config.toml`. Every field optional.
#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    pub interval_ms: Option<u64>,
    #[serde(default)]
    pub panels: Option<Panels>,
    pub bar_width: Option<usize>,
    pub top_n: Option<usize>,
    pub temperature: Option<bool>,
    pub color: Option<bool>,
    pub retention_days: Option<u64>,
    /// T20. Absent table = the 15 s shipped cadence.
    pub powermetrics: Option<FilePowermetrics>,
    /// T18. Absent table = cascade on with the built-in defaults.
    pub storage: Option<FileStorage>,
}

/// `[powermetrics]` table of `config.toml`. Every field optional.
#[derive(Debug, Default, Deserialize)]
pub struct FilePowermetrics {
    /// Milliseconds between powermetrics samples. The root daemon's
    /// `--interval-ms` is kept in step with this value; the dashboard's power
    /// and temperature ring samples on the same cadence.
    pub interval_ms: Option<u64>,
}

/// `[storage]` table of `config.toml`. Every field optional.
#[derive(Debug, Default, Deserialize)]
pub struct FileStorage {
    /// Apply the 1 s → 15 s → 60 s → 300 s → 1800 s → archive cascade on
    /// disk. `false` restores the pre-T18 one-raw-file-per-day layout.
    pub cascade: Option<bool>,
    /// Age cap for raw 1 s data, in hours.
    pub raw_hours: Option<u64>,
    /// Byte cap for raw 1 s data, in MiB. Whichever cap binds first wins.
    pub raw_max_mb: Option<u64>,
    /// Write the one-record-per-day archive tail.
    pub archive: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Panels {
    pub cpu: Option<bool>,
    pub ram: Option<bool>,
    pub disk: Option<bool>,
    pub net: Option<bool>,
    pub proc: Option<bool>,
}

impl Config {
    pub fn load(cli: &Cli) -> Self {
        let mut cfg = Self::default();

        if let Some(path) = config_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                match toml::from_str::<FileConfig>(&text) {
                    Ok(fc) => cfg.apply_file(&fc),
                    Err(e) => eprintln!("syswatch: ignoring bad config {}: {}", path.display(), e),
                }
            }
        }

        // CLI overrides.
        if let Some(ms) = cli.interval_ms {
            cfg.interval_ms = ms;
        }
        if let Some(w) = cli.bar_width {
            cfg.bar_width = w;
        }
        if cli.top_n.is_some() {
            cfg.top_n = cli.top_n.unwrap();
        }
        if cli.no_color {
            cfg.color = false;
        }
        cfg
    }

    fn apply_file(&mut self, fc: &FileConfig) {
        if let Some(v) = fc.interval_ms {
            self.interval_ms = v;
        }
        if let Some(p) = &fc.panels {
            if let Some(v) = p.cpu {
                self.cpu = v;
            }
            if let Some(v) = p.ram {
                self.ram = v;
            }
            if let Some(v) = p.disk {
                self.disk = v;
            }
            if let Some(v) = p.net {
                self.net = v;
            }
            if let Some(v) = p.proc {
                self.proc_panel = v;
            }
        }
        if let Some(v) = fc.bar_width {
            self.bar_width = v;
        }
        if let Some(v) = fc.top_n {
            self.top_n = v;
        }
        if let Some(v) = fc.temperature {
            self.temp = v;
        }
        if let Some(v) = fc.color {
            self.color = v;
        }
        if let Some(v) = fc.retention_days {
            self.retention_days = v;
        }
        if let Some(p) = &fc.powermetrics {
            if let Some(v) = p.interval_ms {
                if v > 0 {
                    self.powermetrics.interval_ms = v;
                }
            }
        }
        if let Some(s) = &fc.storage {
            if let Some(v) = s.cascade {
                self.storage.cascade = v;
            }
            if let Some(v) = s.raw_hours {
                self.storage.raw_hours = v;
            }
            if let Some(v) = s.raw_max_mb {
                self.storage.raw_max_bytes = v * 1024 * 1024;
            }
            if let Some(v) = s.archive {
                self.storage.archive = v;
            }
        }
    }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("syswatch").join("config.toml"));
    }
    dirs_home().map(|h| h.join(".config").join("syswatch").join("config.toml"))
}

pub(crate) fn dirs_home() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(PathBuf::from)
}
