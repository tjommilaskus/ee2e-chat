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

use crate::crypto::Keypair;
use crate::node::{Event, Node, NodeConfig};
use chrono::Local;
use cursive::align::HAlign;
use cursive::theme::{BorderStyle, Color, ColorStyle, PaletteColor, Theme};
use cursive::utils::markup::StyledString;
use cursive::view::{Nameable, Resizable, ScrollStrategy};
use cursive::views::{Dialog, DummyView, EditView, LinearLayout, Panel, ScrollView, TextView};
use cursive::Cursive;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

const MESSAGES: &str = "messages";
const SCROLL: &str = "scroll";
const INPUT: &str = "input";
const HEADER: &str = "header";
const F_NAME: &str = "f_name";
const F_LISTEN: &str = "f_listen";
const F_CONNECT: &str = "f_connect";
const F_ERROR: &str = "f_error";

pub const DEFAULT_LISTEN: &str = "0.0.0.0:9999";

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

    system(siv, format!("you are {name} · {}", node.fingerprint()));
    system(siv, "type /help for commands".to_string());

    // Anything that happened while the setup screen was up now has somewhere to
    // go. Taken rather than borrowed, so `apply` is free to touch user data.
    let pending = siv
        .with_user_data(|state: &mut UiState| {
            state.name = name.clone();
            std::mem::take(&mut state.pending)
        })
        .unwrap_or_default();

    for event in pending {
        apply(siv, event);
    }
}

/// What the setup screen collects, or what the command line supplied instead.
#[derive(Clone, Debug)]
pub struct Startup {
    pub name: String,
    pub listen: String,
    pub connect: Option<String>,
}

impl Default for Startup {
    fn default() -> Self {
        Startup {
            name: String::new(),
            listen: DEFAULT_LISTEN.to_string(),
            connect: None,
        }
    }
}

/// Starts a node from the interface thread.
///
/// The setup screen has to create a node when a button is pressed, but cursive
/// handlers are synchronous and `Node::start` is not. This holds everything the
/// call needs and blocks on the runtime, which is safe here precisely because
/// the interface thread is outside it.
#[derive(Clone)]
pub struct Launcher {
    handle: tokio::runtime::Handle,
    identity: Option<Keypair>,
    events: UnboundedSender<Event>,
}

impl Launcher {
    pub fn new(
        handle: tokio::runtime::Handle,
        identity: Option<Keypair>,
        events: UnboundedSender<Event>,
    ) -> Self {
        Launcher {
            handle,
            identity,
            events,
        }
    }

    /// Bind, and dial the bootstrap peer if one was given.
    ///
    /// Failing to reach that peer is not a failure to start: others can still
    /// connect to us, and the room may simply not be up yet.
    pub fn start(&self, startup: &Startup) -> Result<Node, String> {
        let name = startup.name.trim().to_string();
        if name.is_empty() {
            return Err("a name is required".to_string());
        }

        let listen: SocketAddr = startup
            .listen
            .trim()
            .parse()
            .map_err(|_| format!("{:?} is not an address like 0.0.0.0:9999", startup.listen))?;

        let connect = startup
            .connect
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .map(str::to_string);

        let identity = self.identity.clone();
        let events = self.events.clone();

        self.handle.block_on(async move {
            let (node, _bound) = Node::start(
                NodeConfig {
                    name,
                    listen,
                    identity,
                },
                events.clone(),
            )
            .await
            .map_err(|e| format!("could not listen on {listen}: {e}"))?;

            if let Some(target) = connect {
                match dial(&node, &target).await {
                    Ok(addr) => {
                        let _ = events.send(Event::Notice(format!("dialling {addr}")));
                    }
                    Err(e) => {
                        let _ = events.send(Event::Warning(e));
                        let _ = events.send(Event::Notice(
                            "nobody else has to be running -- others can connect to you".to_string(),
                        ));
                    }
                }
            }

            Ok(node)
        })
    }
}

/// Resolved rather than parsed, so a hostname works as well as an address.
async fn dial(node: &Node, target: &str) -> Result<SocketAddr, String> {
    let addr = tokio::net::lookup_host(target)
        .await
        .map_err(|e| format!("could not resolve {target}: {e}"))?
        .next()
        .ok_or_else(|| format!("{target} resolved to no addresses"))?;

    node.connect(addr)
        .await
        .map_err(|e| format!("could not reach {addr}: {e}"))?;

    Ok(addr)
}

/// Interface state cursive carries for us.
///
/// `pending` exists because events can arrive while the setup screen is still
/// up, before any message view exists to receive them. Dropping those would
/// lose exactly the ones worth seeing -- which identity was loaded, whether the
/// bootstrap peer answered.
struct UiState {
    pending: Vec<Event>,
    name: String,
}

