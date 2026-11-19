//! Pairing: a code on one screen, an encrypted channel on both.
//!
//! # The flow
//!
//! 1. Device A calls [`start_pairing`]. It binds an iroh endpoint, prints a
//!    [`PairingCode`] (six digits for a human to compare, plus a ticket string
//!    that carries A's node id and dialable addresses — there is no rendezvous
//!    server in this design), and waits.
//! 2. Device B calls [`accept_pairing`] with that ticket. It dials A over iroh.
//! 3. Both sides run the handshake below and come back with a
//!    [`PairedConnection`], which encrypts every message with
//!    ChaCha20-Poly1305.
//!
//! # The handshake
//!
//! iroh's QUIC handshake already authenticates a peer's ed25519 node id, but it
//! does not prove that the peer knows the pairing code, and it gives the
//! application no key it can use for its own payload encryption. So the pairing
//! layer adds a short handshake on one bi-directional stream, inside the
//! encrypted transport:
//!
//! ```text
//!             dialer (B)                                  listener (A)
//!   open_bi
//!   -> [0x01 | d_eph(32)]
//!                                     <- [0x01 | l_eph(32) | sig_l(64)]
//!   -> [0x01 | sig_d(64)]
//!                                     <- [0x01]              (accepted)
//! ```
//!
//! * `d_eph` / `l_eph` are freshly generated X25519 public keys (`ring`).
//! * `transcript = SHA256("syswatch/pair/1" | SHA256(code) | id_d | id_l
//!   | d_eph | l_eph)`.
//! * Each side signs `transcript | role` with its ed25519 **identity** key, and
//!   verifies the other's signature against that peer's node id. The listener
//!   checks that node id against the one in the ticket; the dialer checks it
//!   against the connection it actually got.
//! * Both then compute `X25519(d_eph_sk, l_eph_pk) == X25519(l_eph_sk,
//!   d_eph_pk)` and expand it with HKDF-SHA256, salted with the code hash and
//!   bound to the transcript, into two keys: `dialer->listener` and
//!   `listener->dialer`.
//!
//! Why an ephemeral ECDH on top of the identity keys instead of
//! `SecretKey::shared()`: a static-static X25519 between the two identity keys
//! yields one long-lived key per device pair, so one leak decrypts every past
//! session (no forward secrecy), and the derived key is not bound to this
//! handshake — a replayed old transcript would still decrypt. Ephemeral keys
//! fixed per session, plus a transcript that includes both node ids and the
//! code hash, close both holes. The identity keys are still the only long-term
//! secret, so "pairing trusts the node id" remains true.
//!
//! # What the code actually protects
//!
//! Six digits are 20 bits: an attacker who can reach A can guess it. That is
//! why [`MAX_PAIRING_ATTEMPTS`] exists — A stops listening after a handful of
//! failed handshakes — and why the window is five minutes. The code is a
//! *confirmation* secret on top of the node id, not the only thing guarding the
//! channel.
//!
//! Known limit, stated rather than hidden: this is not a PAKE (no SPAKE2, no
//! OPAQUE). Someone who captures flight 2 can verify candidate codes offline
//! against the listener's signature — 10^6 ed25519 verifications — and learn
//! the code afterwards. What that buys them is a code bound to ephemeral keys
//! of a session that has already ended, so it is not a channel compromise; it
//! would matter if a code were ever reused across sessions, which it is not
//! (fresh CSPRNG value per [`crate::PairingCode`], fresh ephemeral keys per
//! handshake). If a deployment ever needs a code to resist offline guessing,
//! that is where a PAKE belongs.

// iroh-net 0.28 carries a blanket "crate renamed" deprecation aimed at 0.29+;
// the workspace is pinned to 0.28 on purpose. Same note as `endpoint.rs`.
#![allow(deprecated)]

use std::sync::Mutex;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use futures_lite::StreamExt;
use iroh_base::key::{PublicKey, Signature};
use iroh_net::endpoint::{get_remote_node_id, Connection};
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::digest;
use ring::hkdf;
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

use crate::endpoint::IrohEndpoint;
use crate::{DeviceIdentity, NetError, PairingCode};

/// The ALPN pairing endpoints speak. Distinct from `syswatch/1` (echo) so a
/// pairing endpoint never answers a liveness probe, and vice versa.
pub const PAIR_ALPN: &[u8] = b"syswatch/pair/1";

/// Same protocol as a string, for logs and error messages.
pub const PAIR_ALPN_STR: &str = "syswatch/pair/1";

/// Largest message [`PairedConnection::recv`] will buffer, in bytes. One
/// message per stream, so this is also the per-stream cap.
pub const MAX_PAIRED_BYTES: usize = 1024 * 1024;

/// Failed handshakes a [`PairingHandle`] tolerates before it gives up.
///
/// Six digits is 20 bits of entropy. Without a cap, an attacker on the same
/// network can brute-force a live code; with a cap of 5, the odds of a blind
/// guess landing inside one code lifetime are ~5 in 10^6.
pub const MAX_PAIRING_ATTEMPTS: u32 = 5;

/// Domain separation label, hashed into every transcript.
const PROTO: &[u8] = b"syswatch/pair/1";

/// Role tags, signed *after* the transcript so a signature cannot be reflected
/// back at its owner.
const ROLE_DIALER: &[u8] = b"dialer";
const ROLE_LISTENER: &[u8] = b"listener";

/// HKDF info prefixes for the two directions.
const INFO_D2L: &[u8] = b"dialer->listener";
const INFO_L2D: &[u8] = b"listener->dialer";

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
/// Poly1305 tag length, appended to every ciphertext.
const TAG_LEN: usize = 16;

/// Frame sizes of the four handshake flights.
const FLIGHT_EPH: usize = 1 + KEY_LEN; // d_eph
const FLIGHT_REPLY: usize = 1 + KEY_LEN + 64; // l_eph + sig
const FLIGHT_PROOF: usize = 1 + 64; // sig
const FLIGHT_ACK: usize = 1;

const STATUS_OK: u8 = 0x01;
const STATUS_REJECT: u8 = 0x00;

// --- state machine -------------------------------------------------------

/// Where a pairing attempt is, from the point of view of this device.
///
/// ```text
/// Waiting ──code minted──> CodeGenerated ──transport up──> Connected ──keys──> Paired
///    ^                                                                          │
///    └──────────────────────────── reset (new code) ────────────────────────────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingState {
    /// No code has been issued yet.
    Waiting,
    /// A code is on screen; nothing has connected.
    CodeGenerated,
    /// A peer reached us and the handshake is in flight.
    Connected,
    /// Handshake verified, session keys derived.
    Paired,
}

impl PairingState {
    fn can_follow(self, next: PairingState) -> bool {
        use PairingState::*;
        matches!(
            (self, next),
            (Waiting, CodeGenerated) | (CodeGenerated, Connected) | (Connected, Paired)
        )
    }

    fn name(self) -> &'static str {
        match self {
            PairingState::Waiting => "Waiting",
            PairingState::CodeGenerated => "CodeGenerated",
            PairingState::Connected => "Connected",
            PairingState::Paired => "Paired",
        }
    }
}

