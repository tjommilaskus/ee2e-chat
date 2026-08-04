//! Room invite codes: who is allowed in.
//!
//! Encryption settles what a peer can *read*; it says nothing about who may
//! join. Without something here, anyone able to reach the listening port can
//! complete a handshake and sit in the room, which is only tolerable on a
//! network where that is already impossible.
//!
//! A room has one code. Everybody in the room holds it, and a connection is
//! refused unless both ends can prove they do. The code itself never crosses
//! the wire: each side sends an HMAC over both public keys and a fresh nonce
//! from each, so an eavesdropper learns nothing and a recorded exchange cannot
//! be replayed into a later one.
//!
//! This answers "are you invited". It does not answer "are you who you claim" —
//! that is what fingerprints are for, and both questions still need asking.

use crate::secretfile;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

type HmacSha256 = Hmac<Sha256>;

/// 80 bits. Far beyond guessing, while still short enough to read down a phone.
const CODE_BYTES: usize = 10;
pub const NONCE_LEN: usize = 32;
pub const PROOF_LEN: usize = 32;

const KEY_CONTEXT: &[u8] = b"tio-chat v1 room key";
const PROOF_CONTEXT: &[u8] = b"tio-chat v1 admission";

/// Crockford's alphabet: no I, L, O or U, so a code cannot be misread as a
/// different valid one when written down or spoken.
const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Clone, PartialEq, Eq)]
pub struct RoomCode {
    bytes: [u8; CODE_BYTES],
}

// Hand-written so a stray `{:?}` cannot put the code on screen or in a log.
impl fmt::Debug for RoomCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoomCode(<redacted>)")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RoomError {
    Malformed(&'static str),
}

impl fmt::Display for RoomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoomError::Malformed(why) => write!(f, "that is not a room code: {why}"),
        }
    }
}

impl std::error::Error for RoomError {}

impl RoomCode {
    pub fn generate() -> Self {
        let mut bytes = [0u8; CODE_BYTES];
        OsRng.fill_bytes(&mut bytes);
        RoomCode { bytes }
    }

    /// Parse a code as typed, which may be lower case, spaced, or run together.
    ///
    /// Deliberately forgiving: this is read off a phone call or a chat message,
    /// and refusing it over a stray dash would only train people to paste
    /// carelessly.
    pub fn parse(text: &str) -> Result<Self, RoomError> {
        let cleaned: Vec<u8> = text
            .bytes()
            .filter(|b| !matches!(b, b'-' | b' ' | b'\t' | b'\n' | b'\r' | b'_'))
            .collect();

        // The prefix is decoration, so it is accepted with or without.
        let cleaned = match cleaned.strip_prefix(b"TIO") {
            Some(rest) => rest.to_vec(),
            None => match cleaned.strip_prefix(b"tio") {
                Some(rest) => rest.to_vec(),
                None => cleaned,
            },
        };

        let expected = CODE_BYTES * 8 / 5;
        if cleaned.len() != expected {
            return Err(RoomError::Malformed("wrong length"));
        }

        let mut bits: u16 = 0;
        let mut width = 0;
        let mut out = Vec::with_capacity(CODE_BYTES);

        for raw in cleaned {
            let upper = raw.to_ascii_uppercase();
            // Crockford treats these as their digit lookalikes, which is the
            // point of leaving them out of the alphabet.
            let upper = match upper {
                b'I' | b'L' => b'1',
                b'O' => b'0',
                other => other,
            };

            let value = ALPHABET
                .iter()
                .position(|c| *c == upper)
                .ok_or(RoomError::Malformed("contains a character it should not"))?;

            bits = (bits << 5) | value as u16;
            width += 5;
            if width >= 8 {
                width -= 8;
                out.push((bits >> width) as u8);
            }
        }

        let bytes = <[u8; CODE_BYTES]>::try_from(out.as_slice())
            .map_err(|_| RoomError::Malformed("wrong length"))?;
        Ok(RoomCode { bytes })
    }

    /// The form shown to people, grouped for reading aloud.
    pub fn display(&self) -> String {
        let mut encoded = String::new();
        let mut bits: u16 = 0;
        let mut width = 0;

        for byte in self.bytes {
            bits = (bits << 8) | byte as u16;
            width += 8;
            while width >= 5 {
                width -= 5;
                encoded.push(ALPHABET[((bits >> width) & 0x1F) as usize] as char);
            }
        }

        let groups: Vec<String> = encoded
            .as_bytes()
            .chunks(4)
            .map(|chunk| String::from_utf8_lossy(chunk).to_string())
            .collect();

        format!("TIO-{}", groups.join("-"))
    }

    /// The key both ends derive from the code.
    ///
    /// A plain hash suffices because the code is generated at full entropy
    /// rather than chosen by a person; there is no dictionary to run against
    /// it, so a slow password hash would buy nothing.
    fn key(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(KEY_CONTEXT);
        hasher.update(self.bytes);
        hasher.finalize().into()
    }

    /// Prove we hold this code, for one specific connection.
    ///
    /// Both public keys and both nonces are covered, so the result is useless
    /// anywhere else: replaying it into another connection fails, and echoing
    /// it back at its sender fails too, since the two sides order the inputs
    /// differently.
    pub fn proof(
        &self,
        my_public_key: &[u8],
        my_nonce: &[u8; NONCE_LEN],
        their_public_key: &[u8],
        their_nonce: &[u8; NONCE_LEN],
    ) -> [u8; PROOF_LEN] {
        let mut mac = HmacSha256::new_from_slice(&self.key()).expect("any length key is accepted");
        mac.update(PROOF_CONTEXT);
        mac.update(my_public_key);
        mac.update(my_nonce);
        mac.update(their_public_key);
        mac.update(their_nonce);
        mac.finalize().into_bytes().into()
    }

    /// Check the proof a peer sent, by recomputing it from their side.
    pub fn verify(
        &self,
        claimed: &[u8; PROOF_LEN],
        their_public_key: &[u8],
        their_nonce: &[u8; NONCE_LEN],
        my_public_key: &[u8],
        my_nonce: &[u8; NONCE_LEN],
    ) -> bool {
        let expected = self.proof(their_public_key, their_nonce, my_public_key, my_nonce);
        // Constant time, so a wrong code cannot be narrowed down byte by byte
        // from how long the comparison takes.
        constant_time_eq(&expected, claimed)
    }
}

fn constant_time_eq(a: &[u8; PROOF_LEN], b: &[u8; PROOF_LEN]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn new_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

pub fn default_path() -> Result<PathBuf, crate::identity::IdentityError> {
    crate::identity::config_dir().map(|dir| dir.join("room"))
}

pub struct Loaded {
    pub code: RoomCode,
    /// No room existed and one was made.
    pub created: bool,
    pub notes: Vec<String>,
}

/// Read the stored room code, creating one if there is none.
///
/// Creating rather than running without is deliberate. An open room is a
/// setting nobody would choose on purpose, so it is not one that can be reached
/// by forgetting to pass a flag.
pub fn load_or_create(path: &Path) -> Result<Loaded, Box<dyn std::error::Error>> {
    if path.exists() {
        let (text, note) = secretfile::read_existing(path)?;
        let code = RoomCode::parse(text.trim())?;
        return Ok(Loaded {
            code,
            created: false,
            notes: note.into_iter().collect(),
        });
    }

    let code = RoomCode::generate();
    save(path, &code)?;
    Ok(Loaded {
        code,
        created: true,
        notes: Vec::new(),
    })
}

pub fn save(path: &Path, code: &RoomCode) -> std::io::Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    secretfile::write_new(path, &code.display())
}
