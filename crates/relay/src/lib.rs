//! Stateless relay: forwards ciphertext between devices that completed pairing.
//!
//! # The whole job
//!
//! A device dials this process, registers under its node id, and from then on
//! may send opaque payloads tagged with another device's node id. The relay
//! writes those bytes to the destination's connection. That is the entire
//! protocol, and everything it does *not* do is the point:
//!
//! * no decryption — the relay's own code contains no cryptography and never
//!   holds a key, so "the relay cannot read your metrics" is a property of the
//!   build rather than a promise (its only transitive crypto comes from the
//!   shared `syswatch-net` crate, which the relay uses for iroh and frames and
//!   never calls for a cipher),
//! * no inspection — a payload is never parsed, only length-checked,
//! * no storage — nothing is written to disk, no message is queued for a peer
//!   that is not currently connected,
//! * no accounts — there is no user table, no token database, no session log.
//!
//! # State
//!
//! The only state is one entry per live connection: `node id -> sender`. It is
//! created at registration, dropped when the connection ends, and bounded by
//! [`MAX_ROUTES`]. A restart of the relay is invisible to the protocol beyond
//! everyone needing to re-register.
//!
//! One node id, one connection: a second registration under the same id replaces
//! the first and the superseded connection is closed. That is deliberate — the
//! alternative lets a stale connection linger and receive traffic meant for the
//! current one, and iroh maps a node id to a single transport address anyway.
//!
//! # Trust
//!
//! The relay is untrusted infrastructure, by design. It sees routing headers and
//! ciphertext, so the worst it can do is drop, delay, reorder or misroute — and
//! a misrouted message fails to authenticate at the wrong device, because
//! pairing keys are per-pair and never pass through the relay
//! ([`syswatch_net::pairing`]). Running the relay on somebody else's machine is
//! a normal deployment, not a compromise.
//!
//! # Where the client lives
//!
//! [`syswatch_net::RelayClient`]. The wire format is defined once, in
//! [`syswatch_net::relay_client::wire`], and used by both sides; the server here
//! never re-implements a frame layout.
//!
//! A device holds a [`syswatch_net::RelayLink`] (one endpoint, its node id) and
//! opens connections on it. Reconnecting after a drop is a `link.connect()` on
//! that same link — a fresh endpoint with the same key cannot redial the same
//! relay until iroh forgets the old connection, which is measured behaviour in
//! iroh 0.28, not a preference.

// iroh-net 0.28 is the line Cargo.lock pins; its API carries a blanket
// "crate renamed" deprecation aimed at 0.29+. Staying on 0.28 is deliberate.
#![allow(deprecated)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_lite::StreamExt;
use iroh_base::key::PublicKey;
use iroh_net::endpoint::{Connection, NodeAddr, get_remote_node_id};
use tokio::sync::{mpsc, oneshot};

use syswatch_net::relay_client::{RELAY_ALPN, wire};
use syswatch_net::{DeviceIdentity, IrohEndpoint, NetError};

/// The port `syswatch-relayd` binds when it is not told otherwise.
pub const DEFAULT_PORT: u16 = 9443;

/// How many undelivered messages a single destination may have queued.
///
/// A destination that stops reading must not turn into unbounded memory on the
/// relay. Once its queue is full, senders get [`wire::Status::Busy`] and can
/// retry; dropping would be silent, and growing the queue would be a leak with
/// extra steps. Depth 8 × [`syswatch_net::MAX_RELAY_BYTES`] bounds one peer to
/// 1 MiB of relay memory.
pub const QUEUE_DEPTH: usize = 8;

/// How many connections may be registered at once.
///
/// Never hit in practice (a fleet is not 4096 devices), but a public relay on a
/// public port *will* be scanned, so "the map is bounded" has to be true rather
/// than assumed. Over the cap, registration is refused with `Busy`.
pub const MAX_ROUTES: usize = 4096;

/// How long a fresh connection has to send its registration frame.
///
/// Without this, an idle TCP-alike handshake would hold a connection slot
/// forever; a port scanner would be indistinguishable from a slow device.
pub const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a refused device gets to read its status before the connection is
/// torn down.
///
/// QUIC `close` discards whatever has not been delivered, so closing the instant
/// a refusal is written would replace "bad token" with "connection lost" on the
/// peer. Two seconds is far longer than a loopback round trip and short enough
/// that a scanner cannot hold a slot.
pub const REFUSAL_GRACE: Duration = Duration::from_secs(2);

