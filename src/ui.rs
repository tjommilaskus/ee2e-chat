//! The terminal interface.
//!
//! Cursive owns a blocking event loop and tokio owns async tasks, so the two
//! cannot simply be nested. Cursive takes the main thread; the tokio runtime
//! runs alongside it and reaches the interface through `CbSink`, a channel of
//! closures that cursive runs on its own thread. Every function here that
//! mutates a view therefore takes `&mut Cursive` and is only ever called from
//! that thread.
//!
//! View construction and event rendering are deliberately separate from `run`,
//! so both can be exercised against cursive's dummy backend in tests.

use crate::node::{Event, Node};
use chrono::Local;
use cursive::align::HAlign;
use cursive::theme::{BorderStyle, Color, ColorStyle, PaletteColor, Theme};
use cursive::utils::markup::StyledString;
use cursive::view::{Nameable, Resizable, ScrollStrategy};
use cursive::views::{EditView, LinearLayout, Panel, ScrollView, TextView};
use cursive::Cursive;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

const MESSAGES: &str = "messages";
const INPUT: &str = "input";
const HEADER: &str = "header";

/// True colour, so the palette matches the intended look rather than whichever
/// sixteen colours the terminal happens to define.
const GREEN: Color = Color::Rgb(0x2E, 0xE6, 0x4D);
const DIM_GREEN: Color = Color::Rgb(0x1C, 0x8C, 0x33);
const CYAN: Color = Color::Rgb(0x2E, 0xE6, 0xD6);
const AMBER: Color = Color::Rgb(0xFF, 0xB0, 0x00);
const RED: Color = Color::Rgb(0xFF, 0x4D, 0x4D);
const BLACK: Color = Color::Rgb(0x08, 0x0C, 0x08);

const HELP: &str = "ESC:quit | Enter:send | Commands: /help, /clear, /peers, /quit";

pub fn theme() -> Theme {
    let mut theme = Theme {
        shadow: false,
        borders: BorderStyle::Simple,
        ..Theme::default()
    };

    let palette = &mut theme.palette;
    palette[PaletteColor::Background] = BLACK;
    palette[PaletteColor::View] = BLACK;
    palette[PaletteColor::Primary] = GREEN;
    palette[PaletteColor::Secondary] = DIM_GREEN;
    palette[PaletteColor::Tertiary] = CYAN;
    palette[PaletteColor::TitlePrimary] = CYAN;
    palette[PaletteColor::TitleSecondary] = CYAN;
    palette[PaletteColor::Highlight] = AMBER;
    palette[PaletteColor::HighlightInactive] = AMBER;
    palette[PaletteColor::HighlightText] = BLACK;

    theme
}

/// Assemble the interface. Separate from `run` so tests can build it against a
/// dummy backend and inspect the result.
pub fn build(siv: &mut Cursive, node: Node) {
    siv.set_theme(theme());

    let name = node.name().to_string();
    let sending = node.clone();

    let input = EditView::new()
        // The one place the palette is bypassed: an amber bar reads as "type
        // here" far more directly than a coloured caret does.
        .style(ColorStyle::new(BLACK, AMBER))
        .on_submit(move |siv, text| submit(siv, &sending, text))
        .with_name(INPUT);

    let layout = LinearLayout::vertical()
        .child(Panel::new(
            TextView::new(header_text(&name)).h_align(HAlign::Center).with_name(HEADER),
        ))
        .child(
            Panel::new(
                ScrollView::new(TextView::new("").with_name(MESSAGES))
                    .scroll_strategy(ScrollStrategy::StickToBottom),
            )
            .title("Messages")
            .full_height(),
        )
        .child(Panel::new(input).title("Message"))
        .child(Panel::new(TextView::new(HELP)));

    // `add_fullscreen_layer` already stretches its child, so wrapping the
    // layout in `full_screen()` as well double-counts the constraint and leaves
    // the last panel a row short.
    siv.add_fullscreen_layer(layout);
    siv.focus_name(INPUT).ok();

    siv.add_global_callback(cursive::event::Key::Esc, |siv| siv.quit());

    system(siv, format!("you are {name} · {}", node.fingerprint()));
    system(siv, "type /help for commands".to_string());
}

/// Run the interface. Blocks until the user quits.
pub fn run(node: Node, mut events: UnboundedReceiver<Event>, handle: tokio::runtime::Handle) {
    let mut siv = cursive::default();
    build(&mut siv, node.clone());

    // Both tasks below end on their own once cursive stops, because the sink
    // send fails as soon as the event loop is gone.
    let sink = siv.cb_sink().clone();
    handle.spawn(async move {
        while let Some(event) = events.recv().await {
            if sink
                .send(Box::new(move |siv: &mut Cursive| apply(siv, event)))
                .is_err()
            {
                break;
            }
        }
    });

    let clock = siv.cb_sink().clone();
    let name = node.name().to_string();
    handle.spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let name = name.clone();
            if clock
                .send(Box::new(move |siv: &mut Cursive| {
                    siv.call_on_name(HEADER, |view: &mut TextView| {
                        view.set_content(header_text(&name));
                    });
                }))
                .is_err()
            {
                break;
            }
        }
    });

    siv.run();
}

