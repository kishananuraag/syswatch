//! Relay client: register with a relay, forward ciphertext, receive deliveries.
//!
//! # What the relay is (and is not)
//!
//! The relay is a stateless switch. It knows two things about a connection: the
//! [`PublicKey`] it was assigned at registration, and the live QUIC connection
//! to write to. It never sees a key, never decrypts, never looks inside a
//! payload, and never stores one. Everything that matters — identity, key
//! agreement, payload encryption — happened in [`crate::pairing`] before a byte
//! reached this module.
//!
//! Two devices that completed pairing do not need the relay to talk if they can
//! reach each other directly (iroh already tries that first). The relay exists
//! for the case they cannot: symmetric NAT, a firewall that only allows
//! outbound, a phone on a carrier network.
//!
//! # Routing ids are transport ids, verified
//!
//! The relay *assigns* each connection its routing id, and the assignment rule
//! is "it must equal the node id in the peer's TLS certificate". A client that
//! registers somebody else's id — the obvious interception attack on a routing
//! switch — is refused with [`wire::Status::BadIdentity`]. That is why a
//! registered id is safe to hand to a third device as a destination: the relay
//! can only misroute if it lies about the certificate it already verified, and
//! a misrouted message still fails to decrypt at the wrong device (the pairing
//! keys are per-pair, not per-relay).
//!
//! # Wire format
//!
//! One QUIC stream per frame — the stream boundary *is* the frame, the same
//! convention [`crate::pairing::PairedConnection`] uses, so nothing has to
//! decide where a message ends. Layouts live in [`wire`] because both sides of
//! the link need them: this client, and the server in `crates/relay`.
//!
//! ```text
//! client -> relay   register   [1]kind [1]version [32]node_id [2]token_len [n]token
//! client -> relay   forward    [1]kind [32]target  [4]len [n]ciphertext
//! relay  -> client  deliver    [1]kind [32]source  [4]len [n]ciphertext
//! relay  -> client  reply      [1]kind [1]status
//! ```
//!
//! # What the payload is
//!
//! Opaque bytes, `MAX_RELAY_BYTES` at most. In practice it is a
//! [`crate::pairing::SessionCipher`] frame, but this module does not know that
//! and does not care: the relay forwards whatever it is given, byte for byte.
//! That is testable from the outside — see `relay_sees_only_ciphertext` — and it
//! is the whole reason the relay is safe to run on someone else's machine.
//!
//! # Relay access token
//!
//! An optional shared secret gates *who may use the relay at all*. It is not
//! authentication of devices and it is not encryption: it is a bouncer. A relay
//! that serves one family or one fleet can require it; a public one can leave it
//! unset and still only route ciphertext.

// iroh-net 0.28 is the line Cargo.lock pins; its API carries a blanket
// "crate renamed" deprecation aimed at 0.29+. Staying on 0.28 is deliberate.
#![allow(deprecated)]

use iroh_base::key::PublicKey;
use iroh_net::endpoint::{Connection, NodeAddr, get_remote_node_id};

use crate::endpoint::IrohEndpoint;
use crate::{DeviceIdentity, NetError};

/// The relay protocol ALPN.
///
/// Separate from `syswatch/1` (echo) and `syswatch/pair/1` (pairing) on purpose:
/// ALPN is negotiated in the QUIC handshake, so a relay never accepts a pairing
/// dial and a device never offers relay frames on a pairing session.
pub const RELAY_ALPN: &[u8] = b"syswatch/relay/1";

/// Same protocol as a string, for logs and error messages.
pub const RELAY_ALPN_STR: &str = "syswatch/relay/1";

/// Largest payload the relay will carry in one message, in bytes.
///
/// A metric snapshot is a few kilobytes and a relay has to buffer a message
/// before it can forward it, so the cap is what bounds a relay's memory to
/// `MAX_ROUTES × queue depth × this`. Anything bigger is a file transfer, and a
/// file transfer does not belong on a routing switch.
pub const MAX_RELAY_BYTES: usize = wire::MAX_FORWARD_BYTES;

/// Re-exported for callers that want to name the wire types without reaching
/// into the submodule path.
pub use wire::Status as RelayStatus;

// --- wire format ---------------------------------------------------------

/// Frame layouts shared by [`RelayClient`] and the relay server.
///
/// Everything is big-endian; every field is fixed width except the trailing
/// payload, so a decoder can reject a frame from its length alone before it
/// looks at a byte of it.
pub mod wire {
    use iroh_base::key::PublicKey;

    /// Protocol version. A mismatch is refused, never guessed at: adding a field
    /// later must not make an old relay interpret a new frame's bytes as a
    /// length.
    pub const VERSION: u8 = 1;

    /// Node ids are ed25519 public keys — 32 bytes, always.
    pub const NODE_ID_LEN: usize = 32;

    /// Longest access token a registration may carry.
    pub const MAX_TOKEN_BYTES: usize = 256;

    /// Longest payload a single forward/delivery may carry.
    pub const MAX_FORWARD_BYTES: usize = 128 * 1024;

    /// `client -> relay`: register this connection under a routing id.
    pub const KIND_REGISTER: u8 = 1;
    /// `client -> relay`: forward this payload to `target`.
    pub const KIND_FORWARD: u8 = 2;
    /// `relay -> client`: here is a payload somebody forwarded to you.
    pub const KIND_DELIVER: u8 = 3;
    /// `relay -> client`: the outcome of a register/forward.
    pub const KIND_REPLY: u8 = 4;

    /// kind + version + node id + token length.
    pub const REGISTER_FIXED_LEN: usize = 1 + 1 + NODE_ID_LEN + 2;
    /// kind + target + length.
    pub const FORWARD_FIXED_LEN: usize = 1 + NODE_ID_LEN + 4;
    /// kind + source + length.
    pub const DELIVER_FIXED_LEN: usize = 1 + NODE_ID_LEN + 4;
    /// kind + status.
    pub const REPLY_LEN: usize = 2;

