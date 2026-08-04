use ee2e_chat::protocol::{Frame, PeerInfo, ProtocolError, MAX_FRAME_BYTES};

fn hello() -> Frame {
    Frame::Hello {
        name: "alice".to_string(),
        public_key: vec![7u8; 32],
        listen_addr: "192.168.1.42:9999".to_string(),
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

fn chat() -> Frame {
    Frame::Chat {
        ciphertext: vec![9, 8, 7, 6, 5],
        nonce: [3u8; 24],
    }
}

#[test]
fn test_every_variant_round_trips() {
    for frame in [hello(), peers(), chat()] {
        let line = frame.to_line().unwrap();
        let decoded = Frame::from_line(&line).unwrap();
        assert_eq!(decoded, frame);
    }
}

#[test]
fn test_empty_peers_list_round_trips() {
    let frame = Frame::Peers { peers: vec![] };
    let line = frame.to_line().unwrap();
    assert_eq!(Frame::from_line(&line).unwrap(), frame);
}

// Framing correctness: a frame must occupy exactly one line, so the encoded
// form has to end with a newline and contain no others.
#[test]
fn test_encoded_frame_ends_with_exactly_one_newline() {
    for frame in [hello(), peers(), chat()] {
        let line = frame.to_line().unwrap();
        assert!(line.ends_with('\n'), "frame must be newline-terminated");
        assert_eq!(
            line.matches('\n').count(),
            1,
            "frame must contain no interior newlines"
        );
    }
}

// The attack this defends against: a peer picks a name containing a newline,
// hoping to split one frame into two and inject a forged second frame.
// serde_json escapes the newline as the two characters \ and n, so it never
// reaches the wire raw.
#[test]
fn test_newline_in_a_field_cannot_split_the_frame() {
    let frame = Frame::Hello {
        name: "alice\n{\"type\":\"Chat\"}".to_string(),
        public_key: vec![7u8; 32],
        listen_addr: "127.0.0.1:9999".to_string(),
    };

    let line = frame.to_line().unwrap();
    assert_eq!(
        line.matches('\n').count(),
        1,
        "an embedded newline must not create a second line"
    );
    assert_eq!(Frame::from_line(&line).unwrap(), frame);
}

#[test]
fn test_from_line_accepts_input_with_or_without_the_terminator() {
    let frame = hello();
    let with_newline = frame.to_line().unwrap();
    let without = with_newline.trim_end_matches('\n');

    assert_eq!(Frame::from_line(&with_newline).unwrap(), frame);
    assert_eq!(Frame::from_line(without).unwrap(), frame);
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
        r#"{"type":"Hello","name":5,"public_key":[],"listen_addr":""}"#, // wrong type
    ];

    for input in bad_inputs {
        assert!(
            Frame::from_line(input).is_err(),
            "expected an error for input: {input:?}"
        );
    }
}

#[test]
fn test_oversized_line_is_rejected() {
    let oversized = "x".repeat(MAX_FRAME_BYTES + 1);

    match Frame::from_line(&oversized) {
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
        listen_addr: "127.0.0.1:1".to_string(),
    };

    assert!(matches!(
        huge.to_line(),
        Err(ProtocolError::TooLarge { .. })
    ));
}

#[test]
fn test_frame_type_is_visible_in_the_encoding() {
    // A self-describing tag keeps the wire format readable while debugging.
    assert!(hello().to_line().unwrap().contains(r#""type":"Hello""#));
    assert!(chat().to_line().unwrap().contains(r#""type":"Chat""#));
}
