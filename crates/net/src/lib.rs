//! Networking crate: device identity, iroh endpoint, pairing.
//!
//! Every device generates an ed25519 keypair on first run. The public key
//! (NodeId) is the device's identity — no accounts, no registration.

pub mod endpoint;

pub use endpoint::{IrohEndpoint, ECHO_ALPN, ECHO_ALPN_STR, MAX_ECHO_BYTES};

use iroh_net::key::SecretKey;
use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCode {
    pub code: String,
    pub expires_at: u64, // unix timestamp
    pub node_id: String,
}

impl PairingCode {
    pub fn generate(node_id: &str) -> Self {
        // 6-digit code from random bytes
        let rng = rand_bytes();
        let code = format!("{:06}", (rng as u32) % 1_000_000);
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 300; // 5 minutes
        Self {
            code,
            expires_at,
            node_id: node_id.to_string(),
        }
    }

    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now > self.expires_at
    }
}

fn rand_bytes() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    hasher.finish()
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
            expires_at: 0,
            node_id: "test".into(),
        };
        assert!(pc.is_expired());
    }

    #[test]
    fn pairing_code_format() {
        let pc = PairingCode::generate("node123");
        assert_eq!(pc.code.len(), 6);
        assert!(pc.code.chars().all(|c| c.is_ascii_digit()));
    }
}