/// The pairing state machine for one device.
///
/// Transitions are checked, not assigned: `Connected` cannot follow `Waiting`,
/// because a connection that never had a code to check cannot be trusted, and
/// `Paired` cannot be reached twice. [`PairingSession::reset`] is the only way
/// back to `Waiting`.
#[derive(Debug, Clone)]
pub struct PairingSession {
    node_id: PublicKey,
    state: PairingState,
    failed_attempts: u32,
}

impl PairingSession {
    pub fn new(identity: &DeviceIdentity) -> Self {
        Self {
            node_id: identity.node_id(),
            state: PairingState::Waiting,
            failed_attempts: 0,
        }
    }

    pub fn node_id(&self) -> PublicKey {
        self.node_id
    }

    pub fn state(&self) -> PairingState {
        self.state
    }

    pub fn failed_attempts(&self) -> u32 {
        self.failed_attempts
    }

    /// Move to `next`, or fail without touching the state.
    pub fn transition(&mut self, next: PairingState) -> Result<(), NetError> {
        if !self.state.can_follow(next) {
            return Err(NetError::Protocol(format!(
                "illegal pairing transition {} -> {}",
                self.state.name(),
                next.name()
            )));
        }
        self.state = next;
        Ok(())
    }

    /// Waiting → CodeGenerated.
    pub fn code_generated(&mut self) -> Result<(), NetError> {
        self.transition(PairingState::CodeGenerated)
    }

    /// CodeGenerated → Connected.
    pub fn connected(&mut self) -> Result<(), NetError> {
        self.transition(PairingState::Connected)
    }

    /// Connected → Paired.
    pub fn paired(&mut self) -> Result<(), NetError> {
        self.transition(PairingState::Paired)
    }

    /// Count a handshake that did not end in `Paired`; returns the new count.
    ///
    /// The count is not reset by an individual failure: the cap is per code, so
    /// only a fresh code (via [`PairingSession::reset`]) clears it.
    pub fn record_failure(&mut self) -> u32 {
        self.failed_attempts = self.failed_attempts.saturating_add(1);
        self.failed_attempts
    }

    /// Back to Waiting with a clean attempt counter, ready for a new code.
    pub fn reset(&mut self) {
        self.state = PairingState::Waiting;
        self.failed_attempts = 0;
    }
}

// --- session cipher ------------------------------------------------------

/// The post-handshake cipher: two ChaCha20-Poly1305 keys, one per direction.
///
/// Directional keys mean a nonce is never reused across directions even if both
/// sides happen to pick the same random value, and a message reflected back at
/// its sender fails to authenticate instead of decrypting.
pub struct SessionCipher {
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
}

impl std::fmt::Debug for SessionCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material, not even a prefix of it.
        f.write_str("SessionCipher(ChaCha20Poly1305: send, recv)")
    }
}

impl SessionCipher {
    /// Build from a send key and a receive key.
    pub fn new(send_key: &[u8; KEY_LEN], recv_key: &[u8; KEY_LEN]) -> Self {
        Self {
            send: ChaCha20Poly1305::new(&Key::from(*send_key)),
            recv: ChaCha20Poly1305::new(&Key::from(*recv_key)),
        }
    }

    /// Encrypt one message: `nonce(12) | ciphertext | tag(16)`.
    ///
    /// The nonce rides with the message. With one message per stream a counter
    /// would work too, but it desynchronises the moment a send fails after the
    /// counter advances; a fresh random 96-bit nonce per message has no state
    /// and stays safe well past the 2^32-message bound where a counter would
    /// have to be re-keyed.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, NetError> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| NetError::Crypto("system RNG unavailable".into()))?;
        let mut frame = Vec::with_capacity(NONCE_LEN + plaintext.len() + TAG_LEN);
        frame.extend_from_slice(&nonce);
        let body = self
            .send
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .map_err(|_| NetError::Crypto("encryption failed".into()))?;
        frame.extend_from_slice(&body);
        Ok(frame)
    }

    /// Decrypt one frame produced by [`SessionCipher::seal`].
    ///
    /// Any tampering — a flipped bit, a truncated tag, a frame from a different
    /// session — fails here rather than returning wrong plaintext.
    pub fn open(&self, frame: &[u8]) -> Result<Vec<u8>, NetError> {
        if frame.len() < NONCE_LEN + TAG_LEN {
            return Err(NetError::Protocol(format!(
                "encrypted frame too short ({} bytes)",
                frame.len()
            )));
        }
        let (nonce, body) = frame.split_at(NONCE_LEN);
        let plaintext = self
            .recv
            .decrypt(Nonce::from_slice(nonce), body)
            .map_err(|_| NetError::Crypto("decryption failed (bad key, nonce or tag)".into()))?;
        Ok(plaintext)
    }
}

// --- paired connection ---------------------------------------------------

/// An encrypted channel to a paired peer.
///
/// Each message travels on its own bi-directional QUIC stream (`send` opens one,
/// `recv` accepts one), so the stream boundary *is* the frame — no length
/// prefix, no head-of-line blocking between messages.
pub struct PairedConnection {
    conn: Connection,
    peer: PublicKey,
    cipher: SessionCipher,
    /// Kept alive on purpose: dropping the last endpoint handle would tear down
    /// the connection this object wraps.
    endpoint: IrohEndpoint,
}

impl std::fmt::Debug for PairedConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairedConnection")
            .field("peer", &self.peer.fmt_short())
            .finish_non_exhaustive()
    }
}

impl PairedConnection {
    /// The peer's ed25519 node id, verified during the handshake.
    pub fn peer_node_id(&self) -> PublicKey {
        self.peer
    }

    /// Encrypt `data` and send it as one message stream.
    pub async fn send(&self, data: &[u8]) -> Result<(), NetError> {
        if data.len() > MAX_PAIRED_BYTES {
            return Err(NetError::TooLarge(data.len(), MAX_PAIRED_BYTES));
        }
        let frame = self.cipher.seal(data)?;
        let (mut send, _recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|err| NetError::Stream(err.to_string()))?;
        send.write_all(&frame)
            .await
            .map_err(|err| NetError::Stream(err.to_string()))?;
        send.finish().map_err(|err| NetError::Stream(err.to_string()))?;
        Ok(())
    }

    /// Wait for the next message from the peer and decrypt it.
    ///
    /// Fails with [`NetError::Crypto`] if the frame does not authenticate: a
    /// forged or replayed message is an error, never plaintext.
    pub async fn recv(&self) -> Result<Vec<u8>, NetError> {
        let (_send, mut recv) = self
            .conn
            .accept_bi()
            .await
            .map_err(|err| NetError::Stream(err.to_string()))?;
        let frame = recv
            .read_to_end(NONCE_LEN + MAX_PAIRED_BYTES + TAG_LEN)
            .await
            .map_err(|err| NetError::Stream(err.to_string()))?;
        self.cipher.open(&frame)
    }

    /// The underlying iroh connection, for APIs this wrapper does not cover.
    pub fn inner(&self) -> &Connection {
        &self.conn
    }

