//! The device half of a paired connection: send the current snapshot each tick.
//!
//! [`MetricStream`] is the mirror image of [`crate::viewer::ViewerSession`]: the
//! device owns the data, so it owns the send loop, and the viewer only ever
//! reads. One snapshot per tick, one paired message per snapshot, payload =
//! the `/api/current` JSON object — the same shape the device already serves
//! over HTTP, so a viewer's tiles and a browser's tiles read the same keys.
//!
//! # Why a closure instead of a snapshot value
//!
//! The tick loop cannot be handed one snapshot: the point is that the numbers
//! change. `snapshot_fn` is called once per tick, so the caller decides where
//! the data comes from (`/api/current`'s cached `Value`, a sampler, a fixture)
//! and this module never learns about `sysinfo`, sensors or the HTTP layer.
//! It is `Fn`, not `FnMut`: the loop may be shared behind a spawn, and interior
//! mutability is the caller's choice (a counter in the tests, a `Mutex` around
//! a sampler in production) rather than a requirement leaked here.
//!
//! # What stops it
//!
//! * [`MetricStream::run`] returns when the channel breaks — the viewer hung up,
//!   the device moved networks, the session was closed. That is an `Err`, not a
//!   silent `Ok`: a device that was supposed to be streaming and is not is a
//!   failure the caller has to see.
//! * [`MetricStream::run_until`] returns `Ok(())` when a caller-supplied future
//!   fires, which is how a device shuts the stream down on its own terms
//!   (SIGTERM, a menu-bar toggle, a test).
//!
//! A failed tick never retries into the same broken channel: one failure ends
//! the loop, and the caller re-pairs.

use std::sync::Mutex;
use std::time::Duration;

use iroh_base::key::PublicKey;
use serde_json::Value;

use crate::pairing::{PairedConnection, MAX_PAIRED_BYTES};
use crate::NetError;

/// The device's default cadence: one snapshot per second, matching the 1 s
/// sampler tick every platform agent already runs.
pub const METRIC_TICK: Duration = Duration::from_secs(1);

/// Sends the device's current snapshot to a paired viewer every tick.
///
/// The closure sits behind a `Mutex` for exactly one reason: `Sync`. A future
/// that borrows `&MetricStream` across an await is `Send` only if the borrow
/// target is `Sync`, and `Box<dyn Fn() + Send>` is not — so without this, the
/// obvious `tokio::spawn(stream.run())` (and any `Arc<MetricStream>` a
/// supervisor might hold) would fail to compile. The lock is held for one
/// closure call, with no `await` inside, so it costs nothing at 1 Hz.
pub struct MetricStream {
    paired: PairedConnection,
    snapshot_fn: Mutex<Box<dyn Fn() -> Value + Send>>,
    tick: Duration,
}

impl std::fmt::Debug for MetricStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricStream")
            .field("peer", &self.paired.peer_node_id().fmt_short())
            .field("tick", &self.tick)
            .finish_non_exhaustive()
    }
}

impl MetricStream {
    /// Build a stream on an already-paired channel.
    ///
    /// Defaults to [`METRIC_TICK`]; use [`MetricStream::with_tick`] for a
    /// different cadence. `snapshot_fn` is called on the tick, not at
    /// construction, so the first message carries the state at send time.
    pub fn new(paired: PairedConnection, snapshot_fn: Box<dyn Fn() -> Value + Send>) -> Self {
        Self {
            paired,
            snapshot_fn: Mutex::new(snapshot_fn),
            tick: METRIC_TICK,
        }
    }

