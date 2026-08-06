//! The interface is exercised headlessly: `build` and `apply` take a
//! `&mut Cursive` and never touch a backend, so a plain `Cursive::new()` is
//! enough to drive them. That catches the failure mode this kind of code is
//! prone to -- a view looked up by a name that does not exist, which silently
//! does nothing rather than failing loudly.

use cursive::view::View;
use cursive::views::{NamedView, ScrollView, TextView};
use cursive::{Cursive, Vec2};
use ee2e_chat::room::RoomCode;
use ee2e_chat::node::{Event, Node, NodeConfig};
use ee2e_chat::ui;

/// The message log: a scroll region wrapping the named text view.
type Log = ScrollView<NamedView<TextView>>;

/// Cursive applies a scroll strategy during layout, so nothing about following
/// can be observed until the tree has been laid out. This is what its own event
/// loop does on every refresh.
fn relayout(siv: &mut Cursive) {
    siv.screen_mut().layout(Vec2::new(100, 40));
}

/// Whether the log is showing the newest line.
fn at_bottom(siv: &mut Cursive) -> bool {
    siv.call_on_name("scroll", |view: &mut Log| view.is_at_bottom())
        .expect("the scroll view should exist")
}

fn viewport_top(siv: &mut Cursive) -> usize {
    siv.call_on_name("scroll", |view: &mut Log| view.content_viewport().top())
        .expect("the scroll view should exist")
}

/// Enough traffic to overflow the log several times over.
fn flood(siv: &mut Cursive, range: std::ops::RangeInclusive<usize>) {
    for n in range {
        ui::apply(
            siv,
            Event::Message {
                from: "bob".to_string(),
                fingerprint: "AAAA-BBBB-CCCC-DDDD".to_string(),
                content: format!("message {n}"),
            },
        );
    }
    relayout(siv);
}

fn press(siv: &mut Cursive, event: cursive::event::Event) {
    siv.on_event(event);
    relayout(siv);
}

/// Every node in a test shares one room, since a node only talks to peers
/// holding the same code. Rejection of a *different* code is covered by its own
/// tests rather than being a side effect of every other one.
fn test_room() -> RoomCode {
    RoomCode::parse("TIO-1111-1111-1111-1111").expect("a valid code")
}


async fn node(name: &str) -> Node {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // Drained rather than dropped, so the node's event sends keep succeeding.
    // The task ends by itself once the node is gone.
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let (node, _addr) = Node::start(
        NodeConfig {
            name: name.to_string(),
            listen: "127.0.0.1:0".parse().unwrap(),
            room: test_room(),
            identity: None,
        },
        tx,
    )
    .await
    .expect("node should start");
    node
}

/// Whether `text` holds a timestamp of `parts` colon-separated pairs -- two for
/// `HH:MM`, three for `HH:MM:SS`.
///
/// Matched by shape rather than by reading the format string, so the assertion
/// holds however the header comes to be built. Counting colons would not work:
/// the header says "User:" as well.
fn shows_time(text: &str, parts: usize) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let width = parts * 3 - 1;

    chars.windows(width).any(|window| {
        window.iter().enumerate().all(|(i, c)| {
            // Every third character is a separator; the rest are digits.
            if i % 3 == 2 {
                *c == ':'
            } else {
                c.is_ascii_digit()
            }
        })
    })
}

