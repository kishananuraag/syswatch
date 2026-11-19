//! Command line: `syswatch-agent [--port N] [--host H] [--static-dir DIR]
//! [--logs-dir DIR] [--tick-ms N]`.
//!
//! Hand-rolled rather than `clap`: the surface is five flags, and the agent is
//! the process that must start fast and small. Unknown flags are an error, not
//! a silent default — a typo'd `--prot 8199` must not quietly bind 8123.

use std::path::PathBuf;

/// Default port — the same port `dashboard/server.py` uses. The Python server
/// stays the live one; the agent is started with `--port 8199` for testing so
/// the two never collide.
pub const DEFAULT_PORT: u16 = 8123;
/// Loopback only, exactly like the Python server (T4a: never expose the
/// dashboard to the network).
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// What the command line asked for. `Help`/`Version` are successful exits, not
/// errors — they carry their text so `main` only has to print it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    Run(Args),
    Help(String),
    Version(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub port: u16,
    pub host: String,
    pub static_dir: PathBuf,
    pub logs_dir: PathBuf,
    /// Sampler tick. 1000 ms is the dashboard's cadence; the only reason to
    /// change it is a test that wants a snapshot sooner.
    pub tick_ms: u64,
}

impl Args {
    /// Parse `argv` (without the program name).
    pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Cli, String> {
        let mut args = Args {
            port: DEFAULT_PORT,
            host: DEFAULT_HOST.to_string(),
            static_dir: default_static_dir(),
            logs_dir: default_logs_dir(),
            tick_ms: 1000,
        };
        let mut it = argv.into_iter().peekable();
        while let Some(arg) = it.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) => (f.to_string(), Some(v.to_string())),
                None => (arg.clone(), None),
            };
            let mut value = |name: &str| -> Result<String, String> {
                inline
                    .clone()
                    .or_else(|| it.next())
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match flag.as_str() {
                "--port" | "-p" => {
                    let v = value("--port")?;
                    let port: u16 = v
                        .parse()
                        .map_err(|_| format!("--port must be 1..=65535, got {v:?}"))?;
                    if port == 0 {
                        return Err("--port 0 is not a usable port".to_string());
                    }
                    args.port = port;
                }
                "--host" => args.host = value("--host")?,
                "--static-dir" => args.static_dir = PathBuf::from(value("--static-dir")?),
                "--logs-dir" => args.logs_dir = PathBuf::from(value("--logs-dir")?),
                "--tick-ms" => {
                    let v = value("--tick-ms")?;
                    let ms: u64 = v
                        .parse()
                        .map_err(|_| format!("--tick-ms must be an integer, got {v:?}"))?;
                    args.tick_ms = ms.max(100);
                }
                "--help" | "-h" => return Ok(Cli::Help(usage())),
                "--version" | "-V" => {
                    return Ok(Cli::Version(env!("CARGO_PKG_VERSION").to_string()));
                }
                other => return Err(format!("unknown argument {other:?}\n\n{}", usage())),
            }
        }
        Ok(Cli::Run(args))
    }
}

pub fn usage() -> String {
    format!(
        "syswatch-agent {}\n\
         SysWatch HTTP agent (axum). Serves /api/health and /api/current, and the\n\
         dashboard's static files. The Python server on port {} stays the live\n\
         one — use --port to run this beside it.\n\n\
         Usage: syswatch-agent [options]\n\n\
         Options:\n\
         \x20 -p, --port <N>        listen port (default {})\n\
         \x20     --host <H>        bind address (default {})\n\
         \x20     --static-dir <D>  dashboard static dir (default {})\n\
         \x20     --logs-dir <D>    collector JSONL dir (default {})\n\
         \x20     --tick-ms <N>     sampler tick (default 1000, min 100)\n\
         \x20 -h, --help            this text\n\
         \x20 -V, --version         agent version",
        env!("CARGO_PKG_VERSION"),
        DEFAULT_PORT,
        DEFAULT_PORT,
        DEFAULT_HOST,
        default_static_dir().display(),
        default_logs_dir().display()
    )
}

/// `dashboard/static`, resolved from this crate's source location so the agent
/// finds the UI regardless of the directory it was started from.
pub fn default_static_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../dashboard/static")
        .components()
        .collect()
}

/// The Rust collector's log directory — the same one the Python server reads
/// (`COLLECTOR_LOGS` in dashboard/server.py).
pub fn default_logs_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SYSWATCH_LOGS") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(home).join("Library/Application Support/syswatch/logs")
    }
    #[cfg(not(target_os = "macos"))]
    {
        PathBuf::from(home).join(".local/share/syswatch/logs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Result<Cli, String> {
        Args::parse(v.iter().map(|s| s.to_string()))
    }

    fn run(v: &[&str]) -> Args {
        match parse(v) {
            Ok(Cli::Run(a)) => a,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn defaults_match_the_python_server() {
        let a = run(&[]);
        assert_eq!(a.port, 8123);
        assert_eq!(a.host, "127.0.0.1");
        assert_eq!(a.tick_ms, 1000);
    }

    #[test]
    fn port_forms() {
        assert_eq!(run(&["--port", "8199"]).port, 8199);
        assert_eq!(run(&["--port=8199"]).port, 8199);
        assert_eq!(run(&["-p", "8199"]).port, 8199);
        assert!(parse(&["--port"]).is_err());
        assert!(parse(&["--port", "0"]).is_err());
        assert!(parse(&["--port", "70000"]).is_err());
        assert!(parse(&["--port", "abc"]).is_err());
    }

    #[test]
    fn help_and_version_are_not_errors() {
        assert!(matches!(parse(&["--help"]), Ok(Cli::Help(_))));
        assert!(matches!(parse(&["-h"]), Ok(Cli::Help(_))));
        assert!(matches!(parse(&["--version"]), Ok(Cli::Version(_))));
    }

    #[test]
    fn unknown_flag_is_an_error_not_a_default() {
        // A typo must never silently fall back to 8123 and shadow the live
        // Python server.
        let err = parse(&["--prot", "8199"]).unwrap_err();
        assert!(err.contains("unknown argument"), "{err}");
    }

    #[test]
    fn static_dir_points_at_the_dashboard() {
        let d = default_static_dir();
        assert!(d.ends_with("dashboard/static"), "{}", d.display());
    }

    #[test]
    fn tick_is_floored() {
        assert_eq!(run(&["--tick-ms", "1"]).tick_ms, 100);
    }
}