    /// The endpoint that carries this connection.
    ///
    /// Held by [`PairedConnection`] so a dialer that handed its endpoint to
    /// nobody else does not lose the socket; exposed for callers that want the
    /// live [`iroh_net::endpoint::NodeAddr`] or to keep serving.
    pub fn endpoint(&self) -> &IrohEndpoint {
        &self.endpoint
    }

    /// Close the channel politely.
    pub fn close(self) {
        self.conn.close(0u32.into(), b"paired session closed");
    }
}

// --- public entry points -------------------------------------------------

/// Start pairing: bind an endpoint, mint a code, hand back a handle to wait on.
///
/// Production posture: relays and discovery on, so the peer can be anywhere.
/// Use [`start_pairing_local`] for tests and offline demos.
pub async fn start_pairing(
    identity: &DeviceIdentity,
) -> Result<(PairingCode, PairingHandle), NetError> {
    let endpoint = IrohEndpoint::bind_alpns(identity, &[PAIR_ALPN]).await?;
    issue_on(identity, endpoint, DEFAULT_TTL).await
}

/// [`start_pairing`] with the endpoint restricted to loopback.
pub async fn start_pairing_local(
    identity: &DeviceIdentity,
) -> Result<(PairingCode, PairingHandle), NetError> {
    let endpoint = IrohEndpoint::bind_local_alpns(identity, &[PAIR_ALPN]).await?;
    issue_on(identity, endpoint, DEFAULT_TTL).await
}

/// The default code lifetime, re-exported from the code type so callers have
/// one number to reason about.
const DEFAULT_TTL: std::time::Duration =
    std::time::Duration::from_secs(crate::PAIRING_CODE_TTL_SECS);

async fn issue_on(
    identity: &DeviceIdentity,
    endpoint: IrohEndpoint,
    ttl: std::time::Duration,
) -> Result<(PairingCode, PairingHandle), NetError> {
    let addr = endpoint.node_addr().await?;
    let mut code = PairingCode::generate_with_ttl(&endpoint.node_id_string(), ttl);
    code.addrs = addr.info.direct_addresses.iter().copied().collect();
    code.relay_url = addr.info.relay_url.as_ref().map(|url| url.to_string());

    let mut session = PairingSession::new(identity);
    session.code_generated()?;

    let handle = PairingHandle {
        endpoint,
        identity: identity.clone(),
        own_node_id: identity.node_id(),
        code: code.clone(),
        session: Mutex::new(session),
    };
    Ok((code, handle))
}

/// Device A's side of a pairing: the code it is showing plus the wait.
pub struct PairingHandle {
    endpoint: IrohEndpoint,
    identity: DeviceIdentity,
    /// Our own node id: the `listener` half of every transcript we sign.
    own_node_id: PublicKey,
    code: PairingCode,
    session: Mutex<PairingSession>,
}

impl std::fmt::Debug for PairingHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingHandle")
            .field("node_id", &self.own_node_id.fmt_short())
            .field("state", &self.state())
            .field("failed_attempts", &self.failed_attempts())
            .finish_non_exhaustive()
    }
}

impl PairingHandle {
    /// The code to show the user. Its ticket string is what the other device
    /// needs (six digits alone carry no address).
    pub fn code(&self) -> &PairingCode {
        &self.code
    }

    /// Current state of the underlying [`PairingSession`].
    pub fn state(&self) -> PairingState {
        self.session.lock().expect("pairing session poisoned").state()
    }

    /// Handshakes that failed so far.
    pub fn failed_attempts(&self) -> u32 {
        self.session
            .lock()
            .expect("pairing session poisoned")
            .failed_attempts()
    }

    /// This device's node id, as the peer will see it.
    pub fn node_id(&self) -> PublicKey {
        self.endpoint.node_id()
    }

    /// The endpoint the handle is listening on (for hosts that want to show
    /// live addresses, or add them to the ticket later).
    pub fn endpoint(&self) -> &IrohEndpoint {
        &self.endpoint
    }

    /// Wait for a peer to complete the handshake.
    ///
    /// Failed attempts (a wrong code, a port scanner, a half-open connection)
    /// are counted and skipped, so a mistyped code does not kill the session.
    /// After [`MAX_PAIRING_ATTEMPTS`] the handle gives up with
    /// [`NetError::TooManyAttempts`]; an expired code fails with
    /// [`NetError::CodeExpired`].
    pub async fn wait_for_peer(&self) -> Result<PairedConnection, NetError> {
        let mut accepted = Box::pin(self.endpoint.accept());
        loop {
            self.check_ready()?;
            let attempts = self.failed_attempts();
            if attempts >= MAX_PAIRING_ATTEMPTS {
                return Err(NetError::TooManyAttempts(attempts));
            }
            let conn = match accepted.next().await {
                Some(conn) => conn,
                None => {
                    return Err(NetError::Iroh("endpoint closed while waiting for a peer".into()))
                }
            };
            match self.run_listener(&conn).await {
                Ok(paired) => return Ok(paired),
                Err(_) => {
                    // Rejected (or unreadable): drop the connection, count it,
                    // keep listening. The reason is deliberately not surfaced
                    // here — `wait_for_peer_once` is the diagnostic path.
                    conn.close(0u32.into(), b"pairing rejected");
                    self.session
                        .lock()
                        .expect("pairing session poisoned")
                        .record_failure();
                }
            }
        }
    }

    /// One attempt: return success, or the first failure's reason.
    ///
    /// Useful for a UI that wants to say "wrong code" instead of silently
    /// retrying, and for tests that assert *why* a pairing failed.
    pub async fn wait_for_peer_once(&self) -> Result<PairedConnection, NetError> {
        self.check_ready()?;
        let mut accepted = Box::pin(self.endpoint.accept());
        let conn = accepted
            .next()
            .await
            .ok_or_else(|| NetError::Iroh("endpoint closed while waiting for a peer".into()))?;
        match self.run_listener(&conn).await {
            Ok(paired) => Ok(paired),
            Err(err) => {
                conn.close(0u32.into(), b"pairing rejected");
                let attempts = self
                    .session
                    .lock()
                    .expect("pairing session poisoned")
                    .record_failure();
                if attempts >= MAX_PAIRING_ATTEMPTS {
                    return Err(NetError::TooManyAttempts(attempts));
                }
                Err(err)
            }
        }
    }

    fn check_code_still_valid(&self) -> Result<(), NetError> {
        if self.code.expires_at == 0 || self.code.is_expired() {
            return Err(NetError::CodeExpired);
        }
        Ok(())
    }

    /// A handle is good for exactly one pairing: after `Paired` it refuses to
    /// start over, because a second channel would be built on a code that has
    /// already served its purpose.
    fn check_ready(&self) -> Result<(), NetError> {
        match self.state() {
            PairingState::CodeGenerated => {}
            PairingState::Paired => {
                return Err(NetError::Protocol(
                    "this pairing handle is already paired; mint a new code".into(),
                ))
            }
            other => {
                return Err(NetError::Protocol(format!(
                    "handle is in {other:?}, not waiting for a peer"
                )))
            }
        }
        self.check_code_still_valid()
    }