fn header_text(name: &str) -> String {
    format!(
        "⌐ RETRO CHAT ¬  User: {name}  ⌐ {} ¬",
        Local::now().format("%H:%M:%S")
    )
}

/// Render one event into the message log.
pub fn apply(siv: &mut Cursive, event: Event) {
    match event {
        Event::Listening(addr) => system(siv, format!("listening on {addr}")),

        Event::PeerJoined { name, fingerprint } => {
            system(siv, format!("{name} joined the chat · {fingerprint}"))
        }

        Event::PeerLeft { name, fingerprint } => {
            system(siv, format!("{name} left the chat · {fingerprint}"))
        }

        // Security-relevant, so it is styled as an alarm rather than a notice:
        // two different keys are presenting the same name, and the fingerprints
        // are the only way to tell them apart.
        Event::NameConflict {
            name,
            existing,
            incoming,
        } => {
            alarm(siv, format!("two peers are both calling themselves {name}"));
            alarm(siv, format!("  already here: {existing}"));
            alarm(siv, format!("  just joined:  {incoming}"));
            alarm(siv, "  compare fingerprints before trusting either".to_string());
        }

        Event::Message {
            from, content, ..
        } => message(siv, &from, &content),

        Event::Notice(text) => system(siv, text),
        Event::Warning(text) => alarm(siv, text),
    }
}

fn append(siv: &mut Cursive, line: StyledString) {
    siv.call_on_name(MESSAGES, |view: &mut TextView| {
        view.append(line);
        view.append("\n");
    });
}

/// A chat message, over two lines: a timestamp rule, then the message itself.
fn message(siv: &mut Cursive, from: &str, content: &str) {
    // The sender's own timestamp travels inside the ciphertext and is
    // authenticated, but it comes from their clock. Showing local arrival time
    // keeps the log monotonic even when a peer's clock is wrong.
    let now = Local::now().format("%H:%M:%S");

    let mut stamp = StyledString::styled("┌[", DIM_GREEN);
    stamp.append_styled(now.to_string(), DIM_GREEN);
    stamp.append_styled("]", DIM_GREEN);
    append(siv, stamp);

    let mut line = StyledString::styled("└─ ", DIM_GREEN);
    line.append_styled(from, GREEN);
    line.append_styled(" ▶ ", CYAN);
    line.append_styled(content, GREEN);
    append(siv, line);
}

fn system(siv: &mut Cursive, text: String) {
    append(siv, StyledString::styled(format!("[{text}]"), CYAN));
}

fn alarm(siv: &mut Cursive, text: String) {
    append(siv, StyledString::styled(format!("!! {text}"), RED));
}

fn submit(siv: &mut Cursive, node: &Node, text: &str) {
    let text = text.trim().to_string();

    // Clearing returns a callback that cursive expects to be run.
    if let Some(callback) = siv.call_on_name(INPUT, |view: &mut EditView| view.set_content("")) {
        callback(siv);
    }

    if text.is_empty() {
        return;
    }

    match text.strip_prefix('/') {
        Some(command) => run_command(siv, node, command.trim()),
        None => node.say(&text),
    }
}

fn run_command(siv: &mut Cursive, node: &Node, command: &str) {
    match command {
        "quit" | "q" | "exit" => siv.quit(),

        "clear" => {
            siv.call_on_name(MESSAGES, |view: &mut TextView| view.set_content(""));
        }

        "peers" => {
            let peers = node.peers();
            if peers.is_empty() {
                system(siv, "nobody else is connected".to_string());
            } else {
                system(siv, format!("{} connected", peers.len()));
                for peer in peers {
                    let mut line = StyledString::styled("   ", GREEN);
                    line.append_styled(&peer.name, GREEN);
                    line.append_styled(" · ", DIM_GREEN);
                    line.append_styled(&peer.fingerprint, CYAN);
                    append(siv, line);
                }
                system(
                    siv,
                    "read these aloud to check nobody is intercepting".to_string(),
                );
            }
        }

        "help" | "" => {
            system(siv, "/peers  who is here, with key fingerprints".to_string());
            system(siv, "/clear  empty this window".to_string());
            system(siv, "/quit   leave".to_string());
        }

        other => alarm(siv, format!("unknown command: /{other}")),
    }
}
