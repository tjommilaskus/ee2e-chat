use ee2e_chat::room::{self, RoomCode, RoomError};

fn pk(byte: u8) -> Vec<u8> {
    vec![byte; 32]
}

#[test]
fn test_a_code_survives_being_written_down_and_typed_back() {
    let code = RoomCode::generate();
    let shown = code.display();

    assert_eq!(RoomCode::parse(&shown).unwrap(), code);
}

#[test]
fn test_codes_are_not_repeated() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        assert!(
            seen.insert(RoomCode::generate().display()),
            "generated the same code twice"
        );
    }
}

/// Read off a phone call or pasted from a chat message, so it has to survive
/// the mangling that implies.
#[test]
fn test_parsing_forgives_how_people_actually_type_it() {
    let code = RoomCode::generate();
    let shown = code.display();
    let bare = shown.trim_start_matches("TIO-").replace('-', "");

    let variants = [
        shown.clone(),
        shown.to_lowercase(),
        shown.replace('-', ""),
        shown.replace('-', " "),
        format!("  {shown}  "),
        bare.clone(),
        bare.to_lowercase(),
    ];

    for variant in variants {
        assert_eq!(
            RoomCode::parse(&variant).unwrap(),
            code,
            "should have parsed {variant:?}"
        );
    }
}

/// Crockford leaves these out of the alphabet precisely so they can be folded
/// onto the digits they resemble.
#[test]
fn test_lookalike_characters_are_folded() {
    let code = RoomCode::parse("TIO-1111-1111-1111-1111").unwrap();
    assert_eq!(RoomCode::parse("TIO-IIII-LLLL-1111-1111").unwrap(), code);

    let zeros = RoomCode::parse("TIO-0000-0000-0000-0000").unwrap();
    assert_eq!(RoomCode::parse("TIO-OOOO-0000-0000-0000").unwrap(), zeros);
}

#[test]
fn test_rubbish_is_rejected() {
    for bad in ["", "TIO-", "hello", "TIO-1111", "TIO-1111-1111-1111-11111", "!!!!-!!!!-!!!!-!!!!"] {
        assert!(
            matches!(RoomCode::parse(bad), Err(RoomError::Malformed(_))),
            "should have rejected {bad:?}"
        );
    }
}

/// A code in a log or a screenshot is a room anyone can join.
#[test]
fn test_debug_does_not_print_the_code() {
    let code = RoomCode::generate();
    let rendered = format!("{code:?}");

    assert!(!rendered.contains(&code.display()), "got: {rendered}");
    assert!(rendered.contains("redacted"), "got: {rendered}");
}

// ---------------------------------------------------------------------------
// Admission proofs
// ---------------------------------------------------------------------------

#[test]
fn test_two_holders_of_the_same_code_accept_each_other() {
    let code = RoomCode::generate();
    let (a_nonce, b_nonce) = (room::new_nonce(), room::new_nonce());

    let a_proof = code.proof(&pk(1), &a_nonce, &pk(2), &b_nonce);
    let b_proof = code.proof(&pk(2), &b_nonce, &pk(1), &a_nonce);

    // Each checks the other's from the other's point of view.
    assert!(code.verify(&b_proof, &pk(2), &b_nonce, &pk(1), &a_nonce));
    assert!(code.verify(&a_proof, &pk(1), &a_nonce, &pk(2), &b_nonce));
}

/// The point of the whole exercise.
#[test]
fn test_a_different_code_is_refused() {
    let theirs = RoomCode::generate();
    let ours = RoomCode::generate();
    let (a_nonce, b_nonce) = (room::new_nonce(), room::new_nonce());

    let outsider = theirs.proof(&pk(1), &a_nonce, &pk(2), &b_nonce);

    assert!(
        !ours.verify(&outsider, &pk(1), &a_nonce, &pk(2), &b_nonce),
        "someone without the code must not get in"
    );
}

/// Echoing a proof straight back at its sender must not work, which is why the
/// two sides order the inputs differently.
#[test]
fn test_a_proof_cannot_be_reflected_at_its_sender() {
    let code = RoomCode::generate();
    let (a_nonce, b_nonce) = (room::new_nonce(), room::new_nonce());

    let a_proof = code.proof(&pk(1), &a_nonce, &pk(2), &b_nonce);

    // Alice receives her own proof back, presented as Bob's.
    assert!(
        !code.verify(&a_proof, &pk(2), &b_nonce, &pk(1), &a_nonce),
        "a reflected proof must be refused"
    );
}

/// A proof recorded from one connection must be useless in the next, which is
/// what the per-connection nonces are for.
#[test]
fn test_a_proof_cannot_be_replayed_into_another_connection() {
    let code = RoomCode::generate();
    let (a_nonce, b_nonce) = (room::new_nonce(), room::new_nonce());
    let captured = code.proof(&pk(1), &a_nonce, &pk(2), &b_nonce);

    // Same parties, new connection, so new nonces.
    let fresh_b_nonce = room::new_nonce();
    assert!(
        !code.verify(&captured, &pk(1), &a_nonce, &pk(2), &fresh_b_nonce),
        "a captured proof must not open a later connection"
    );
}

/// Bound to the keys as well as the nonces, so it cannot be carried across to a
/// different pair of peers.
#[test]
fn test_a_proof_is_tied_to_the_keys_it_was_made_for() {
    let code = RoomCode::generate();
    let (a_nonce, b_nonce) = (room::new_nonce(), room::new_nonce());
    let proof = code.proof(&pk(1), &a_nonce, &pk(2), &b_nonce);

    assert!(!code.verify(&proof, &pk(9), &a_nonce, &pk(2), &b_nonce));
    assert!(!code.verify(&proof, &pk(1), &a_nonce, &pk(9), &b_nonce));
}

#[test]
fn test_flipping_any_bit_of_a_proof_breaks_it() {
    let code = RoomCode::generate();
    let (a_nonce, b_nonce) = (room::new_nonce(), room::new_nonce());
    let proof = code.proof(&pk(1), &a_nonce, &pk(2), &b_nonce);

    for index in [0, proof.len() / 2, proof.len() - 1] {
        let mut tampered = proof;
        tampered[index] ^= 0x01;
        assert!(!code.verify(&tampered, &pk(1), &a_nonce, &pk(2), &b_nonce));
    }
}

#[test]
fn test_nonces_are_not_repeated() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        assert!(seen.insert(room::new_nonce()), "nonce repeated");
    }
}
