#[test]
fn test_message_serialization() {
    let msg = ee2e_chat::messages::Message {
        from: "alice".to_string(),
        content: "hello".to_string(),
        timestamp: 1234567890,
    };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: ee2e_chat::messages::Message = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.from, "alice");
    assert_eq!(parsed.content, "hello");
}
#[test]
fn test_network_message_serialization() {
    let net_msg = ee2e_chat::messages::NetworkMessage {
        from: "alice".to_string(),
        ciphertext: vec![1, 2, 3],
        nonce: [0u8; 12],
    };
    let json = serde_json::to_string(&net_msg).unwrap();
    let parsed: ee2e_chat::messages::NetworkMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.from, "alice");
}

#[test]
fn test_keypair_generation() {
    let kp = ee2e_chat::crypto::generate_keypair();
    assert_eq!(kp.public_key.len(), 32);
    assert_eq!(kp.secret_key.len(), 32);
}

#[test]
fn test_encrypt_decrypt_roundtrip() {
    let kp = ee2e_chat::crypto::generate_keypair();
    let plaintext = b"hello world";
    let (ciphertext, nonce) = ee2e_chat::crypto::seal(plaintext, &kp.public_key);
    assert!(!ciphertext.is_empty());
    let decrypted = ee2e_chat::crypto::open(&ciphertext, &nonce, &kp.secret_key).unwrap();
    assert_eq!(decrypted, plaintext);
}

// The security property that actually matters: a message sealed to Alice must
// not be readable by Bob. A roundtrip test alone would still pass if `seal`
// ignored the recipient key entirely, so this is the test that catches that.
#[test]
fn test_wrong_key_cannot_decrypt() {
    let alice = ee2e_chat::crypto::generate_keypair();
    let bob = ee2e_chat::crypto::generate_keypair();

    let (ciphertext, nonce) = ee2e_chat::crypto::seal(b"secret for alice", &alice.public_key);

    assert!(
        ee2e_chat::crypto::open(&ciphertext, &nonce, &bob.secret_key).is_err(),
        "Bob must not be able to decrypt a message sealed to Alice"
    );
}

// Poly1305 is an authenticator, not just a checksum: flipping any byte of the
// ciphertext must be detected rather than silently yielding garbage plaintext.
#[test]
fn test_tampered_ciphertext_is_rejected() {
    let kp = ee2e_chat::crypto::generate_keypair();
    let (mut ciphertext, nonce) = ee2e_chat::crypto::seal(b"hello world", &kp.public_key);

    let last = ciphertext.len() - 1;
    ciphertext[last] ^= 0x01;

    assert!(
        ee2e_chat::crypto::open(&ciphertext, &nonce, &kp.secret_key).is_err(),
        "Tampered ciphertext must fail authentication"
    );
}
