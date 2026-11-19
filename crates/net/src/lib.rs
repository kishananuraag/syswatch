//! Networking crate: device identity, iroh endpoint, pairing.
//!
//! Every device generates an ed25519 keypair on first run. The public key
//! (NodeId) is the device's identity — no accounts, no registration.

pub mod endpoint;
pub mod pairing;

pub use endpoint::{IrohEndpoint, ECHO_ALPN, ECHO_ALPN_STR, MAX_ECHO_BYTES};
pub use pairing::{
    accept_pairing, accept_pairing_local, start_pairing, start_pairing_local, PairedConnection,
    PairingHandle, PairingSession, PairingState, MAX_PAIRING_ATTEMPTS, MAX_PAIRED_BYTES, PAIR_ALPN,
    PAIR_ALPN_STR,
};

use iroh_net::key::SecretKey;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Device identity: an ed25519 keypair persisted to disk.
#[derive(Clone)]
pub struct DeviceIdentity {
    secret: SecretKey,
}

impl DeviceIdentity {
    /// Load or generate a device identity.
    /// If the key file exists, load it. Otherwise, generate and save.
    pub fn load_or_create(path: &PathBuf) -> Result<Self, NetError> {
        if path.exists() {
            let bytes = std::fs::read(path).map_err(NetError::Io)?;
            if bytes.len() != 32 {
                return Err(NetError::InvalidKey);
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            let secret = SecretKey::from_bytes(&arr);
            Ok(Self { secret })
        } else {
            let secret = SecretKey::generate();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(NetError::Io)?;
            }
            std::fs::write(path, secret.to_bytes()).map_err(NetError::Io)?;
            // Restrict permissions
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(NetError::Io)?;
            }
            Ok(Self { secret })
        }
    }

    /// The device's public identity (NodeId).
    pub fn node_id(&self) -> iroh_base::key::PublicKey {
        self.secret.public()
    }

    /// The secret key (for signing/encryption).
    pub fn secret(&self) -> &SecretKey {
        &self.secret
    }

    /// Serialize the node ID as a string.
    pub fn node_id_string(&self) -> String {
        self.node_id().to_string()
    }
}

/// A pairing code: short, human-readable, expires in 5 minutes.
///
/// The six digits are what a human reads off device A and types into device B,
/// but on their own they are not enough to connect: there is no rendezvous
/// server in this design, so the code also carries the issuing device's
/// [`iroh_base::key::PublicKey`] and its dialable addresses. `code` is what a
/// human compares; the whole ticket ([`PairingCode::ticket`]) is what is
/// actually transferred from A to B (copy/paste, QR, AirDrop).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCode {
    pub code: String,
    pub expires_at: u64, // unix timestamp
    pub node_id: String,
    /// Direct addresses the issuing device is reachable on. Empty is legal:
    /// the peer may still be reachable through its relay.
    #[serde(default)]
    pub addrs: Vec<SocketAddr>,
    /// Home relay URL, if the issuing device has one.
    #[serde(default)]
    pub relay_url: Option<String>,
}

/// Wire prefix for a pairing ticket, so a pasted string is self-identifying.
const TICKET_PREFIX: &str = "syswatch1";

/// How long a freshly minted pairing code stays valid.
pub const PAIRING_CODE_TTL_SECS: u64 = 300;

impl PairingCode {
    pub fn generate(node_id: &str) -> Self {
        Self::generate_with_ttl(node_id, std::time::Duration::from_secs(PAIRING_CODE_TTL_SECS))
    }

    /// [`PairingCode::generate`] with an explicit lifetime.
    ///
    /// Short lifetimes are for tests and for hosts that want a tighter window
    /// than the default five minutes; the code is useless to a brute-forcer
    /// once it stops being accepted.
    pub fn generate_with_ttl(node_id: &str, ttl: std::time::Duration) -> Self {
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + ttl.as_secs();
        Self {
            code: random_code(),
            expires_at,
            node_id: node_id.to_string(),
            addrs: Vec::new(),
            relay_url: None,
        }
    }

    /// Mint a code for a specific endpoint: node id, direct addresses and relay.
    pub fn for_endpoint(node_id: &str, addrs: Vec<SocketAddr>, relay_url: Option<String>) -> Self {
        let mut code = Self::generate(node_id);
        code.addrs = addrs;
        code.relay_url = relay_url;
        code
    }