/// Relay-side failures, all of them transport-level.
///
/// A *rejected device* is not an error here: it is a [`wire::Status`] written
/// back on its own stream, so one bad client cannot affect anybody else.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("net error: {0}")]
    Net(#[from] NetError),
    #[error("relay io error: {0}")]
    Io(String),
}

/// A running (or ready-to-run) relay.
///
/// Bind, then [`RelayServer::run`] (or [`RelayServer::run_until`] for a shutdown
/// hook). Cloning is cheap and shares the route table: a clone exists to hand
/// the accept loop to a task while the original keeps answering [`RelayServer::stats`].
pub struct RelayServer {
    endpoint: IrohEndpoint,
    /// Optional access token. `None` = anyone may use this relay and still only
    /// get ciphertext routed.
    token: Option<Vec<u8>>,
    routes: Arc<Routes>,
}

impl Clone for RelayServer {
    fn clone(&self) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            token: self.token.clone(),
            routes: self.routes.clone(),
        }
    }
}

impl std::fmt::Debug for RelayServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayServer")
            .field("node_id", &self.endpoint.node_id().fmt_short())
            .field("port", &self.endpoint.bound_port())
            .field("token", &self.token.is_some())
            .field("routes", &self.routes.len())
            .finish_non_exhaustive()
    }
}

impl RelayServer {
    /// Bind a relay on every interface at `port` (`0` = OS picks).
    ///
    /// Production posture: no upstream relay, no discovery — this *is* the
    /// rendezvous point, and its address is published by whoever runs it.
    pub async fn bind(identity: &DeviceIdentity, port: u16) -> Result<Self, RelayError> {
        let endpoint = IrohEndpoint::bind_port(identity, &[RELAY_ALPN], port).await?;
        Ok(Self::new(endpoint, None))
    }

    /// [`RelayServer::bind`] restricted to loopback, for tests and demos.
    pub async fn bind_local(identity: &DeviceIdentity, port: u16) -> Result<Self, RelayError> {
        let endpoint = IrohEndpoint::bind_local_port(identity, &[RELAY_ALPN], port).await?;
        Ok(Self::new(endpoint, None))
    }

    /// Wrap an already-bound endpoint as a relay.
    ///
    /// The endpoint must offer [`RELAY_ALPN`]; anything else will not negotiate
    /// a connection with [`syswatch_net::RelayClient`].
    pub fn new(endpoint: IrohEndpoint, token: Option<Vec<u8>>) -> Self {
        Self {
            endpoint,
            token: token.filter(|token| !token.is_empty()),
            routes: Arc::new(Routes::default()),
        }
    }

    /// Require an access token at registration (builder form).
    ///
    /// An empty token is treated as "no token": wiring an unset environment
    /// variable must not turn a public relay into a locked one.
    pub fn with_token(mut self, token: Option<Vec<u8>>) -> Self {
        self.token = token.filter(|token| !token.is_empty());
        self
    }

    /// The relay's node id — this is what clients dial.
    pub fn node_id(&self) -> PublicKey {
        self.endpoint.node_id()
    }

    /// The UDP port actually bound (`None` only if no socket was reported).
    pub fn bound_port(&self) -> Option<u16> {
        self.endpoint.bound_port()
    }

    /// This relay's dialable address (direct addresses; no home relay).
    pub async fn node_addr(&self) -> Result<NodeAddr, RelayError> {
        Ok(self.endpoint.node_addr().await?)
    }

    /// The endpoint carrying the relay, for hosts that want transport stats.
    pub fn endpoint(&self) -> &IrohEndpoint {
        &self.endpoint
    }

    /// How many devices are registered right now.
    ///
    /// Exposed because it is the honest measure of "stateless": after every
    /// device disconnects this returns to 0, and a test can watch it happen.
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    /// In-memory counters since start. Useful for a health line; never persisted.
    pub fn stats(&self) -> RelayStats {
        RelayStats {
            active_routes: self.routes.len(),
            registered: self.routes.registered.load(Ordering::Relaxed),
            forwarded: self.routes.forwarded.load(Ordering::Relaxed),
            forwarded_bytes: self.routes.forwarded_bytes.load(Ordering::Relaxed),
            refused: self.routes.refused.load(Ordering::Relaxed),
        }
    }

    /// Accept connections until the endpoint closes.
    pub async fn run(self) {
        self.run_until(std::future::pending::<()>()).await;
    }

