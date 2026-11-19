//! The viewer half of a paired connection: read-only metric snapshots.
//!
//! # What a viewer is
//!
//! A viewer is simply the *other* side of a [`PairedConnection`]: the phone (or
//! laptop, or second mac) that completed pairing with a device and now wants to
//! watch it. There is no separate login, no token and no registration — the
//! pairing handshake is the whole trust story, so by the time a
//! [`PairedConnection`] exists the channel is authenticated, encrypted and bound
//! to both node ids.
//!
//! # Why `connect` is not async
//!
//! The network work is [`crate::accept_pairing`]: it dials, runs the X25519 +
//! signature handshake and only returns once the channel is live. Handing that
//! `PairedConnection` to [`ViewerSession::connect`] therefore cannot fail and
//! needs no `await`. The brief's `connect(paired) -> Self` signature is exactly
//! this split, and keeping it makes the dial explicit at the call site instead
//! of hiding a 20 s handshake inside a constructor.
//!
//! # Wire format
//!
//! One snapshot per paired message, and the payload is the snapshot JSON object
//! *exactly* as `/api/current` returns it — no envelope, no length prefix (the
//! QUIC stream boundary is the frame, see [`PairedConnection::send`]).
//!
//! No envelope is deliberate: the device already has to produce this shape for
//! its own HTTP endpoint, and a viewer that wants to display the same tiles
//! wants the same keys. A JSON object is also self-identifying, so the viewer
//! can reject anything else ([`NetError::BadSnapshot`]) instead of decoding a
//! half-trusted frame into zeroed fields.
//!
//! # Read-only, by construction
//!
//! [`ViewerSession`] has no send path at all — no `send`, no `request`, no
//! command channel. `PairedConnection` is bidirectional at the transport level,
//! but the viewer API exposes only [`ViewerSession::next_snapshot`], so a
//! compromised or curious viewer cannot drive the device through this type. The
//! reverse direction stays with the device ([`crate::stream::MetricStream`]).
//!
//! # Unknown fields
//!
//! [`Snapshot`] types the blocks a UI actually reads and keeps every other
//! top-level key verbatim in [`Snapshot::extra`]. A device that adds a block
//! later still shows up on an older viewer instead of failing to deserialize,
//! and a block that is genuinely unavailable stays `null` — never `0`.

use std::collections::BTreeMap;

use iroh_base::key::PublicKey;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::pairing::PairedConnection;
use crate::NetError;

/// Receives metric snapshots from a paired device.
///
/// The device sends one snapshot per tick ([`crate::stream::METRIC_TICK`]);
/// every call to [`ViewerSession::next_snapshot`] waits for the next one.
pub struct ViewerSession {
    paired: PairedConnection,
}

impl std::fmt::Debug for ViewerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerSession")
            .field("peer", &self.paired.peer_node_id().fmt_short())
            .finish_non_exhaustive()
    }
}

impl ViewerSession {
    /// Wrap an already-paired channel as a viewer.
    ///
    /// Infallible on purpose: the handshake that proves the peer is
    /// [`crate::accept_pairing`]'s job, and it has already run.
    pub fn connect(paired: PairedConnection) -> Self {
        Self { paired }
    }

    /// The device's ed25519 node id, verified during pairing.
    pub fn peer_node_id(&self) -> PublicKey {
        self.paired.peer_node_id()
    }

    /// The underlying channel, for callers that need the live node address.
    pub fn paired(&self) -> &PairedConnection {
        &self.paired
    }

    /// Wait for the device's next snapshot and decrypt it.
    ///
    /// Fails with [`NetError::Crypto`] if the frame does not authenticate
    /// (forged or replayed), with [`NetError::Stream`] if the device went away,
    /// and with [`NetError::BadSnapshot`] if the payload is not a snapshot
    /// object. There is no "stale snapshot" path: a tick that never arrives is
    /// an error the caller can time out, not a repeat of the previous one.
    pub async fn next_snapshot(&self) -> Result<Snapshot, NetError> {
        let bytes = self.paired.recv().await?;
        Snapshot::from_json(&bytes)
    }

    /// Close the channel politely.
    pub fn close(self) {
        self.paired.close();
    }
}

// --- snapshot shape ------------------------------------------------------