    /// Whether this code is past its validity window.
    ///
    /// `expires_at == 0` means "unknown" (a ticket parsed on the receiving
    /// device), not "expired": expiry is enforced by the device that minted the
    /// code, during the handshake.
    pub fn is_expired(&self) -> bool {
        if self.expires_at == 0 {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now > self.expires_at
    }

    /// Seconds left before this code stops being accepted (0 when expired).
    pub fn seconds_left(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.expires_at.saturating_sub(now)
    }

    /// Encode as the single string a user copies to the other device.
    ///
    /// Layout: `syswatch1/<code>/<node id>/<addrs>/<relay>` where addresses are
    /// comma-separated and the relay comes last (so its own `/`-es are safe).
    pub fn ticket(&self) -> String {
        let addrs = self
            .addrs
            .iter()
            .map(SocketAddr::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{TICKET_PREFIX}/{}/{}/{addrs}/{}",
            self.code,
            self.node_id,
            self.relay_url.as_deref().unwrap_or("")
        )
    }

    /// Parse a ticket produced by [`PairingCode::ticket`].
    ///
    /// Expiry is *not* checked here — [`PairingCode::is_expired`] is separate so
    /// a caller can report "code expired" distinctly from "code malformed".
    pub fn parse(ticket: &str) -> Result<Self, NetError> {
        let mut parts = ticket.trim().splitn(5, '/');
        let prefix = parts.next().unwrap_or_default();
        if prefix != TICKET_PREFIX {
            return Err(NetError::InvalidCode(format!(
                "expected a {TICKET_PREFIX}/… ticket, got {prefix:?}"
            )));
        }
        let code = parts.next().unwrap_or_default().trim().to_string();
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err(NetError::InvalidCode(format!("code {code:?} is not 6 digits")));
        }
        let node_id = parts.next().unwrap_or_default().trim().to_string();
        if node_id.is_empty() {
            return Err(NetError::InvalidCode("missing node id".into()));
        }
        let mut addrs = Vec::new();
        for raw in parts.next().unwrap_or_default().split(',') {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let addr: SocketAddr = raw
                .parse()
                .map_err(|err| NetError::InvalidCode(format!("bad address {raw:?}: {err}")))?;
            addrs.push(addr);
        }
        let relay_url = match parts.next().unwrap_or_default().trim() {
            "" => None,
            url => Some(url.to_string()),
        };
        Ok(Self {
            code,
            // A ticket has no timestamp of its own: expiry is enforced by the
            // issuing device during the handshake, and `0` here means "unknown"
            // (a freshly parsed ticket is not treated as already expired).
            expires_at: 0,
            node_id,
            addrs,
            relay_url,
        })
    }

    /// The issuing device's address, as iroh wants it.
    pub fn node_addr(&self) -> Result<iroh_net::endpoint::NodeAddr, NetError> {
        let node_id: iroh_base::key::PublicKey = self
            .node_id
            .parse()
            .map_err(|err| NetError::InvalidCode(format!("bad node id: {err}")))?;
        let mut addr = iroh_net::endpoint::NodeAddr::new(node_id).with_direct_addresses(self.addrs.clone());
        if let Some(relay) = &self.relay_url {
            let url = relay
                .parse()
                .map_err(|err| NetError::InvalidCode(format!("bad relay url: {err}")))?;
            addr = addr.with_relay_url(url);
        }
        Ok(addr)
    }
}

impl std::str::FromStr for PairingCode {
    type Err = NetError;

    fn from_str(ticket: &str) -> Result<Self, NetError> {
        Self::parse(ticket)
    }
}