    /// Listener half of the handshake (device A, the one showing the code).
    ///
    /// The dialer's node id is *not* known in advance — the code names the
    /// listener, not the dialer — so the only thing that has to be proven here
    /// is knowledge of the code, which the dialer's signature over the
    /// code-bearing transcript does.
    async fn run_listener(&self, conn: &Connection) -> Result<PairedConnection, NetError> {
        let remote = get_remote_node_id(conn)
            .map_err(|err| NetError::Protocol(format!("no remote node id: {err}")))?;

        let (mut send, mut recv) = conn
            .accept_bi()
            .await
            .map_err(|err| NetError::Stream(err.to_string()))?;

        // Flight 1: the dialer's ephemeral public key.
        let mut flight1 = [0u8; FLIGHT_EPH];
        recv.read_exact(&mut flight1)
            .await
            .map_err(|err| NetError::Protocol(format!("handshake flight 1: {err}")))?;
        if flight1[0] != STATUS_OK {
            let _ = reject(&mut send).await;
            return Err(NetError::Protocol("dialer sent a rejection".into()));
        }
        let mut d_eph = [0u8; KEY_LEN];
        d_eph.copy_from_slice(&flight1[1..]);

        // Our own ephemeral key, then the transcript both sides will sign.
        let l_eph = Ephemeral::generate()?;
        let transcript = transcript(&self.code.code, remote, self.own_node_id, &d_eph, &l_eph.public);
        let sig_l = self
            .identity
            .secret()
            .sign(&signed_message(&transcript, ROLE_LISTENER));

        // Flight 2: our ephemeral key and our signature on that transcript.
        let mut flight2 = Vec::with_capacity(FLIGHT_REPLY);
        flight2.push(STATUS_OK);
        flight2.extend_from_slice(&l_eph.public);
        flight2.extend_from_slice(&sig_l.to_bytes());
        send.write_all(&flight2)
            .await
            .map_err(|err| NetError::Stream(err.to_string()))?;

        // Flight 3: the dialer's proof. It must have signed *this* transcript,
        // which includes SHA256 of our code — possible only if it knows it.
        // A peer that opens the handshake and then walks away is a failed code
        // attempt too: either it did not know the code, or it did not like our
        // signature, and neither case earns a retry on this connection.
        let mut flight3 = [0u8; FLIGHT_PROOF];
        if recv.read_exact(&mut flight3).await.is_err() {
            return Err(NetError::WrongCode);
        }
        if flight3[0] != STATUS_OK {
            let _ = reject(&mut send).await;
            return Err(NetError::WrongCode);
        }
        let mut sig_d_bytes = [0u8; 64];
        sig_d_bytes.copy_from_slice(&flight3[1..]);
        let sig_d = Signature::from_bytes(&sig_d_bytes);
        if remote
            .verify(&signed_message(&transcript, ROLE_DIALER), &sig_d)
            .is_err()
        {
            // Tell the dialer instead of dropping the stream: "wrong code" is a
            // recoverable, user-visible outcome.
            let _ = reject(&mut send).await;
            return Err(NetError::WrongCode);
        }

        // Flight 4: accept, sent only after verification.
        send.write_all(&[STATUS_OK])
            .await
            .map_err(|err| NetError::Stream(err.to_string()))?;
        send.finish().map_err(|err| NetError::Stream(err.to_string()))?;

        let shared = l_eph.diffie_hellman(&d_eph)?;
        let keys = session_keys(&shared, &self.code.code, &transcript)?;
        let cipher = SessionCipher::new(&keys.l2d, &keys.d2l);

        {
            let mut session = self.session.lock().expect("pairing session poisoned");
            session.transition(PairingState::Connected)?;
            session.paired()?;
        }

        Ok(PairedConnection {
            conn: conn.clone(),
            peer: remote,
            cipher,
            endpoint: self.endpoint.clone(),
        })
    }
}

/// Send a rejection ack and close the handshake stream politely.
async fn reject(send: &mut iroh_net::endpoint::SendStream) -> Result<(), NetError> {
    send.write_all(&[STATUS_REJECT])
        .await
        .map_err(|err| NetError::Stream(err.to_string()))?;
    send.finish().map_err(|err| NetError::Stream(err.to_string()))
}

/// Device B's side: dial the code's endpoint and complete the handshake.
///
/// Production posture: this side binds its own endpoint with relays enabled, so
/// it works from behind NAT. [`accept_pairing_local`] binds loopback-only.
pub async fn accept_pairing(
    identity: &DeviceIdentity,
    code: &PairingCode,
) -> Result<PairedConnection, NetError> {
    let endpoint = IrohEndpoint::bind_alpns(identity, &[PAIR_ALPN]).await?;
    dial_with(endpoint, identity, code).await
}

/// [`accept_pairing`] with the dialing endpoint restricted to loopback.
pub async fn accept_pairing_local(
    identity: &DeviceIdentity,
    code: &PairingCode,
) -> Result<PairedConnection, NetError> {
    let endpoint = IrohEndpoint::bind_local_alpns(identity, &[PAIR_ALPN]).await?;
    dial_with(endpoint, identity, code).await
}

async fn dial_with(
    endpoint: IrohEndpoint,
    identity: &DeviceIdentity,
    code: &PairingCode,
) -> Result<PairedConnection, NetError> {
    if code.is_expired() {
        return Err(NetError::CodeExpired);
    }
    let node_addr = code.node_addr()?;
    let listener = node_addr.node_id;

    let conn = endpoint.connect_addr_alpn(node_addr, PAIR_ALPN).await?;

    // The connection must land on the node the ticket named, or we are talking
    // to someone else than the device the user pointed at.
    let remote = get_remote_node_id(&conn)
        .map_err(|err| NetError::Protocol(format!("no remote node id: {err}")))?;
    if remote != listener {
        return Err(NetError::Protocol(format!(
            "dialled {} but reached {}",
            listener.fmt_short(),
            remote.fmt_short()
        )));
    }

    let mut session = PairingSession::new(identity);
    session.code_generated()?;
    session.connected()?;

    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|err| NetError::Stream(err.to_string()))?;

    let d_eph = Ephemeral::generate()?;

    // Flight 1: our ephemeral public key.
    let mut flight1 = Vec::with_capacity(FLIGHT_EPH);
    flight1.push(STATUS_OK);
    flight1.extend_from_slice(&d_eph.public);
    send.write_all(&flight1)
        .await
        .map_err(|err| NetError::Stream(err.to_string()))?;

    // Flight 2: the listener's ephemeral key and its signature.
    let mut flight2 = [0u8; FLIGHT_REPLY];
    recv.read_exact(&mut flight2)
        .await
        .map_err(|err| NetError::Protocol(format!("handshake flight 2: {err}")))?;
    if flight2[0] != STATUS_OK {
        return Err(NetError::WrongCode);
    }
    let mut l_eph = [0u8; KEY_LEN];
    l_eph.copy_from_slice(&flight2[1..1 + KEY_LEN]);
    let mut sig_l_bytes = [0u8; 64];
    sig_l_bytes.copy_from_slice(&flight2[1 + KEY_LEN..]);
    let sig_l = Signature::from_bytes(&sig_l_bytes);

    let transcript = transcript(
        &code.code,
        identity.node_id(),
        listener,
        &d_eph.public,
        &l_eph,
    );
    listener
        .verify(&signed_message(&transcript, ROLE_LISTENER), &sig_l)
        .map_err(|_| {
            // iroh already proved that the node we reached holds `listener`'s
            // identity key, so a signature over a different transcript means
            // the transcript differed — and the only changeable input on our
            // side is the code the user typed. Report it as such.
            NetError::WrongCode
        })?;

    // Flight 3: our proof that we know the code, over the same transcript.
    let sig_d = identity
        .secret()
        .sign(&signed_message(&transcript, ROLE_DIALER));
    let mut flight3 = Vec::with_capacity(FLIGHT_PROOF);
    flight3.push(STATUS_OK);
    flight3.extend_from_slice(&sig_d.to_bytes());
    send.write_all(&flight3)
        .await
        .map_err(|err| NetError::Stream(err.to_string()))?;
    send.finish().map_err(|err| NetError::Stream(err.to_string()))?;

    // Flight 4: the listener's verdict.
    let mut ack = [0u8; FLIGHT_ACK];
    recv.read_exact(&mut ack)
        .await
        .map_err(|err| NetError::Protocol(format!("handshake flight 4: {err}")))?;
    if ack[0] != STATUS_OK {
        return Err(NetError::WrongCode);
    }

    let shared = d_eph.diffie_hellman(&l_eph)?;
    let keys = session_keys(&shared, &code.code, &transcript)?;

    session.paired()?;

    Ok(PairedConnection {
        conn,
        peer: listener,
        cipher: SessionCipher::new(&keys.d2l, &keys.l2d),
        endpoint,
    })
}