    /// [`RelayServer::run`] with a caller-supplied stop signal.
    ///
    /// Returning from this drops the accept loop, not the connections: writers
    /// and forwarders keep serving their peers until those peers go away, which
    /// is the polite shutdown for a router (a caller that wants a hard stop
    /// drops the endpoint).
    pub async fn run_until(self, stop: impl Future<Output = ()>) {
        let endpoint = self.endpoint.clone();
        let mut accepted = Box::pin(endpoint.accept());
        let mut stop = Box::pin(stop);
        loop {
            tokio::select! {
                _ = &mut stop => return,
                next = accepted.next() => match next {
                    Some(conn) => {
                        let routes = self.routes.clone();
                        let token = self.token.clone();
                        tokio::spawn(serve_connection(conn, routes, token));
                    }
                    // Endpoint closed: nothing more will be accepted.
                    None => return,
                },
            }
        }
    }
}

/// What the relay has done since it started, in memory only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RelayStats {
    /// Devices registered right now.
    pub active_routes: usize,
    /// Successful registrations since start (a reconnect counts again).
    pub registered: u64,
    /// Payloads handed to a destination's queue.
    pub forwarded: u64,
    /// Total bytes forwarded. Always exactly the sum of the payload lengths —
    /// the relay adds headers on the wire, never to the payload it reports.
    pub forwarded_bytes: u64,
    /// Registrations and forwards that ended in a non-`Ok` status.
    pub refused: u64,
}

/// The route table: node id -> delivery queue. Everything else is counters.
#[derive(Default)]
struct Routes {
    map: Mutex<HashMap<PublicKey, Route>>,
    registered: AtomicU64,
    forwarded: AtomicU64,
    forwarded_bytes: AtomicU64,
    refused: AtomicU64,
}

struct Route {
    tx: mpsc::Sender<(PublicKey, Vec<u8>)>,
    /// Identity of this registration, so a stale connection cannot remove the
    /// entry that replaced it.
    generation: Arc<()>,
}

impl Routes {
    fn len(&self) -> usize {
        self.map.lock().expect("route table poisoned").len()
    }

    /// Insert or replace. Returns false when the table is at [`MAX_ROUTES`] and
    /// this id is new.
    fn insert(&self, id: PublicKey, route: Route) -> bool {
        // The guard must not live across the later lock, but there is no await
        // here at all: one short critical section, then release.
        let mut map = self.map.lock().expect("route table poisoned");
        if !map.contains_key(&id) && map.len() >= MAX_ROUTES {
            return false;
        }
        map.insert(id, route);
        true
    }

    fn sender(&self, id: &PublicKey) -> Option<mpsc::Sender<(PublicKey, Vec<u8>)>> {
        self.map
            .lock()
            .expect("route table poisoned")
            .get(id)
            .map(|route| route.tx.clone())
    }

    /// Remove `id` only if it is still the registration `generation` describes.
    fn remove(&self, id: &PublicKey, generation: &Arc<()>) {
        let mut map = self.map.lock().expect("route table poisoned");
        if let Some(route) = map.get(id)
            && Arc::ptr_eq(&route.generation, generation)
        {
            map.remove(id);
        }
    }

    fn count_refused(&self) {
        self.refused.fetch_add(1, Ordering::Relaxed);
    }
}