    /// Why a byte string is not a frame this build understands.
    ///
    /// Typed rather than one opaque string because the two sides act on it
    /// differently: the relay maps a version mismatch to
    /// [`Status::BadVersion`] and reports the rest as [`Status::Malformed`],
    /// while a client maps it to [`crate::NetError::Protocol`].
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum FrameError {
        #[error("not a relay frame: expected kind {expected}, got {got}")]
        Kind { expected: u8, got: u8 },
        #[error("unsupported relay protocol version {0}")]
        Version(u8),
        #[error("relay frame truncated: {0} more bytes needed")]
        Truncated(usize),
        #[error("relay frame has {0} trailing bytes")]
        Trailing(usize),
        #[error("relay frame carries no valid node id")]
        NodeId,
        #[error("relay token too large ({0} bytes, limit {1})")]
        TokenTooLarge(usize, usize),
        #[error("relay payload too large ({0} bytes, limit {1})")]
        PayloadTooLarge(usize, usize),
        #[error("unknown relay status byte {0}")]
        Status(u8),
    }

    /// Outcome byte in a [`KIND_REPLY`] frame.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(u8)]
    pub enum Status {
        /// Registered, or queued for delivery.
        Ok = 0,
        /// The peer speaks a protocol version this relay does not.
        BadVersion = 1,
        /// The registered id is not the id in the peer's TLS certificate.
        BadIdentity = 2,
        /// This relay requires an access token and the one presented was wrong.
        BadToken = 3,
        /// The frame did not decode.
        Malformed = 4,
        /// Nobody is currently registered under that routing id.
        UnknownPeer = 5,
        /// The payload (or token) exceeded the relay's limit.
        TooLarge = 6,
        /// The destination is the sender. A relay is a switch, not a mirror.
        SelfForward = 7,
        /// The destination's delivery queue is full, or the relay is at capacity.
        Busy = 8,
    }

    impl Status {
        /// The byte that goes on the wire.
        pub fn as_byte(self) -> u8 {
            self as u8
        }

        /// Inverse of [`Status::as_byte`]; `None` for a byte this build does not
        /// define (a newer relay, or garbage).
        pub fn from_byte(byte: u8) -> Option<Self> {
            Some(match byte {
                0 => Status::Ok,
                1 => Status::BadVersion,
                2 => Status::BadIdentity,
                3 => Status::BadToken,
                4 => Status::Malformed,
                5 => Status::UnknownPeer,
                6 => Status::TooLarge,
                7 => Status::SelfForward,
                8 => Status::Busy,
                _ => return None,
            })
        }

