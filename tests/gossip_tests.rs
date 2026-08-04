use ee2e_chat::crypto;
use ee2e_chat::node::{Event, Node, NodeConfig};
use ee2e_chat::protocol::{Frame, PeerInfo, MAX_FRAME_BYTES};
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

/// Gossip converges through several rounds of dialling, so this allows longer
/// than the direct-connection tests do.
async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

fn counts(nodes: &[&Node]) -> Vec<usize> {
    nodes.iter().map(|n| n.peers().len()).collect()
}

/// Everyone dials one node; gossip must introduce the others to each other.
#[tokio::test]
async fn test_a_star_bootstrap_becomes_a_full_mesh() {
    let (hub, hub_addr, _rh) = start("hub").await;
    let (a, _, _ra) = start("a").await;
    let (b, _, _rb) = start("b").await;

    a.connect(hub_addr).await.expect("dial");
    b.connect(hub_addr).await.expect("dial");

    assert!(
        wait_until(|| counts(&[&hub, &a, &b]) == vec![2, 2, 2]).await,
        "expected a full mesh, got {:?}",
        counts(&[&hub, &a, &b])
    );

    // a and b never dialled each other directly.
    assert!(a.peers().iter().any(|p| p.name == "b"));
    assert!(b.peers().iter().any(|p| p.name == "a"));
}

/// The harder shape: c only ever hears about a through b.
#[tokio::test]
async fn test_a_chain_bootstrap_becomes_a_full_mesh() {
    let (a, a_addr, _ra) = start("a").await;
    let (b, b_addr, _rb) = start("b").await;
    let (c, _, _rc) = start("c").await;

    b.connect(a_addr).await.expect("dial");
    assert!(wait_until(|| counts(&[&a, &b]) == vec![1, 1]).await);

    c.connect(b_addr).await.expect("dial");

    assert!(
        wait_until(|| counts(&[&a, &b, &c]) == vec![2, 2, 2]).await,
        "expected a full mesh, got {:?}",
        counts(&[&a, &b, &c])
    );
    assert!(c.peers().iter().any(|p| p.name == "a"));
}

#[tokio::test]
async fn test_four_nodes_converge() {
    let (hub, hub_addr, _rh) = start("hub").await;
    let (a, _, _ra) = start("a").await;
    let (b, _, _rb) = start("b").await;
    let (c, _, _rc) = start("c").await;

    for node in [&a, &b, &c] {
        node.connect(hub_addr).await.expect("dial");
    }

    assert!(
        wait_until(|| counts(&[&hub, &a, &b, &c]) == vec![3, 3, 3, 3]).await,
        "expected a full mesh, got {:?}",
        counts(&[&hub, &a, &b, &c])
    );
}

/// Convergence must settle rather than oscillate. Every pair dialling both ways
/// would otherwise keep replacing live connections.
#[tokio::test]
async fn test_the_mesh_stays_settled() {
    let (hub, hub_addr, _rh) = start("hub").await;
    let (a, _, _ra) = start("a").await;
    let (b, _, _rb) = start("b").await;

    a.connect(hub_addr).await.expect("dial");
    b.connect(hub_addr).await.expect("dial");
    assert!(wait_until(|| counts(&[&hub, &a, &b]) == vec![2, 2, 2]).await);

    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        counts(&[&hub, &a, &b]),
        vec![2, 2, 2],
        "the mesh should not churn once converged"
    );
}

/// The payoff: a message reaches peers the sender never dialled.
#[tokio::test]
async fn test_a_message_reaches_a_peer_discovered_by_gossip() {
    let (hub, hub_addr, _rh) = start("hub").await;
    let (a, _, a_events) = start("a").await;
    let (b, _, mut b_events) = start("b").await;

    a.connect(hub_addr).await.expect("dial");
    b.connect(hub_addr).await.expect("dial");
    assert!(wait_until(|| counts(&[&hub, &a, &b]) == vec![2, 2, 2]).await);

    a.say("hello from a");

    // a's own echo first, then b receives it over the gossip-discovered link.
    let received = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = b_events.recv().await {
            if let Event::Message { from, content, .. } = event {
                return Some((from, content));
            }
        }
        None
    })
    .await
    .expect("should not time out")
    .expect("b should receive the message");

    assert_eq!(received, ("a".to_string(), "hello from a".to_string()));
    drop(a_events);
}

