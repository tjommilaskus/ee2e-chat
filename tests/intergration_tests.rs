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
        nonce: [0u8; 24],
    };
    let json = serde_json::to_string(&net_msg).unwrap();
    let parsed: ee2e_chat::messages::NetworkMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.from, "alice");
}