/// One `/api/current` payload, as sent by the device.
///
/// Typed where a UI binds a value, verbatim [`Value`] where the block is still
/// moving (battery, GPU, disk, pressure, power, collector, agent). Keys this
/// struct does not name land in [`Snapshot::extra`]; missing keys and `null`
/// both parse as `None`/empty, because "the device has no number for this" must
/// never render as `0`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// `2026-11-20T00:41:07Z` — when the device took the sample.
    #[serde(default)]
    pub ts: Option<String>,
    /// Which implementation produced it (`psutil`, `syswatch-agent (rust)`, …).
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub cpu: Cpu,
    /// `ram` block. The Python server's legacy `memory` alias is a duplicate of
    /// this block and is preserved verbatim in [`Snapshot::extra`] rather than
    /// typed twice.
    #[serde(default)]
    pub ram: Ram,
    #[serde(default)]
    pub net: Net,
    #[serde(default)]
    pub temps_c: Vec<Temperature>,
    #[serde(default)]
    pub processes: Processes,
    #[serde(default, deserialize_with = "opt_u32")]
    pub proc_count: Option<u32>,
    #[serde(default)]
    pub boot_ts: Option<String>,
    #[serde(default)]
    pub load: Load,
    /// Battery block (`pct`, `health_pct`, `charging`, …) — verbatim.
    #[serde(default)]
    pub battery: Value,
    /// GPU block (`util_pct`, `mem_used_mb`, …) — verbatim.
    #[serde(default)]
    pub gpu: Value,
    /// Disk throughput — verbatim.
    #[serde(default)]
    pub disk_io: Value,
    /// Disk capacity + volumes — verbatim.
    #[serde(default)]
    pub disk: Value,
    /// CPU / memory / storage pressure — verbatim.
    #[serde(default)]
    pub pressure: Value,
    /// Package power from the privileged daemon — verbatim.
    #[serde(default)]
    pub power: Value,
    /// What the device cannot measure, with the reason — verbatim.
    #[serde(default)]
    pub unavailable: Value,
    /// What the collector log contributed — verbatim.
    #[serde(default)]
    pub collector: Value,
    /// Device-side agent metadata (`version`, `pid`, `ticks`, …) — verbatim.
    #[serde(default)]
    pub agent: Value,
    /// Every other top-level key, unchanged.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Snapshot {
    /// Parse one stream message into a snapshot.
    ///
    /// Accepts only a JSON **object**: an array, a string or a bare number is
    /// [`NetError::BadSnapshot`], not an empty [`Snapshot`] that looks like a
    /// device with no metrics.
    pub fn from_json(bytes: &[u8]) -> Result<Self, NetError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|err| NetError::BadSnapshot(format!("not JSON: {err}")))?;
        if !value.is_object() {
            return Err(NetError::BadSnapshot(format!(
                "payload is a JSON {}, not an /api/current object",
                json_kind(&value)
            )));
        }
        serde_json::from_value(value)
            .map_err(|err| NetError::BadSnapshot(format!("not an /api/current snapshot: {err}")))
    }

    /// The snapshot as JSON, so a viewer can serve it onwards unchanged
    /// (`/api/current` on a relay, a file, a log line).
    pub fn to_json(&self) -> Result<Value, NetError> {
        serde_json::to_value(self)
            .map_err(|err| NetError::BadSnapshot(format!("could not re-encode snapshot: {err}")))
    }

    /// The device's CPU total, when it reported one.
    pub fn cpu_pct(&self) -> Option<f64> {
        self.cpu.total_pct
    }

    /// The device's RAM percentage, when it reported one.
    pub fn ram_pct(&self) -> Option<f64> {
        self.ram.pct
    }
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `cpu` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cpu {
    #[serde(default)]
    pub total_pct: Option<f64>,
    #[serde(default)]
    pub per_core_pct: Vec<f64>,
    #[serde(default)]
    pub freq_mhz: Option<f64>,
    #[serde(default, deserialize_with = "opt_u32")]
    pub cores: Option<u32>,
    /// Efficiency/performance rows, index-ordered (`0..E-1` efficiency,
    /// `E..` performance) exactly as the device reported them.
    #[serde(default)]
    pub clusters: Vec<Cluster>,
    #[serde(default)]
    pub cluster_basis: Option<String>,
    #[serde(default)]
    pub load: Load,
}

/// One row of `cpu.clusters`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cluster {
    #[serde(default, deserialize_with = "opt_u32")]
    pub index: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "opt_u32")]
    pub cores: Option<u32>,
    #[serde(default)]
    pub util_pct: Option<f64>,
    #[serde(default)]
    pub freq_mhz: Option<f64>,
}

