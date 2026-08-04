//! Authenticated end-to-end encryption.
//!
//! Each participant holds a long-lived X25519 keypair. To send a message the
//! sender performs Diffie-Hellman between *their own secret key* and the
//! recipient's public key; the recipient performs the mirror-image exchange and
//! arrives at the same shared secret. Because the sender's long-term key is an
//! input, a ciphertext that decrypts correctly could only have been produced by
//! the holder of that key -- which is what gives us sender authentication.
//!
//! This is the same construction as NaCl's `crypto_box`.

use chacha20poly1305::{
    aead::{Aead, Payload},
    Key, KeyInit, XChaCha20Poly1305, XNonce,
};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::fmt;
use x25519_dalek::{PublicKey, StaticSecret};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;

/// Domain separator mixed into the key derivation so that a shared secret
/// derived here can never collide with one derived for some other purpose.
const KDF_CONTEXT: &[u8] = b"ee2e-chat v1 message key";

/// Separate context so a fingerprint can never coincide with a derived key.
const FINGERPRINT_CONTEXT: &[u8] = b"ee2e-chat v1 fingerprint";

#[derive(Debug, PartialEq, Eq)]
pub enum CryptoError {
    BadPublicKey,
    BadSecretKey,
    Encrypt,
    /// Covers both "wrong key" and "message was tampered with" -- deliberately
    /// indistinguishable, so an attacker learns nothing from the error.
    Decrypt,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            CryptoError::BadPublicKey => "public key must be 32 bytes",
            CryptoError::BadSecretKey => "secret key must be 32 bytes",
            CryptoError::Encrypt => "encryption failed",
            CryptoError::Decrypt => "could not decrypt or authenticate message",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for CryptoError {}

/// A long-lived X25519 keypair. The public key is published to the server so
/// other clients can encrypt to us; the secret key never leaves this process.
#[derive(Clone)]
pub struct Keypair {
    pub public_key: Vec<u8>,
    pub secret_key: Vec<u8>,
}

// Hand-written so that an accidental `{:?}` never prints the secret key.
impl fmt::Debug for Keypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keypair")
            .field("public_key", &hex(&self.public_key))
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn generate_keypair() -> Keypair {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);

    Keypair {
        public_key: public.as_bytes().to_vec(),
        secret_key: secret.to_bytes().to_vec(),
    }
}

/// A short, human-comparable rendering of a public key.
///
/// Users read these to each other over a channel an attacker does not control;
/// if they match, there is no machine-in-the-middle. Truncating to 64 bits is
/// adequate because an attacker has to find a *second preimage* of one specific
/// displayed value, and because nobody would read out more digits than this.
pub fn fingerprint(public_key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_CONTEXT);
    hasher.update(public_key);
    let digest = hasher.finalize();

    digest[..8]
        .chunks(2)
        .map(|pair| format!("{:02X}{:02X}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join("-")
}

/// Derive the public key that corresponds to a secret key.
pub fn public_key_for(secret_key: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let secret = to_secret(secret_key)?;
    Ok(PublicKey::from(&secret).as_bytes().to_vec())
}

fn to_secret(bytes: &[u8]) -> Result<StaticSecret, CryptoError> {
    let arr = <[u8; KEY_LEN]>::try_from(bytes).map_err(|_| CryptoError::BadSecretKey)?;
    Ok(StaticSecret::from(arr))
}

fn to_public(bytes: &[u8]) -> Result<PublicKey, CryptoError> {
    let arr = <[u8; KEY_LEN]>::try_from(bytes).map_err(|_| CryptoError::BadPublicKey)?;
    Ok(PublicKey::from(arr))
}

/// The raw X25519 output is a curve coordinate rather than uniformly random
/// bits, so it is hashed before being used as a cipher key.
fn derive_key(shared: &[u8; KEY_LEN]) -> Key {
    let mut hasher = Sha256::new();
    hasher.update(KDF_CONTEXT);
    hasher.update(shared);
    Key::clone_from_slice(&hasher.finalize())
}

/// Encrypt `plaintext` from us to the holder of `recipient_public`.
///
/// Returns `(ciphertext, nonce)`. Both must reach the recipient intact.
pub fn seal(
    plaintext: &[u8],
    recipient_public: &[u8],
    sender_secret: &[u8],
) -> Result<(Vec<u8>, [u8; NONCE_LEN]), CryptoError> {
    let recipient = to_public(recipient_public)?;
    let sender = to_secret(sender_secret)?;
    let sender_public = PublicKey::from(&sender);

    let shared = sender.diffie_hellman(&recipient);
    let cipher = XChaCha20Poly1305::new(&derive_key(shared.as_bytes()));

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from(nonce_bytes);

    // Binding the sender's public key as associated data makes the direction of
    // travel part of what gets authenticated. Without it, Alice and Bob derive
    // an identical key, so an attacker could reflect Alice's own message back at
    // her and it would appear to come from Bob.
    let payload = Payload {
        msg: plaintext,
        aad: sender_public.as_bytes(),
    };

    let ciphertext = cipher
        .encrypt(&nonce, payload)
        .map_err(|_| CryptoError::Encrypt)?;

    Ok((ciphertext, nonce_bytes))
}

/// Decrypt a message that `sender_public` sent to us.
///
/// Succeeds only if the message was genuinely produced by the holder of
/// `sender_public` and has not been altered in transit.
pub fn open(
    ciphertext: &[u8],
    nonce_bytes: &[u8; NONCE_LEN],
    recipient_secret: &[u8],
    sender_public: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let sender = to_public(sender_public)?;
    let recipient = to_secret(recipient_secret)?;

    let shared = recipient.diffie_hellman(&sender);
    let cipher = XChaCha20Poly1305::new(&derive_key(shared.as_bytes()));

    let nonce = XNonce::from(*nonce_bytes);
    let payload = Payload {
        msg: ciphertext,
        aad: sender.as_bytes(),
    };

    cipher
        .decrypt(&nonce, payload)
        .map_err(|_| CryptoError::Decrypt)
}