/// One device's whole lifetime: register, then forward until it goes away.
async fn serve_connection(conn: Connection, routes: Arc<Routes>, token: Option<Vec<u8>>) {
    let source = match get_remote_node_id(&conn) {
        Ok(id) => id,
        // Without a verified TLS identity there is nothing to route under, so
        // there is no safe default: refuse the connection.
        Err(_) => {
            conn.close(0u32.into(), b"no peer identity");
            return;
        }
    };

    let (generation, mut rx) = match register(&conn, source, token.as_deref(), &routes).await {
        Ok(registered) => registered,
        Err(status) => {
            conn.close(0u32.into(), status.as_str().as_bytes());
            return;
        }
    };
    routes.registered.fetch_add(1, Ordering::Relaxed);

    // Delivery writer. One task per connection so a destination that stops
    // reading cannot block a sender, and the bounded queue is the only buffer.
    let (done_tx, mut done_rx) = oneshot::channel::<()>();
    let writer_conn = conn.clone();
    let writer_routes = routes.clone();
    tokio::spawn(async move {
        while let Some((source, payload)) = rx.recv().await {
            let Ok(frame) = wire::encode_deliver(&source, &payload) else {
                continue;
            };
            let Ok((mut send, _recv)) = writer_conn.open_bi().await else {
                break;
            };
            if send.write_all(&frame).await.is_err() || send.finish().is_err() {
                break;
            }
            writer_routes.forwarded.fetch_add(1, Ordering::Relaxed);
            writer_routes
                .forwarded_bytes
                .fetch_add(payload.len() as u64, Ordering::Relaxed);
        }
        let _ = done_tx.send(());
    });

    // Forward loop: every subsequent stream from this device is a forward.
    //
    // A frame that never finishes only stalls *this* device's forwards: the
    // delivery writer runs independently, so other devices keep routing to it
    // (and get `Busy` if its queue fills). Nothing here can block the relay.
    loop {
        let accepted = tokio::select! {
            // The writer died (destination unreachable): there is no point
            // accepting work that cannot be delivered.
            _ = &mut done_rx => break,
            accepted = conn.accept_bi() => accepted,
        };
        let Ok((mut send, mut recv)) = accepted else {
            break;
        };
        let Ok(buf) = recv
            .read_to_end(wire::FORWARD_FIXED_LEN + wire::MAX_FORWARD_BYTES)
            .await
        else {
            break;
        };
        let status = match wire::decode_forward(&buf) {
            Ok(forward) if forward.target == source => wire::Status::SelfForward,
            Ok(forward) => match routes.sender(&forward.target) {
                None => wire::Status::UnknownPeer,
                Some(tx) => {
                    // Bounded queue: full means "busy, try again", never "drop".
                    match tx.try_send((source, forward.payload.to_vec())) {
                        Ok(()) => wire::Status::Ok,
                        Err(_) => wire::Status::Busy,
                    }
                }
            },
            Err(wire::FrameError::Version(_)) => wire::Status::BadVersion,
            Err(wire::FrameError::PayloadTooLarge(_, _)) => wire::Status::TooLarge,
            Err(_) => wire::Status::Malformed,
        };
        if status != wire::Status::Ok {
            routes.count_refused();
        }
        let _ = send.write_all(&wire::encode_reply(status)).await;
        let _ = send.finish();
    }

    routes.remove(&source, &generation);
    conn.close(0u32.into(), b"relay route closed");
}

/// Read the first stream on `conn`, decide whether this device may register,
/// and put the route in the table.
///
/// The identity rule is the one that matters: the id a device asks to be routed
/// under must be the id in its TLS certificate. Without that check any device
/// could claim another's id and receive its traffic — a routing switch has to
/// verify the thing it routes on, and iroh already hands us the verified id.
///
/// Returns the registration generation (so the entry can be removed later
/// without clobbering a newer one for the same id) and the delivery queue.
async fn register(
    conn: &Connection,
    remote: PublicKey,
    token: Option<&[u8]>,
    routes: &Routes,
) -> Result<(Arc<()>, mpsc::Receiver<(PublicKey, Vec<u8>)>), wire::Status> {
    let (mut reply, buf) = match tokio::time::timeout(REGISTER_TIMEOUT, async {
        let (send, mut recv) = conn.accept_bi().await.map_err(|_| wire::Status::Malformed)?;
        let buf = recv
            .read_to_end(wire::REGISTER_FIXED_LEN + wire::MAX_TOKEN_BYTES)
            .await
            .map_err(|_| wire::Status::Malformed)?;
        Ok::<_, wire::Status>((send, buf))
    })
    .await
    {
        // No usable frame inside the window: a scanner, a half-open client, or
        // a device that changed its mind. Treated as a malformed registration.
        Err(_) => {
            routes.count_refused();
            return Err(wire::Status::Malformed);
        }
        Ok(Err(status)) => {
            routes.count_refused();
            return Err(status);
        }
        Ok(Ok(parts)) => parts,
    };

    let mut status = match wire::decode_register(&buf) {
        Err(wire::FrameError::Version(_)) => wire::Status::BadVersion,
        Err(wire::FrameError::TokenTooLarge(_, _)) => wire::Status::TooLarge,
        Err(_) => wire::Status::Malformed,
        Ok(reg) if reg.node_id != remote => wire::Status::BadIdentity,
        Ok(reg) if !token_matches(token, reg.token) => wire::Status::BadToken,
        Ok(_) => wire::Status::Ok,
    };

    // The route is created before the reply, so "ok" is a promise the relay can
    // keep: there is no window in which a device believes it is registered but
    // the table has no entry for it.
    let mut registration = None;
    if status == wire::Status::Ok {
        if routes.len() >= MAX_ROUTES {
            status = wire::Status::Busy;
        } else {
            let generation = Arc::new(());
            let (tx, rx) = mpsc::channel(QUEUE_DEPTH);
            if routes.insert(
                remote,
                Route {
                    tx,
                    generation: generation.clone(),
                },
            ) {
                registration = Some((generation, rx));
            } else {
                // Lost the capacity race against another registration.
                status = wire::Status::Busy;
            }
        }
    }

    if registration.is_none() {
        // Counted before the reply, so an operator (or a test) that reads the
        // status also sees the counter move: the two facts must not disagree.
        routes.count_refused();
    }
    let _ = reply.write_all(&wire::encode_reply(status)).await;
    let _ = reply.finish();

    if let Some(registered) = registration {
        return Ok(registered);
    }

    // A refusal has to be *readable*: closing the connection here would discard
    // the very bytes that say why it was refused, and the peer would see a dead
    // connection instead of "bad token". Give it the grace window to hang up on
    // its own; the caller closes after this returns.
    let _ = tokio::time::timeout(REFUSAL_GRACE, conn.closed()).await;
    Err(status)
}