/// `ram` block. `pct` is `used_bytes / total_bytes`, the same basis `used_bytes`
/// is reported on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ram {
    #[serde(default, deserialize_with = "opt_u64")]
    pub used_bytes: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub total_bytes: Option<u64>,
    /// Present on the Rust agent (`total − used`), absent on the psutil server.
    #[serde(default, deserialize_with = "opt_u64")]
    pub available_bytes: Option<u64>,
    #[serde(default)]
    pub pct: Option<f64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub swap_used_bytes: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub swap_total_bytes: Option<u64>,
}

/// `net` block: per-interface rates plus their sum.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Net {
    #[serde(default)]
    pub ifaces: Vec<Iface>,
    #[serde(default)]
    pub down_bps: Option<f64>,
    #[serde(default)]
    pub up_bps: Option<f64>,
}

/// One interface row of `net.ifaces`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Iface {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub down_bps: Option<f64>,
    #[serde(default)]
    pub up_bps: Option<f64>,
}

/// One row of `temps_c`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Temperature {
    #[serde(default)]
    pub label: Option<String>,
    /// Degrees Celsius. `None` means the device has no sensor reading, not 0 °C.
    #[serde(default)]
    pub c: Option<f64>,
}

/// `processes` block: the device's top rows by CPU and by memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Processes {
    #[serde(default)]
    pub by_cpu: Vec<ProcessRow>,
    #[serde(default)]
    pub by_mem: Vec<ProcessRow>,
}

/// One process row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessRow {
    #[serde(default, deserialize_with = "opt_u64")]
    pub pid: Option<u64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cpu_pct: Option<f64>,
    #[serde(default)]
    pub mem_mb: Option<f64>,
}

/// Load averages, also embedded in `cpu.load`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Load {
    #[serde(default)]
    pub one: Option<f64>,
    #[serde(default)]
    pub five: Option<f64>,
    #[serde(default)]
    pub fifteen: Option<f64>,
}

// --- tolerant number readers --------------------------------------------

// The two implementations of `/api/current` agree on numbers today (integers
// stay integers), but a viewer is the wrong place to be strict about it: a
// field that arrives as `486.0` instead of `486` must not cost the whole
// snapshot. These accept a JSON number in either representation and treat
// `null`, a missing key, or a non-numeric value as "no value" rather than an
// error — a UI can then say "unavailable" instead of failing to connect.

fn opt_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(number(deserializer)?.and_then(|n| {
        n.as_u64()
            .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
    }))
}

fn opt_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(number(deserializer)?.and_then(|n| {
        n.as_u64()
            .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
            .and_then(|n| u32::try_from(n).ok())
    }))
}

fn number<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<Value>::deserialize(deserializer)?
        .filter(|value| value.is_number()))
}

