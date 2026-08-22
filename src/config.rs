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
    }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("syswatch").join("config.toml"));
    }
    dirs_home().map(|h| h.join(".config").join("syswatch").join("config.toml"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(PathBuf::from)
}
