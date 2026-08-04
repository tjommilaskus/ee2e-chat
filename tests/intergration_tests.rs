use ee2e_chat::crypto;
use ee2e_chat::messages::{Message, NetworkMessage};

#[test]
fn test_message_serialization() {
    let msg = Message {
        from: "alice".to_string(),
        content: "hello".to_string(),
        timestamp: 1234567890,
    };
    let json = msg.to_json().unwrap();
    let parsed = Message::from_json(&json).unwrap();
    assert_eq!(parsed.from, "alice");
    assert_eq!(parsed.content, "hello");
    assert_eq!(parsed.timestamp, 1234567890);
}

#[test]
fn test_network_message_serialization() {
    let net_msg = NetworkMessage::new("alice".to_string(), vec![1, 2, 3], [7u8; 24]);
    let json = net_msg.to_json().unwrap();
    let parsed = NetworkMessage::from_json(&json).unwrap();
    assert_eq!(parsed.from, "alice");
    assert_eq!(parsed.ciphertext, vec![1, 2, 3]);
    assert_eq!(parsed.nonce, [7u8; 24]);
}

#[test]
fn test_keypair_generation() {
    let kp = crypto::generate_keypair();
    assert_eq!(kp.public_key.len(), 32);
    assert_eq!(kp.secret_key.len(), 32);

    // Two keypairs must never come out the same.
    let other = crypto::generate_keypair();
    assert_ne!(kp.public_key, other.public_key);
}

#[test]
fn test_debug_does_not_leak_secret_key() {
    let kp = crypto::generate_keypair();
    let rendered = format!("{:?}", kp);
    let secret_hex: String = kp.secret_key.iter().map(|b| format!("{:02x}", b)).collect();

    assert!(
        !rendered.contains(&secret_hex),
        "Debug output must not contain the secret key"
    );
    assert!(rendered.contains("redacted"));
}

#[test]
fn test_encrypt_decrypt_roundtrip() {
    let alice = crypto::generate_keypair();
    let bob = crypto::generate_keypair();
    let plaintext = b"hello world";

    let (ciphertext, nonce) = crypto::seal(plaintext, &bob.public_key, &alice.secret_key).unwrap();
    assert_ne!(ciphertext, plaintext);

    let decrypted = crypto::open(&ciphertext, &nonce, &bob.secret_key, &alice.public_key).unwrap();
    assert_eq!(decrypted, plaintext);
}

// The security property that matters most: a message sealed to Bob must not be
// readable by anyone else. A roundtrip test alone would still pass if `seal`
// ignored the recipient key entirely, so this is what catches that.
#[test]
fn test_third_party_cannot_decrypt() {
    let alice = crypto::generate_keypair();
    let bob = crypto::generate_keypair();
    let mallory = crypto::generate_keypair();

    let (ciphertext, nonce) =
        crypto::seal(b"secret for bob", &bob.public_key, &alice.secret_key).unwrap();

    assert_eq!(
        crypto::open(&ciphertext, &nonce, &mallory.secret_key, &alice.public_key),
        Err(crypto::CryptoError::Decrypt),
        "Mallory must not read a message addressed to Bob"
    );
}

// Sender authentication. Mallory relays Alice's real ciphertext to Bob but
// claims it came from herself; Bob looks up Mallory's public key and the
// message must fail to open.
#[test]
fn test_cannot_impersonate_another_sender() {
    let alice = crypto::generate_keypair();
    let bob = crypto::generate_keypair();
    let mallory = crypto::generate_keypair();

    let (ciphertext, nonce) =
        crypto::seal(b"transfer the money", &bob.public_key, &alice.secret_key).unwrap();

    assert_eq!(
        crypto::open(&ciphertext, &nonce, &bob.secret_key, &mallory.public_key),
        Err(crypto::CryptoError::Decrypt),
        "A message must not verify against the wrong sender's public key"
    );
}

// Alice and Bob derive the same shared secret, so without direction binding an
// attacker could echo Alice's message back to her and have it appear to be from
// Bob. The sender's public key is authenticated as associated data to stop this.
#[test]
fn test_reflected_message_is_rejected() {
    let alice = crypto::generate_keypair();
    let bob = crypto::generate_keypair();

    let (ciphertext, nonce) =
        crypto::seal(b"are you there?", &bob.public_key, &alice.secret_key).unwrap();

    // Bounce Alice's own ciphertext back at her, labelled as coming from Bob.
    assert_eq!(
        crypto::open(&ciphertext, &nonce, &alice.secret_key, &bob.public_key),
        Err(crypto::CryptoError::Decrypt),
        "Alice must not accept her own message reflected back as Bob's"
    );
}

// Poly1305 is an authenticator, not just a checksum: flipping any byte must be
// detected rather than silently yielding garbage plaintext.
#[test]
fn test_tampered_ciphertext_is_rejected() {
    let alice = crypto::generate_keypair();
    let bob = crypto::generate_keypair();

    let (ciphertext, nonce) =
        crypto::seal(b"hello world", &bob.public_key, &alice.secret_key).unwrap();

    for index in [0, ciphertext.len() / 2, ciphertext.len() - 1] {
        let mut tampered = ciphertext.clone();
        tampered[index] ^= 0x01;

        assert_eq!(
            crypto::open(&tampered, &nonce, &bob.secret_key, &alice.public_key),
            Err(crypto::CryptoError::Decrypt),
            "Flipping byte {index} must fail authentication"
        );
    }
}

#[test]
fn test_tampered_nonce_is_rejected() {
    let alice = crypto::generate_keypair();
    let bob = crypto::generate_keypair();

    let (ciphertext, mut nonce) =
        crypto::seal(b"hello world", &bob.public_key, &alice.secret_key).unwrap();
    nonce[0] ^= 0x01;

    assert_eq!(
        crypto::open(&ciphertext, &nonce, &bob.secret_key, &alice.public_key),
        Err(crypto::CryptoError::Decrypt)
    );
}

// Malformed keys arrive from the network, so they must produce errors rather
// than panicking the whole client.
#[test]
fn test_malformed_keys_error_instead_of_panicking() {
    let alice = crypto::generate_keypair();
    let short = vec![0u8; 31];

    assert_eq!(
        crypto::seal(b"hi", &short, &alice.secret_key),
        Err(crypto::CryptoError::BadPublicKey)
    );
    assert_eq!(
        crypto::seal(b"hi", &alice.public_key, &short),
        Err(crypto::CryptoError::BadSecretKey)
    );
    assert_eq!(
        crypto::open(&[0u8; 64], &[0u8; 24], &short, &alice.public_key),
        Err(crypto::CryptoError::BadSecretKey)
    );
}

#[test]
fn test_same_plaintext_encrypts_differently_each_time() {
    let alice = crypto::generate_keypair();
    let bob = crypto::generate_keypair();

    let (first, n1) = crypto::seal(b"same text", &bob.public_key, &alice.secret_key).unwrap();
    let (second, n2) = crypto::seal(b"same text", &bob.public_key, &alice.secret_key).unwrap();

    assert_ne!(n1, n2, "Each message must use a fresh nonce");
    assert_ne!(first, second, "Ciphertexts must not repeat");
}

#[test]
fn test_public_key_for_matches_generated_pair() {
    let kp = crypto::generate_keypair();
    assert_eq!(crypto::public_key_for(&kp.secret_key).unwrap(), kp.public_key);
}