// --- tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::MetricStream;
    use crate::test_support::{pair_loopback, sample_snapshot};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;
    use tokio::time::timeout;

    const T: Duration = Duration::from_secs(20);

    #[test]
    fn parses_a_real_api_current_payload() {
        // Shape copied from a live `curl 127.0.0.1:8200/api/current` on this
        // machine, with the long justification strings shortened.
        let body = serde_json::json!({
            "ts": "2026-11-20T00:41:07Z",
            "source": "syswatch-agent (rust)",
            "cpu": {
                "total_pct": 10.8,
                "per_core_pct": [21.6, 18.0, 19.1, 12.6],
                "freq_mhz": 3504.0,
                "cores": 8,
                "clusters": [
                    {"index": 0, "name": "efficiency", "cores": 4, "util_pct": 17.8},
                    {"index": 4, "name": "performance", "cores": 4, "util_pct": 3.9}
                ],
                "cluster_basis": "efficiency cores occupy logical CPUs 0..E-1",
                "load": {"one": 2.26, "five": 2.886, "fifteen": 3.858}
            },
            "ram": {"used_bytes": 6072057856u64, "total_bytes": 8589934592u64,
                    "available_bytes": 2517876736u64, "pct": 70.7,
                    "swap_used_bytes": 1458372608u64, "swap_total_bytes": 2147483648u64},
            "net": {"ifaces": [{"name": "en0", "down_bps": 10243.0, "up_bps": 2829.4}],
                    "down_bps": 10243.0, "up_bps": 2829.4},
            "temps_c": [{"label": "Battery 1", "c": 30.5}],
            "processes": {"by_cpu": [{"pid": 75497, "name": "opencode", "cpu_pct": 5.8, "mem_mb": 255.9}],
                          "by_mem": [{"pid": 67419, "name": "Chrome Helper", "cpu_pct": 5.8, "mem_mb": 479.4}]},
            "proc_count": 431,
            "boot_ts": "2026-11-17T15:59:46Z",
            "load": {"one": 2.26, "five": 2.886, "fifteen": 3.858},
            "battery": {"pct": 80.0, "charging": false, "temp_c": 30.39, "cycles": 350.0},
            "gpu": {"available": true, "util_pct": 0.0, "mem_used_mb": 454.6},
            "disk_io": {"read_bps": 13273666.1, "write_bps": 565394.5, "read_iops": null},
            "disk": {"available": true, "used_pct": 64.17, "volumes": [{"mount": "/", "used_pct": 64.17}]},
            "pressure": {"cpu": {"available": true, "level": "normal"}},
            "unavailable": {"neural_engine": "powermetrics ANEMetrics needs root"},
            // legacy alias the Python server still emits for alert rules
            "memory": {"used_bytes": 6072057856u64, "pct": 70.7},
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let snap = Snapshot::from_json(&bytes).expect("real payload must parse");

        assert_eq!(snap.ts.as_deref(), Some("2026-11-20T00:41:07Z"));
        assert_eq!(snap.cpu.total_pct, Some(10.8));
        assert_eq!(snap.cpu.per_core_pct.len(), 4);
        assert_eq!(snap.cpu.cores, Some(8));
        assert_eq!(snap.cpu.clusters.len(), 2);
        assert_eq!(snap.cpu.clusters[1].name.as_deref(), Some("performance"));
        assert_eq!(snap.cpu.load.fifteen, Some(3.858));
        assert_eq!(snap.ram.total_bytes, Some(8_589_934_592));
        assert_eq!(snap.ram.pct, Some(70.7));
        assert_eq!(snap.net.ifaces[0].name.as_deref(), Some("en0"));
        assert_eq!(snap.temps_c[0].c, Some(30.5));
        assert_eq!(snap.processes.by_cpu[0].pid, Some(75497));
        assert_eq!(snap.proc_count, Some(431));
        assert_eq!(snap.load.one, Some(2.26));
        assert_eq!(snap.gpu["util_pct"], serde_json::json!(0.0));
        assert!(snap.disk_io["read_iops"].is_null(), "null must stay null");
        assert!(snap.unavailable.is_object());

        // The duplicate alias is preserved, not dropped and not confused with
        // the typed `ram` block.
        assert_eq!(snap.extra["memory"]["pct"], serde_json::json!(70.7));
        assert!(!snap.extra.contains_key("battery"), "typed keys stay out of extra");

        // Re-encoding keeps every key it was given.
        let back = snap.to_json().unwrap();
        assert_eq!(back["ts"], body["ts"]);
        assert_eq!(back["memory"], body["memory"]);
        assert_eq!(back["gpu"], body["gpu"]);
    }

    #[test]
    fn missing_and_null_fields_are_none_not_zero() {
        // The minimal payload: no blocks at all. Every number is absent, and a
        // viewer must be able to tell "not reported" from 0.
        let snap = Snapshot::from_json(br#"{"ts":"2026-11-20T00:00:00Z"}"#).unwrap();
        assert_eq!(snap.cpu.total_pct, None);
        assert_eq!(snap.ram.pct, None);
        assert_eq!(snap.proc_count, None);
        assert!(snap.cpu.per_core_pct.is_empty());
        assert!(snap.battery.is_null());

        // Explicit nulls read the same way.
        let nulls = Snapshot::from_json(br#"{"cpu":{"total_pct":null},"proc_count":null,"ram":{"used_bytes":null}}"#)
            .unwrap();
        assert_eq!(nulls.cpu.total_pct, None);
        assert_eq!(nulls.proc_count, None);
        assert_eq!(nulls.ram.used_bytes, None);

        // And an integral float is accepted where an int is expected, rather
        // than losing the snapshot over a JSON spelling.
        let floaty = Snapshot::from_json(br#"{"proc_count":486.0,"cpu":{"cores":8.0}}"#).unwrap();
        assert_eq!(floaty.proc_count, Some(486));
        assert_eq!(floaty.cpu.cores, Some(8));
    }

    #[test]
    fn non_snapshot_payloads_are_rejected() {
        for bad in [
            &b""[..],
            b"not json",
            b"[]",
            b"42",
            b"\"a string\"",
            b"null",
            b"{\"cpu\": \"fast\"}", // a block of the wrong type
        ] {
            let err = Snapshot::from_json(bad).expect_err(&format!(
                "{} must not parse as a snapshot",
                String::from_utf8_lossy(bad)
            ));
            assert!(matches!(err, NetError::BadSnapshot(_)), "got {err}");
        }
    }

    #[tokio::test]
    async fn device_streams_and_viewer_reads_every_tick() {
        let pair = pair_loopback("t29_roundtrip").await;
        let device_id = pair.device_id;
        let viewer_id = pair.viewer_id;

        // Identity first: each side sees the other's paired node id.
        assert_eq!(pair.device.peer_node_id(), viewer_id);
        let session = ViewerSession::connect(pair.viewer);
        assert_eq!(session.peer_node_id(), device_id);

        // Device side: pull a fresh snapshot from the closure on every tick and
        // send it. The closure counts, so each snapshot is distinguishable.
        let counter = Arc::new(AtomicU64::new(0));
        let pulled = counter.clone();
        let stream = MetricStream::new(
            pair.device,
            Box::new(move || sample_snapshot(pulled.fetch_add(1, Ordering::SeqCst))),
        )
        .with_tick(Duration::from_millis(20));
        let device_task = tokio::spawn(stream.run());

        let mut got = Vec::new();
        for _ in 0..3 {
            let snap = timeout(T, session.next_snapshot())
                .await
                .expect("viewer timed out waiting for a snapshot")
                .expect("snapshot failed");
            got.push(snap);
        }

        // The ticks arrive in order, one snapshot per send, with the values the
        // device actually produced (not defaults, not zeroes).
        for (i, snap) in got.iter().enumerate() {
            assert_eq!(snap.agent["ticks"], serde_json::json!(i as u64), "tick {i}");
            assert_eq!(snap.cpu.total_pct, Some(12.5 + i as f64));
            assert_eq!(snap.ram.pct, Some(70.7));
            assert_eq!(snap.proc_count, Some(431));
            assert_eq!(snap.net.ifaces.len(), 1);
            assert_eq!(snap.processes.by_cpu.len(), 1);
        }
        // At least three pulls happened, one per send. (Not exactly three: the
        // device may already have pulled the next tick while this assertion
        // runs — the ordering check above is the property that matters.)
        assert!(
            counter.load(Ordering::SeqCst) >= 3,
            "expected at least three pulls, got {}",
            counter.load(Ordering::SeqCst)
        );

        // The viewer hanging up ends the device's stream instead of leaving it
        // looping into a dead channel.
        session.close();
        let ended = timeout(T, device_task)
            .await
            .expect("device stream hung after the viewer closed")
            .expect("device task panicked");
        assert!(ended.is_err(), "a closed channel must end run() with an error");
    }

    #[tokio::test]
    async fn garbage_from_the_device_is_an_error_not_a_snapshot() {
        let pair = pair_loopback("t29_garbage").await;
        let session = ViewerSession::connect(pair.viewer);

        // A paired device can send anything it likes; the viewer must not turn
        // that into an empty snapshot.
        pair.device.send(b"{\"not\": \"a snapshot\"} trailing").await.unwrap();
        let err = timeout(T, session.next_snapshot())
            .await
            .expect("timed out")
            .expect_err("truncated JSON must not parse");
        assert!(matches!(err, NetError::BadSnapshot(_)), "got {err}");

        // A JSON array is well-formed and still not a snapshot.
        pair.device.send(b"[1,2,3]").await.unwrap();
        let err = timeout(T, session.next_snapshot())
            .await
            .expect("timed out")
            .expect_err("an array must not parse as a snapshot");
        assert!(matches!(err, NetError::BadSnapshot(_)), "got {err}");

        // And a real one still works afterwards: a bad frame does not poison
        // the channel or the cipher.
        pair.device
            .send(&serde_json::to_vec(&sample_snapshot(7)).unwrap())
            .await
            .unwrap();
        let snap = timeout(T, session.next_snapshot())
            .await
            .expect("timed out")
            .expect("a good snapshot after a bad one must still arrive");
        assert_eq!(snap.agent["ticks"], serde_json::json!(7));

        session.close();
    }
}
