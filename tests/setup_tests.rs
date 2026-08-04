//! The setup screen and the launcher behind it.
//!
//! `Launcher::start` blocks on the runtime, which is only legal from outside
//! one -- exactly where it runs in production, on cursive's thread. So these
//! are plain `#[test]`s that own a runtime rather than `#[tokio::test]`s, which
//! would already be inside one and panic.

use cursive::views::TextView;
use cursive::Cursive;
use ee2e_chat::node::{Event, Node, NodeConfig};
use ee2e_chat::ui::{self, Launcher, Startup};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver};

fn launcher() -> (Runtime, Launcher, UnboundedReceiver<Event>) {
    let runtime = Runtime::new().expect("runtime");
    let (tx, rx) = mpsc::unbounded_channel();
    let launcher = Launcher::new(runtime.handle().clone(), None, tx);
    (runtime, launcher, rx)
}

fn startup(name: &str, listen: &str, connect: Option<&str>) -> Startup {
    Startup {
        name: name.to_string(),
        listen: listen.to_string(),
        connect: connect.map(str::to_string),
    }
}

#[test]
fn test_starts_a_node_from_valid_input() {
    let (_rt, launcher, _rx) = launcher();

    let node = launcher
        .start(&startup("alice", "127.0.0.1:0", None))
        .expect("should start");

    assert_eq!(node.name(), "alice");
    assert!(!node.fingerprint().is_empty());
}

#[test]
fn test_a_blank_name_is_refused() {
    let (_rt, launcher, _rx) = launcher();

    for blank in ["", "   ", "\t"] {
        let err = launcher
            .start(&startup(blank, "127.0.0.1:0", None))
            .expect_err("a blank name should be refused");
        assert!(err.contains("name"), "got: {err}");
    }
}

#[test]
fn test_the_name_is_trimmed() {
    let (_rt, launcher, _rx) = launcher();

    let node = launcher
        .start(&startup("  alice  ", "127.0.0.1:0", None))
        .expect("should start");

    assert_eq!(node.name(), "alice");
}

/// The error has to say what a valid address looks like, since this is the
/// field a first-time user is most likely to get wrong.
#[test]
fn test_a_bad_listen_address_is_refused_with_an_example() {
    let (_rt, launcher, _rx) = launcher();

    let err = launcher
        .start(&startup("alice", "not an address", None))
        .expect_err("should be refused");

    assert!(err.contains("0.0.0.0:9999"), "got: {err}");
}

#[test]
fn test_a_port_already_in_use_is_reported() {
    let (rt, launcher, _rx) = launcher();

    let first = launcher
        .start(&startup("alice", "127.0.0.1:0", None))
        .expect("should start");
    let taken = rt.block_on(async { format!("127.0.0.1:{}", first.listen_port()) });

    let err = launcher
        .start(&startup("bob", &taken, None))
        .expect_err("the second bind should fail");
    assert!(err.contains("could not listen"), "got: {err}");
}

/// An unreachable bootstrap peer must not stop us starting: others can still
/// connect to us, and the room may simply not be up yet.
#[test]
fn test_an_unreachable_peer_does_not_prevent_starting() {
    let (_rt, launcher, mut rx) = launcher();

    let node = launcher
        .start(&startup("alice", "127.0.0.1:0", Some("127.0.0.1:1")))
        .expect("should still start");

    assert_eq!(node.name(), "alice");

    let mut warned = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(&event, Event::Warning(text) if text.contains("could not reach")) {
            warned = true;
        }
    }
    assert!(warned, "the failure should still be reported");
}

#[test]
fn test_a_blank_connect_field_is_treated_as_none() {
    let (_rt, launcher, mut rx) = launcher();

    launcher
        .start(&startup("alice", "127.0.0.1:0", Some("   ")))
        .expect("should start");

    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(&event, Event::Warning(text) if text.contains("resolve")),
            "a blank field should not be dialled: {event:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Buffering
// ---------------------------------------------------------------------------

async fn node(name: &str) -> Node {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let (node, _addr) = Node::start(
        NodeConfig {
            name: name.to_string(),
            listen: "127.0.0.1:0".parse().unwrap(),
            identity: None,
        },
        tx,
    )
    .await
    .expect("node should start");
    node
}

fn transcript(siv: &mut Cursive) -> String {
    siv.call_on_name("messages", |view: &mut TextView| {
        view.get_content().source().to_string()
    })
    .expect("the messages view should exist")
}

/// Startup events are emitted before the user has finished with the setup
/// screen, so there is nowhere to put them yet. Dropping them would lose
/// exactly the ones worth reading -- which identity was loaded, whether the
/// bootstrap peer answered.
#[tokio::test]
async fn test_events_arriving_before_the_chat_exists_are_replayed() {
    let mut siv = Cursive::new();
    ui::prepare(&mut siv);

    ui::apply(&mut siv, Event::Notice("identity loaded".to_string()));
    ui::apply(&mut siv, Event::Warning("could not reach peer".to_string()));

    assert!(
        siv.call_on_name("messages", |_: &mut TextView| ()).is_none(),
        "there should be no chat view yet"
    );

    ui::build(&mut siv, node("alice").await);

    let text = transcript(&mut siv);
    assert!(text.contains("identity loaded"), "got: {text}");
    assert!(text.contains("could not reach peer"), "got: {text}");
}

#[tokio::test]
async fn test_replayed_events_keep_their_order() {
    let mut siv = Cursive::new();
    ui::prepare(&mut siv);

    for n in 1..=3 {
        ui::apply(&mut siv, Event::Notice(format!("step {n}")));
    }
    ui::build(&mut siv, node("alice").await);

    let text = transcript(&mut siv);
    let positions: Vec<_> = (1..=3)
        .map(|n| text.find(&format!("step {n}")).expect("present"))
        .collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "out of order: {text}");
}

/// Once the chat exists, events go straight through rather than accumulating.
#[tokio::test]
async fn test_events_after_the_chat_exists_are_not_buffered() {
    let mut siv = Cursive::new();
    ui::prepare(&mut siv);
    ui::build(&mut siv, node("alice").await);

    ui::apply(&mut siv, Event::Notice("live".to_string()));
    assert!(transcript(&mut siv).contains("live"));
}