        /// Machine-friendly name, used in error messages and logs.
        pub fn as_str(self) -> &'static str {
            match self {
                Status::Ok => "ok",
                Status::BadVersion => "bad version",
                Status::BadIdentity => "bad identity",
                Status::BadToken => "bad token",
                Status::Malformed => "malformed frame",
                Status::UnknownPeer => "unknown peer",
                Status::TooLarge => "too large",
                Status::SelfForward => "self forward",
                Status::Busy => "busy",
            }
        }
    }

    impl std::fmt::Display for Status {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.as_str())
        }
    }

    impl From<FrameError> for crate::NetError {
        fn from(err: FrameError) -> Self {
            match err {
                // Keep the size semantics a caller can act on.
                FrameError::PayloadTooLarge(n, limit) | FrameError::TokenTooLarge(n, limit) => {
                    crate::NetError::TooLarge(n, limit)
                }
                FrameError::Version(v) => crate::NetError::Protocol(format!(
                    "relay speaks protocol version {v}, this build speaks {VERSION}"
                )),
                other => crate::NetError::Protocol(other.to_string()),
            }
        }
    }

    /// A decoded registration.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Registration<'a> {
        /// The id the client wants to be routed under.
        pub node_id: PublicKey,
        /// Access token as presented (empty when the frame carried none).
        pub token: &'a [u8],
    }

    /// A decoded forward request.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Forward<'a> {
        pub target: PublicKey,
        pub payload: &'a [u8],
    }

    /// A decoded delivery.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Delivery<'a> {
        pub source: PublicKey,
        pub payload: &'a [u8],
    }

    /// Encode a registration frame.
    pub fn encode_register(
        node_id: &PublicKey,
        token: Option<&[u8]>,
    ) -> Result<Vec<u8>, FrameError> {
        let token = token.unwrap_or(&[]);
        if token.len() > MAX_TOKEN_BYTES {
            return Err(FrameError::TokenTooLarge(token.len(), MAX_TOKEN_BYTES));
        }
        let mut out = Vec::with_capacity(REGISTER_FIXED_LEN + token.len());
        out.push(KIND_REGISTER);
        out.push(VERSION);
        out.extend_from_slice(node_id.as_bytes());
        out.extend_from_slice(&(token.len() as u16).to_be_bytes());
        out.extend_from_slice(token);
        Ok(out)
    }

    /// Decode a registration frame. The whole buffer must be the frame.
    pub fn decode_register(buf: &[u8]) -> Result<Registration<'_>, FrameError> {
        let rest = &mut &buf[..];
        expect_kind(rest, KIND_REGISTER)?;
        let version = take(rest, 1)?[0];
        if version != VERSION {
            return Err(FrameError::Version(version));
        }
        let node_id = node_id(take(rest, NODE_ID_LEN)?)?;
        let token_len = u16::from_be_bytes(
            take(rest, 2)?
                .try_into()
                .expect("take() returned exactly 2 bytes"),
        ) as usize;
        if token_len > MAX_TOKEN_BYTES {
            return Err(FrameError::TokenTooLarge(token_len, MAX_TOKEN_BYTES));
        }
        let token = take(rest, token_len)?;
        expect_end(rest)?;
        Ok(Registration { node_id, token })
    }

    /// Encode a forward frame.
    pub fn encode_forward(target: &PublicKey, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        if payload.len() > MAX_FORWARD_BYTES {
            return Err(FrameError::PayloadTooLarge(payload.len(), MAX_FORWARD_BYTES));
        }
        let mut out = Vec::with_capacity(FORWARD_FIXED_LEN + payload.len());
        out.push(KIND_FORWARD);
        out.extend_from_slice(target.as_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        Ok(out)
    }

    /// Decode a forward frame. The whole buffer must be the frame.
    pub fn decode_forward(buf: &[u8]) -> Result<Forward<'_>, FrameError> {
        let rest = &mut &buf[..];
        expect_kind(rest, KIND_FORWARD)?;
        let target = node_id(take(rest, NODE_ID_LEN)?)?;
        let len = length(rest)?;
        if len > MAX_FORWARD_BYTES {
            return Err(FrameError::PayloadTooLarge(len, MAX_FORWARD_BYTES));
        }
        let payload = take(rest, len)?;
        expect_end(rest)?;
        Ok(Forward { target, payload })
    }

    /// Encode a delivery frame.
    pub fn encode_deliver(source: &PublicKey, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        if payload.len() > MAX_FORWARD_BYTES {
            return Err(FrameError::PayloadTooLarge(payload.len(), MAX_FORWARD_BYTES));
        }
        let mut out = Vec::with_capacity(DELIVER_FIXED_LEN + payload.len());
        out.push(KIND_DELIVER);
        out.extend_from_slice(source.as_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        Ok(out)
    }

    /// Decode a delivery frame. The whole buffer must be the frame.
    pub fn decode_deliver(buf: &[u8]) -> Result<Delivery<'_>, FrameError> {
        let rest = &mut &buf[..];
        expect_kind(rest, KIND_DELIVER)?;
        let source = node_id(take(rest, NODE_ID_LEN)?)?;
        let len = length(rest)?;
        if len > MAX_FORWARD_BYTES {
            return Err(FrameError::PayloadTooLarge(len, MAX_FORWARD_BYTES));
        }
        let payload = take(rest, len)?;
        expect_end(rest)?;
        Ok(Delivery { source, payload })
    }

    /// Encode a reply frame: two bytes, no payload.
    pub fn encode_reply(status: Status) -> [u8; REPLY_LEN] {
        [KIND_REPLY, status.as_byte()]
    }

    /// Decode a reply frame.
    pub fn decode_reply(buf: &[u8]) -> Result<Status, FrameError> {
        let rest = &mut &buf[..];
        expect_kind(rest, KIND_REPLY)?;
        let byte = take(rest, 1)?[0];
        let status = Status::from_byte(byte).ok_or(FrameError::Status(byte))?;
        expect_end(rest)?;
        Ok(status)
    }

    /// The kind byte of a frame, for a dispatcher that must decide before it
    /// knows which decoder to run.
    pub fn frame_kind(buf: &[u8]) -> Result<u8, FrameError> {
        Ok(*buf.first().ok_or(FrameError::Truncated(1))?)
    }

    // --- primitives ------------------------------------------------------

    fn take<'a>(rest: &mut &'a [u8], n: usize) -> Result<&'a [u8], FrameError> {
        if rest.len() < n {
            return Err(FrameError::Truncated(n - rest.len()));
        }
        let (head, tail) = rest.split_at(n);
        *rest = tail;
        Ok(head)
    }

    fn expect_kind(rest: &mut &[u8], expected: u8) -> Result<(), FrameError> {
        let got = take(rest, 1)?[0];
        if got != expected {
            return Err(FrameError::Kind { expected, got });
        }
        Ok(())
    }

    fn expect_end(rest: &[u8]) -> Result<(), FrameError> {
        if rest.is_empty() {
            Ok(())
        } else {
            Err(FrameError::Trailing(rest.len()))
        }
    }

    fn length(rest: &mut &[u8]) -> Result<usize, FrameError> {
        let bytes: [u8; 4] = take(rest, 4)?
            .try_into()
            .expect("take() returned exactly 4 bytes");
        Ok(u32::from_be_bytes(bytes) as usize)
    }

    fn node_id(bytes: &[u8]) -> Result<PublicKey, FrameError> {
        let arr: [u8; NODE_ID_LEN] = bytes.try_into().map_err(|_| FrameError::NodeId)?;
        PublicKey::from_bytes(&arr).map_err(|_| FrameError::NodeId)
    }
}

// --- client --------------------------------------------------------------

/// A registered connection to a relay.
///
/// Cheap to hold and safe to share (`Send + Sync`): every call takes `&self`
/// and the QUIC connection underneath is the shared, multiplexed object — the
/// same one that carries concurrent forwards and deliveries without
/// head-of-line blocking between them.
///
/// # Reconnecting
///
/// Do not build a second one-shot client with the same identity to retry: as of
/// iroh 0.28 the relay maps a node id to one transport address, so a *new*
/// endpoint using the same key cannot redial the same relay until iroh forgets
/// the old connection (measured: the dial hangs past 8 s, while a redial on the
/// same endpoint completes in 12 ms). Keep a [`RelayLink`] for the device's
/// lifetime and call [`RelayLink::connect`] again — that is the production
/// shape anyway: one endpoint per device, connections come and go.
pub struct RelayClient {
    conn: Connection,
    /// A handle to the endpoint that carries this connection, so the socket
    /// cannot be dropped out from under it.
    endpoint: IrohEndpoint,
    routing_id: PublicKey,
    relay: PublicKey,
}

impl std::fmt::Debug for RelayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayClient")
            .field("relay", &self.relay.fmt_short())
            .field("routing_id", &self.routing_id.fmt_short())
            .finish_non_exhaustive()
    }
}

/// A device's relay endpoint: bind once, connect (and reconnect) as needed.
///
/// The endpoint is the expensive, identity-bearing half of the link — its node
/// id is derived from the device key at bind time and does not change across
/// reconnects. Keeping it alive across a relay restart is what makes "the relay
/// rebooted, re-register" a 12 ms operation instead of a hang (see
/// [`RelayClient`]'s note on reconnecting).
pub struct RelayLink {
    endpoint: IrohEndpoint,
    relay: NodeAddr,
    token: Option<Vec<u8>>,
}

