use ee2e_chat::node::{Event, Node, NodeConfig};
use ee2e_chat::protocol::{Frame, MAX_FRAME_BYTES};
use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::codec::{Framed, LinesCodec};

/// Port zero so the OS picks a free one; tests then use the address it actually
/// bound. Hard-coding ports makes tests collide with each other and with
/// whatever else is running.
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

/// Poll until a condition holds. Connections settle asynchronously, so tests
/// wait for an outcome rather than assuming one has already happened.
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

type Raw = Framed<TcpStream, LinesCodec>;

/// A hand-driven client, for behaviour a well-behaved node would never produce.
async fn raw_client(addr: SocketAddr) -> Raw {
    let stream = TcpStream::connect(addr).await.expect("connect");
    Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME_BYTES))
}

async fn send_raw(client: &mut Raw, line: &str) {
    client.send(line.to_string()).await.expect("send");
}

/// Whether the node closed the connection, as opposed to leaving it open.
async fn closed_by_peer(client: &mut Raw) -> bool {
    let wait = tokio::time::timeout(Duration::from_secs(3), async {
        // Drain anything still queued -- the node's own Hello arrives before it
        // has read ours -- until the stream ends.
        while let Some(frame) = client.next().await {
            if frame.is_err() {
                return true;
            }
        }
        true
    });
    wait.await.unwrap_or(false)
}

#[tokio::test]
async fn test_two_nodes_complete_the_handshake() {
    let (alice, alice_addr, _ra) = start("alice").await;
    let (bob, _bob_addr, _rb) = start("bob").await;

    bob.connect(alice_addr).await.expect("dial");

    assert!(
        wait_until(|| alice.peers().len() == 1 && bob.peers().len() == 1).await,
        "both nodes should admit each other"
    );

    assert_eq!(alice.peers()[0].name, "bob");
    assert_eq!(bob.peers()[0].name, "alice");
}

/// Each side must record the other's real fingerprint, since that is what the
/// user will compare out of band.
#[tokio::test]
async fn test_each_side_records_the_others_fingerprint() {
    let (alice, alice_addr, _ra) = start("alice").await;
    let (bob, _bob_addr, _rb) = start("bob").await;

    bob.connect(alice_addr).await.expect("dial");
    assert!(wait_until(|| alice.peers().len() == 1 && bob.peers().len() == 1).await);

    assert_eq!(alice.peers()[0].fingerprint, bob.fingerprint());
    assert_eq!(bob.peers()[0].fingerprint, alice.fingerprint());
    assert_ne!(alice.fingerprint(), bob.fingerprint());
}

/// The reason Hello carries a port rather than an address. Alice must record
/// Bob's *listening* port, not the ephemeral source port his dial came from --
/// gossiping the latter would send everyone to a closed door.
#[tokio::test]
async fn test_recorded_address_uses_the_listening_port_not_the_source_port() {
    let (alice, alice_addr, _ra) = start("alice").await;
    let (bob, bob_addr, _rb) = start("bob").await;

    bob.connect(alice_addr).await.expect("dial");
    assert!(wait_until(|| alice.peers().len() == 1).await);

    let recorded: SocketAddr = alice.peers()[0].listen_addr.parse().expect("valid address");

    assert_eq!(recorded.port(), bob_addr.port(), "should be Bob's listen port");
    assert_eq!(recorded.ip(), bob_addr.ip());
}

/// Pointing --connect at yourself must not produce a phantom peer. The same
/// path catches gossip echoing our own entry back to us.
#[tokio::test]
async fn test_node_refuses_to_connect_to_itself() {
    let (alice, alice_addr, _ra) = start("alice").await;

    alice.connect(alice_addr).await.expect("dial");
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(alice.peers().is_empty(), "a node must not admit itself");
}

/// `Node` is a handle, not the node itself. Dropping one while its tasks are
/// still running must not tear down live connections, or holding a node in a
/// short-lived scope would silently disconnect everyone.
#[tokio::test]
async fn test_dropping_a_node_handle_leaves_its_connections_up() {
    let (alice, alice_addr, _ra) = start("alice").await;
    let (bob, _bob_addr, _rb) = start("bob").await;

    bob.connect(alice_addr).await.expect("dial");
    assert!(wait_until(|| alice.peers().len() == 1).await);

    drop(bob);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(alice.peers().len(), 1);
}

#[tokio::test]
async fn test_raw_client_disconnecting_removes_the_peer() {
    let (alice, alice_addr, _ra) = start("alice").await;
    let mut client = raw_client(alice_addr).await;

    let hello = Frame::Hello {
        name: "mallory".to_string(),
        public_key: vec![9u8; 32],
        listen_port: 4242,
    };
    send_raw(&mut client, &hello.encode().unwrap()).await;

    assert!(wait_until(|| alice.peers().len() == 1).await, "should admit");

    drop(client);
    assert!(
        wait_until(|| alice.peers().is_empty()).await,
        "closing the socket should remove the peer"
    );
}

// ---------------------------------------------------------------------------
// Hostile input. Every case must close only the offending connection and leave
// the node serving everyone else.
// ---------------------------------------------------------------------------

async fn assert_handshake_rejected(label: &str, first_line: String) {
    let (alice, alice_addr, _ra) = start("alice").await;
    let mut client = raw_client(alice_addr).await;

    send_raw(&mut client, &first_line).await;

    assert!(
        closed_by_peer(&mut client).await,
        "{label}: node should close the connection"
    );
    assert!(alice.peers().is_empty(), "{label}: no peer should be admitted");

    // The node must still be usable afterwards.
    let (bob, _bob_addr, _rb) = start("bob").await;
    bob.connect(alice_addr).await.expect("dial");
    assert!(
        wait_until(|| alice.peers().len() == 1).await,
        "{label}: node should still accept good connections"
    );
}

