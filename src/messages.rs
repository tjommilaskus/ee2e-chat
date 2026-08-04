use crate::crypto::NONCE_LEN;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// A message as the user typed it. This form never touches the network -- it is
/// encrypted into a `NetworkMessage` first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub from: String,
    pub content: String,
    pub timestamp: u64,
}

impl Message {
    pub fn new(from: String, content: String) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Message {
            from,
            content,
            timestamp,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// The encrypted form that actually crosses the wire. The server can read
/// `from`, but not `ciphertext`.
///
/// `from` is an untrusted routing hint: the recipient must look up that name's
/// public key and let `crypto::open` prove the claim, rather than trusting it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkMessage {
    pub from: String,
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; NONCE_LEN],
}

impl NetworkMessage {
    pub fn new(from: String, ciphertext: Vec<u8>, nonce: [u8; NONCE_LEN]) -> Self {
        NetworkMessage {
            from,
            ciphertext,
            nonce,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}