// --- handshake primitives ------------------------------------------------

/// One X25519 keypair, generated fresh for a single handshake.
struct Ephemeral {
    private: EphemeralPrivateKey,
    public: [u8; KEY_LEN],
}

impl Ephemeral {
    fn generate() -> Result<Self, NetError> {
        let private = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new())
            .map_err(|_| NetError::Crypto("X25519 key generation failed".into()))?;
        let public_key = private
            .compute_public_key()
            .map_err(|_| NetError::Crypto("X25519 public key failed".into()))?;
        let mut public = [0u8; KEY_LEN];
        public.copy_from_slice(public_key.as_ref());
        Ok(Self { private, public })
    }

    /// Raw ECDH output. `ring` also rejects the low-order points for us: an
    /// all-zero shared secret is an error, not a key.
    fn diffie_hellman(self, peer: &[u8; KEY_LEN]) -> Result<Zeroizing<[u8; KEY_LEN]>, NetError> {
        let peer_key = UnparsedPublicKey::new(&X25519, peer);
        // ring's kdf closure returns the value directly; `None` stands for "the
        // peer sent a key of the wrong length", which cannot happen for X25519
        // but must not panic if it ever does.
        let agreed = agreement::agree_ephemeral(self.private, &peer_key, |shared| {
            let mut out = Zeroizing::new([0u8; KEY_LEN]);
            out.copy_from_slice(&<[u8; KEY_LEN]>::try_from(shared).ok()?);
            Some(out)
        })
        .map_err(|_| {
            NetError::Crypto("X25519 agreement failed (low-order or invalid peer key)".into())
        })?;
        agreed.ok_or_else(|| NetError::Crypto("X25519 shared secret has the wrong length".into()))
    }
}

/// `SHA256(code)` — the code never enters a transcript or a KDF in the clear.
fn code_hash(code: &str) -> [u8; KEY_LEN] {
    let digest = digest::digest(&digest::SHA256, code.as_bytes());
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(digest.as_ref());
    out
}

/// The value both sides sign: everything that identifies this session.
fn transcript(
    code: &str,
    dialer: PublicKey,
    listener: PublicKey,
    dialer_ephemeral: &[u8; KEY_LEN],
    listener_ephemeral: &[u8; KEY_LEN],
) -> [u8; KEY_LEN] {
    let mut buf = Vec::with_capacity(PROTO.len() + 3 * KEY_LEN + 64);
    buf.extend_from_slice(PROTO);
    buf.extend_from_slice(&code_hash(code));
    buf.extend_from_slice(dialer.as_bytes());
    buf.extend_from_slice(listener.as_bytes());
    buf.extend_from_slice(dialer_ephemeral);
    buf.extend_from_slice(listener_ephemeral);
    let digest = digest::digest(&digest::SHA256, &buf);
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(digest.as_ref());
    out
}

/// `transcript || role` — what each side actually signs.
fn signed_message(transcript: &[u8; KEY_LEN], role: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(KEY_LEN + role.len());
    msg.extend_from_slice(transcript);
    msg.extend_from_slice(role);
    msg
}

struct HkdfLen(usize);

impl hkdf::KeyType for HkdfLen {
    fn len(&self) -> usize {
        self.0
    }
}

/// The two directional AEAD keys derived from one handshake.
struct SessionKeys {
    /// Key for dialer → listener traffic.
    d2l: Zeroizing<[u8; KEY_LEN]>,
    /// Key for listener → dialer traffic.
    l2d: Zeroizing<[u8; KEY_LEN]>,
}