#[tokio::test]
async fn test_first_frame_must_be_a_hello() {
    let chat = Frame::Chat {
        ciphertext: vec![1, 2, 3],
        nonce: [0u8; 24],
    };
    assert_handshake_rejected("chat before hello", chat.encode().unwrap()).await;
}

#[tokio::test]
async fn test_rejects_a_public_key_of_the_wrong_length() {
    let hello = Frame::Hello {
        name: "mallory".to_string(),
        public_key: vec![1u8; 31],
        listen_port: 4242,
    };
    assert_handshake_rejected("short key", hello.encode().unwrap()).await;
}

#[tokio::test]
async fn test_rejects_an_empty_name() {
    let hello = Frame::Hello {
        name: "   ".to_string(),
        public_key: vec![1u8; 32],
        listen_port: 4242,
    };
    assert_handshake_rejected("blank name", hello.encode().unwrap()).await;
}

#[tokio::test]
async fn test_rejects_an_absurdly_long_name() {
    let hello = Frame::Hello {
        name: "n".repeat(500),
        public_key: vec![1u8; 32],
        listen_port: 4242,
    };
    assert_handshake_rejected("long name", hello.encode().unwrap()).await;
}

#[tokio::test]
async fn test_rejects_a_zero_listen_port() {
    let hello = Frame::Hello {
        name: "mallory".to_string(),
        public_key: vec![1u8; 32],
        listen_port: 0,
    };
    assert_handshake_rejected("port zero", hello.encode().unwrap()).await;
}

#[tokio::test]
async fn test_rejects_garbage() {
    assert_handshake_rejected("not json", "}{ this is not json".to_string()).await;
}

#[tokio::test]
async fn test_survives_an_oversized_line() {
    let (alice, alice_addr, _ra) = start("alice").await;

    // Written to the socket directly, since the codec would refuse to encode a
    // line this long on the way out.
    use tokio::io::AsyncWriteExt;
    let mut stream = TcpStream::connect(alice_addr).await.unwrap();
    stream
        .write_all(&vec![b'x'; MAX_FRAME_BYTES * 2])
        .await
        .unwrap();
    stream.write_all(b"\n").await.unwrap();
    drop(stream);

    let (bob, _bob_addr, _rb) = start("bob").await;
    bob.connect(alice_addr).await.expect("dial");
    assert!(
        wait_until(|| alice.peers().len() == 1).await,
        "node should still work after an oversized line"
    );
}

/// Two nodes dialling each other at once must settle on exactly one connection.
/// This is the registry tiebreak running against real sockets, where the race
/// is genuine rather than simulated.
#[tokio::test]
async fn test_simultaneous_dial_settles_on_one_connection() {
    let (alice, alice_addr, _ra) = start("alice").await;
    let (bob, bob_addr, _rb) = start("bob").await;

    let (a, b) = tokio::join!(alice.connect(bob_addr), bob.connect(alice_addr));
    a.expect("dial");
    b.expect("dial");

    assert!(
        wait_until(|| alice.peers().len() == 1 && bob.peers().len() == 1).await,
        "expected exactly one peer each, got alice={} bob={}",
        alice.peers().len(),
        bob.peers().len()
    );

    // And it must stay settled rather than oscillating.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(alice.peers().len(), 1);
    assert_eq!(bob.peers().len(), 1);
    assert_eq!(alice.peers()[0].name, "bob");
    assert_eq!(bob.peers()[0].name, "alice");
}

/// One node must handle several inbound connections arriving at once.
///
/// Only the hub's side is asserted here; what the dialling nodes go on to do
/// with each other once gossip introduces them belongs to `gossip_tests`.
#[tokio::test]
async fn test_one_node_accepts_several_simultaneous_connections() {
    let (hub, hub_addr, _rh) = start("hub").await;
    let (a, _, _ra) = start("a").await;
    let (b, _, _rb) = start("b").await;
    let (c, _, _rc) = start("c").await;

    let (ra, rb, rc) = tokio::join!(
        a.connect(hub_addr),
        b.connect(hub_addr),
        c.connect(hub_addr)
    );
    ra.expect("dial");
    rb.expect("dial");
    rc.expect("dial");

    assert!(
        wait_until(|| hub.peers().len() == 3).await,
        "hub should hold three peers, got {}",
        hub.peers().len()
    );

    let names: Vec<String> = hub.peers().into_iter().map(|p| p.name).collect();
    for expected in ["a", "b", "c"] {
        assert!(names.contains(&expected.to_string()), "missing {expected} in {names:?}");
    }
}

#[tokio::test]
async fn test_reports_a_join_event() {
    let (alice, alice_addr, mut events) = start("alice").await;
    let (bob, _bob_addr, _rb) = start("bob").await;

    bob.connect(alice_addr).await.expect("dial");

    let joined = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            if let Event::PeerJoined { name, fingerprint } = event {
                return Some((name, fingerprint));
            }
        }
        None
    })
    .await
    .expect("should not time out")
    .expect("should see a join");

    assert_eq!(joined.0, "bob");
    assert_eq!(joined.1, bob.fingerprint());
    assert_eq!(alice.peers().len(), 1);
}
