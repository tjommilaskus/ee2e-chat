use ee2e_chat::protocol::{Frame, PeerInfo, ProtocolError, MAX_FRAME_BYTES};

fn hello() -> Frame {
    Frame::Hello {
        name: "alice".to_string(),
        public_key: vec![7u8; 32],
        listen_port: 9999,
        nonce: [3u8; 32],
    }
}

fn peers() -> Frame {
    Frame::Peers {
        peers: vec![
            PeerInfo {
                name: "bob".to_string(),
                public_key: vec![1u8; 32],
                listen_addr: "192.168.1.43:9999".to_string(),
            },
            PeerInfo {
                name: "carol".to_string(),
                public_key: vec![2u8; 32],
                listen_addr: "192.168.1.44:9999".to_string(),
            },
        ],
    }
}

fn auth() -> Frame {
    Frame::Auth { proof: [5u8; 32] }
}

fn chat() -> Frame {
    Frame::Chat {
        ciphertext: vec![9, 8, 7, 6, 5],
        nonce: [3u8; 24],
    }
}

#[test]
fn test_every_variant_round_trips() {
    for frame in [hello(), auth(), peers(), chat()] {
        let line = frame.encode().unwrap();
        let decoded = Frame::decode(&line).unwrap();
        assert_eq!(decoded, frame);
    }
}

#[test]
fn test_empty_peers_list_round_trips() {
    let frame = Frame::Peers { peers: vec![] };
    let line = frame.encode().unwrap();
    assert_eq!(Frame::decode(&line).unwrap(), frame);
}

// The codec supplies the terminator, so an encoded frame must contain no
// newline whatsoever -- otherwise one frame would arrive as two.
#[test]
fn test_encoded_frame_contains_no_newline() {
    for frame in [hello(), auth(), peers(), chat()] {
        assert!(
            !frame.encode().unwrap().contains('\n'),
            "encoded frame must not contain a newline"
        );
    }
}

// The attack this defends against: a peer picks a name containing a newline,
// hoping to split one frame into two and have the second parsed as a forged
// frame in its own right. serde_json escapes the newline as the two characters
// \ and n, so a raw one never reaches the wire.
#[test]
fn test_newline_in_a_field_cannot_split_the_frame() {
    let frame = Frame::Hello {
        name: "alice\n{\"type\":\"Chat\",\"ciphertext\":[],\"nonce\":[]}".to_string(),
        public_key: vec![7u8; 32],
        listen_port: 9999,
        nonce: [3u8; 32],
    };

    let line = frame.encode().unwrap();
    assert!(
        !line.contains('\n'),
        "an embedded newline must not survive into the encoding"
    );
    assert_eq!(Frame::decode(&line).unwrap(), frame);
}

// The codec strips the terminator before handing us a line, but tolerating one
// costs nothing and makes the function usable against a raw capture.
#[test]
fn test_decode_tolerates_a_trailing_newline() {
    let frame = hello();
    let line = frame.encode().unwrap();

    assert_eq!(Frame::decode(&line).unwrap(), frame);
    assert_eq!(Frame::decode(&format!("{line}\n")).unwrap(), frame);
}

// Everything below is input an attacker controls, so it must produce errors
// rather than panics.

#[test]
fn test_malformed_input_errors_without_panicking() {
    let bad_inputs = [
        "",
        "\n",
        "not json at all",
        "{",
        "{}",
        "[1,2,3]",
        "null",
        r#"{"type":"Hello"}"#,                       // missing fields
        r#"{"type":"Nonexistent","x":1}"#,           // unknown variant
        r#"{"type":"Chat","ciphertext":[1],"nonce":[1,2,3]}"#, // nonce wrong length
        r#"{"type":"Hello","name":5,"public_key":[],"listen_port":1,"nonce":[]}"#, // wrong type
    ];

    for input in bad_inputs {
        assert!(
            Frame::decode(input).is_err(),
            "expected an error for input: {input:?}"
        );
    }
}

#[test]
fn test_oversized_line_is_rejected() {
    let oversized = "x".repeat(MAX_FRAME_BYTES + 1);

    match Frame::decode(&oversized) {
        Err(ProtocolError::TooLarge { size, .. }) => {
            assert_eq!(size, MAX_FRAME_BYTES + 1)
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

// The size guard must trip before the JSON parser runs, otherwise a huge
// payload is fully parsed before being rejected.
#[test]
fn test_oversized_valid_json_is_still_rejected() {
    let huge = Frame::Hello {
        name: "a".repeat(MAX_FRAME_BYTES),
        public_key: vec![0u8; 32],
        listen_port: 1,
        nonce: [3u8; 32],
    };

    assert!(matches!(
        huge.encode(),
        Err(ProtocolError::TooLarge { .. })
    ));
}

#[test]
fn test_frame_type_is_visible_in_the_encoding() {
    // A self-describing tag keeps the wire format readable while debugging.
    assert!(hello().encode().unwrap().contains(r#""type":"Hello""#));
    assert!(chat().encode().unwrap().contains(r#""type":"Chat""#));
}
