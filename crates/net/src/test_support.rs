//! Test-only helpers shared by `viewer` and `stream`.
//!
//! Both halves of T29 need the same two things: a *real* paired channel between
//! two loopback endpoints (pairing is the only way to get the session keys, so a
//! fake `PairedConnection` would test nothing), and a snapshot body shaped like
//! a live `/api/current`.
//!
//! The pairing flow here is the same one `pairing.rs`'s end-to-end tests run —
//! `start_pairing_local` on one identity, the pasted ticket on the other — kept
//! in one place because two more modules now need it.

use std::path::PathBuf;
use std::time::Duration;

use iroh_base::key::PublicKey;
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::pairing::{PairedConnection, accept_pairing_local, start_pairing_local};
use crate::{DeviceIdentity, PairingCode};

/// Pairing is local and fast, but a hung handshake must fail the test rather
/// than hang the suite.
const PAIR_TIMEOUT: Duration = Duration::from_secs(20);

/// An identity file unique to this test, removed on drop.
pub(crate) struct TempIdentity {
    path: PathBuf,
    pub(crate) identity: DeviceIdentity,
}

impl TempIdentity {
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("syswatch_t29_{name}.key"));
        let _ = std::fs::remove_file(&path);
        let identity = DeviceIdentity::load_or_create(&path).expect("identity");
        Self { path, identity }
    }
}

impl Drop for TempIdentity {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Two sides of one completed pairing.
pub(crate) struct Pairing {
    /// The side that minted the code — in T29 terms, the device being watched.
    pub(crate) device: PairedConnection,
    /// The side that pasted the ticket — the viewer.
    pub(crate) viewer: PairedConnection,
    pub(crate) device_id: PublicKey,
    pub(crate) viewer_id: PublicKey,
}

/// Pair two loopback endpoints and return both ends.
pub(crate) async fn pair_loopback(name: &str) -> Pairing {
    let device_identity = TempIdentity::new(&format!("{name}_device"));
    let viewer_identity = TempIdentity::new(&format!("{name}_viewer"));

    let (code, handle) = timeout(PAIR_TIMEOUT, start_pairing_local(&device_identity.identity))
        .await
        .expect("start_pairing timed out")
        .expect("start_pairing failed");

    // The viewer only ever sees the ticket string, as a user would.
    let entered = PairingCode::parse(&code.ticket()).expect("ticket must parse");

    let listener = tokio::spawn(async move {
        timeout(PAIR_TIMEOUT, handle.wait_for_peer())
            .await
            .expect("wait_for_peer timed out")
            .expect("wait_for_peer failed")
    });

    let viewer = timeout(
        PAIR_TIMEOUT,
        accept_pairing_local(&viewer_identity.identity, &entered),
    )
    .await
    .expect("accept_pairing timed out")
    .expect("accept_pairing failed");

    let device = listener.await.expect("listener task panicked");

    Pairing {
        device,
        viewer,
        device_id: device_identity.identity.node_id(),
        viewer_id: viewer_identity.identity.node_id(),
    }
}

/// A snapshot body shaped like a live `/api/current` response.
///
/// Key order, nesting and nullability follow `curl 127.0.0.1:8200/api/current`
/// on this machine (the Rust agent, taken 2026-11-20): every block a UI reads is
/// present, `read_iops` is `null` because the SDK exposes no op counters, and
/// the legacy `memory` alias the Python server still emits rides along.
///
/// `tick` is stamped into `agent.ticks` and into `cpu.total_pct`, so a test can
/// tell two sends apart without trusting arrival order.
pub(crate) fn sample_snapshot(tick: u64) -> Value {
    json!({
        "ts": format!("2026-11-20T00:41:{:02}Z", tick % 60),
        "source": "syswatch-agent (rust)",
        "cpu": {
            "total_pct": 12.5 + tick as f64,
            "per_core_pct": [21.6, 18.0, 19.1, 12.6, 9.0, 4.5, 0.9, 0.9],
            "freq_mhz": 3504.0,
            "cores": 8,
            "clusters": [
                {"index": 0, "name": "efficiency", "cores": 4, "util_pct": 17.8},
                {"index": 4, "name": "performance", "cores": 4, "util_pct": 3.9}
            ],
            "cluster_basis": "efficiency cores occupy logical CPUs 0..E-1",
            "load": {"one": 2.26, "five": 2.886, "fifteen": 3.858}
        },
        "ram": {
            "used_bytes": 6072057856u64,
            "total_bytes": 8589934592u64,
            "available_bytes": 2517876736u64,
            "pct": 70.7,
            "swap_used_bytes": 1458372608u64,
            "swap_total_bytes": 2147483648u64
        },
        "net": {
            "ifaces": [{"name": "en0", "down_bps": 10243.0, "up_bps": 2829.4}],
            "down_bps": 10243.0,
            "up_bps": 2829.4
        },
        "temps_c": [{"label": "Battery 1", "c": 30.5}],
        "processes": {
            "by_cpu": [{"pid": 75497, "name": "opencode", "cpu_pct": 5.8, "mem_mb": 255.9}],
            "by_mem": [{"pid": 75497, "name": "opencode", "cpu_pct": 5.8, "mem_mb": 255.9}]
        },
        "proc_count": 431,
        "boot_ts": "2026-11-17T15:59:46Z",
        "load": {"one": 2.26, "five": 2.886, "fifteen": 3.858},
        "battery": {"pct": 80.0, "charging": false, "cycles": 350.0, "health_pct": 80.9},
        "gpu": {"available": true, "util_pct": 0.0, "mem_used_mb": 454.6, "cores": 10},
        "disk_io": {"read_bps": 13273666.1, "write_bps": 565394.5, "read_iops": null},
        "disk": {"available": true, "used_pct": 64.17, "mount": "/System/Volumes/Data"},
        "pressure": {"cpu": {"available": true, "level": "normal", "pct": 28.2}},
        "unavailable": {"neural_engine": "powermetrics ANEMetrics needs root"},
        "memory": {"used_bytes": 6072057856u64, "pct": 70.7},
        "power": {"available": true, "package_w": 0.363},
        "collector": {"available": true, "temperature_c": 51.85},
        "agent": {"version": "0.4.0", "pid": 91572, "ticks": tick}
    })
}