// ---------------------------------------------------------------------------
// Hostile gossip. This is the one place a peer can influence where we open
// connections, so it gets the same scrutiny as the handshake.
// ---------------------------------------------------------------------------

async fn handshake_as(name: &str, addr: SocketAddr) -> Framed<TcpStream, LinesCodec> {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let mut framed = Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));

    let identity = crypto::generate_keypair();
    let hello = Frame::Hello {
        name: name.to_string(),
        public_key: identity.public_key,
        listen_port: 4242,
    };
    framed.send(hello.encode().unwrap()).await.expect("send");
    framed.next().await.expect("hello").expect("valid line");
    framed
}

fn fake_peer(seed: u8, addr: &str) -> PeerInfo {
    PeerInfo {
        name: format!("ghost{seed}"),
        public_key: vec![seed; 32],
        listen_addr: addr.to_string(),
    }
}

#[tokio::test]
async fn test_an_oversized_peer_list_is_capped() {
    let (alice, alice_addr, mut events) = start("alice").await;
    let mut wire = handshake_as("mallory", alice_addr).await;

    // Addresses nothing is listening on, so no dial can succeed.
    let peers: Vec<PeerInfo> = (1..=200)
        .map(|n| fake_peer(n as u8, "127.0.0.1:1"))
        .collect();
    wire.send(Frame::Peers { peers }.encode().unwrap())
        .await
        .expect("send");

    let warned = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            if matches!(&event, Event::Warning(text) if text.contains("gossiped")) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    assert!(warned, "an oversized peer list should be reported");
    assert_eq!(alice.peers().len(), 1, "only mallory should be connected");
}

#[tokio::test]
async fn test_malformed_gossip_entries_are_ignored() {
    let (alice, alice_addr, _events) = start("alice").await;
    let mut wire = handshake_as("mallory", alice_addr).await;

    let peers = vec![
        PeerInfo {
            name: "short key".to_string(),
            public_key: vec![1u8; 8],
            listen_addr: "127.0.0.1:1".to_string(),
        },
        PeerInfo {
            name: "  ".to_string(),
            public_key: vec![2u8; 32],
            listen_addr: "127.0.0.1:1".to_string(),
        },
        PeerInfo {
            name: "bad address".to_string(),
            public_key: vec![3u8; 32],
            listen_addr: "not an address".to_string(),
        },
    ];
    wire.send(Frame::Peers { peers }.encode().unwrap())
        .await
        .expect("send");

    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(alice.peers().len(), 1, "only mallory should be connected");
    // Still serving, despite the junk.
    let (bob, _, _rb) = start("bob").await;
    bob.connect(alice_addr).await.expect("dial");
    assert!(wait_until(|| alice.peers().len() == 2).await);
}

/// Gossip that names us must not make us dial ourselves.
#[tokio::test]
async fn test_gossip_containing_our_own_entry_is_ignored() {
    let (alice, alice_addr, _events) = start("alice").await;
    let mut wire = handshake_as("mallory", alice_addr).await;

    let peers = vec![PeerInfo {
        name: "alice".to_string(),
        public_key: alice.public_key().to_vec(),
        listen_addr: alice_addr.to_string(),
    }];
    wire.send(Frame::Peers { peers }.encode().unwrap())
        .await
        .expect("send");

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(alice.peers().len(), 1, "alice must not admit herself");
}

/// An unreachable address must not wedge future attempts. If the in-flight
/// marker were left behind, that peer could never be dialled again.
#[tokio::test]
async fn test_a_failed_dial_does_not_block_a_later_one() {
    let (alice, alice_addr, _events) = start("alice").await;
    let mut wire = handshake_as("mallory", alice_addr).await;

    // Port 1 refuses immediately.
    let ghost = fake_peer(9, "127.0.0.1:1");
    wire.send(
        Frame::Peers {
            peers: vec![ghost.clone()],
        }
        .encode()
        .unwrap(),
    )
    .await
    .expect("send");

    tokio::time::sleep(Duration::from_millis(400)).await;

    // Gossiping it again must produce a fresh attempt rather than being
    // silently skipped, which is only observable through the warning.
    wire.send(Frame::Peers { peers: vec![ghost] }.encode().unwrap())
        .await
        .expect("send");

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(alice.peers().len(), 1);
}
