//! `syswatch-relayd` — run a stateless relay until told to stop.
//!
//! No config file and no state directory on purpose: everything it needs is a
//! port, an identity (generated on first run, so its node id is stable across
//! restarts) and optionally an access token from the environment.
//!
//! ```text
//! syswatch-relayd [--port 9443] [--local] [--identity PATH]
//! ```
//!
//! `SYSWATCH_RELAY_TOKEN` gates who may register. The token is read from the
//! environment rather than argv so it does not land in `ps` output; it is never
//! printed, and its length is not either.
//!
//! SIGTERM / SIGINT stop it: the accept loop returns and the process exits 0,
//! leaving nothing behind — there is nothing to leave behind.

use std::path::PathBuf;
use std::process::ExitCode;

use syswatch_relay::{DEFAULT_PORT, RelayServer};
use syswatch_net::DeviceIdentity;

/// Environment variable holding the optional relay access token.
const TOKEN_ENV: &str = "SYSWATCH_RELAY_TOKEN";

struct Args {
    port: u16,
    local: bool,
    /// `None` = use the platform default (or `./relay.key` if it already exists).
    identity: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut port = DEFAULT_PORT;
    let mut local = false;
    let mut identity = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" | "-p" => {
                let value = args.next().ok_or("--port needs a number")?;
                port = value
                    .parse()
                    .map_err(|_| format!("--port {value:?} is not a port (0-65535)"))?;
            }
            "--local" => local = true,
            "--identity" | "-i" => {
                identity = Some(PathBuf::from(
                    args.next().ok_or("--identity needs a path")?,
                ));
            }
            "--help" | "-h" => {
                println!("usage: syswatch-relayd [--port 9443] [--local] [--identity PATH]");
                println!("       {TOKEN_ENV}=… to require an access token");
                println!("       --port 0 lets the OS pick (the chosen port is printed)");
                println!("       --local binds loopback only");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }

    Ok(Args {
        port,
        local,
        identity,
    })
}

/// Default identity location: `~/Library/Application Support/syswatch/relay.key`
/// on macOS, `~/.local/share/syswatch/relay.key` elsewhere.
///
/// The node id is derived from this file, so losing it means clients that pinned
/// the old id have to be re-pointed at the new one.
fn default_identity_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let relative: &[&str] = if cfg!(target_os = "macos") {
        &["Library", "Application Support", "syswatch", "relay.key"]
    } else {
        &[".local", "share", "syswatch", "relay.key"]
    };
    match home {
        Some(home) => {
            let mut path = home;
            for part in relative {
                path.push(part);
            }
            path
        }
        // No HOME (unusual, but a container can): a local file is better than a
        // panic, and the path is printed so it can be moved.
        None => PathBuf::from("relay.key"),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("syswatch-relayd: {err}");
            eprintln!("usage: syswatch-relayd [--port 9443] [--local] [--identity PATH]");
            // Exit code 2 is the conventional "bad usage", and it keeps `--help`
            // (0) distinguishable from a typo.
            return ExitCode::from(2);
        }
    };

    // `--identity` wins; otherwise a key in the current directory (a hand-run
    // relay keeps its identity with it), otherwise the platform default.
    let identity_path = match args.identity {
        Some(path) => path,
        None if PathBuf::from("relay.key").exists() => PathBuf::from("relay.key"),
        None => default_identity_path(),
    };

    let identity = match DeviceIdentity::load_or_create(&identity_path) {
        Ok(identity) => identity,
        Err(err) => {
            eprintln!(
                "syswatch-relayd: cannot load identity {}: {err}",
                identity_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let token = std::env::var_os(TOKEN_ENV).map(|value| value.to_string_lossy().into_owned());
    let token = token.filter(|value| !value.is_empty()).map(String::into_bytes);
    let token_required = token.is_some();

    let server = match if args.local {
        RelayServer::bind_local(&identity, args.port).await
    } else {
        RelayServer::bind(&identity, args.port).await
    } {
        Ok(server) => server.with_token(token),
        Err(err) => {
            eprintln!("syswatch-relayd: cannot bind port {}: {err}", args.port);
            return ExitCode::FAILURE;
        }
    };

    // The lines an operator needs: who this relay is, and where it listens.
    // No token, no route table, no payloads.
    println!("relay node id {}", server.node_id());
    println!("relay listening port {}", server.bound_port().unwrap_or(0));
    if args.local {
        println!("relay mode loopback-only");
    }
    // Deliberately no value, no fingerprint, no length.
    println!(
        "relay token {}",
        if token_required {
            "required"
        } else {
            "not required"
        }
    );
    println!("relay ready");

    let stop = shutdown_signal();
    server.run_until(stop).await;

    println!("relay stopped");
    ExitCode::SUCCESS
}

/// Resolve when SIGINT or SIGTERM arrives.
///
/// A relay is a service: closing on a signal has to be quick and quiet. If the
/// signal handlers cannot be installed, fall back to running until killed rather
/// than exiting on start.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(term) => term,
            Err(_) => return std::future::pending().await,
        };
        let mut int = match signal(SignalKind::interrupt()) {
            Ok(int) => int,
            Err(_) => return std::future::pending().await,
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