/// Expand the ECDH output into the two directional AEAD keys.
///
/// Salt = the code hash, so a code alone never yields keys; info = the
/// transcript hash, so keys are bound to this exact pair of ephemeral keys and
/// this exact pair of node ids.
fn session_keys(
    shared: &[u8; KEY_LEN],
    code: &str,
    transcript: &[u8; KEY_LEN],
) -> Result<SessionKeys, NetError> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &code_hash(code));
    let prk = salt.extract(shared);
    let mut d2l = Zeroizing::new([0u8; KEY_LEN]);
    let mut l2d = Zeroizing::new([0u8; KEY_LEN]);

    for (info, out) in [INFO_D2L, INFO_L2D].into_iter().zip([&mut d2l, &mut l2d]) {
        let mut labelled = Vec::with_capacity(transcript.len() + info.len());
        labelled.extend_from_slice(transcript);
        labelled.extend_from_slice(info);
        let info_slices: [&[u8]; 1] = [&labelled];
        let okm = prk
            .expand(&info_slices, HkdfLen(KEY_LEN))
            .map_err(|_| NetError::Crypto("HKDF expand failed".into()))?;
        okm.fill(&mut out[..])
            .map_err(|_| NetError::Crypto("HKDF fill failed".into()))?;
    }

    Ok(SessionKeys { d2l, l2d })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_base::key::SecretKey;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::time::timeout;

    const PAIR_TIMEOUT: Duration = Duration::from_secs(20);

    /// An identity file unique to this test, removed on drop.
    struct TempIdentity {
        path: PathBuf,
        identity: DeviceIdentity,
    }

    impl TempIdentity {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("syswatch_t28_{name}.key"));
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

    // --- state machine ---

    #[test]
    fn session_walks_waiting_to_paired() {
        let id = TempIdentity::new("sm");
        let mut session = PairingSession::new(&id.identity);
        assert_eq!(session.state(), PairingState::Waiting);
        assert_eq!(session.node_id(), id.identity.node_id());

        session.code_generated().unwrap();
        assert_eq!(session.state(), PairingState::CodeGenerated);
        session.connected().unwrap();
        assert_eq!(session.state(), PairingState::Connected);
        session.paired().unwrap();
        assert_eq!(session.state(), PairingState::Paired);

        // A new pairing starts from Waiting, with the attempt counter cleared.
        session.record_failure();
        assert_eq!(session.failed_attempts(), 1);
        session.reset();
        assert_eq!(session.state(), PairingState::Waiting);
        assert_eq!(session.failed_attempts(), 0);
    }

    #[test]
    fn session_rejects_skipped_states() {
        let id = TempIdentity::new("skip");
        let mut session = PairingSession::new(&id.identity);

        // No code, no connection.
        let err = session.connected().unwrap_err();
        assert!(
            matches!(err, NetError::Protocol(ref msg) if msg.contains("Waiting -> Connected")),
            "unexpected: {err}"
        );
        assert_eq!(session.state(), PairingState::Waiting, "state must not move");

        session.code_generated().unwrap();
        assert!(session.paired().is_err(), "cannot pair without a connection");
        assert_eq!(session.state(), PairingState::CodeGenerated);

        session.connected().unwrap();
        assert!(session.code_generated().is_err(), "cannot re-issue a code mid-handshake");
        session.paired().unwrap();
        assert!(session.paired().is_err(), "cannot pair twice");
        assert!(session.connected().is_err());
    }

    // --- crypto primitives ---

    #[test]
    fn x25519_agreement_is_symmetric() {
        let a = Ephemeral::generate().unwrap();
        let b = Ephemeral::generate().unwrap();
        let a_pub = a.public;
        let b_pub = b.public;
        let from_a = a.diffie_hellman(&b_pub).unwrap();
        let from_b = b.diffie_hellman(&a_pub).unwrap();
        assert_eq!(*from_a, *from_b);
        assert_ne!(*from_a, [0u8; KEY_LEN]);
    }

    #[test]
    fn x25519_rejects_a_low_order_point() {
        // The all-zero "public key" is a low-order point: agreement must fail
        // rather than hand back an all-zero secret.
        let a = Ephemeral::generate().unwrap();
        let result = a.diffie_hellman(&[0u8; KEY_LEN]);
        assert!(
            matches!(result, Err(NetError::Crypto(_))),
            "low-order point accepted"
        );
    }

    #[test]
    fn session_keys_depend_on_code_and_transcript() {
        let a = Ephemeral::generate().unwrap();
        let b = Ephemeral::generate().unwrap();
        let a_pub = a.public;
        let b_pub = b.public;
        let shared = a.diffie_hellman(&b_pub).unwrap();
        assert_eq!(*shared, *b.diffie_hellman(&a_pub).unwrap());

        let dialer = SecretKey::generate().public();
        let listener = SecretKey::generate().public();

        let t1 = transcript("123456", dialer, listener, &a_pub, &b_pub);
        let t2 = transcript("123456", dialer, listener, &a_pub, &b_pub);
        let t_other_code = transcript("654321", dialer, listener, &a_pub, &b_pub);
        let t_swapped = transcript("123456", listener, dialer, &a_pub, &b_pub);
        assert_eq!(t1, t2, "transcript must be deterministic");
        assert_ne!(t1, t_other_code, "code must be bound");
        assert_ne!(t1, t_swapped, "roles must be bound");

        let keys = session_keys(&shared, "123456", &t1).unwrap();
        assert_ne!(&*keys.d2l, &*keys.l2d, "directions must not share a key");
        assert_ne!(&*keys.d2l, &*shared, "raw ECDH output must not be the key");

        // Same inputs, same keys — the property both handshake sides rely on.
        let again = session_keys(&shared, "123456", &t1).unwrap();
        assert_eq!(&*keys.d2l, &*again.d2l);
        assert_eq!(&*keys.l2d, &*again.l2d);

        // A different code gives a different key even with identical ECDH.
        let other_code = session_keys(&shared, "654321", &t1).unwrap();
        assert_ne!(&*keys.d2l, &*other_code.d2l);

        // And so does a different transcript.
        let other_transcript = session_keys(&shared, "123456", &t_other_code).unwrap();
        assert_ne!(&*keys.d2l, &*other_transcript.d2l);
    }

    #[test]
    fn transcript_binds_every_input() {
        let a = Ephemeral::generate().unwrap().public;
        let b = Ephemeral::generate().unwrap().public;
        let dialer = SecretKey::generate().public();
        let listener = SecretKey::generate().public();
        let base = transcript("123456", dialer, listener, &a, &b);

        let other_eph = Ephemeral::generate().unwrap().public;
        assert_ne!(base, transcript("123456", dialer, listener, &other_eph, &b));
        assert_ne!(base, transcript("123456", dialer, listener, &a, &other_eph));

        // Signature domains are split by role, so a peer cannot replay its own
        // signature back as the other role.
        assert_ne!(
            signed_message(&base, ROLE_DIALER),
            signed_message(&base, ROLE_LISTENER)
        );

        // A real signature over the dialer message verifies, and the same
        // signature does not verify for the listener role.
        let secret = SecretKey::generate();
        let sig = secret.sign(&signed_message(&base, ROLE_DIALER));
        secret
            .public()
            .verify(&signed_message(&base, ROLE_DIALER), &sig)
            .expect("dialer signature must verify");
        assert!(
            secret
                .public()
                .verify(&signed_message(&base, ROLE_LISTENER), &sig)
                .is_err(),
            "role must not be swappable"
        );
    }

    #[test]
    fn cipher_roundtrips_and_rejects_tampering() {
        let k1 = [7u8; KEY_LEN];
        let k2 = [9u8; KEY_LEN];
        let send_side = SessionCipher::new(&k1, &k2);
        let recv_side = SessionCipher::new(&k2, &k1);

        let msg = b"syswatch paired payload";
        let frame = send_side.seal(msg).unwrap();
        assert_ne!(&frame[NONCE_LEN..], &msg[..], "payload must be ciphertext");
        assert_eq!(frame.len(), NONCE_LEN + msg.len() + TAG_LEN);
        assert_eq!(recv_side.open(&frame).unwrap(), msg);

        // Two seals of the same plaintext differ: fresh nonce every message.
        let frame2 = send_side.seal(msg).unwrap();
        assert_ne!(frame[..NONCE_LEN], frame2[..NONCE_LEN]);
        assert_ne!(frame, frame2);

        // Bit flip in the ciphertext.
        let mut tampered = frame.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(matches!(recv_side.open(&tampered), Err(NetError::Crypto(_))));

        // Bit flip in the nonce.
        let mut bad_nonce = frame.clone();
        bad_nonce[0] ^= 0xff;
        assert!(matches!(recv_side.open(&bad_nonce), Err(NetError::Crypto(_))));

        // Truncated frame.
        assert!(matches!(
            recv_side.open(&frame[..NONCE_LEN + TAG_LEN - 1]),
            Err(NetError::Protocol(_))
        ));

        // A third party with a different key learns nothing.
        let stranger = SessionCipher::new(&[3u8; KEY_LEN], &[4u8; KEY_LEN]);
        assert!(matches!(stranger.open(&frame), Err(NetError::Crypto(_))));

        // A frame reflected back at its sender fails: send/recv keys differ.
        assert!(matches!(send_side.open(&frame), Err(NetError::Crypto(_))));
    }

    // --- end to end ---

    /// Pair two loopback endpoints and return both paired sides.
    async fn pair(name: &str) -> (PairedConnection, PairedConnection, PublicKey, PublicKey) {
        let id_a = TempIdentity::new(&format!("{name}_a"));
        let id_b = TempIdentity::new(&format!("{name}_b"));

        let (code, handle) = timeout(PAIR_TIMEOUT, start_pairing_local(&id_a.identity))
            .await
            .expect("start_pairing timed out")
            .expect("start_pairing failed");
        assert_eq!(handle.state(), PairingState::CodeGenerated);
        assert_eq!(code.node_id, id_a.identity.node_id_string());

        // Device B only ever sees the ticket string — the code as a user pastes it.
        let ticket = code.ticket();
        let entered = PairingCode::parse(&ticket).expect("ticket must parse");

        let listener = tokio::spawn(async move {
            let paired = timeout(PAIR_TIMEOUT, handle.wait_for_peer())
                .await
                .expect("wait_for_peer timed out")
                .expect("wait_for_peer failed");
            (paired, handle.state())
        });

        let dialer = timeout(PAIR_TIMEOUT, accept_pairing_local(&id_b.identity, &entered))
            .await
            .expect("accept_pairing timed out")
            .expect("accept_pairing failed");

        let (listener_conn, listener_state) = listener.await.expect("listener task panicked");
        assert_eq!(listener_state, PairingState::Paired);

        (
            dialer,
            listener_conn,
            id_a.identity.node_id(),
            id_b.identity.node_id(),
        )
    }

    #[tokio::test]
    async fn two_endpoints_pair_and_exchange_encrypted_messages() {
        let (dialer, listener, id_a, id_b) = pair("e2e").await;

        // Identity, not assertion: each side sees the other's ed25519 node id.
        assert_eq!(dialer.peer_node_id(), id_a);
        assert_eq!(listener.peer_node_id(), id_b);

        // B → A.
        timeout(PAIR_TIMEOUT, dialer.send(b"hello from B"))
            .await
            .expect("send timed out")
            .expect("send failed");
        let got = timeout(PAIR_TIMEOUT, listener.recv())
            .await
            .expect("recv timed out")
            .expect("recv failed");
        assert_eq!(got, b"hello from B");

        // A → B, on the same pairing (the opposite directional key).
        timeout(PAIR_TIMEOUT, listener.send(b"hello from A"))
            .await
            .expect("send timed out")
            .expect("send failed");
        let got = timeout(PAIR_TIMEOUT, dialer.recv())
            .await
            .expect("recv timed out")
            .expect("recv failed");
        assert_eq!(got, b"hello from A");

        // Several messages in a row, both directions, including an empty one.
        for i in 0..5u8 {
            let msg = vec![i; 1024];
            dialer.send(&msg).await.unwrap();
            assert_eq!(listener.recv().await.unwrap(), msg);
        }
        dialer.send(b"").await.unwrap();
        assert_eq!(listener.recv().await.unwrap(), Vec::<u8>::new());

        dialer.close();
    }

    #[tokio::test]
    async fn paired_channel_uses_the_negotiated_keys_not_the_plaintext() {
        // Prove the payload really is sealed: pair, then pair again with a
        // different identity pair, and check that a frame from one session does
        // not open under the other session's cipher.
        let (dialer, listener, _, _) = pair("sealed").await;

        let frame = dialer.cipher.seal(b"top secret").unwrap();
        assert_ne!(&frame[NONCE_LEN..], &b"top secret"[..]);

        // The listener holds the mirror-image cipher: it opens the frame.
        assert_eq!(listener.cipher.open(&frame).unwrap(), b"top secret");

        // A second, independent pairing must not open it.
        let (other_dialer, _other_listener, _, _) = pair("sealed2").await;
        assert!(matches!(
            other_dialer.cipher.open(&frame),
            Err(NetError::Crypto(_))
        ));

        dialer.close();
        other_dialer.close();
    }

    #[tokio::test]
    async fn a_handle_pairs_once_and_then_refuses() {
        let id_a = TempIdentity::new("once_a");
        let id_b = TempIdentity::new("once_b");

        let (code, handle) = start_pairing_local(&id_a.identity).await.unwrap();
        let entered = PairingCode::parse(&code.ticket()).unwrap();

        let waiter = tokio::spawn(async move {
            let paired = timeout(PAIR_TIMEOUT, handle.wait_for_peer())
                .await
                .expect("wait_for_peer timed out")
                .expect("wait_for_peer failed");
            // Second call on the same, now-paired handle: refused before any
            // socket work, so this returns instead of hanging.
            let again = handle.wait_for_peer().await;
            (paired, again, handle.state())
        });

        let dialer = timeout(PAIR_TIMEOUT, accept_pairing_local(&id_b.identity, &entered))
            .await
            .expect("timed out")
            .expect("pairing failed");

        let (listener, again, state) = waiter.await.unwrap();
        assert_eq!(state, PairingState::Paired);
        assert!(
            matches!(again, Err(NetError::Protocol(ref msg)) if msg.contains("already paired")),
            "expected a refusal, got {:?}",
            again.err()
        );

        dialer.send(b"one shot").await.unwrap();
        assert_eq!(listener.recv().await.unwrap(), b"one shot");
        dialer.close();
    }

    #[tokio::test]
    async fn wire_bytes_are_ciphertext_not_plaintext() {
        let (dialer, listener, _, _) = pair("wire").await;

        let secret = b"Cardinal PIN 4711";
        dialer.send(secret).await.unwrap();

        // Read the raw stream the listener would have consumed, bypassing
        // `recv()` so the bytes on the wire are what we look at.
        let (_send, mut raw_recv) = listener.inner().accept_bi().await.expect("no stream");
        let wire = raw_recv
            .read_to_end(4096)
            .await
            .expect("could not read raw frame");

        assert!(
            !wire.windows(secret.len()).any(|chunk| chunk == secret),
            "plaintext found on the wire"
        );
        assert_eq!(
            wire.len(),
            NONCE_LEN + secret.len() + TAG_LEN,
            "frame must be nonce + ciphertext + tag"
        );
        assert_eq!(listener.cipher.open(&wire).unwrap(), secret);

        dialer.close();
    }

    #[tokio::test]
    async fn wrong_code_is_rejected_by_the_listener() {
        let id_a = TempIdentity::new("wrong_a");
        let id_b = TempIdentity::new("wrong_b");

        let (code, handle) = start_pairing_local(&id_a.identity).await.unwrap();

        // Same ticket, one digit off — what a typo looks like.
        let mut entered = PairingCode::parse(&code.ticket()).unwrap();
        entered.code = if code.code == "000000" {
            "111111".into()
        } else {
            "000000".into()
        };

        let listener = tokio::spawn(async move {
            let result = timeout(PAIR_TIMEOUT, handle.wait_for_peer_once()).await;
            (result, handle.failed_attempts())
        });

        let dialer = timeout(PAIR_TIMEOUT, accept_pairing_local(&id_b.identity, &entered))
            .await
            .expect("accept_pairing timed out");
        assert!(
            matches!(dialer, Err(NetError::WrongCode)),
            "dialer must be told the code is wrong, got {:?}",
            dialer.err()
        );

        let (listener_result, attempts) = listener.await.unwrap();
        let listener_result = listener_result.expect("wait_for_peer_once hung");
        assert!(
            matches!(listener_result, Err(NetError::WrongCode)),
            "listener must reject, got {:?}",
            listener_result.err()
        );
        assert_eq!(attempts, 1, "a rejection is a failed attempt");
    }

    #[tokio::test]
    async fn wrong_code_does_not_kill_the_session_and_the_right_code_still_works() {
        let id_a = TempIdentity::new("retry_a");
        let id_b = TempIdentity::new("retry_b");
        let id_c = TempIdentity::new("retry_c");

        let (code, handle) = start_pairing_local(&id_a.identity).await.unwrap();

        let mut wrong = PairingCode::parse(&code.ticket()).unwrap();
        wrong.code = if code.code == "000000" {
            "999999".into()
        } else {
            "000000".into()
        };

        // First: a peer with the wrong code. wait_for_peer must skip it.
        let waiter = tokio::spawn({
            let handle_state = handle.state();
            async move {
                let paired = timeout(PAIR_TIMEOUT, handle.wait_for_peer())
                    .await
                    .expect("wait_for_peer timed out");
                (paired, handle_state, handle.failed_attempts())
            }
        });

        let _ = timeout(PAIR_TIMEOUT, accept_pairing_local(&id_c.identity, &wrong))
            .await
            .expect("timed out")
            .expect_err("wrong code must not pair");

        // Then: the real peer, with the right ticket.
        let good = PairingCode::parse(&code.ticket()).unwrap();
        let dialer = timeout(PAIR_TIMEOUT, accept_pairing_local(&id_b.identity, &good))
            .await
            .expect("timed out")
            .expect("right code must pair after one failure");

        let (paired, state_before, failures) = waiter.await.unwrap();
        let listener = paired.expect("listener must pair on the second attempt");
        assert_eq!(state_before, PairingState::CodeGenerated);
        assert_eq!(failures, 1);
        assert_eq!(listener.peer_node_id(), id_b.identity.node_id());

        dialer.send(b"after a typo").await.unwrap();
        assert_eq!(listener.recv().await.unwrap(), b"after a typo");

        dialer.close();
    }

    #[tokio::test]
    async fn too_many_failed_attempts_shuts_the_handle_down() {
        let id_a = TempIdentity::new("brute_a");
        let id_b = TempIdentity::new("brute_b");
        let (code, handle) = start_pairing_local(&id_a.identity).await.unwrap();

        // A stranger that connects and immediately walks away is a failed
        // attempt just like a wrong code: it must be counted, not ignored.
        let ep = IrohEndpoint::bind_local_alpns(&id_b.identity, &[PAIR_ALPN])
            .await
            .unwrap();

        let addr = code.node_addr().unwrap();
        let node_id = handle.node_id();
        assert_eq!(code.node_id, node_id.to_string());

        let waiter = tokio::spawn(async move {
            let result = timeout(PAIR_TIMEOUT, handle.wait_for_peer()).await;
            (result, handle.failed_attempts())
        });

        for _ in 0..MAX_PAIRING_ATTEMPTS {
            let conn = ep
                .connect_addr_alpn(addr.clone(), PAIR_ALPN)
                .await
                .expect("connect failed");
            assert_eq!(get_remote_node_id(&conn).unwrap(), node_id);
            conn.close(0u32.into(), b"bye");
        }

        let (result, attempts) = waiter.await.expect("waiter timed out");
        let result = result.expect("wait_for_peer hung after the attempt cap");
        assert!(
            matches!(result, Err(NetError::TooManyAttempts(n)) if n == MAX_PAIRING_ATTEMPTS),
            "expected TooManyAttempts, got {:?}",
            result.err()
        );
        assert_eq!(attempts, MAX_PAIRING_ATTEMPTS);
    }

    #[tokio::test]
    async fn expired_code_is_refused_by_both_sides() {
        let id_a = TempIdentity::new("exp_a");
        let id_b = TempIdentity::new("exp_b");

        // A code minted with a one second life, then left to die.
        let endpoint = IrohEndpoint::bind_local_alpns(&id_a.identity, &[PAIR_ALPN])
            .await
            .unwrap();
        let (code, handle) = issue_on(&id_a.identity, endpoint, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(!code.is_expired(), "a fresh code must be valid");
        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert!(code.is_expired(), "code should be stale by now");

        // The dialer refuses to even dial an expired ticket.
        let mut stale = PairingCode::parse(&code.ticket()).unwrap();
        stale.expires_at = 1;
        let err = accept_pairing_local(&id_b.identity, &stale)
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::CodeExpired), "got {err}");

        // The listener refuses to keep waiting once its own code is stale.
        let err = timeout(PAIR_TIMEOUT, handle.wait_for_peer())
            .await
            .expect("wait_for_peer hung on an expired code")
            .unwrap_err();
        assert!(
            matches!(err, NetError::CodeExpired),
            "listener must expire its own code, got {err}"
        );
    }

    /// ALPN is negotiated inside the QUIC handshake, so a pairing dialer that
    /// reaches an echo-only endpoint is refused before a single payload byte.
    ///
    /// Ignored by default: iroh's dial does not report the mismatch until its
    /// 30 s handshake timeout expires (measured on this machine: 30.01 s), which
    /// is far too slow for the default suite. Run with
    /// `cargo test foreign_alpn -- --ignored` to check the property.
    #[tokio::test]
    #[ignore = "iroh gives up on a mismatched ALPN only at its 30s timeout"]
    async fn foreign_alpn_cannot_pair() {
        let id_a = TempIdentity::new("alpn_a");
        let id_b = TempIdentity::new("alpn_b");

        let echo_ep = IrohEndpoint::bind_local(&id_a.identity).await.unwrap();
        let addr = echo_ep.node_addr().await.unwrap();
        let code = PairingCode::for_endpoint(
            &echo_ep.node_id_string(),
            addr.info.direct_addresses.iter().copied().collect(),
            None,
        );

        let result = timeout(Duration::from_secs(90), accept_pairing_local(&id_b.identity, &code))
            .await
            .expect("iroh never reported the ALPN mismatch");
        assert!(
            result.is_err(),
            "an echo-only endpoint must not accept a pairing"
        );
    }
}
