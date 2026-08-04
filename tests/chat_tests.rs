use ee2e_chat::crypto;
use ee2e_chat::messages::Message;
use ee2e_chat::node::{Event, Node, NodeConfig};
use ee2e_chat::protocol::{Frame, MAX_FRAME_BYTES};
use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::codec::{Framed, LinesCodec};

async fn start(name: &str) -> (Node, SocketAddr, UnboundedReceiver<Event>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (node, addr) = Node::start(
        NodeConfig {
            name: name.to_string(),
            listen: "127.0.0.1:0".parse().unwrap(),
        },
        tx,
    )
    .await
    .expect("node should start");
    (node, addr, rx)
}

async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Wait for the next chat message, ignoring joins and notices.
async fn next_message(events: &mut UnboundedReceiver<Event>) -> Option<(String, String)> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            if let Event::Message { from, content, .. } = event {
                return Some((from, content));
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

async fn next_warning(events: &mut UnboundedReceiver<Event>) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            if let Event::Warning(text) = event {
                return Some(text);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// Drain our own echo so tests can then look for the peer's message.
async fn skip_own_echo(events: &mut UnboundedReceiver<Event>, name: &str) {
    let (from, _) = next_message(events).await.expect("own echo");
    assert_eq!(from, name, "first message should be our own echo");
}

#[tokio::test]
async fn test_two_nodes_exchange_a_message() {
    let (alice, alice_addr, mut alice_events) = start("alice").await;
    let (bob, _bob_addr, mut bob_events) = start("bob").await;

    bob.connect(alice_addr).await.expect("dial");
    assert!(wait_until(|| alice.peers().len() == 1 && bob.peers().len() == 1).await);

    bob.say("hello alice");
    skip_own_echo(&mut bob_events, "bob").await;

    let (from, content) = next_message(&mut alice_events).await.expect("a message");
    assert_eq!(from, "bob");
    assert_eq!(content, "hello alice");
}

#[tokio::test]
async fn test_messages_flow_in_both_directions() {
    let (alice, alice_addr, mut alice_events) = start("alice").await;
    let (bob, _bob_addr, mut bob_events) = start("bob").await;

    bob.connect(alice_addr).await.expect("dial");
    assert!(wait_until(|| alice.peers().len() == 1 && bob.peers().len() == 1).await);

    bob.say("ping");
    skip_own_echo(&mut bob_events, "bob").await;
    assert_eq!(
        next_message(&mut alice_events).await.unwrap(),
        ("bob".to_string(), "ping".to_string())
    );

    alice.say("pong");
    skip_own_echo(&mut alice_events, "alice").await;
    assert_eq!(
        next_message(&mut bob_events).await.unwrap(),
        ("alice".to_string(), "pong".to_string())
    );
}

/// Everyone in the room receives it, each under their own key.
#[tokio::test]
async fn test_a_message_reaches_every_peer() {
    let (hub, hub_addr, mut hub_events) = start("hub").await;
    let (a, _, mut a_events) = start("a").await;
    let (b, _, mut b_events) = start("b").await;

    a.connect(hub_addr).await.expect("dial");
    b.connect(hub_addr).await.expect("dial");
    assert!(wait_until(|| hub.peers().len() == 2).await);
    assert!(wait_until(|| a.peers().len() == 1 && b.peers().len() == 1).await);

    hub.say("to everyone");
    skip_own_echo(&mut hub_events, "hub").await;

    assert_eq!(
        next_message(&mut a_events).await.unwrap(),
        ("hub".to_string(), "to everyone".to_string())
    );
    assert_eq!(
        next_message(&mut b_events).await.unwrap(),
        ("hub".to_string(), "to everyone".to_string())
    );
}

#[tokio::test]
async fn test_sender_sees_its_own_message() {
    let (_alice, alice_addr, _ae) = start("alice").await;
    let (bob, _bob_addr, mut bob_events) = start("bob").await;

    bob.connect(alice_addr).await.expect("dial");
    assert!(wait_until(|| bob.peers().len() == 1).await);

    bob.say("did this send?");

    let (from, content) = next_message(&mut bob_events).await.expect("echo");
    assert_eq!(from, "bob");
    assert_eq!(content, "did this send?");
}

#[tokio::test]
async fn test_speaking_to_an_empty_room_warns() {
    let (alice, _addr, mut events) = start("alice").await;

    alice.say("anyone there?");

    let warning = next_warning(&mut events).await.expect("a warning");
    assert!(warning.contains("nobody"), "got: {warning}");
}

#[tokio::test]
async fn test_blank_input_is_ignored() {
    let (_alice, alice_addr, _ae) = start("alice").await;
    let (bob, _bob_addr, mut bob_events) = start("bob").await;
    bob.connect(alice_addr).await.expect("dial");
    assert!(wait_until(|| bob.peers().len() == 1).await);

    bob.say("   ");
    bob.say("");
    bob.say("real message");

    // The first message to arrive must be the only non-blank one.
    let (_, content) = next_message(&mut bob_events).await.unwrap();
    assert_eq!(content, "real message");
}

// ---------------------------------------------------------------------------
// End-to-end encryption, observed at the wire
// ---------------------------------------------------------------------------

/// Complete a handshake by hand and return the connection plus the identity we
/// presented, so a test can then read or write raw frames.
async fn handshake_as(
    name: &str,
    addr: SocketAddr,
) -> (Framed<TcpStream, LinesCodec>, crypto::Keypair, Vec<u8>) {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let mut framed = Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));

    let identity = crypto::generate_keypair();
    let hello = Frame::Hello {
        name: name.to_string(),
        public_key: identity.public_key.clone(),
        listen_port: 4242,
    };
    framed.send(hello.encode().unwrap()).await.expect("send");

    // The node greets us first, which is how we learn its public key.
    let line = framed.next().await.expect("hello").expect("valid line");
    let their_key = match Frame::decode(&line).unwrap() {
        Frame::Hello { public_key, .. } => public_key,
        other => panic!("expected Hello, got {other:?}"),
    };

    (framed, identity, their_key)
}

