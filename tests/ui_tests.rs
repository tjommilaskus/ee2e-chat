//! The interface is exercised headlessly: `build` and `apply` take a
//! `&mut Cursive` and never touch a backend, so a plain `Cursive::new()` is
//! enough to drive them. That catches the failure mode this kind of code is
//! prone to -- a view looked up by a name that does not exist, which silently
//! does nothing rather than failing loudly.

use cursive::views::TextView;
use cursive::Cursive;
use ee2e_chat::node::{Event, Node, NodeConfig};
use ee2e_chat::ui;

async fn node(name: &str) -> Node {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // Drained rather than dropped, so the node's event sends keep succeeding.
    // The task ends by itself once the node is gone.
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let (node, _addr) = Node::start(
        NodeConfig {
            name: name.to_string(),
            listen: "127.0.0.1:0".parse().unwrap(),
        },
        tx,
    )
    .await
    .expect("node should start");
    node
}

/// The text currently in the message log.
fn transcript(siv: &mut Cursive) -> String {
    siv.call_on_name("messages", |view: &mut TextView| {
        view.get_content().source().to_string()
    })
    .expect("the messages view should exist")
}

async fn ui_for(name: &str) -> Cursive {
    let mut siv = Cursive::new();
    ui::build(&mut siv, node(name).await);
    siv
}

#[tokio::test]
async fn test_building_the_ui_creates_every_named_view() {
    let mut siv = ui_for("alice").await;

    // A typo in any of these names would make the UI silently stop updating.
    assert!(siv
        .call_on_name("messages", |_: &mut TextView| ())
        .is_some());
    assert!(siv
        .call_on_name("header", |_: &mut TextView| ())
        .is_some());
    assert!(siv
        .call_on_name("input", |_: &mut cursive::views::EditView| ())
        .is_some());
}

/// Your own fingerprint has to be visible, or you cannot read it out to anyone.
#[tokio::test]
async fn test_shows_your_own_fingerprint_on_startup() {
    let mut siv = Cursive::new();
    let node = node("alice").await;
    let fingerprint = node.fingerprint();
    ui::build(&mut siv, node);

    let text = transcript(&mut siv);
    assert!(text.contains("alice"), "got: {text}");
    assert!(text.contains(&fingerprint), "got: {text}");
}

#[tokio::test]
async fn test_renders_a_chat_message() {
    let mut siv = ui_for("alice").await;

    ui::apply(
        &mut siv,
        Event::Message {
            from: "bob".to_string(),
            fingerprint: "AAAA-BBBB-CCCC-DDDD".to_string(),
            content: "hi alice".to_string(),
        },
    );

    let text = transcript(&mut siv);
    assert!(text.contains("bob"), "got: {text}");
    assert!(text.contains("hi alice"), "got: {text}");
    assert!(text.contains('▶'), "expected the message marker, got: {text}");
}

#[tokio::test]
async fn test_renders_joins_and_departures_with_fingerprints() {
    let mut siv = ui_for("alice").await;

    ui::apply(
        &mut siv,
        Event::PeerJoined {
            name: "bob".to_string(),
            fingerprint: "AAAA-BBBB-CCCC-DDDD".to_string(),
        },
    );
    ui::apply(
        &mut siv,
        Event::PeerLeft {
            name: "bob".to_string(),
            fingerprint: "AAAA-BBBB-CCCC-DDDD".to_string(),
        },
    );

    let text = transcript(&mut siv);
    assert!(text.contains("bob joined the chat"), "got: {text}");
    assert!(text.contains("bob left the chat"), "got: {text}");
    assert_eq!(
        text.matches("AAAA-BBBB-CCCC-DDDD").count(),
        2,
        "both lines should carry the fingerprint, got: {text}"
    );
}

/// The whole point of a name conflict is that the fingerprints are the only way
/// to tell the two apart, so both must appear.
#[tokio::test]
async fn test_a_name_conflict_shows_both_fingerprints() {
    let mut siv = ui_for("alice").await;

    ui::apply(
        &mut siv,
        Event::NameConflict {
            name: "bob".to_string(),
            existing: "1111-1111-1111-1111".to_string(),
            incoming: "2222-2222-2222-2222".to_string(),
        },
    );

    let text = transcript(&mut siv);
    assert!(text.contains("1111-1111-1111-1111"), "got: {text}");
    assert!(text.contains("2222-2222-2222-2222"), "got: {text}");
    assert!(text.contains("compare fingerprints"), "got: {text}");
}

#[tokio::test]
async fn test_warnings_are_shown_rather_than_swallowed() {
    let mut siv = ui_for("alice").await;

    ui::apply(
        &mut siv,
        Event::Warning("could not decrypt a message from bob".to_string()),
    );

    let text = transcript(&mut siv);
    assert!(text.contains("could not decrypt"), "got: {text}");
}

#[tokio::test]
async fn test_messages_accumulate_in_order() {
    let mut siv = ui_for("alice").await;

    for n in 1..=3 {
        ui::apply(
            &mut siv,
            Event::Message {
                from: "bob".to_string(),
                fingerprint: "AAAA-BBBB-CCCC-DDDD".to_string(),
                content: format!("message {n}"),
            },
        );
    }

    let text = transcript(&mut siv);
    let first = text.find("message 1").expect("first");
    let second = text.find("message 2").expect("second");
    let third = text.find("message 3").expect("third");
    assert!(first < second && second < third, "out of order: {text}");
}

#[tokio::test]
async fn test_the_theme_uses_the_retro_palette() {
    use cursive::theme::{Color, PaletteColor};

    let theme = ui::theme();
    assert_eq!(theme.palette[PaletteColor::Primary], Color::Rgb(0x2E, 0xE6, 0x4D));
    assert_eq!(theme.palette[PaletteColor::TitlePrimary], Color::Rgb(0x2E, 0xE6, 0xD6));
    assert_eq!(theme.palette[PaletteColor::Background], Color::Rgb(0x0A, 0x0E, 0x16));
    assert_ne!(
        theme.palette[PaletteColor::View],
        theme.palette[PaletteColor::Background],
        "panels should sit a shade above the backdrop"
    );
    assert!(!theme.shadow, "shadows would break the flat retro look");
}
