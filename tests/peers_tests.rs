use ee2e_chat::peers::{Admission, NameClash, Peer, PeerRegistry, Rejection};
use ee2e_chat::protocol::PeerInfo;

const INBOUND: bool = true;
const OUTBOUND: bool = false;

fn key(byte: u8) -> Vec<u8> {
    vec![byte; 32]
}

fn peer(name: &str, k: u8) -> Peer {
    Peer::new(name.to_string(), key(k), format!("127.0.0.1:{}", 9000 + k as u16))
}

fn registry(me: u8) -> PeerRegistry {
    PeerRegistry::new(key(me), "me".to_string())
}

#[test]
fn test_admits_and_looks_up_a_peer() {
    let mut reg = registry(1);
    let bob = peer("bob", 2);

    assert!(matches!(
        reg.admit(bob.clone(), INBOUND),
        Admission::Admitted { name_conflict: None }
    ));

    assert_eq!(reg.len(), 1);
    assert_eq!(reg.get(&key(2)).unwrap().name, "bob");
    assert_eq!(reg.get(&key(2)).unwrap().fingerprint, bob.fingerprint);
}

#[test]
fn test_removes_a_peer() {
    let mut reg = registry(1);
    reg.admit(peer("bob", 2), INBOUND);
    reg.remove(&key(2));

    assert_eq!(reg.len(), 0);
    assert!(reg.get(&key(2)).is_none());
}

// Gossip echoes our own entry back to us almost immediately, so refusing to
// admit ourselves is routine rather than an edge case.
#[test]
fn test_refuses_to_admit_itself() {
    let mut reg = registry(1);
    let myself = Peer::new("me".to_string(), key(1), "127.0.0.1:9001".to_string());

    assert!(matches!(
        reg.admit(myself, INBOUND),
        Admission::Rejected(Rejection::SelfConnection)
    ));
    assert_eq!(reg.len(), 0);
}

// ---------------------------------------------------------------------------
// Simultaneous dial
//
// Once gossip runs, two nodes will sometimes dial each other at the same
// moment and end up holding two connections for one pair. Both must drop the
// same one, without exchanging any further messages to agree on which.
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Survivor {
    /// The connection this node dialled.
    Outbound,
    /// The connection the peer dialled.
    Inbound,
}

