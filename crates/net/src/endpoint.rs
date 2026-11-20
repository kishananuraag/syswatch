//! The iroh endpoint: bind, accept, connect.
//!
//! One [`IrohEndpoint`] per device. It is bound from the device's
//! [`DeviceIdentity`], so the endpoint's `NodeId` *is* the device's public key —
//! there is no separate account or registration step.
//!
//! T27 scope: transport only. The endpoint binds, publishes its [`PublicKey`],
//! accepts incoming connections, dials peers, and speaks one ALPN
//! ([`ECHO_ALPN`] = `syswatch/1`) so the link can be proven end to end. Pairing
//! and payload encryption are layered on top in later tasks.

// iroh-net 0.28 is the line Cargo.lock pins (and the one the rest of the
// workspace resolves against); its API carries a blanket "crate renamed"
// deprecation aimed at the newer 0.29+ release line. Staying on 0.28 is
// deliberate, so the marker is noise here rather than a fix-it signal.
#![allow(deprecated)]

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

use futures_lite::{stream, Stream};
use iroh_base::key::PublicKey;
use iroh_net::endpoint::{Connection, Endpoint, NodeAddr};
use iroh_net::relay::RelayMode;

use crate::{DeviceIdentity, NetError};

/// The only application protocol this crate speaks today.
///
/// iroh negotiates ALPN during the QUIC handshake, so a peer that does not
/// offer exactly these bytes is rejected before any payload is read.
pub const ECHO_ALPN: &[u8] = b"syswatch/1";

/// Same protocol as a string, for logs and error messages.
pub const ECHO_ALPN_STR: &str = "syswatch/1";

/// Largest echo payload we will buffer, in bytes. Bounded on purpose: the
/// payload of T27 is a liveness probe, not a file transfer.
pub const MAX_ECHO_BYTES: usize = 64 * 1024;

/// A bound iroh endpoint.
///
/// Cloning is cheap — the inner [`Endpoint`] is a handle to the shared
/// magicsock/QUIC state, so the clone talks on the same socket and NodeId.
#[derive(Clone)]
pub struct IrohEndpoint {
    endpoint: Endpoint,
}

impl IrohEndpoint {
    /// Bind an endpoint on the given device identity.
    ///
    /// This is the production path: default relay mode (n0 relays) so two
    /// devices behind NAT can reach each other, and the echo ALPN pre-registered
    /// so incoming connections are accepted rather than reset.
    ///
    /// `bind` is async because iroh must open UDP sockets and start its
    /// magicsock actor before a `NodeId` can be dialled.
    pub async fn bind(identity: &DeviceIdentity) -> Result<Self, NetError> {
        Self::bind_builder(builder_for(identity, &[ECHO_ALPN])).await
    }

    /// Bind an endpoint that speaks only `alpns`.
    ///
    /// Same production posture as [`IrohEndpoint::bind`] (default relay mode),
    /// used by the pairing layer from T28 on: a pairing endpoint must not offer
    /// the echo protocol, and an echo endpoint must not accept pairing.
    pub async fn bind_alpns(identity: &DeviceIdentity, alpns: &[&[u8]]) -> Result<Self, NetError> {
        Self::bind_builder(builder_for(identity, alpns)).await
    }

    /// Bind an endpoint restricted to loopback, with relays and discovery off.
    ///
    /// Same code path as [`IrohEndpoint::bind`] minus anything that needs the
    /// network, so tests (and offline demos) can connect two endpoints on one
    /// machine without leaving it.
    pub async fn bind_local(identity: &DeviceIdentity) -> Result<Self, NetError> {
        Self::bind_builder(builder_for(identity, &[ECHO_ALPN]).local(0)).await
    }

    /// [`IrohEndpoint::bind_local`] for an arbitrary ALPN set.
    pub async fn bind_local_alpns(
        identity: &DeviceIdentity,
        alpns: &[&[u8]],
    ) -> Result<Self, NetError> {
        Self::bind_builder(builder_for(identity, alpns).local(0)).await
    }

    /// [`IrohEndpoint::bind_local_alpns`] on a caller-chosen UDP port.
    ///
    /// `port == 0` lets the OS pick, exactly like [`IrohEndpoint::bind_local_alpns`].
    /// A fixed port is what a *server* needs — the relay in `crates/relay` has to
    /// be dial-able at a known address, not at whatever port came up. Still
    /// loopback-only, still relays-off, so this binds nothing off-box.
    ///
    /// Panics-free failure mode: a port already in use comes back as
    /// [`NetError::Iroh`], not a hang.
    pub async fn bind_local_port(
        identity: &DeviceIdentity,
        alpns: &[&[u8]],
        port: u16,
    ) -> Result<Self, NetError> {
        Self::bind_builder(builder_for(identity, alpns).local(port)).await
    }