/// The claim the whole project rests on: what crosses the wire is unreadable.
#[tokio::test]
async fn test_plaintext_never_appears_on_the_wire() {
    let (alice, alice_addr, _ae) = start("alice").await;
    let (mut wire, _identity, _alice_key) = handshake_as("observer", alice_addr).await;
    assert!(wait_until(|| alice.peers().len() == 1).await);

    let secret = "the treasure is buried under the oak";
    alice.say(secret);

    let line = wire.next().await.expect("a frame").expect("valid line");

    assert!(
        !line.contains(secret),
        "plaintext appeared on the wire: {line}"
    );
    assert!(
        !line.contains("alice"),
        "the sender's name leaked in plaintext: {line}"
    );

    // And the bytes themselves, not just the JSON text.
    let Frame::Chat { ciphertext, .. } = Frame::decode(&line).unwrap() else {
        panic!("expected a Chat frame");
    };
    assert!(
        !ciphertext
            .windows(secret.len())
            .any(|w| w == secret.as_bytes()),
        "plaintext found in the ciphertext bytes"
    );
}

/// An observer holding the wrong key cannot read what it captured, even with
/// the full frame in hand.
#[tokio::test]
async fn test_an_eavesdropper_cannot_decrypt_what_it_captures() {
    let (alice, alice_addr, _ae) = start("alice").await;
    let (mut wire, _observer, alice_key) = handshake_as("observer", alice_addr).await;
    assert!(wait_until(|| alice.peers().len() == 1).await);

    alice.say("for the observer only");
    let line = wire.next().await.expect("a frame").expect("valid line");
    let Frame::Chat { ciphertext, nonce } = Frame::decode(&line).unwrap() else {
        panic!("expected a Chat frame");
    };

    // An unrelated party with their own keypair gets nowhere.
    let mallory = crypto::generate_keypair();
    assert!(
        crypto::open(&ciphertext, &nonce, &mallory.secret_key, &alice_key).is_err(),
        "a third party must not be able to decrypt"
    );
}

/// The intended recipient *can* read it -- otherwise the test above would pass
/// on a message that is simply broken.
#[tokio::test]
async fn test_the_intended_recipient_can_decrypt() {
    let (alice, alice_addr, _ae) = start("alice").await;
    let (mut wire, observer, alice_key) = handshake_as("observer", alice_addr).await;
    assert!(wait_until(|| alice.peers().len() == 1).await);

    alice.say("readable by the recipient");
    let line = wire.next().await.expect("a frame").expect("valid line");
    let Frame::Chat { ciphertext, nonce } = Frame::decode(&line).unwrap() else {
        panic!("expected a Chat frame");
    };

    let plaintext = crypto::open(&ciphertext, &nonce, &observer.secret_key, &alice_key)
        .expect("the recipient should be able to decrypt");
    let message = Message::from_json(std::str::from_utf8(&plaintext).unwrap()).unwrap();

    assert_eq!(message.from, "alice");
    assert_eq!(message.content, "readable by the recipient");
}

/// Impersonation at the display layer. The crypto is entirely valid -- Mallory
/// really did seal this with her own key -- but the name inside disagrees with
/// the identity her handshake established, so it must not be shown as Bob's.
#[tokio::test]
async fn test_rejects_a_message_signed_with_another_name() {
    let (alice, alice_addr, mut alice_events) = start("alice").await;
    let (mut wire, mallory, alice_key) = handshake_as("mallory", alice_addr).await;
    assert!(wait_until(|| alice.peers().len() == 1).await);

    let forged = Message::new("bob".to_string(), "send me your password".to_string());
    let (ciphertext, nonce) = crypto::seal(
        forged.to_json().unwrap().as_bytes(),
        &alice_key,
        &mallory.secret_key,
    )
    .unwrap();

    wire.send(Frame::Chat { ciphertext, nonce }.encode().unwrap())
        .await
        .expect("send");

    let warning = next_warning(&mut alice_events).await.expect("a warning");
    assert!(
        warning.contains("mallory") && warning.contains("bob"),
        "the warning should name both identities, got: {warning}"
    );
    assert!(warning.contains("discarded"), "got: {warning}");
}

/// Undecryptable input must not be able to knock us off the network.
#[tokio::test]
async fn test_garbage_ciphertext_does_not_drop_the_connection() {
    let (alice, alice_addr, mut alice_events) = start("alice").await;
    let (mut wire, _identity, _alice_key) = handshake_as("noisy", alice_addr).await;
    assert!(wait_until(|| alice.peers().len() == 1).await);

    for _ in 0..5 {
        let junk = Frame::Chat {
            ciphertext: vec![0xAB; 64],
            nonce: [0u8; 24],
        };
        wire.send(junk.encode().unwrap()).await.expect("send");
    }

    let warning = next_warning(&mut alice_events).await.expect("a warning");
    assert!(warning.contains("decrypt"), "got: {warning}");

    // Still connected, and still able to receive a real message.
    assert_eq!(alice.peers().len(), 1);
}
