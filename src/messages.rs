use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// A message as the user typed it.
///
/// This form never travels in the clear: it is serialised, sealed for one
/// specific recipient, and carried inside a `protocol::Frame::Chat`. Because
/// `from` sits inside the ciphertext it is authenticated rather than claimed,
/// and the receiver additionally checks it against the identity established by
/// that connection's handshake.
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