    /// Bind every interface on a caller-chosen UDP port, relays and discovery off.
    ///
    /// The relay's production posture: it *is* the rendezvous point, so it must
    /// not depend on someone else's relay to be reachable. Discovery is off for
    /// the same reason — nobody needs to look its address up, its address is
    /// published by whoever operates it.
    pub async fn bind_port(
        identity: &DeviceIdentity,
        alpns: &[&[u8]],
        port: u16,
    ) -> Result<Self, NetError> {
        Self::bind_builder(builder_for(identity, alpns).any_interface(port)).await
    }

    /// The UDP port this endpoint is actually listening on (IPv4 socket).
    ///
    /// `None` only if the endpoint reported no bound socket at all. Callers that
    /// asked for port 0 use this to learn what the OS chose.
    pub fn bound_port(&self) -> Option<u16> {
        let (v4, _v6) = self.endpoint.bound_sockets();
        (v4.port() != 0).then_some(v4.port())
    }

    async fn bind_builder(builder: iroh_net::endpoint::Builder) -> Result<Self, NetError> {
        let endpoint = builder.bind().await.map_err(iroh_err)?;
        Ok(Self { endpoint })
    }

    /// The device's public identity, as seen by peers.
    pub fn node_id(&self) -> PublicKey {
        self.endpoint.node_id()
    }

    /// The node id as the base32 string users copy/paste.
    pub fn node_id_string(&self) -> String {
        self.node_id().to_string()
    }

    /// Dial a peer by `NodeId` alone.
    ///
    /// Requires addressing information to already be known — either relay plus
    /// direct addresses learned from a previous connection, or entries added
    /// with [`IrohEndpoint::add_node_addr`]. Use
    /// [`IrohEndpoint::connect_addr`] when you have a fresh [`NodeAddr`].
    pub async fn connect(&self, node_id: PublicKey) -> Result<Connection, NetError> {
        self.connect_alpn(node_id, ECHO_ALPN).await
    }

    /// Dial a peer at a known address (direct addresses and/or relay URL).
    pub async fn connect_addr(&self, addr: NodeAddr) -> Result<Connection, NetError> {
        self.connect_addr_alpn(addr, ECHO_ALPN).await
    }

    /// [`IrohEndpoint::connect`] for an explicit application protocol.
    pub async fn connect_alpn(
        &self,
        node_id: PublicKey,
        alpn: &[u8],
    ) -> Result<Connection, NetError> {
        self.endpoint
            .connect(NodeAddr::new(node_id), alpn)
            .await
            .map_err(iroh_err)
    }

    /// [`IrohEndpoint::connect_addr`] for an explicit application protocol.
    ///
    /// ALPN is negotiated in the QUIC handshake, so dialing a peer that does not
    /// offer `alpn` fails here rather than on the first write.
    pub async fn connect_addr_alpn(
        &self,
        addr: NodeAddr,
        alpn: &[u8],
    ) -> Result<Connection, NetError> {
        self.endpoint.connect(addr, alpn).await.map_err(iroh_err)
    }

    /// Teach this endpoint where a peer lives, so a later
    /// [`IrohEndpoint::connect`] by id can find it.
    pub fn add_node_addr(&self, addr: NodeAddr) -> Result<(), NetError> {
        self.endpoint.add_node_addr(addr).map_err(iroh_err)
    }

    /// This endpoint's own dialable address (direct addresses + home relay).
    ///
    /// Waits for the first direct-address sweep, so it can take a moment on a
    /// cold start.
    pub async fn node_addr(&self) -> Result<NodeAddr, NetError> {
        self.endpoint.node_addr().await.map_err(iroh_err)
    }

    /// Stream of accepted connections.
    ///
    /// Ends when the endpoint is closed. Individual handshake failures (a
    /// port-scanner, a stale address) are swallowed: they are noise, not
    /// events, and the stream must not die because of one bad packet.
    pub fn accept(&self) -> impl Stream<Item = Connection> + Send + 'static {
        let endpoint = self.endpoint.clone();
        stream::unfold(endpoint, |endpoint| async move {
            loop {
                let incoming = endpoint.accept().await?;
                let Ok(connecting) = incoming.accept() else {
                    continue;
                };
                if let Ok(conn) = connecting.await {
                    return Some((conn, endpoint));
                }
            }
        })
    }

    /// The underlying iroh endpoint, for APIs this wrapper does not cover yet.
    pub fn inner(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Close the endpoint, releasing its sockets.
    pub async fn close(self) {
        self.endpoint.close(0u32.into(), b"bye").await.ok();
    }
}

fn builder_for(identity: &DeviceIdentity, alpns: &[&[u8]]) -> iroh_net::endpoint::Builder {
    Endpoint::builder()
        .secret_key(identity.secret().clone())
        .alpns(alpns.iter().map(|alpn| alpn.to_vec()).collect())
}