/// Apply the theme and set up the state both screens rely on.
///
/// Separate from `run` so a test can drive `apply` and `build` against a bare
/// `Cursive` and still exercise the buffering.
pub fn prepare(siv: &mut Cursive) {
    siv.set_theme(theme());
    siv.set_user_data(UiState {
        pending: Vec::new(),
        name: String::new(),
    });
}

/// Run the interface. Blocks until the user quits.
///
/// A name in `startup` means there is nothing left to ask and the node starts
/// at once. Without one the setup screen is shown, prefilled with whatever else
/// was supplied, and the node starts when the user presses Connect.
pub fn run(launcher: Launcher, mut events: UnboundedReceiver<Event>, startup: Startup) {
    let mut siv = cursive::default();
    prepare(&mut siv);

    // Registered once here rather than in `build`, so they work on the setup
    // screen too -- otherwise there would be no way out of it but the mouse.
    siv.add_global_callback(cursive::event::Key::Esc, |siv| siv.quit());
    // In raw mode Ctrl+C arrives as an ordinary key rather than a signal, so
    // without this it would do nothing and the app would look wedged.
    siv.add_global_callback(cursive::event::Event::CtrlChar('c'), |siv| siv.quit());

    // The message input holds focus once connected, so these are global rather
    // than handled by the scroll view. EditView ignores both, and before the
    // chat exists they find no view and quietly do nothing.
    siv.add_global_callback(cursive::event::Key::PageUp, |siv| scroll(siv, true));
    siv.add_global_callback(cursive::event::Key::PageDown, |siv| scroll(siv, false));

    // Both tasks below end on their own once cursive stops, because the sink
    // send fails as soon as the event loop is gone.
    let sink = siv.cb_sink().clone();
    let handle = launcher.handle.clone();
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
    handle.spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if clock
                .send(Box::new(|siv: &mut Cursive| {
                    // Read back rather than captured, since the name is not
                    // known until the setup screen is done with.
                    let name = siv
                        .user_data::<UiState>()
                        .map(|state| state.name.clone())
                        .unwrap_or_default();
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

    if startup.name.trim().is_empty() {
        setup_screen(&mut siv, launcher, startup);
    } else {
        match launcher.start(&startup) {
            Ok(node) => build(&mut siv, node),
            Err(e) => {
                eprintln!("{e}");
                return;
            }
        }
    }

    siv.run();
}

fn setup_screen(siv: &mut Cursive, launcher: Launcher, defaults: Startup) {
    let field = |name: &'static str, content: &str| {
        EditView::new()
            .content(content)
            .style(ColorStyle::new(BACKDROP, AMBER))
            .with_name(name)
            .fixed_width(46)
    };

    // Prefilled from the command line, so `--listen` and `--connect` are still
    // worth passing even when the name is left to the form.
    let form = LinearLayout::vertical()
        .child(TextView::new("Your name"))
        .child(field(F_NAME, &defaults.name))
        .child(DummyView)
        .child(TextView::new("Listen on"))
        .child(field(F_LISTEN, &defaults.listen))
        .child(DummyView)
        .child(TextView::new("Connect to a peer  (blank to wait for others)"))
        .child(field(F_CONNECT, defaults.connect.as_deref().unwrap_or("")))
        .child(DummyView)
        .child(TextView::new(StyledString::styled("", RED)).with_name(F_ERROR));

    siv.add_layer(
        Dialog::around(form)
            .title("⌐ TIO CHAT ¬")
            .button("Connect", move |siv| connect_pressed(siv, &launcher))
            .button("Quit", |siv| siv.quit()),
    );

    siv.focus_name(F_NAME).ok();
}

fn field_value(siv: &mut Cursive, name: &str) -> String {
    siv.call_on_name(name, |view: &mut EditView| view.get_content().to_string())
        .unwrap_or_default()
}

fn connect_pressed(siv: &mut Cursive, launcher: &Launcher) {
    let startup = Startup {
        name: field_value(siv, F_NAME),
        listen: field_value(siv, F_LISTEN),
        connect: Some(field_value(siv, F_CONNECT)),
    };

    match launcher.start(&startup) {
        Ok(node) => {
            siv.pop_layer();
            build(siv, node);
        }
        // Reported in place so the entered values survive and can be corrected,
        // rather than dropping the user back to an empty form.
        Err(e) => {
            siv.call_on_name(F_ERROR, |view: &mut TextView| {
                view.set_content(StyledString::styled(e, RED));
            });
        }
    }
}

fn header_text(name: &str) -> String {
    format!(
        "⌐ TIO CHAT ¬  User: {name}  ⌐ {} ¬",
        Local::now().format("%H:%M:%S")
    )
}

/// Render one event into the message log.
///
/// Before the chat view exists -- during setup -- events are held instead, and
/// replayed once there is somewhere to put them.
pub fn apply(siv: &mut Cursive, event: Event) {
    if siv.call_on_name(MESSAGES, |_: &mut TextView| ()).is_none() {
        siv.with_user_data(|state: &mut UiState| state.pending.push(event));
        return;
    }

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