/// Replays one collision on a single node -- two connections to the same peer,
/// arriving in the given order -- and reports which one is left.
fn survivor_on_node(my_key: u8, their_key: u8, first_is_inbound: bool) -> Survivor {
    let mut reg = registry(my_key);
    let them = peer("them", their_key);

    match reg.admit(them.clone(), first_is_inbound) {
        Admission::Admitted { .. } => {}
        other => panic!("first connection should be admitted, got {other:?}"),
    }

    let second_is_inbound = !first_is_inbound;
    let second_won = match reg.admit(them, second_is_inbound) {
        Admission::Superseded => true,
        Admission::Rejected(Rejection::DuplicateConnection) => false,
        other => panic!("second connection should collide, got {other:?}"),
    };

    let winner_is_inbound = if second_won {
        second_is_inbound
    } else {
        first_is_inbound
    };

    if winner_is_inbound {
        Survivor::Inbound
    } else {
        Survivor::Outbound
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Dialer {
    Lower,
    Higher,
}

/// The decisive test. Checking one node in isolation would pass even if both
/// nodes dropped their connection, or both kept theirs -- which are precisely
/// the two ways this can fail. So both sides of the same collision are replayed
/// and required to name the same survivor.
#[test]
fn test_both_nodes_drop_the_same_connection() {
    let lower = 0x01;
    let higher = 0x02;

    // Arrival order is a race, and differs independently on each node, so every
    // combination has to reach the same conclusion.
    for lower_first_inbound in [true, false] {
        for higher_first_inbound in [true, false] {
            let on_lower = survivor_on_node(lower, higher, lower_first_inbound);
            let on_higher = survivor_on_node(higher, lower, higher_first_inbound);

            // Restate both answers as "who dialled the surviving connection",
            // so the two nodes can be compared directly.
            let lower_says = match on_lower {
                Survivor::Outbound => Dialer::Lower,
                Survivor::Inbound => Dialer::Higher,
            };
            let higher_says = match on_higher {
                Survivor::Outbound => Dialer::Higher,
                Survivor::Inbound => Dialer::Lower,
            };

            assert_eq!(
                lower_says, higher_says,
                "nodes disagreed (orders: lower={lower_first_inbound}, higher={higher_first_inbound})"
            );
            assert_eq!(
                lower_says,
                Dialer::Lower,
                "the connection dialled by the smaller key should survive"
            );
        }
    }
}

/// The rule must not depend on which connection happened to arrive first,
/// otherwise two nodes racing differently would reach opposite conclusions.
#[test]
fn test_tiebreak_is_independent_of_arrival_order() {
    assert_eq!(
        survivor_on_node(0x01, 0x02, true),
        survivor_on_node(0x01, 0x02, false)
    );
    assert_eq!(
        survivor_on_node(0x02, 0x01, true),
        survivor_on_node(0x02, 0x01, false)
    );
}

/// A lone connection must never be dropped. The tiebreak only applies when
/// there is genuinely a second connection to choose between -- otherwise every
/// node with a larger key would refuse the connections it dialled.
#[test]
fn test_a_single_connection_survives_regardless_of_key_order() {
    for (me, them) in [(0x01, 0x02), (0x02, 0x01)] {
        for inbound in [INBOUND, OUTBOUND] {
            let mut reg = registry(me);
            assert!(
                matches!(
                    reg.admit(peer("them", them), inbound),
                    Admission::Admitted { .. }
                ),
                "me={me:#x} them={them:#x} inbound={inbound}"
            );
            assert_eq!(reg.len(), 1);
        }
    }
}

/// A collision leaves exactly one entry, whichever connection won.
#[test]
fn test_collision_does_not_duplicate_the_peer() {
    let mut reg = registry(1);
    reg.admit(peer("bob", 2), OUTBOUND);
    reg.admit(peer("bob", 2), INBOUND);

    assert_eq!(reg.len(), 1);
}

// ---------------------------------------------------------------------------
// Trust on first use
// ---------------------------------------------------------------------------

/// Names are user-chosen and carry no authority, so two peers may pick the
/// same one. The registry keys on public key and surfaces the clash for the UI
/// to warn about, rather than letting one peer silently displace the other.
#[test]
fn test_reports_a_name_conflict_between_different_keys() {
    let mut reg = registry(1);
    let real = peer("alice", 2);
    reg.admit(real.clone(), INBOUND);

    match reg.admit(peer("alice", 3), INBOUND) {
        Admission::Admitted {
            name_conflict: Some(NameClash::WithPeer { fingerprint }),
        } => assert_eq!(fingerprint, real.fingerprint),
        other => panic!("expected a name conflict, got {other:?}"),
    }

    // Both are still present and distinguishable by fingerprint.
    assert_eq!(reg.len(), 2);
    assert_ne!(
        reg.get(&key(2)).unwrap().fingerprint,
        reg.get(&key(3)).unwrap().fingerprint
    );
}

/// Reconnecting under the same key and name is ordinary, not a conflict.
#[test]
fn test_same_key_reconnecting_is_not_a_name_conflict() {
    let mut reg = registry(1);
    reg.admit(peer("bob", 2), INBOUND);
    reg.remove(&key(2));

    assert!(matches!(
        reg.admit(peer("bob", 2), INBOUND),
        Admission::Admitted { name_conflict: None }
    ));
}

// ---------------------------------------------------------------------------
// Gossip filtering
// ---------------------------------------------------------------------------

fn info(name: &str, k: u8) -> PeerInfo {
    PeerInfo {
        name: name.to_string(),
        public_key: key(k),
        listen_addr: format!("127.0.0.1:{}", 9000 + k as u16),
    }
}

#[test]
fn test_undialed_skips_connected_peers_and_ourselves() {
    let mut reg = registry(1);
    reg.admit(peer("bob", 2), INBOUND);

    let gossiped = vec![info("me", 1), info("bob", 2), info("carol", 3)];
    let to_dial = reg.undialed(&gossiped);

    assert_eq!(to_dial.len(), 1);
    assert_eq!(to_dial[0].public_key, key(3));
}

/// A peer named twice in one gossip frame must not be dialled twice.
#[test]
fn test_undialed_deduplicates() {
    let reg = registry(1);
    let gossiped = vec![info("carol", 3), info("carol", 3), info("carol", 3)];

    assert_eq!(reg.undialed(&gossiped).len(), 1);
}

#[test]
fn test_undialed_on_an_empty_gossip_frame() {
    let reg = registry(1);
    assert!(reg.undialed(&[]).is_empty());
}

// ---------------------------------------------------------------------------
// Impersonation of the local user
//
// We are not in our own registry, so a peer taking our name would otherwise
// pass unremarked -- the one clash most worth reporting, since anyone skimming
// for our messages would find theirs.
// ---------------------------------------------------------------------------

#[test]
fn test_a_peer_taking_our_name_is_reported() {
    let mut reg = PeerRegistry::new(key(1), "alice".to_string());

    assert_eq!(
        reg.admit(peer("alice", 2), INBOUND),
        Admission::Admitted {
            name_conflict: Some(NameClash::WithYou)
        }
    );
    // Reported, not refused: they are still a real peer we can talk to.
    assert_eq!(reg.len(), 1);
}

/// Names that differ only by case or padding read as the same name to a person,
/// which is the only thing a display name is for.
#[test]
fn test_near_miss_spellings_of_our_name_are_reported() {
    for taken in ["ALICE", "Alice", "  alice  "] {
        let mut reg = PeerRegistry::new(key(1), "alice".to_string());
        assert_eq!(
            reg.admit(peer(taken, 2), INBOUND),
            Admission::Admitted {
                name_conflict: Some(NameClash::WithYou)
            },
            "{taken:?} should be reported as our name"
        );
    }
}

#[test]
fn test_near_miss_spellings_between_peers_are_reported() {
    let mut reg = PeerRegistry::new(key(1), "me".to_string());
    reg.admit(peer("bob", 2), INBOUND);

    assert!(matches!(
        reg.admit(peer("BOB", 3), INBOUND),
        Admission::Admitted {
            name_conflict: Some(NameClash::WithPeer { .. })
        }
    ));
}

#[test]
fn test_a_distinct_name_is_not_a_conflict() {
    let mut reg = PeerRegistry::new(key(1), "alice".to_string());
    assert_eq!(
        reg.admit(peer("bob", 2), INBOUND),
        Admission::Admitted { name_conflict: None }
    );
}