    /// Send every `tick` instead of every second.
    ///
    /// A zero tick is honoured as "no sleep": the loop then sends as fast as the
    /// channel accepts, which is a load test rather than a cadence.
    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }

    /// The cadence this stream runs at.
    pub fn tick(&self) -> Duration {
        self.tick
    }

    /// The viewer this stream is sending to.
    pub fn peer_node_id(&self) -> PublicKey {
        self.paired.peer_node_id()
    }

    /// Take one snapshot from `snapshot_fn` and encode it as the message body.
    ///
    /// Rejects a `snapshot_fn` that does not return a JSON **object**: the
    /// viewer parses `/api/current` shape, and sending `null` or a bare number
    /// would be a protocol bug worth failing on at the sender, where the reason
    /// is still visible.
    fn pull(&self) -> Result<Vec<u8>, NetError> {
        let snapshot = {
            // `into_inner` on poisoning: a closure that panicked once must not
            // turn every later tick into a panic on the mutex instead.
            let snapshot_fn = self
                .snapshot_fn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            snapshot_fn()
        };
        if !snapshot.is_object() {
            return Err(NetError::Protocol(
                "snapshot_fn returned a non-object: a metric snapshot is the /api/current object"
                    .into(),
            ));
        }
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|err| NetError::Protocol(format!("snapshot is not serialisable: {err}")))?;
        if bytes.len() > MAX_PAIRED_BYTES {
            return Err(NetError::TooLarge(bytes.len(), MAX_PAIRED_BYTES));
        }
        Ok(bytes)
    }

    /// Take one snapshot and send it as one message; returns the payload size.
    pub async fn send_once(&self) -> Result<usize, NetError> {
        let bytes = self.pull()?;
        self.paired.send(&bytes).await?;
        Ok(bytes.len())
    }

    /// Send a snapshot every tick until the channel fails.
    ///
    /// Runs forever by design: a device streams for as long as the viewer is
    /// there. Spawn it (`tokio::spawn(stream.run())`) and keep the join handle,
    /// or use [`MetricStream::run_until`] when the device needs to stop itself.
    pub async fn run(self) -> Result<(), NetError> {
        self.run_until(std::future::pending::<()>()).await
    }

    /// [`MetricStream::run`], stopping with `Ok(())` once `stop` resolves.
    ///
    /// The first snapshot goes out immediately — a viewer that just paired does
    /// not wait a full tick for its first number.
    pub async fn run_until<F>(self, stop: F) -> Result<(), NetError>
    where
        F: std::future::Future<Output = ()>,
    {
        let mut stop = std::pin::pin!(stop);
        loop {
            self.send_once().await?;
            // `sleep(0)` is honoured as "no sleep": the branch is ready at
            // once, so a zero tick sends as fast as the channel accepts while
            // `stop` is still polled on every iteration.
            tokio::select! {
                _ = &mut stop => return Ok(()),
                _ = tokio::time::sleep(self.tick) => {}
            }
        }
    }

    /// Close the channel politely.
    pub fn close(self) {
        self.paired.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{pair_loopback, sample_snapshot};
    use crate::viewer::ViewerSession;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::time::timeout;

    const T: Duration = Duration::from_secs(20);

    #[test]
    fn the_stream_can_be_spawned_and_shared() {
        // The property the `Mutex` in `MetricStream` exists for: `run()` can be
        // spawned without the caller wrapping anything, and the stream itself
        // can be put behind an `Arc` and ticked from a supervisor.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MetricStream>();
        assert_send_sync::<ViewerSession>();
    }

    #[tokio::test]
    async fn send_once_sends_exactly_one_snapshot() {
        let pair = pair_loopback("t29_once").await;
        let session = ViewerSession::connect(pair.viewer);
        let stream = MetricStream::new(pair.device, Box::new(|| sample_snapshot(1)));

        let sent = timeout(T, stream.send_once()).await.expect("send timed out").unwrap();
        assert!(sent > 0);

        let snap = timeout(T, session.next_snapshot())
            .await
            .expect("timed out")
            .expect("snapshot failed");
        assert_eq!(snap.agent["ticks"], serde_json::json!(1));

        // Nothing else was sent: one send, one message. A short timeout is the
        // assertion (the channel is idle, not closed).
        let extra = timeout(Duration::from_millis(300), session.next_snapshot()).await;
        assert!(extra.is_err(), "send_once must not send a second snapshot");

        stream.close();
    }

    #[tokio::test]
    async fn run_until_sends_every_tick_and_stops_on_request() {
        let pair = pair_loopback("t29_ticks").await;
        let session = ViewerSession::connect(pair.viewer);

        let counter = Arc::new(AtomicU64::new(0));
        let pulled = counter.clone();
        let stream = MetricStream::new(
            pair.device,
            Box::new(move || sample_snapshot(pulled.fetch_add(1, Ordering::SeqCst))),
        )
        .with_tick(Duration::from_millis(15));

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let device_task = tokio::spawn(stream.run_until(async move {
            let _ = stop_rx.await;
        }));

        // Five snapshots, in tick order, each with fresh values: the closure
        // really is re-read every tick rather than sent once and repeated.
        let mut ticks = Vec::new();
        for _ in 0..5 {
            let snap = timeout(T, session.next_snapshot())
                .await
                .expect("viewer timed out")
                .expect("snapshot failed");
            ticks.push(snap.agent["ticks"].as_u64().unwrap());
        }
        assert_eq!(ticks, vec![0, 1, 2, 3, 4]);

        stop_tx.send(()).unwrap();
        timeout(T, device_task)
            .await
            .expect("stream did not stop when asked")
            .expect("device task panicked")
            .expect("run_until must end with Ok when asked to stop");

        let pulled = counter.load(Ordering::SeqCst);
        assert!(pulled >= 5, "expected at least the five pulls we saw, got {pulled}");

        session.close();
    }

    #[tokio::test]
    async fn a_non_object_snapshot_is_refused_before_it_is_sent() {
        // A `MetricStream` owns its channel, so each shape needs its own
        // pairing; two shapes cover the check (number, array) and the rest of
        // the non-object cases take the same branch.
        for (i, bad) in [serde_json::json!(42), serde_json::json!([1, 2, 3])]
            .into_iter()
            .enumerate()
        {
            let pair = pair_loopback(&format!("t29_nonobject_{i}")).await;
            let session = ViewerSession::connect(pair.viewer);

            let stream = MetricStream::new(pair.device, Box::new(move || bad.clone()));
            let err = stream
                .send_once()
                .await
                .expect_err("a non-object snapshot must not be sent");
            assert!(matches!(err, NetError::Protocol(_)), "got {err}");

            // Nothing reached the viewer: the check runs before `send`. The
            // stream is kept alive for this check, so a timeout means "no
            // frame", not "the channel was dropped".
            let nothing = timeout(Duration::from_millis(200), session.next_snapshot()).await;
            assert!(nothing.is_err(), "the viewer must not have received a frame");

            stream.close();
        }
    }

    #[tokio::test]
    async fn run_stops_when_the_viewer_hangs_up() {
        let pair = pair_loopback("t29_hangup").await;
        let session = ViewerSession::connect(pair.viewer);

        let stream = MetricStream::new(pair.device, Box::new(|| sample_snapshot(0)))
            .with_tick(Duration::from_millis(20));
        let device_task = tokio::spawn(stream.run());

        timeout(T, session.next_snapshot())
            .await
            .expect("viewer timed out")
            .expect("snapshot failed");

        session.close();

        let ended = timeout(T, device_task)
            .await
            .expect("stream kept running after the viewer closed")
            .expect("device task panicked");
        assert!(
            matches!(ended, Err(NetError::Stream(_))),
            "a closed channel must surface as NetError::Stream, got {ended:?}"
        );
    }
}