/// Restrict a builder to loopback: no relay, no discovery, nothing off-box.
///
/// A free function would have to be called after `builder_for` and before
/// `bind`; a trait keeps the loopback decision in one place instead of three
/// builder calls repeated at every bind variant.
trait LocalOnly {
    fn local(self, port: u16) -> Self;
}

impl LocalOnly for iroh_net::endpoint::Builder {
    fn local(self, port: u16) -> Self {
        self.bind_addr_v4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
            .bind_addr_v6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, port, 0, 0))
            .relay_mode(RelayMode::Disabled)
            .clear_discovery()
    }
}

/// Bind every interface, still relays-off and discovery-off (server posture).
trait AnyInterface {
    fn any_interface(self, port: u16) -> Self;
}

impl AnyInterface for iroh_net::endpoint::Builder {
    fn any_interface(self, port: u16) -> Self {
        self.bind_addr_v4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
            // Port 0 for IPv6 on purpose: with an explicit port both sockets ask
            // for the same number and the second bind loses the race on macOS.
            // The v4 socket carries the published port; v6 stays ephemeral so a
            // dual-stack host still has a v6 path.
            .bind_addr_v6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0))
            .relay_mode(RelayMode::Disabled)
            .clear_discovery()
    }
}

fn iroh_err(err: impl std::fmt::Display) -> NetError {
    NetError::Iroh(err.to_string())
}

// --- echo protocol -------------------------------------------------------
//
// One bi-directional QUIC stream per exchange: client opens, writes, finishes;
// server reads to end and writes the same bytes back. No framing needed because
// the stream boundary is the frame.

/// Serve echo exchanges on `conn` until the peer closes the connection.
///
/// Returns the number of exchanges served.
pub async fn echo_serve(conn: &Connection) -> Result<usize, NetError> {
    let mut served = 0usize;
    loop {
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            // Peer is done, or the connection went away: stop serving.
            Err(_) => return Ok(served),
        };
        let msg = match recv.read_to_end(MAX_ECHO_BYTES).await {
            Ok(msg) => msg,
            Err(err) => return Err(NetError::Stream(err.to_string())),
        };
        send.write_all(&msg)
            .await
            .map_err(|err| NetError::Stream(err.to_string()))?;
        send.finish()
            .map_err(|err| NetError::Stream(err.to_string()))?;
        served += 1;
    }
}

