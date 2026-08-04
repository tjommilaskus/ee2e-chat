use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, Key, KeyInit, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;
use x25519_dalek::{PublicKey, StaticSecret};

/// A long-lived X25519 keypair. The public key is shared with the server so
/// other clients can encrypt to us; the secret key never leaves this process.
#[derive(Debug, Clone)]
pub struct Keypair {
    pub public_key: Vec<u8>,
    pub secret_key: Vec<u8>,
}

pub fn generate_keypair() -> Keypair {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);

    Keypair {
        public_key: public.as_bytes().to_vec(),
        secret_key: secret.to_bytes().to_vec(),
    }
}

/// Encrypt `plaintext` so that only the holder of `public_key_bytes` can read it.
///
/// Returns `(ciphertext, nonce)` where ciphertext is the 32-byte ephemeral
/// public key followed by the ChaCha20-Poly1305 output.
pub fn seal(plaintext: &[u8], public_key_bytes: &[u8]) -> (Vec<u8>, [u8; 12]) {
    let recipient_bytes =
        <[u8; 32]>::try_from(public_key_bytes).expect("Invalid public key length");
    let recipient_public = PublicKey::from(recipient_bytes);

    // Fresh ephemeral keypair per message, so each message gets its own shared secret.
    let ephemeral_secret = StaticSecret::random_from_rng(OsRng);
    let ephemeral_public = PublicKey::from(&ephemeral_secret);

    let shared_secret = ephemeral_secret.diffie_hellman(&recipient_public);
    let key = Key::from(*shared_secret.as_bytes());
    let cipher = ChaCha20Poly1305::new(&key);

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);

    let encrypted = cipher.encrypt(&nonce, plaintext).expect("Encryption failed");

    let mut ciphertext = ephemeral_public.as_bytes().to_vec();
    ciphertext.extend_from_slice(&encrypted);

    (ciphertext, nonce_bytes)
}

/// Decrypt a message produced by `seal` using our own secret key.
pub fn open(
    ciphertext: &[u8],
    nonce_bytes: &[u8; 12],
    secret_key_bytes: &[u8],
) -> Result<Vec<u8>, String> {
    if ciphertext.len() < 32 {
        return Err("Ciphertext too short".to_string());
    }

    let ephemeral_bytes =
        <[u8; 32]>::try_from(&ciphertext[..32]).map_err(|_| "Invalid ephemeral public key")?;
    let ephemeral_public = PublicKey::from(ephemeral_bytes);
    let encrypted_data = &ciphertext[32..];

    let secret_bytes =
        <[u8; 32]>::try_from(secret_key_bytes).map_err(|_| "Invalid secret key length")?;
    let secret = StaticSecret::from(secret_bytes);

    // Same shared secret the sender derived, from the other side of the exchange.
    let shared_secret = secret.diffie_hellman(&ephemeral_public);
    let key = Key::from(*shared_secret.as_bytes());
    let cipher = ChaCha20Poly1305::new(&key);

    let nonce = Nonce::from(*nonce_bytes);
    cipher
        .decrypt(&nonce, encrypted_data)
        .map_err(|e| format!("Decryption failed: {:?}", e))
}