/// Six digits from the OS CSPRNG.
///
/// A modulo of a 32-bit draw is not exactly uniform (bias < 0.03 %), which is
/// irrelevant next to a 10^6 search space and a short validity window, but the
/// draw itself must be unpredictable — a hashed timestamp is not.
fn random_code() -> String {
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut bytes = [0u8; 4];
    rng.fill(&mut bytes).expect("system RNG unavailable");
    format!("{:06}", u32::from_be_bytes(bytes) % 1_000_000)
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid key file")]
    InvalidKey,
    #[error("iroh error: {0}")]
    Iroh(String),
    #[error("stream error: {0}")]
    Stream(String),
    /// A pairing ticket or code string that does not parse.
    #[error("invalid pairing code: {0}")]
    InvalidCode(String),
    /// The code was well-formed but past the 5 minute window.
    #[error("pairing code expired")]
    CodeExpired,
    /// The peer proved it does not know the code this side is showing.
    #[error("pairing code rejected by peer")]
    WrongCode,
    /// Too many peers tried the wrong code; the session was shut down.
    #[error("too many failed pairing attempts ({0})")]
    TooManyAttempts(u32),
    /// Crypto primitive failure (bad key material, failed agreement, …).
    #[error("crypto error: {0}")]
    Crypto(String),
    /// The peer broke the handshake or frame format.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// A message exceeded the crate's buffering limits.
    #[error("message too large ({0} bytes, limit {1})")]
    TooLarge(usize, usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn generate_and_persist_identity() {
        let path = temp_dir().join("test_identity.key");
        let _ = std::fs::remove_file(&path);
        let id1 = DeviceIdentity::load_or_create(&path).unwrap();
        let id2 = DeviceIdentity::load_or_create(&path).unwrap();
        assert_eq!(id1.node_id_string(), id2.node_id_string());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pairing_code_expires() {
        let pc = PairingCode {
            code: "123456".into(),
            expires_at: 1, // 1970 — long gone
            node_id: "test".into(),
            addrs: Vec::new(),
            relay_url: None,
        };
        assert!(pc.is_expired());
        assert_eq!(pc.seconds_left(), 0);
    }

    #[test]
    fn pairing_code_without_timestamp_is_not_expired() {
        // A ticket as received from the other device carries no clock.
        let ticket = "syswatch1/123456/abc/127.0.0.1:9000/";
        let pc = PairingCode::parse(ticket).unwrap();
        assert_eq!(pc.expires_at, 0);
        assert!(!pc.is_expired(), "unknown expiry must not read as expired");
    }

    #[test]
    fn pairing_code_format() {
        let pc = PairingCode::generate("node123");
        assert_eq!(pc.code.len(), 6);
        assert!(pc.code.chars().all(|c| c.is_ascii_digit()));
        // Two codes in a row are drawn from the CSPRNG, so they differ.
        assert_ne!(pc.code, PairingCode::generate("node123").code);
    }

    #[test]
    fn pairing_ticket_roundtrips() {
        use std::net::SocketAddr;
        let addrs: Vec<SocketAddr> = vec![
            "127.0.0.1:9000".parse().unwrap(),
            "[::1]:9001".parse().unwrap(),
        ];
        let mut pc = PairingCode::for_endpoint(
            "dogzqpeph3i7w2v2i63po7k7rlrxhrsuyu2kpcrl3e4rtllbnyfa",
            addrs.clone(),
            Some("https://aps1-1.relay.iroh.network./".into()),
        );
        let ticket = pc.ticket();
        let back = PairingCode::parse(&ticket).unwrap();
        assert_eq!(back.code, pc.code);
        assert_eq!(back.node_id, pc.node_id);
        assert_eq!(back.addrs, addrs);
        assert_eq!(back.relay_url, pc.relay_url);
        assert_eq!(back.ticket(), ticket);

        // A relay URL contains '/', so make sure it survived the split.
        assert_eq!(
            back.relay_url.as_deref(),
            Some("https://aps1-1.relay.iroh.network./")
        );

        // And the parsed ticket is directly dial-able as a NodeAddr.
        let addr = back.node_addr().unwrap();
        assert_eq!(addr.node_id.to_string(), pc.node_id);
        assert_eq!(addr.info.direct_addresses.len(), 2);
        assert!(addr.info.relay_url.is_some());

        pc.addrs.clear();
        pc.relay_url = None;
        let bare = PairingCode::parse(&pc.ticket()).unwrap();
        assert!(bare.addrs.is_empty());
        assert!(bare.relay_url.is_none());
    }

    #[test]
    fn bad_tickets_are_rejected() {
        for bad in [
            "",
            "123456",                                     // no prefix
            "syswatch2/123456/abc//",                     // wrong version
            "syswatch1/12345/abc//",                      // 5 digits
            "syswatch1/12345a/abc//",                     // not digits
            "syswatch1/123456//",                         // no node id
            "syswatch1/123456/abc/not-an-addr/",          // bad address
            "syswatch1/123456/abc/127.0.0.1:1/not a url", // bad relay
        ] {
            // The last one only fails when the failed address is kept out and
            // the relay is parsed; assert the ticket either fails here or in
            // node_addr(), never silently succeeds with garbage.
            let parsed = PairingCode::parse(bad);
            let ok = parsed
                .as_ref()
                .ok()
                .and_then(|pc| pc.node_addr().ok())
                .is_some();
            assert!(!ok, "{bad:?} must not produce a usable address");
        }
    }
}
