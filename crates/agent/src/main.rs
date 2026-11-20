//! `syswatch-agent` — the SysWatch HTTP agent.
//!
//! Serves the dashboard API from Rust: `/api/health`, `/api/current`,
//! `/api/sensors`, `/api/series`, `/api/consumers`, `/api/apps`, and the six
//! history/hardware endpoints (`/api/graph`, `/api/history`, `/api/power`,
//! `/api/analytics`, `/api/battery`, `/api/thermal`), with the dashboard's
//! static files alongside. `dashboard/server.py` stays the live server on 8123;
//! this binary is designed to run beside it on a test port (`--port 8199`) until
//! every endpoint has a Rust implementation.
//!
//! Startup order matters and is deliberate:
//!
//! 1. parse the flags (a typo is an error, never a silent default);
//! 2. build ONE snapshot synchronously, so the first request after the port
//!    opens already has real data (no 503 on a cold start);
//! 3. start the sampler thread that refreshes it every tick — the HTTP
//!    handlers never sample, they only read the slot — and feed the same
//!    snapshot into the history ring that `/api/graph` and `/api/history` plot;
//! 4. bind loopback only, and print the ready line the tooling greps for.
//!
//! Nothing in this binary writes to the collector's log, the root daemon's
//! `sample.json`, the battery history, or any other process's file.

mod analytics;
mod battery;
mod cascade_write;
mod cli;
mod collector;
mod consumers;
mod graph;
mod gpu;
mod histdata;
mod history;
mod ioreg;
mod power;
mod routes;
mod sampler;
mod sensors;
mod series;
mod thermal;
mod timefmt;

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use cli::{Args, Cli};
use histdata::History;
use routes::AppState;
use sampler::Sampler;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match Args::parse(argv) {
        Ok(Cli::Run(a)) => a,
        Ok(Cli::Help(text)) => {
            println!("{text}");
            return;
        }
        Ok(Cli::Version(v)) => {
            println!("syswatch-agent {v}");
            return;
        }
        Err(msg) => {
            eprintln!("syswatch-agent: {msg}");
            std::process::exit(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("syswatch-agent: could not start the async runtime: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = runtime.block_on(serve(args)) {
        eprintln!("syswatch-agent: {e}");
        std::process::exit(1);
    }
}

fn log(msg: &str) {
    eprintln!("{} syswatch-agent: {msg}", timefmt::rfc3339_utc(timefmt::now_unix()));
}

async fn serve(args: Args) -> Result<(), String> {
    let start = Instant::now();

    if !args.static_dir.join("index.html").is_file() {
        log(&format!(
            "warning: {} has no index.html — the API will work, the UI will 404",
            args.static_dir.display()
        ));
    }

    // 1. First snapshot, synchronously: the port does not open with an empty
    //    slot, so `/api/current` answers on the first request.
    let mut sampler = Sampler::new(args.logs_dir.clone());
    // T18: only a run that asked for it writes the cascade; the default agent
    // still owns no file.
    if let Some(dir) = args.cascade_dir.clone() {
        sampler = sampler.with_cascade(cascade_write::CascadeWriter::new(dir.clone()));
        log(&format!("cascade store: writing this agent's samples to {}", dir.display()));
    }
    let first = sampler.build();
    log(&format!(
        "sampler primed: {} logical CPUs in {} cluster(s), {}% cpu, {} MiB used",
        sampler.ncpu(),
        sampler.clusters().len(),
        first["cpu"]["total_pct"],
        first["ram"]["used_bytes"].as_u64().unwrap_or(0) / 1_048_576,
    ));
    if first["collector"]["available"] == serde_json::json!(false) {
        log(&format!(
            "collector log: {} (the agent's own sensors still fill /api/current)",
            first["collector"]["reason"].as_str().unwrap_or("unavailable")
        ));
    } else {
        log(&format!(
            "collector log: {} (age {} s)",
            first["collector"]["path"].as_str().unwrap_or("?"),
            first["collector"]["age_s"],
        ));
    }

    let snapshot: Arc<RwLock<Option<serde_json::Value>>> = Arc::new(RwLock::new(Some(first)));

    // The history ring shares the sampler's snapshots: `/api/graph` and
    // `/api/history` plot the last two hours of them at 1 s, which is what makes
    // a short window meaningful even when the collector is not running.
    let history = Arc::new(History::new(args.logs_dir.clone()));
    if let Some(snap) = snapshot.read().ok().and_then(|g| g.clone()) {
        history.push(&snap);
    }

    // 2. Sampler thread. A plain OS thread, not a tokio task: the work is
    //    blocking (ioreg spawns, sysinfo process sweeps) and must never sit on
    //    the runtime the handlers use.
    {
        let slot = Arc::clone(&snapshot);
        let ring = Arc::clone(&history);
        let tick = Duration::from_millis(args.tick_ms);
        std::thread::Builder::new()
            .name("syswatch-sampler".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(tick);
                    let snap = sampler.build();
                    ring.push(&snap);
                    match slot.write() {
                        Ok(mut w) => *w = Some(snap),
                        Err(_) => break, // poisoned: shut the sampler down
                    }
                }
            })
            .map_err(|e| format!("could not start the sampler thread: {e}"))?;
    }

    // 3. Bind. Loopback only, exactly like the Python server.
    let addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        format!(
            "cannot bind {addr}: {e}\n  \
             the live dashboard is dashboard/server.py on 8123 — start the agent on \
             another port, e.g. --port 8199"
        )
    })?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("bound socket has no address: {e}"))?
        .port();

    let state = AppState {
        start,
        port,
        snapshot,
        static_dir: args.static_dir.clone(),
        logs_dir: args.logs_dir.clone(),
        data_dir: args.data_dir.clone(),
        history,
        tick_ms: args.tick_ms,
    };

    log(&format!(
        "serving /api/{{health,current,sensors,series,consumers,apps,graph,history,power,analytics,battery,thermal}} \
         on http://{addr} (static: {}, tick {} ms)",
        args.static_dir.display(),
        args.tick_ms
    ));
    // The Python server prints "SYSWATCH_DASHBOARD_READY on port N"; tooling
    // that starts either binary greps one pattern, so keep the same shape.
    println!("SYSWATCH_AGENT_READY on port {port}");

    axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("server stopped: {e}"))?;
    log("shutdown complete");
    Ok(())
}

/// Ctrl-C or SIGTERM: stop accepting, let in-flight requests finish. The
/// Python server handles SIGTERM the same way (T4a #3) so a supervisor script
/// can stop either implementation with one signal.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let term = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}