impl std::fmt::Debug for RelayLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayLink")
            .field("relay", &self.relay.node_id.fmt_short())
            .field("routing_id", &self.endpoint.node_id().fmt_short())
            .field("token", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

impl RelayLink {
    /// Bind a production-posture endpoint (relays and discovery on) for `relay`.
    pub async fn open(identity: &DeviceIdentity, relay: NodeAddr) -> Result<Self, NetError> {
        Self::open_with_token(identity, relay, None).await
    }

    /// [`RelayLink::open`] with a relay access token.
    pub async fn open_with_token(
        identity: &DeviceIdentity,
        relay: NodeAddr,
        token: Option<&[u8]>,
    ) -> Result<Self, NetError> {
        let endpoint = IrohEndpoint::bind_alpns(identity, &[RELAY_ALPN]).await?;
        Ok(Self::with_endpoint(endpoint, relay, token))
    }

    /// [`RelayLink::open`] restricted to loopback (tests, offline demos).
    pub async fn open_local(identity: &DeviceIdentity, relay: NodeAddr) -> Result<Self, NetError> {
        Self::open_local_with_token(identity, relay, None).await
    }

    /// [`RelayLink::open_local`] with a relay access token.
    pub async fn open_local_with_token(
        identity: &DeviceIdentity,
        relay: NodeAddr,
        token: Option<&[u8]>,
    ) -> Result<Self, NetError> {
        let endpoint = IrohEndpoint::bind_local_alpns(identity, &[RELAY_ALPN]).await?;
        Ok(Self::with_endpoint(endpoint, relay, token))
    }

    /// Wrap an endpoint the caller already owns.
    ///
    /// The endpoint must offer [`RELAY_ALPN`] and be bound from the identity the
    /// relay should route under.
    pub fn with_endpoint(
        endpoint: IrohEndpoint,
        relay: NodeAddr,
        token: Option<&[u8]>,
    ) -> Self {
        Self {
            endpoint,
            relay,
            token: token.filter(|token| !token.is_empty()).map(<[u8]>::to_vec),
        }
    }

    /// The node id this link registers as.
    pub fn routing_id(&self) -> PublicKey {
        self.endpoint.node_id()
    }

    /// The relay this link dials.
    pub fn relay_node_id(&self) -> PublicKey {
        self.relay.node_id
    }

    /// The endpoint underneath (kept alive by this link).
    pub fn endpoint(&self) -> &IrohEndpoint {
        &self.endpoint
    }

    /// Dial the relay and register. Call again after a drop.
    pub async fn connect(&self) -> Result<RelayClient, NetError> {
        self.connect_with_token(self.token.as_deref()).await
    }

    /// [`RelayLink::connect`] with a one-off token, overriding the link's.
    ///
    /// For a token that rotates: the endpoint (and the node id it registers as)
    /// must survive the rotation, so the new secret is a parameter of the
    /// attempt, not a reason to rebuild the device.
    pub async fn connect_with_token(&self, token: Option<&[u8]>) -> Result<RelayClient, NetError> {
        RelayClient::register_on(&self.endpoint, self.relay.clone(), token).await
    }
}

impl RelayClient {
    /// Dial `relay` and register under this device's node id.
    ///
    /// Production posture: relays and discovery on, so the relay can be dialled
    /// from anywhere (via its own [`NodeAddr`]).
    ///
    /// One-shot: this convenience binds its own endpoint, which is why it is the
    /// right call for a single connection and the wrong one for a retry loop —
    /// use [`RelayLink`] there.
    pub async fn connect(identity: &DeviceIdentity, relay: NodeAddr) -> Result<Self, NetError> {
        Self::connect_with_token(identity, relay, None).await
    }

    /// [`RelayClient::connect`] presenting a relay access token.
    ///
    /// `None` and `Some(b"")` are the same thing on the wire: a registration
    /// with no token. A relay that requires one answers
    /// [`NetError::RelayRejected`] with `"bad token"`.
    pub async fn connect_with_token(
        identity: &DeviceIdentity,
        relay: NodeAddr,
        token: Option<&[u8]>,
    ) -> Result<Self, NetError> {
        RelayLink::open_with_token(identity, relay, token)
            .await?
            .connect()
            .await
    }

    /// [`RelayClient::connect`] with everything kept on loopback.
    ///
    /// Tests and offline demos: no relay service, no discovery, nothing off-box.
    pub async fn connect_local(
        identity: &DeviceIdentity,
        relay: NodeAddr,
    ) -> Result<Self, NetError> {
        Self::connect_local_with_token(identity, relay, None).await
    }

    /// [`RelayClient::connect_local`] presenting a relay access token.
    pub async fn connect_local_with_token(
        identity: &DeviceIdentity,
        relay: NodeAddr,
        token: Option<&[u8]>,
    ) -> Result<Self, NetError> {
        RelayLink::open_local_with_token(identity, relay, token)
            .await?
            .connect()
            .await
    }

    /// Dial + register on an already-bound endpoint.
    async fn register_on(
        endpoint: &IrohEndpoint,
        relay: NodeAddr,
        token: Option<&[u8]>,
    ) -> Result<Self, NetError> {
        let expected = relay.node_id;
        let conn = endpoint.connect_addr_alpn(relay, RELAY_ALPN).await?;

        // iroh verified the TLS certificate against `expected`, so this is a
        // read-back of that check rather than a second, independent one.
        let relay_id = match get_remote_node_id(&conn) {
            Ok(id) if id == expected => id,
            Ok(id) => {
                conn.close(0u32.into(), b"relay identity mismatch");
                return Err(NetError::Iroh(format!(
                    "relay presented node id {id}, expected {expected}"
                )));
            }
            Err(err) => {
                conn.close(0u32.into(), b"no relay identity");
                return Err(NetError::Iroh(format!("relay has no node id: {err}")));
            }
        };

        let routing_id = endpoint.node_id();
        let frame = wire::encode_register(&routing_id, token)?;
        let (mut send, mut reply) = conn.open_bi().await.map_err(stream_err)?;
        send.write_all(&frame).await.map_err(stream_err)?;
        send.finish().map_err(stream_err)?;
        let buf = reply
            .read_to_end(wire::REPLY_LEN)
            .await
            .map_err(stream_err)?;
        let status = if buf.is_empty() {
            conn.close(0u32.into(), b"no registration reply");
            return Err(NetError::Stream(
                "relay closed the registration stream without a reply".into(),
            ));
        } else {
            wire::decode_reply(&buf)?
        };
        if !matches!(status, wire::Status::Ok) {
            conn.close(0u32.into(), b"registration rejected");
            return Err(NetError::RelayRejected(status.as_str().to_string()));
        }

        Ok(Self {
            conn,
            endpoint: endpoint.clone(),
            routing_id,
            relay: relay_id,
        })
    }

    /// The id this connection is registered under (this device's node id).
    pub fn routing_id(&self) -> PublicKey {
        self.routing_id
    }

    /// The relay's node id.
    pub fn relay_node_id(&self) -> PublicKey {
        self.relay
    }

    /// The underlying iroh connection, for callers that need transport stats.
    pub fn inner(&self) -> &Connection {
        &self.conn
    }

    /// The endpoint carrying this connection.
    pub fn endpoint(&self) -> &IrohEndpoint {
        &self.endpoint
    }

    /// Send `data` to the device registered as `peer`.
    ///
    /// Returns once the relay has *queued* the payload on the destination's
    /// connection — not once the destination has read it: the relay cannot know
    /// that, and pretending otherwise would be a lie in the API. Delivery
    /// failures after that point are the pairing layer's business (an unopenable
    /// frame is a `NetError::Crypto` at the recipient), which is the correct
    /// place for them: the relay cannot tell a good ciphertext from a bad one.
    ///
    /// Fails with [`NetError::RelayRejected`] when the relay declines
    /// (`"unknown peer"`, `"busy"`, `"self forward"`), and with
    /// [`NetError::TooLarge`] before anything is written when `data` exceeds
    /// [`MAX_RELAY_BYTES`].
    pub async fn forward_to(&self, peer: PublicKey, data: &[u8]) -> Result<(), NetError> {
        let frame = wire::encode_forward(&peer, data)?;
        let (mut send, mut reply) = self.conn.open_bi().await.map_err(stream_err)?;
        send.write_all(&frame).await.map_err(stream_err)?;
        send.finish().map_err(stream_err)?;
        let buf = reply
            .read_to_end(wire::REPLY_LEN)
            .await
            .map_err(stream_err)?;
        let status = if buf.is_empty() {
            return Err(NetError::Stream(
                "relay closed the forward stream without a reply".into(),
            ));
        } else {
            wire::decode_reply(&buf)?
        };
        match status {
            wire::Status::Ok => Ok(()),
            other => Err(NetError::RelayRejected(other.as_str().to_string())),
        }
    }

    /// Wait for the next payload forwarded to this device.
    ///
    /// `None` means the channel is finished — the relay went away, or it sent
    /// something that is not a delivery frame. The second case is a protocol
    /// violation rather than a quiet close; [`RelayClient::try_recv`] is the
    /// form that tells the two apart.
    pub async fn recv(&self) -> Option<(PublicKey, Vec<u8>)> {
        self.try_recv().await.unwrap_or_default()
    }

    /// [`RelayClient::recv`] without swallowing the reason.
    ///
    /// `Ok(None)` = the relay connection ended. `Err` = it broke the frame
    /// format, which is worth surfacing: silently dropping a malformed frame
    /// would let a misbehaving relay look healthy.
    pub async fn try_recv(&self) -> Result<Option<(PublicKey, Vec<u8>)>, NetError> {
        let (_send, mut recv) = match self.conn.accept_bi().await {
            Ok(pair) => pair,
            // The connection is gone (closed by the relay, or the network);
            // there will be no more deliveries on it.
            Err(_) => return Ok(None),
        };
        let buf = recv
            .read_to_end(wire::DELIVER_FIXED_LEN + wire::MAX_FORWARD_BYTES)
            .await
            .map_err(stream_err)?;
        let delivery = wire::decode_deliver(&buf)?;
        Ok(Some((delivery.source, delivery.payload.to_vec())))
    }

    /// Close the relay connection politely.
    ///
    /// The relay drops the route as soon as it notices, so nobody forwards into
    /// a dead connection for longer than the QUIC close takes.
    pub fn close(self) {
        self.conn.close(0u32.into(), b"relay client closed");
    }
}

fn stream_err(err: impl std::fmt::Display) -> NetError {
    NetError::Stream(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures_lite::StreamExt;
    use iroh_net::key::SecretKey;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    use crate::pairing::SessionCipher;
    use crate::test_support::TempIdentity;

    /// Every network step here is loopback and fast; a hung one must fail the
    /// test rather than hang the suite.
    const STEP: Duration = Duration::from_secs(20);

    /// Queue depth of the test relay's per-peer delivery channel.
    const TEST_QUEUE: usize = 8;

    type Deliveries = mpsc::Sender<(PublicKey, Vec<u8>)>;

    /// An in-process relay built from the *same* [`wire`] types the real one in
    /// `crates/relay` uses.
    ///
    /// Deliberately dumb: it routes, counts, and records the exact payloads it
    /// forwarded. That record is the evidence for the two opacity claims a
    /// client can actually check from its side of the link — the payload reaches
    /// the peer byte for byte, and the relay never needed a key to do it.
    struct TestRelay {
        endpoint: IrohEndpoint,
        routes: Arc<Mutex<HashMap<PublicKey, Deliveries>>>,
        forwarded: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl TestRelay {
        async fn start(name: &str) -> Self {
            let identity = TempIdentity::new(name);
            let endpoint =
                timeout(STEP, IrohEndpoint::bind_local_alpns(&identity.identity, &[RELAY_ALPN]))
                    .await
                    .expect("bind timed out")
                    .expect("bind failed");

            let routes: Arc<Mutex<HashMap<PublicKey, Deliveries>>> = Arc::default();
            let forwarded: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();

            let accept_endpoint = endpoint.clone();
            let accept_routes = routes.clone();
            let accept_forwarded = forwarded.clone();
            tokio::spawn(async move {
                let mut accepted = Box::pin(accept_endpoint.accept());
                while let Some(conn) = accepted.next().await {
                    tokio::spawn(serve(
                        conn,
                        accept_routes.clone(),
                        accept_forwarded.clone(),
                    ));
                }
            });

            Self {
                endpoint,
                routes,
                forwarded,
            }
        }

        async fn node_addr(&self) -> NodeAddr {
            timeout(STEP, self.endpoint.node_addr())
                .await
                .expect("node_addr timed out")
                .expect("node_addr failed")
        }

        /// Payloads the relay forwarded, in arrival order.
        fn forwarded(&self) -> Vec<Vec<u8>> {
            self.forwarded.lock().expect("forwarded poisoned").clone()
        }

        fn route_count(&self) -> usize {
            self.routes.lock().expect("routes poisoned").len()
        }

        /// Tear the relay down: close the endpoint so every connection dies.
        async fn shutdown(&self) {
            self.endpoint.clone().close().await;
        }
    }

    async fn serve(
        conn: Connection,
        routes: Arc<Mutex<HashMap<PublicKey, Deliveries>>>,
        forwarded: Arc<Mutex<Vec<Vec<u8>>>>,
    ) {
        let Ok(remote) = get_remote_node_id(&conn) else {
            return;
        };

        // Flight 1: registration. Anything else is refused.
        let Ok((mut reply_send, mut reply_recv)) = conn.accept_bi().await else {
            return;
        };
        let Ok(buf) = reply_recv
            .read_to_end(wire::REGISTER_FIXED_LEN + wire::MAX_TOKEN_BYTES)
            .await
        else {
            return;
        };
        let status = match wire::decode_register(&buf) {
            Ok(reg) if reg.node_id == remote => wire::Status::Ok,
            Ok(_) => wire::Status::BadIdentity,
            Err(wire::FrameError::Version(_)) => wire::Status::BadVersion,
            Err(_) => wire::Status::Malformed,
        };
        let _ = reply_send.write_all(&wire::encode_reply(status)).await;
        let _ = reply_send.finish();
        if status != wire::Status::Ok {
            conn.close(0u32.into(), b"registration refused");
            return;
        }

        let (tx, mut rx) = mpsc::channel::<(PublicKey, Vec<u8>)>(TEST_QUEUE);
        routes.lock().expect("routes poisoned").insert(remote, tx);

        // Delivery writer: one stream per delivery, so ordering is the channel's
        // and the relay never blocks a sender on a slow destination.
        let writer_conn = conn.clone();
        tokio::spawn(async move {
            while let Some((source, payload)) = rx.recv().await {
                let Ok(frame) = wire::encode_deliver(&source, &payload) else {
                    break;
                };
                let Ok((mut send, _recv)) = writer_conn.open_bi().await else {
                    break;
                };
                if send.write_all(&frame).await.is_err() || send.finish().is_err() {
                    break;
                }
            }
        });

        // Flight 2..n: forwards.
        loop {
            let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                break;
            };
            let Ok(buf) = recv
                .read_to_end(wire::FORWARD_FIXED_LEN + wire::MAX_FORWARD_BYTES)
                .await
            else {
                break;
            };
            let status = match wire::decode_forward(&buf) {
                Ok(fwd) => {
                    if fwd.target == remote {
                        wire::Status::SelfForward
                    } else {
                        let route = routes
                            .lock()
                            .expect("routes poisoned")
                            .get(&fwd.target)
                            .cloned();
                        match route {
                            None => wire::Status::UnknownPeer,
                            Some(tx) => match tx.try_send((remote, fwd.payload.to_vec())) {
                                Ok(()) => {
                                    // Recorded only once it is actually queued for
                                    // a live peer: "forwarded" must mean forwarded.
                                    forwarded
                                        .lock()
                                        .expect("forwarded poisoned")
                                        .push(fwd.payload.to_vec());
                                    wire::Status::Ok
                                }
                                Err(_) => wire::Status::Busy,
                            },
                        }
                    }
                }
                Err(_) => wire::Status::Malformed,
            };
            let _ = send.write_all(&wire::encode_reply(status)).await;
            let _ = send.finish();
        }

        routes.lock().expect("routes poisoned").remove(&remote);
    }

    /// Register `identity` against `relay`.
    async fn client(identity: &TempIdentity, relay: &TestRelay) -> RelayClient {
        timeout(
            STEP,
            RelayClient::connect_local(&identity.identity, relay.node_addr().await),
        )
        .await
        .expect("register timed out")
        .expect("register failed")
    }

    /// One delivery with a deadline, so a lost message fails instead of hanging.
    async fn next(client: &RelayClient) -> (PublicKey, Vec<u8>) {
        timeout(STEP, client.try_recv())
            .await
            .expect("recv timed out")
            .expect("recv failed")
            .expect("relay connection ended")
    }

    // --- wire codec ------------------------------------------------------

    #[test]
    fn register_frame_roundtrips() {
        let id = SecretKey::generate().public();

        let bare = wire::encode_register(&id, None).unwrap();
        assert_eq!(bare.len(), wire::REGISTER_FIXED_LEN);
        let reg = wire::decode_register(&bare).unwrap();
        assert_eq!(reg.node_id, id);
        assert!(reg.token.is_empty());

        let token = b"family-secret";
        let with_token = wire::encode_register(&id, Some(token)).unwrap();
        assert_eq!(with_token.len(), wire::REGISTER_FIXED_LEN + token.len());
        let reg = wire::decode_register(&with_token).unwrap();
        assert_eq!(reg.node_id, id);
        assert_eq!(reg.token, token);

        // An oversized token is refused at encode time, not truncated.
        let too_long = vec![7u8; wire::MAX_TOKEN_BYTES + 1];
        assert!(matches!(
            wire::encode_register(&id, Some(&too_long)),
            Err(wire::FrameError::TokenTooLarge(_, _))
        ));
    }

    #[test]
    fn bad_registration_frames_are_rejected() {
        let id = SecretKey::generate().public();
        let good = wire::encode_register(&id, Some(b"tok")).unwrap();

        // Wrong kind: a forward frame is not a registration.
        let mut wrong_kind = good.clone();
        wrong_kind[0] = wire::KIND_FORWARD;
        assert!(matches!(
            wire::decode_register(&wrong_kind),
            Err(wire::FrameError::Kind { .. })
        ));

        // Future protocol version: refused, never guessed at.
        let mut future = good.clone();
        future[1] = wire::VERSION + 1;
        assert_eq!(
            wire::decode_register(&future),
            Err(wire::FrameError::Version(wire::VERSION + 1))
        );

        // Truncated at every length the frame has.
        for cut in 1..good.len() {
            assert!(
                matches!(
                    wire::decode_register(&good[..cut]),
                    Err(wire::FrameError::Truncated(_))
                ),
                "a {cut}-byte prefix must not decode"
            );
        }

        // Trailing bytes: the stream boundary is the frame, so extra bytes are a
        // bug, not a second frame.
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(matches!(
            wire::decode_register(&trailing),
            Err(wire::FrameError::Trailing(1))
        ));

        // A token length that lies about the buffer it was given.
        let mut lying = good.clone();
        let token_len_at = wire::REGISTER_FIXED_LEN - 2;
        lying[token_len_at..wire::REGISTER_FIXED_LEN].copy_from_slice(&99u16.to_be_bytes());
        assert!(matches!(
            wire::decode_register(&lying),
            Err(wire::FrameError::Truncated(_))
        ));

        // Empty buffer.
        assert!(matches!(
            wire::decode_register(&[]),
            Err(wire::FrameError::Truncated(1))
        ));
    }

    #[test]
    fn forward_and_delivery_frames_roundtrip() {
        let target = SecretKey::generate().public();
        let source = SecretKey::generate().public();
        let payload = vec![0xABu8; 4096];

        let frame = wire::encode_forward(&target, &payload).unwrap();
        assert_eq!(frame.len(), wire::FORWARD_FIXED_LEN + payload.len());
        let fwd = wire::decode_forward(&frame).unwrap();
        assert_eq!(fwd.target, target);
        assert_eq!(fwd.payload, &payload[..]);

        let frame = wire::encode_deliver(&source, &payload).unwrap();
        assert_eq!(frame.len(), wire::DELIVER_FIXED_LEN + payload.len());
        let delivery = wire::decode_deliver(&frame).unwrap();
        assert_eq!(delivery.source, source);
        assert_eq!(delivery.payload, &payload[..]);

        // Empty payloads are legal: a message can be empty, and the relay must
        // not invent bytes.
        let empty = wire::encode_forward(&target, &[]).unwrap();
        assert_eq!(wire::decode_forward(&empty).unwrap().payload, &[] as &[u8]);

        // One byte over the cap is refused before it is written.
        let too_big = vec![0u8; wire::MAX_FORWARD_BYTES + 1];
        assert!(matches!(
            wire::encode_forward(&target, &too_big),
            Err(wire::FrameError::PayloadTooLarge(_, _))
        ));
        assert!(matches!(
            wire::encode_deliver(&source, &too_big),
            Err(wire::FrameError::PayloadTooLarge(_, _))
        ));

        // A cross-kind frame is not a delivery.
        let frame = wire::encode_forward(&target, &payload).unwrap();
        assert!(matches!(
            wire::decode_deliver(&frame),
            Err(wire::FrameError::Kind { .. })
        ));
    }

    #[test]
    fn reply_frames_roundtrip() {
        for status in [
            wire::Status::Ok,
            wire::Status::BadVersion,
            wire::Status::BadIdentity,
            wire::Status::BadToken,
            wire::Status::Malformed,
            wire::Status::UnknownPeer,
            wire::Status::TooLarge,
            wire::Status::SelfForward,
            wire::Status::Busy,
        ] {
            let frame = wire::encode_reply(status);
            assert_eq!(frame.len(), wire::REPLY_LEN);
            assert_eq!(wire::decode_reply(&frame).unwrap(), status);
        }

        // An undefined status byte is an error, not a silent `Ok`.
        assert_eq!(
            wire::decode_reply(&[wire::KIND_REPLY, 0xEE]),
            Err(wire::FrameError::Status(0xEE))
        );
        assert!(matches!(
            wire::decode_reply(&[wire::KIND_DELIVER, 0]),
            Err(wire::FrameError::Kind { .. })
        ));
    }

    #[test]
    fn frame_errors_map_to_net_errors() {
        // Size problems keep their size semantics so a caller can act on them.
        assert!(matches!(
            NetError::from(wire::FrameError::PayloadTooLarge(9, 8)),
            NetError::TooLarge(9, 8)
        ));
        // Everything else is a protocol error naming the reason.
        let err = NetError::from(wire::FrameError::Version(9));
        assert!(matches!(err, NetError::Protocol(_)));
        assert!(err.to_string().contains("version 9"));
    }

    // --- end to end ------------------------------------------------------

    #[tokio::test]
    async fn two_clients_exchange_messages_through_the_relay() {
        let relay = TestRelay::start("t30_exchange").await;
        let id_a = TempIdentity::new("t30_exchange_a");
        let id_b = TempIdentity::new("t30_exchange_b");
        let a = client(&id_a, &relay).await;
        let b = client(&id_b, &relay).await;

        // Each side is registered under its own node id, and the relay knows
        // both — nothing invented on the client's behalf.
        assert_eq!(a.routing_id(), id_a.identity.node_id());
        assert_eq!(b.routing_id(), id_b.identity.node_id());
        assert_eq!(a.relay_node_id(), relay.endpoint.node_id());
        assert_eq!(relay.route_count(), 2);

        // A -> B, three messages, in order.
        for n in 0..3u8 {
            let body = vec![n; 32];
            timeout(STEP, a.forward_to(b.routing_id(), &body))
                .await
                .expect("forward timed out")
                .expect("forward failed");
        }
        for n in 0..3u8 {
            let (source, body) = next(&b).await;
            assert_eq!(source, a.routing_id(), "delivery must name the sender");
            assert_eq!(body, vec![n; 32], "messages must arrive in order");
        }

        // B -> A on the same connections: routing is symmetric.
        timeout(STEP, b.forward_to(a.routing_id(), b"pong"))
            .await
            .expect("forward timed out")
            .expect("forward failed");
        let (source, body) = next(&a).await;
        assert_eq!(source, b.routing_id());
        assert_eq!(body, b"pong");

        // Nothing is left over in either direction.
        for stalled in [&a, &b] {
            assert!(
                timeout(Duration::from_millis(300), stalled.try_recv())
                    .await
                    .is_err(),
                "no extra delivery should be waiting"
            );
        }
    }

    #[tokio::test]
    async fn unknown_peer_is_refused() {
        let relay = TestRelay::start("t30_unknown").await;
        let me = TempIdentity::new("t30_unknown_me");
        let client = client(&me, &relay).await;

        let stranger = SecretKey::generate().public();
        let err = timeout(STEP, client.forward_to(stranger, b"hello"))
            .await
            .expect("forward timed out")
            .expect_err("an unregistered destination must not report success");
        assert!(
            matches!(&err, NetError::RelayRejected(reason) if reason == "unknown peer"),
            "got {err:?}"
        );

        // The relay dropped the frame; it did not buffer it for a future peer.
        assert!(relay.forwarded().is_empty());
    }

    #[tokio::test]
    async fn forwarding_to_self_is_refused() {
        let relay = TestRelay::start("t30_self").await;
        let me = TempIdentity::new("t30_self_me");
        let client = client(&me, &relay).await;

        let err = timeout(STEP, client.forward_to(client.routing_id(), b"echo?"))
            .await
            .expect("forward timed out")
            .expect_err("a relay is a switch, not a mirror");
        assert!(
            matches!(&err, NetError::RelayRejected(reason) if reason == "self forward"),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn oversized_payload_fails_before_the_wire() {
        let relay = TestRelay::start("t30_big").await;
        let me = TempIdentity::new("t30_big_me");
        let other = TempIdentity::new("t30_big_other");
        let client = client(&me, &relay).await;
        let peer = self::client(&other, &relay).await;

        let too_big = vec![0u8; MAX_RELAY_BYTES + 1];
        let err = timeout(STEP, client.forward_to(peer.routing_id(), &too_big))
            .await
            .expect("forward timed out")
            .expect_err("oversized payload must be refused");
        assert!(
            matches!(err, NetError::TooLarge(n, limit) if n == MAX_RELAY_BYTES + 1 && limit == MAX_RELAY_BYTES),
            "got {err:?}"
        );

        // Refused locally: the relay never saw a byte of it.
        assert!(relay.forwarded().is_empty());

        // The largest legal payload still goes through untouched.
        let max = vec![0x5Au8; MAX_RELAY_BYTES];
        timeout(STEP, client.forward_to(peer.routing_id(), &max))
            .await
            .expect("forward timed out")
            .expect("a payload at the limit is allowed");
        let (_, body) = next(&peer).await;
        assert_eq!(body.len(), MAX_RELAY_BYTES);
        assert_eq!(body, max);
    }

    #[tokio::test]
    async fn relay_sees_only_ciphertext() {
        let relay = TestRelay::start("t30_opaque").await;
        let id_a = TempIdentity::new("t30_opaque_a");
        let id_b = TempIdentity::new("t30_opaque_b");
        let a = client(&id_a, &relay).await;
        let b = client(&id_b, &relay).await;

        // The pairing layer's cipher, used exactly as a paired device would:
        // the relay gets a sealed frame and has no key to open it.
        let send_key = [7u8; 32];
        let recv_key = [9u8; 32];
        let sender = SessionCipher::new(&send_key, &recv_key);
        let receiver = SessionCipher::new(&recv_key, &send_key);
        let plaintext = b"cpu=18.9 ram=70.4 this text must never reach the relay";

        let frame = sender.seal(plaintext).unwrap();
        assert!(!frame.windows(plaintext.len()).any(|w| w == plaintext));

        timeout(STEP, a.forward_to(b.routing_id(), &frame))
            .await
            .expect("forward timed out")
            .expect("forward failed");

        // What the relay forwarded is byte-for-byte the sealed frame: no
        // expansion, no rewriting, no inspection.
        let forwarded = relay.forwarded();
        assert_eq!(forwarded.len(), 1);
        assert_eq!(forwarded[0], frame);

        // The real recipient opens it; a third party with different keys cannot.
        let (source, received) = next(&b).await;
        assert_eq!(source, a.routing_id());
        assert_eq!(received, frame);
        assert_eq!(receiver.open(&received).unwrap(), plaintext);

        let stranger = SessionCipher::new(&[1u8; 32], &[2u8; 32]);
        assert!(stranger.open(&forwarded[0]).is_err());
    }

    #[tokio::test]
    async fn closing_the_relay_ends_recv() {
        let relay = TestRelay::start("t30_gone").await;
        let me = TempIdentity::new("t30_gone_me");
        let client = client(&me, &relay).await;

        // Nothing to read yet: recv blocks, it does not return a fake message.
        assert!(
            timeout(Duration::from_millis(250), client.try_recv())
                .await
                .is_err()
        );

        relay.shutdown().await;

        // After the relay goes away the channel is finished. `recv` says so with
        // `None` rather than hanging or erroring.
        let ended = timeout(STEP, client.recv())
            .await
            .expect("recv did not notice the relay going away");
        assert!(ended.is_none());    }

    #[tokio::test]
    async fn relay_client_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RelayClient>();
    }
}