/// The text currently in the header.
fn header(siv: &mut Cursive) -> String {
    siv.call_on_name("header", |view: &mut TextView| {
        view.get_content().source().to_string()
    })
    .expect("the header view should exist")
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

/// The header clock is the only thing that changes with nothing happening, and
/// cursive redraws the whole screen whenever it does. At seconds resolution
/// that is a full repaint every second for the whole session, which a console
/// that paints slowly shows as a permanent flicker -- the bug this guards.
///
/// Asserted on the rendered header rather than the format string, so it holds
/// however the clock comes to be built.
#[tokio::test]
async fn test_the_header_clock_does_not_show_seconds() {
    let mut siv = ui_for("alice").await;
    let text = header(&mut siv);

    assert!(
        shows_time(&text, 2),
        "the header should carry an HH:MM clock: {text}"
    );
    assert!(
        !shows_time(&text, 3),
        "an HH:MM:SS clock repaints the whole screen every second: {text}"
    );
}

#[tokio::test]
async fn test_the_header_names_you() {
    let mut siv = ui_for("alice").await;
    assert!(header(&mut siv).contains("alice"), "got {}", header(&mut siv));
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
            impersonating_you: false,
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

/// Someone taking your name must be worded unmistakably, not as a generic
/// clash between two other people.
#[tokio::test]
async fn test_impersonation_of_you_is_called_out_explicitly() {
    let mut siv = ui_for("alice").await;

    ui::apply(
        &mut siv,
        Event::NameConflict {
            name: "alice".to_string(),
            existing: "1111-1111-1111-1111".to_string(),
            incoming: "2222-2222-2222-2222".to_string(),
            impersonating_you: true,
        },
    );

    let text = transcript(&mut siv);
    assert!(text.contains("YOUR name"), "got: {text}");
    assert!(text.contains("1111-1111-1111-1111"), "got: {text}");
    assert!(text.contains("2222-2222-2222-2222"), "got: {text}");
}

/// The whole point of the log: you should not have to touch anything to see
/// what was just said.
#[tokio::test]
async fn test_the_log_follows_new_messages() {
    let mut siv = ui_for("alice").await;
    ui::bind_keys(&mut siv);

    flood(&mut siv, 1..=40);

    assert!(
        at_bottom(&mut siv),
        "the newest message should be on screen without scrolling"
    );
}

/// Paging up is how you read back through history, so new arrivals must not
/// yank you away from what you are reading.
#[tokio::test]
async fn test_paging_up_stops_the_log_following() {
    let mut siv = ui_for("alice").await;
    ui::bind_keys(&mut siv);
    flood(&mut siv, 1..=40);

    press(&mut siv, cursive::event::Key::PageUp.into());
    let held = viewport_top(&mut siv);

    flood(&mut siv, 41..=50);

    assert_eq!(
        viewport_top(&mut siv),
        held,
        "messages arriving should not move a reader who has scrolled up"
    );
}

/// Regression: a single PageUp used to strand the log for good. It lands at the
/// top of the content, which is not the bottom, so following was switched off
/// and the only way back was paging down as far as the content went -- a race
/// that cannot be won while messages are still arriving.
#[tokio::test]
async fn test_returning_to_the_newest_message_resumes_following() {
    let mut siv = ui_for("alice").await;
    ui::bind_keys(&mut siv);
    flood(&mut siv, 1..=40);

    // Far enough up that the bottom is several pages away.
    for _ in 0..5 {
        press(&mut siv, cursive::event::Key::PageUp.into());
    }
    assert!(!at_bottom(&mut siv), "the test should have scrolled away");

    press(&mut siv, cursive::event::Event::CtrlChar('e'));
    assert!(at_bottom(&mut siv), "Ctrl+E should jump back to the newest message");

    // And it has to keep following from then on, not just jump the once.
    flood(&mut siv, 41..=60);
    assert!(
        at_bottom(&mut siv),
        "following should have resumed, not just jumped once"
    );
}

/// Someone who joins in is no longer reading history, and would otherwise send
/// a message into a log they cannot see.
#[tokio::test]
async fn test_sending_a_message_resumes_following() {
    use cursive::event::{Event as Key_, EventResult, Key};
    use cursive::views::EditView;

    let mut siv = ui_for("alice").await;
    ui::bind_keys(&mut siv);
    flood(&mut siv, 1..=40);

    for _ in 0..5 {
        press(&mut siv, cursive::event::Key::PageUp.into());
    }
    assert!(!at_bottom(&mut siv), "the test should have scrolled away");

    if let Some(cb) = siv.call_on_name("input", |view: &mut EditView| view.set_content("hello")) {
        cb(&mut siv);
    }
    let result = siv
        .call_on_name("input", |view: &mut EditView| {
            view.on_event(Key_::Key(Key::Enter))
        })
        .expect("input view should exist");
    if let EventResult::Consumed(Some(cb)) = result {
        cb(&mut siv);
    }
    relayout(&mut siv);

    assert!(
        at_bottom(&mut siv),
        "sending a message should return you to the newest message"
    );
}

#[tokio::test]
async fn test_peers_command_lists_your_own_fingerprint() {
    let mut siv = Cursive::new();
    let node = node("alice").await;
    let fingerprint = node.fingerprint();
    ui::build(&mut siv, node);

    ui::apply(&mut siv, Event::Notice("---".to_string()));
    let before = transcript(&mut siv).len();

    // Driven through the real submit path rather than by calling the handler
    // directly, so this covers the wiring as well as the output.
    use cursive::event::{Event as Key_, EventResult, Key};
    use cursive::view::View;
    use cursive::views::EditView;

    if let Some(cb) = siv.call_on_name("input", |view: &mut EditView| view.set_content("/peers")) {
        cb(&mut siv);
    }
    let result = siv
        .call_on_name("input", |view: &mut EditView| {
            view.on_event(Key_::Key(Key::Enter))
        })
        .expect("input view should exist");
    if let EventResult::Consumed(Some(cb)) = result {
        cb(&mut siv);
    }

    let text = transcript(&mut siv);
    assert!(text.len() > before, "the command should have produced output");
    assert!(text.contains(&fingerprint), "own fingerprint missing: {text}");
    assert!(text.contains("(you)"), "got: {text}");
}
