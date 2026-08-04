//! The wire format: what one node says to another.
//!
//! Frames are newline-delimited JSON. TCP delivers a byte stream rather than
//! discrete messages -- a single read can return half a frame, or two frames at
//! once -- so a delimiter is required to recover message boundaries.
//!
//! A bare newline is safe as that delimiter because `serde_json` escapes any
//! newline inside a string as the two characters `\` and `n`. A peer therefore
//! cannot smuggle a frame break through a field it controls, such as its name.
//!
//! This module performs no I/O. Applying the framing to a socket is `node.rs`.

use crate::crypto::NONCE_LEN;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Longest permitted frame, excluding the newline, matching how
/// `LinesCodec::new_with_max_length` counts. Generous enough for a peer list of
/// a few hundred, small enough that a hostile peer cannot exhaust memory.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub enum ProtocolError {
    TooLarge { size: usize, max: usize },
    Malformed(serde_json::Error),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::TooLarge { size, max } => {
                write!(f, "frame of {size} bytes exceeds the {max} byte limit")
            }
            ProtocolError::Malformed(e) => write!(f, "malformed frame: {e}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProtocolError::Malformed(e) => Some(e),
            ProtocolError::TooLarge { .. } => None,
        }
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(e: serde_json::Error) -> Self {
        ProtocolError::Malformed(e)
    }
}

/// What one node tells another about a third.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub public_key: Vec<u8>,
    pub listen_addr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Frame {
    /// First frame in both directions on every connection.
    ///
    /// Carries only the *port* we accept connections on, never a full address.
    /// A node that binds `0.0.0.0` has no idea which of its addresses a peer
    /// can actually reach, and advertising `0.0.0.0` would be useless to
    /// gossip. The receiver already knows the IP this connection came from, so
    /// it pairs that with this port to get an address that demonstrably works.
    Hello {
        name: String,
        public_key: Vec<u8>,
        listen_port: u16,
    },

    /// Gossip. The recipient dials anyone here it is not already connected to.
    Peers { peers: Vec<PeerInfo> },

    /// One message, already sealed for the peer on the other end of this
    /// connection. No sender field: the handshake settled that, so including
    /// one would only reintroduce something forgeable.
    Chat {
        ciphertext: Vec<u8>,
        nonce: [u8; NONCE_LEN],
    },
}

impl Frame {
    /// Encode to the body of one line, **without** a terminator.
    ///
    /// Appending the newline belongs to the codec that writes this, not here;
    /// doing it in both places would put a blank line on the wire. What this
    /// guarantees instead is that the result contains no newline of its own, so
    /// the codec's terminator is unambiguous.
    pub fn encode(&self) -> Result<String, ProtocolError> {
        let line = serde_json::to_string(self)?;

        if line.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::TooLarge {
                size: line.len(),
                max: MAX_FRAME_BYTES,
            });
        }

        Ok(line)
    }

    /// Decode one line, with or without a trailing newline.
    ///
    /// Every input here came from the network, so this reports errors and never
    /// panics.
    pub fn decode(line: &str) -> Result<Frame, ProtocolError> {
        // Rejected before parsing, so an oversized payload is never fully
        // deserialised only to be discarded.
        if line.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::TooLarge {
                size: line.len(),
                max: MAX_FRAME_BYTES,
            });
        }

        Ok(serde_json::from_str(line)?)
    }
}
