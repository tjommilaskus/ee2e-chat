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
const SCROLL: &str = "scroll";
const INPUT: &str = "input";
const HEADER: &str = "header";

/// Lines moved per PageUp/PageDown.
const PAGE: usize = 10;

/// True colour, so the palette matches the intended look rather than whichever
/// sixteen colours the terminal happens to define.
const GREEN: Color = Color::Rgb(0x2E, 0xE6, 0x4D);
const DIM_GREEN: Color = Color::Rgb(0x1C, 0x8C, 0x33);
const CYAN: Color = Color::Rgb(0x2E, 0xE6, 0xD6);
const AMBER: Color = Color::Rgb(0xFF, 0xB0, 0x00);
const RED: Color = Color::Rgb(0xFF, 0x4D, 0x4D);

/// Behind everything, in the margins either side of the centred interface.
const BACKDROP: Color = Color::Rgb(0x0A, 0x0E, 0x16);
/// Inside the panels. A shade lighter than the backdrop so the interface reads
/// as a panel sitting on a surface rather than a hole cut in one.
const PANEL: Color = Color::Rgb(0x14, 0x1B, 0x2A);

/// Widest the interface is allowed to grow.
///
/// A chat log stretched across a full-width terminal is unpleasant to read and
/// looks nothing like the intended layout, so beyond this the interface stops
/// growing and centres itself instead.
const MAX_WIDTH: usize = 100;

const HELP: &str =
    "ESC:quit | Enter:send | PgUp/PgDn:scroll | Commands: /help, /clear, /peers, /quit";

pub fn theme() -> Theme {
    let mut theme = Theme {
        shadow: false,
        borders: BorderStyle::Simple,
        ..Theme::default()
    };

    let palette = &mut theme.palette;
    palette[PaletteColor::Background] = BACKDROP;
    palette[PaletteColor::View] = PANEL;
    palette[PaletteColor::Primary] = GREEN;
    palette[PaletteColor::Secondary] = DIM_GREEN;
    palette[PaletteColor::Tertiary] = CYAN;
    palette[PaletteColor::TitlePrimary] = CYAN;
    palette[PaletteColor::TitleSecondary] = CYAN;
    palette[PaletteColor::Highlight] = AMBER;
    palette[PaletteColor::HighlightInactive] = AMBER;
    palette[PaletteColor::HighlightText] = BACKDROP;

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
        .style(ColorStyle::new(BACKDROP, AMBER))
        .on_submit(move |siv, text| submit(siv, &sending, text))
        .with_name(INPUT);

    let layout = LinearLayout::vertical()
        .child(Panel::new(
            TextView::new(header_text(&name)).h_align(HAlign::Center).with_name(HEADER),
        ))
        .child(
            Panel::new(
                ScrollView::new(TextView::new("").with_name(MESSAGES))
                    .scroll_strategy(ScrollStrategy::StickToBottom)
                    .with_name(SCROLL),
            )
            .title("Messages")
            .full_height(),
        )
        .child(Panel::new(input).title("Message"))
        .child(Panel::new(TextView::new(HELP)));

    // `add_layer` centres its child, unlike `add_fullscreen_layer` which pins
    // it to the whole screen. Capping the width and letting it centre keeps the
    // interface readable on a wide terminal; on a narrow one the cap never
    // binds and it fills the width as before.
    // `full_width` makes the layout claim everything offered rather than
    // shrinking to its content, and `max_width` limits what is offered. Together
    // they give min(terminal width, MAX_WIDTH). `max_width` alone would only cap
    // it, leaving the interface as narrow as its longest line.
    siv.add_layer(layout.full_width().max_width(MAX_WIDTH).full_height());
    siv.focus_name(INPUT).ok();

    siv.add_global_callback(cursive::event::Key::Esc, |siv| siv.quit());
    // In raw mode Ctrl+C arrives as an ordinary key rather than a signal, so
    // without this it would do nothing and the app would look wedged.
    siv.add_global_callback(cursive::event::Event::CtrlChar('c'), |siv| siv.quit());

    // The input holds focus, so these are global rather than handled by the
    // scroll view itself. EditView ignores PageUp/PageDown, so nothing is
    // being taken away from it.
    siv.add_global_callback(cursive::event::Key::PageUp, |siv| scroll(siv, true));
    siv.add_global_callback(cursive::event::Key::PageDown, |siv| scroll(siv, false));

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
        "⌐ TIO CHAT ¬  User: {name}  ⌐ {} ¬",
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
            impersonating_you,
        } => {
            if impersonating_you {
                alarm(siv, format!("someone just joined using YOUR name, {name}"));
                alarm(siv, format!("  you:  {existing}"));
                alarm(siv, format!("  them: {incoming}"));
                alarm(siv, "  anything they say will look like it came from you".to_string());
            } else {
                alarm(siv, format!("two peers are both calling themselves {name}"));
                alarm(siv, format!("  already here: {existing}"));
                alarm(siv, format!("  just joined:  {incoming}"));
                alarm(siv, "  compare fingerprints before trusting either".to_string());
            }
        }

        Event::Message {
            from, content, ..
        } => message(siv, &from, &content),

        Event::Notice(text) => system(siv, text),
        Event::Warning(text) => alarm(siv, text),
    }
}

/// Page the message log, and manage whether it keeps following new messages.
///
/// Scrolling up has to stop the log auto-following, or the next message would
/// yank the reader straight back to the bottom. Reaching the bottom again
/// restores it, so returning to live needs no separate key.
fn scroll(siv: &mut Cursive, up: bool) {
    siv.call_on_name(
        SCROLL,
        |view: &mut ScrollView<cursive::views::NamedView<TextView>>| {
            let top = view.content_viewport().top();
            let target = if up {
                view.set_scroll_strategy(ScrollStrategy::KeepRow);
                top.saturating_sub(PAGE)
            } else {
                top + PAGE
            };
            view.set_offset(cursive::Vec2::new(0, target));

            if view.is_at_bottom() {
                view.set_scroll_strategy(ScrollStrategy::StickToBottom);
            }
        },
    );
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
            // Ours is listed too: verifying a fingerprint takes both sides, and
            // the other person needs to hear yours read back.
            let mut you = StyledString::styled("   ", GREEN);
            you.append_styled(node.name(), GREEN);
            you.append_styled(" (you) · ", DIM_GREEN);
            you.append_styled(node.fingerprint(), CYAN);
            append(siv, you);

            let peers = node.peers();
            if peers.is_empty() {
                system(siv, "nobody else is connected".to_string());
                return;
            }

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

        "help" | "" => {
            system(siv, "/peers  who is here, with key fingerprints".to_string());
            system(siv, "/clear  empty this window".to_string());
            system(siv, "/quit   leave".to_string());
            system(siv, "PgUp/PgDn scrolls; reaching the bottom resumes following".to_string());
        }

        other => alarm(siv, format!("unknown command: /{other}")),
    }
}