/// Access-token check.
///
/// A plain comparison: the token is a bouncer at the door, not a key. It gates
/// *who may route through this relay*, and a leaked one costs bandwidth, never
/// confidentiality — payloads are already sealed by the pairing layer.
fn token_matches(required: Option<&[u8]>, presented: &[u8]) -> bool {
    match required {
        None => true,
        Some(required) => required == presented,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use tokio::time::{sleep, timeout};
    use tokio::time::Instant;

    use syswatch_net::relay_client::wire::FrameError;
    use syswatch_net::{RelayClient, RelayLink, RelayStatus};

    const STEP: Duration = Duration::from_secs(20);

    struct TempIdentity {
        path: std::path::PathBuf,
        identity: DeviceIdentity,
    }

    impl TempIdentity {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("syswatch_t30_{name}.key"));
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

    /// A relay on loopback plus the identity it runs as.
    async fn relay(name: &str, token: Option<&[u8]>) -> (RelayServer, TempIdentity) {
        let identity = TempIdentity::new(name);
        let server = timeout(STEP, RelayServer::bind_local(&identity.identity, 0))
            .await
            .expect("bind timed out")
            .expect("bind failed");
        let server = server.with_token(token.map(|token| token.to_vec()));
        (server, identity)
    }

    /// Spawn the accept loop; the handle is aborted at test end by the drop of
    /// the runtime, and the server clone shares the route table with `server`.
    fn serve(server: &RelayServer) -> tokio::task::JoinHandle<()> {
        tokio::spawn(server.clone().run())
    }

    async fn addr_of(server: &RelayServer) -> NodeAddr {
        timeout(STEP, server.node_addr())
            .await
            .expect("node_addr timed out")
            .expect("node_addr failed")
    }

    /// Bind a device's endpoint for this relay (one per device, kept alive).
    async fn link(
        identity: &TempIdentity,
        server: &RelayServer,
        token: Option<&[u8]>,
    ) -> RelayLink {
        timeout(
            STEP,
            RelayLink::open_local_with_token(&identity.identity, addr_of(server).await, token),
        )
        .await
        .expect("link timed out")
        .expect("link failed")
    }

    /// Register on an existing link (this is the reconnect path, too).
    async fn connect(link: &RelayLink) -> Result<RelayClient, NetError> {
        timeout(STEP, link.connect())
            .await
            .expect("register timed out")
    }

    /// One-shot convenience: bind a fresh endpoint and register once.
    ///
    /// Only for tests that register a device exactly once — a second attempt
    /// with a fresh endpoint on the same identity is the iroh case documented on
    /// [`RelayClient`], so retries go through [`link`] + [`connect`].
    async fn register(
        identity: &TempIdentity,
        server: &RelayServer,
        token: Option<&[u8]>,
    ) -> Result<RelayClient, NetError> {
        let link = link(identity, server, token).await;
        connect(&link).await
    }

    async fn next(client: &RelayClient) -> (PublicKey, Vec<u8>) {
        timeout(STEP, client.try_recv())
            .await
            .expect("recv timed out")
            .expect("recv failed")
            .expect("relay connection ended")
    }

    #[tokio::test]
    async fn two_devices_exchange_messages_through_the_relay() {
        let (server, _relay_id) = relay("exchange", None).await;
        let _accept = serve(&server);

        let id_a = TempIdentity::new("exchange_a");
        let id_b = TempIdentity::new("exchange_b");
        let a = register(&id_a, &server, None).await.expect("a registers");
        let b = register(&id_b, &server, None).await.expect("b registers");

        assert_eq!(server.route_count(), 2);
        assert_eq!(a.routing_id(), id_a.identity.node_id());
        assert_eq!(b.routing_id(), id_b.identity.node_id());

        // A -> B: several messages, in order, tagged with A's id at the peer.
        for n in 0..4u8 {
            timeout(STEP, a.forward_to(b.routing_id(), &[n; 64]))
                .await
                .expect("forward timed out")
                .expect("forward failed");
        }
        for n in 0..4u8 {
            let (source, body) = next(&b).await;
            assert_eq!(source, a.routing_id());
            assert_eq!(body, vec![n; 64]);
        }

        // B -> A on the same registrations.
        timeout(STEP, b.forward_to(a.routing_id(), b"metrics"))
            .await
            .expect("forward timed out")
            .expect("forward failed");
        let (source, body) = next(&a).await;
        assert_eq!(source, b.routing_id());
        assert_eq!(body, b"metrics");

        // Counters describe exactly what happened.
        let stats = server.stats();
        assert_eq!(stats.active_routes, 2);
        assert_eq!(stats.registered, 2);
        assert_eq!(stats.forwarded, 5);
        assert_eq!(stats.forwarded_bytes, 4 * 64 + 7);
        assert_eq!(server.route_count(), 2);
    }

    #[tokio::test]
    async fn unknown_peer_and_self_forward_are_refused() {
        let (server, _relay_id) = relay("refuse", None).await;
        let _accept = serve(&server);

        let id_a = TempIdentity::new("refuse_a");
        let a = register(&id_a, &server, None).await.expect("registers");

        let stranger = iroh_base::key::SecretKey::generate().public();
        let err = timeout(STEP, a.forward_to(stranger, b"hello"))
            .await
            .expect("forward timed out")
            .expect_err("unknown peer must not succeed");
        assert!(
            matches!(&err, NetError::RelayRejected(reason) if reason == "unknown peer"),
            "got {err:?}"
        );

        let err = timeout(STEP, a.forward_to(a.routing_id(), b"hello"))
            .await
            .expect("forward timed out")
            .expect_err("self forward must not succeed");
        assert!(
            matches!(&err, NetError::RelayRejected(reason) if reason == "self forward"),
            "got {err:?}"
        );

        // Neither attempt was counted as a delivery.
        let stats = server.stats();
        assert_eq!(stats.forwarded, 0);
        assert_eq!(stats.forwarded_bytes, 0);
        assert_eq!(stats.refused, 2);
    }

    #[tokio::test]
    async fn access_token_is_required_when_configured() {
        let (server, _relay_id) = relay("token", Some(b"family-secret")).await;
        let _accept = serve(&server);

        let id_a = TempIdentity::new("token_a");
        // One endpoint for this device: a refused attempt is followed by a retry
        // on the same link, exactly as a real device would.
        let device = link(&id_a, &server, None).await;

        // No token: refused, and the refusal names the reason.
        let err = timeout(STEP, device.connect_with_token(None))
            .await
            .expect("register timed out")
            .expect_err("a relay with a token must refuse an unauthenticated device");
        assert!(
            matches!(&err, NetError::RelayRejected(reason) if reason == "bad token"),
            "got {err:?}"
        );

        // A wrong token is refused too.
        let err = timeout(STEP, device.connect_with_token(Some(b"wrong")))
            .await
            .expect("register timed out")
            .expect_err("a wrong token must be refused");
        assert!(
            matches!(&err, NetError::RelayRejected(reason) if reason == "bad token"),
            "got {err:?}"
        );
        assert_eq!(server.route_count(), 0, "a refusal leaves no route");

        // The right token registers — after two refusals, on the same endpoint.
        let a = timeout(STEP, device.connect_with_token(Some(b"family-secret")))
            .await
            .expect("register timed out")
            .expect("correct token registers");
        assert_eq!(server.route_count(), 1);

        // Re-registering the same id replaces the route rather than adding one.
        let b = timeout(STEP, device.connect_with_token(Some(b"family-secret")))
            .await
            .expect("register timed out")
            .expect("re-register succeeds");
        assert_eq!(server.route_count(), 1, "same node id replaces its route");

        // The newest connection is the one that receives.
        let id_peer = TempIdentity::new("token_peer");
        let peer = register(&id_peer, &server, Some(b"family-secret"))
            .await
            .expect("peer registers");
        timeout(STEP, peer.forward_to(a.routing_id(), b"to-the-new-route"))
            .await
            .expect("forward timed out")
            .expect("forward failed");
        let (source, body) = next(&b).await;
        assert_eq!(source, peer.routing_id());
        assert_eq!(body, b"to-the-new-route");

        // The replaced client's connection is superseded: the relay closes it
        // rather than letting two connections claim one node id (which is also
        // what stops iroh from delivering to the stale one). What matters is
        // that it never receives the payload.
        // Closed by the relay, already gone, or simply idle: all fine.
        if let Ok(Ok(Some((_, body)))) = timeout(Duration::from_millis(300), a.try_recv()).await {
            panic!("the replaced route must not receive traffic, got {body:?}");
        }
    }

    #[tokio::test]
    async fn oversized_payload_is_refused_by_the_limits_not_the_network() {
        let (server, _relay_id) = relay("big", None).await;
        let _accept = serve(&server);

        let id_a = TempIdentity::new("big_a");
        let id_b = TempIdentity::new("big_b");
        let a = register(&id_a, &server, None).await.expect("registers");
        let b = register(&id_b, &server, None).await.expect("registers");

        let too_big = vec![0u8; syswatch_net::MAX_RELAY_BYTES + 1];
        let err = timeout(STEP, a.forward_to(b.routing_id(), &too_big))
            .await
            .expect("forward timed out")
            .expect_err("oversized payload must be refused");
        assert!(matches!(err, NetError::TooLarge(_, _)), "got {err:?}");

        // Exactly at the limit goes through untouched.
        let at_limit = vec![0x33u8; syswatch_net::MAX_RELAY_BYTES];
        timeout(STEP, a.forward_to(b.routing_id(), &at_limit))
            .await
            .expect("forward timed out")
            .expect("a payload at the limit is allowed");
        let (_, body) = next(&b).await;
        assert_eq!(body, at_limit);
    }

    #[tokio::test]
    async fn routes_disappear_when_devices_leave() {
        let (server, _relay_id) = relay("leave", None).await;
        let _accept = serve(&server);

        let id_a = TempIdentity::new("leave_a");
        let id_b = TempIdentity::new("leave_b");
        let a = register(&id_a, &server, None).await.expect("registers");
        let b = register(&id_b, &server, None).await.expect("registers");
        assert_eq!(server.route_count(), 2);

        // Send one message so the two connections are genuinely open in both
        // directions before the teardown is measured.
        timeout(STEP, a.forward_to(b.routing_id(), b"bye"))
            .await
            .expect("forward timed out")
            .expect("forward failed");
        let _ = next(&b).await;

        b.close();
        let gone = wait_for_routes(&server, 1).await;
        assert_eq!(gone, 1, "a closed device must leave no route behind");

        a.close();
        let gone = wait_for_routes(&server, 0).await;
        assert_eq!(gone, 0, "the relay must be back to zero state");

        // Stateless: nothing registered, nothing forwarded since.
        let stats = server.stats();
        assert_eq!(stats.active_routes, 0);
    }

    /// Poll until the route count reaches `want`, fail loudly if it does not.
    async fn wait_for_routes(server: &RelayServer, want: usize) -> usize {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let now = server.route_count();
            if now == want {
                return now;
            }
            if Instant::now() > deadline {
                return now;
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn relay_binds_the_requested_port() {
        let identity = TempIdentity::new("port");

        // Port 0: the OS picks, and the relay reports what it got.
        let ephemeral = timeout(STEP, RelayServer::bind_local(&identity.identity, 0))
            .await
            .expect("bind timed out")
            .expect("bind failed");
        let picked = ephemeral.bound_port().expect("a bound port");
        assert_ne!(picked, 0);

        // The advertised address agrees with the bound port.
        let addr = addr_of(&ephemeral).await;
        assert_eq!(addr.node_id, ephemeral.node_id());
        assert!(
            addr.info
                .direct_addresses
                .iter()
                .any(|direct| direct.port() == picked),
            "node addr {addr:?} must advertise the bound port {picked}"
        );
        ephemeral.endpoint().clone().close().await;

        // An explicit port is honoured. Retried because the port is reserved
        // and released first — another process could take it in between.
        let mut bound = None;
        for _ in 0..5 {
            let Ok(probe) = std::net::UdpSocket::bind("127.0.0.1:0") else {
                continue;
            };
            let port = probe.local_addr().expect("probe addr").port();
            drop(probe);
            match timeout(STEP, RelayServer::bind_local(&identity.identity, port)).await {
                Ok(Ok(server)) => {
                    assert_eq!(server.bound_port(), Some(port));
                    bound = Some(server);
                    break;
                }
                // Lost the race: try another port.
                _ => continue,
            }
        }
        let server = bound.expect("could not bind a reserved loopback port in 5 tries");
        server.endpoint().clone().close().await;
    }

    #[tokio::test]
    async fn wrong_wire_frames_get_typed_statuses() {
        let (server, _relay_id) = relay("wire", None).await;
        let _accept = serve(&server);

        let me = TempIdentity::new("wire_me");
        let endpoint = IrohEndpoint::bind_local_alpns(&me.identity, &[RELAY_ALPN])
            .await
            .expect("bind");

        // A raw client: this is how you test refusals the typed client will not
        // produce, because it refuses bad frames before writing them.
        async fn raw(endpoint: &IrohEndpoint, server: &RelayServer) -> Connection {
            timeout(STEP, endpoint.connect_addr_alpn(addr_of(server).await, RELAY_ALPN))
                .await
                .expect("connect timed out")
                .expect("connect failed")
        }

        async fn send_registration(conn: &Connection, frame: &[u8]) -> RelayStatus {
            let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
            send.write_all(frame).await.expect("write");
            send.finish().expect("finish");
            let buf = recv.read_to_end(2).await.expect("read reply");
            wire::decode_reply(&buf).expect("reply frame")
        }

        // 1. A device claiming somebody else's node id.
        let victim = iroh_base::key::SecretKey::generate().public();
        let conn = raw(&endpoint, &server).await;
        let frame = wire::encode_register(&victim, None).expect("encode");
        assert_eq!(
            send_registration(&conn, &frame).await,
            wire::Status::BadIdentity
        );
        conn.close(0u32.into(), b"done");

        // 2. A version from the future.
        let conn = raw(&endpoint, &server).await;
        let mut future = wire::encode_register(&me.identity.node_id(), None).expect("encode");
        future[1] = wire::VERSION + 1;
        assert_eq!(
            send_registration(&conn, &future).await,
            wire::Status::BadVersion
        );
        conn.close(0u32.into(), b"done");

        // 3. Garbage that is not a frame at all.
        let conn = raw(&endpoint, &server).await;
        assert_eq!(
            send_registration(&conn, b"GET / HTTP/1.1\r\n\r\n").await,
            wire::Status::Malformed
        );
        conn.close(0u32.into(), b"done");

        // None of the refusals left a route behind.
        assert_eq!(server.route_count(), 0);
        assert_eq!(server.stats().registered, 0);
        assert_eq!(server.stats().refused, 3);

        // 4. A legitimate registration on the same endpoint still works — a bad
        //    client must not poison the relay. Same endpoint, so this is also the
        //    reconnect shape a device uses after a drop.
        let link = RelayLink::with_endpoint(endpoint.clone(), addr_of(&server).await, None);
        let client = connect(&link)
            .await
            .expect("register after refusals must succeed");
        assert_eq!(server.route_count(), 1);
        client.close();
    }

    #[tokio::test]
    async fn the_relay_forwards_ciphertext_it_cannot_read() {
        let (server, _relay_id) = relay("opaque", None).await;
        let _accept = serve(&server);

        let id_a = TempIdentity::new("opaque_a");
        let id_b = TempIdentity::new("opaque_b");
        let a = register(&id_a, &server, None).await.expect("registers");
        let b = register(&id_b, &server, None).await.expect("registers");

        // A paired device's cipher, used exactly as the pairing layer would.
        use syswatch_net::pairing::SessionCipher;
        let sender = SessionCipher::new(&[11u8; 32], &[22u8; 32]);
        let receiver = SessionCipher::new(&[22u8; 32], &[11u8; 32]);
        let plaintext = b"{\"cpu\":{\"total_pct\":18.9},\"ram\":{\"pct\":70.4}}";

        let frame = sender.seal(plaintext).expect("seal");
        let forwarded_before = server.stats().forwarded_bytes;

        timeout(STEP, a.forward_to(b.routing_id(), &frame))
            .await
            .expect("forward timed out")
            .expect("forward failed");
        let (source, received) = next(&b).await;

        // Byte-for-byte: the relay added nothing and removed nothing.
        assert_eq!(source, a.routing_id());
        assert_eq!(received, frame);
        assert_eq!(
            server.stats().forwarded_bytes - forwarded_before,
            frame.len() as u64
        );

        // And the bytes carry no readable plaintext for anyone in the middle.
        assert!(
            !frame.windows(plaintext.len()).any(|window| window == plaintext),
            "sealed frame must not contain its plaintext"
        );

        // The intended recipient reads it; a third key does not.
        assert_eq!(receiver.open(&received).expect("open"), plaintext);
        let stranger = SessionCipher::new(&[99u8; 32], &[98u8; 32]);
        assert!(stranger.open(&frame).is_err());
    }

    #[tokio::test]
    async fn the_frame_errors_are_not_silently_ok() {
        // A guard on the mapping the relay relies on: anything that is not a
        // version or size problem must read as `Malformed`, never as `Ok`.
        assert_eq!(
            wire::decode_register(b"\x02garbage"),
            Err(FrameError::Kind {
                expected: wire::KIND_REGISTER,
                got: wire::KIND_FORWARD,
            })
        );
    }
}