/// Send `msg` on a fresh stream and return whatever the peer echoed back.
pub async fn echo(conn: &Connection, msg: &[u8]) -> Result<Vec<u8>, NetError> {
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|err| NetError::Stream(err.to_string()))?;
    send.write_all(msg)
        .await
        .map_err(|err| NetError::Stream(err.to_string()))?;
    send.finish()
        .map_err(|err| NetError::Stream(err.to_string()))?;
    recv.read_to_end(MAX_ECHO_BYTES)
        .await
        .map_err(|err| NetError::Stream(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::StreamExt;
    use iroh_net::key::SecretKey;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::time::timeout;

    /// An identity file unique to this test, removed on drop.
    struct TempIdentity {
        path: PathBuf,
        identity: DeviceIdentity,
    }

    impl TempIdentity {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("syswatch_t27_{name}.key"));
            let _ = std::fs::remove_file(&path);
            let identity = DeviceIdentity::load_or_create(&path).unwrap();
            Self { path, identity }
        }
    }

    impl Drop for TempIdentity {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[tokio::test]
    async fn binds_and_publishes_node_id() {
        let id = TempIdentity::new("bind");
        let ep = timeout(Duration::from_secs(15), IrohEndpoint::bind_local(&id.identity))
            .await
            .expect("bind timed out")
            .expect("bind failed");

        // The endpoint's NodeId is the identity's public key — nothing invented.
        assert_eq!(ep.node_id(), id.identity.node_id());
        assert_eq!(ep.node_id_string(), id.identity.node_id_string());
        assert!(!ep.node_id_string().is_empty());

        // bind_local really is local: the socket is bound and every advertised
        // direct address is loopback, so the later test cannot silently pass by
        // reaching out to the LAN.
        let (v4, _v6) = ep.inner().bound_sockets();
        assert!(v4.ip().is_loopback(), "bound on {v4}, expected loopback");

        let addr = timeout(Duration::from_secs(15), ep.node_addr())
            .await
            .expect("node_addr timed out")
            .expect("node_addr failed");
        assert_eq!(addr.node_id, ep.node_id());
        assert!(!addr.info.direct_addresses.is_empty());
        for direct in &addr.info.direct_addresses {
            assert!(direct.ip().is_loopback(), "direct addr {direct} is not loopback");
        }
        assert!(addr.info.relay_url.is_none(), "relays must be off for bind_local");
    }

    #[tokio::test]
    async fn two_endpoints_connect_and_echo() {
        let id_a = TempIdentity::new("peer_a");
        let id_b = TempIdentity::new("peer_b");

        let ep_a = timeout(Duration::from_secs(15), IrohEndpoint::bind_local(&id_a.identity))
            .await
            .expect("bind a timed out")
            .expect("bind a failed");
        let ep_b = timeout(Duration::from_secs(15), IrohEndpoint::bind_local(&id_b.identity))
            .await
            .expect("bind b timed out")
            .expect("bind b failed");

        // Distinct sockets, same identity as published.
        assert_ne!(ep_a.node_id(), ep_b.node_id());
        assert_eq!(ep_a.node_id(), id_a.identity.node_id());

        let addr_a = timeout(Duration::from_secs(15), ep_a.node_addr())
            .await
            .expect("node_addr timed out")
            .expect("node_addr failed");
        assert_eq!(addr_a.node_id, ep_a.node_id());

        // Server: take the first accepted connection and echo on it.
        let server = tokio::spawn(async move {
            let mut accepted = Box::pin(ep_a.accept());
            let conn = timeout(Duration::from_secs(15), accepted.next())
                .await
                .expect("accept timed out")
                .expect("accept stream ended");
            assert_eq!(
                iroh_net::endpoint::get_remote_node_id(&conn).unwrap(),
                id_b.identity.node_id()
            );
            echo_serve(&conn).await
        });

        let conn = timeout(Duration::from_secs(15), ep_b.connect_addr(addr_a))
            .await
            .expect("connect timed out")
            .expect("connect failed");

        let reply = timeout(Duration::from_secs(15), echo(&conn, b"syswatch/1 ping"))
            .await
            .expect("echo timed out")
            .expect("echo failed");
        assert_eq!(reply, b"syswatch/1 ping");

        // Second exchange on the same connection: the stream loop still serves.
        let reply2 = timeout(Duration::from_secs(15), echo(&conn, b"again"))
            .await
            .expect("echo timed out")
            .expect("echo failed");
        assert_eq!(reply2, b"again");

        conn.close(0u32.into(), b"done");
        let served = timeout(Duration::from_secs(15), server)
            .await
            .expect("server task timed out")
            .expect("server task panicked")
            .expect("echo_serve failed");
        assert_eq!(served, 2);
    }

    #[tokio::test]
    async fn connect_by_node_id_after_add_node_addr() {
        let id_a = TempIdentity::new("byid_a");
        let id_b = TempIdentity::new("byid_b");

        let ep_a = timeout(Duration::from_secs(15), IrohEndpoint::bind_local(&id_a.identity))
            .await
            .expect("bind a timed out")
            .expect("bind a failed");
        let ep_b = timeout(Duration::from_secs(15), IrohEndpoint::bind_local(&id_b.identity))
            .await
            .expect("bind b timed out")
            .expect("bind b failed");

        let addr_a = timeout(Duration::from_secs(15), ep_a.node_addr())
            .await
            .expect("node_addr timed out")
            .expect("node_addr failed");

        // The brief's API: dial by PublicKey only, using addressing info the
        // endpoint was taught beforehand.
        let node_id_a = addr_a.node_id;
        ep_b.add_node_addr(addr_a).expect("add_node_addr failed");

        let server = tokio::spawn(async move {
            let mut accepted = Box::pin(ep_a.accept());
            let conn = timeout(Duration::from_secs(15), accepted.next())
                .await
                .expect("accept timed out")
                .expect("accept stream ended");
            echo_serve(&conn).await
        });

        let conn = timeout(Duration::from_secs(15), ep_b.connect(node_id_a))
            .await
            .expect("connect timed out")
            .expect("connect failed by node id");
        assert_eq!(
            iroh_net::endpoint::get_remote_node_id(&conn).unwrap(),
            node_id_a
        );

        let reply = timeout(Duration::from_secs(15), echo(&conn, b"by-id"))
            .await
            .expect("echo timed out")
            .expect("echo failed");
        assert_eq!(reply, b"by-id");

        conn.close(0u32.into(), b"done");
        let served = timeout(Duration::from_secs(15), server)
            .await
            .expect("server task timed out")
            .expect("server task panicked")
            .expect("echo_serve failed");
        assert_eq!(served, 1);
    }

    #[tokio::test]
    async fn connect_to_unknown_node_errors() {
        let id = TempIdentity::new("unknown");
        let ep = timeout(Duration::from_secs(15), IrohEndpoint::bind_local(&id.identity))
            .await
            .expect("bind timed out")
            .expect("bind failed");

        // A NodeId with no address information can never be dialled.
        let stranger = SecretKey::generate().public();
        let result = timeout(Duration::from_secs(15), ep.connect(stranger))
            .await
            .expect("connect hung instead of failing");
        assert!(matches!(result, Err(NetError::Iroh(_))));
    }
}
