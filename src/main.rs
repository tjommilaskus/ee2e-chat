// The modules live in the library crate (src/lib.rs). Declaring them again here
// with `mod` would compile a second, incompatible copy of every type into this
// binary, so this file only ever `use`s them.

use clap::Parser;
use ee2e_chat::identity;
use ee2e_chat::node::{Event, Node};
use ee2e_chat::ui;
use ee2e_chat::room::{self, RoomCode};
use ee2e_chat::ui::{Launcher, Startup};
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "ee2e-chat",
    version,
    about = "End-to-end encrypted peer-to-peer terminal chat"
)]
struct Args {
    /// Display name shown to other peers. Carries no authority -- peers are
    /// identified by key fingerprint. Omit it to be asked on a setup screen.
    #[arg(long)]
    name: Option<String>,

    /// Address to accept connections on.
    #[arg(long, default_value = ui::DEFAULT_LISTEN)]
    listen: String,

    /// A peer to dial on startup. The rest of the room is discovered from it.
    #[arg(long)]
    connect: Option<String>,

    /// Where the long-term keypair is stored.
    /// Defaults to $XDG_CONFIG_HOME/ee2e-chat/identity.
    #[arg(long, value_name = "PATH")]
    identity: Option<PathBuf>,

    /// Use a throwaway keypair instead of the stored one. Your fingerprint will
    /// differ from every previous session, so nobody can recognise you.
    #[arg(long, conflicts_with = "identity")]
    ephemeral: bool,

    /// The room to join, as given to you by whoever runs it. Remembered, so it
    /// only needs passing once.
    #[arg(long, value_name = "CODE")]
    room: Option<String>,

    /// Start a brand new room and print its code. Everyone already in the old
    /// one stays there; this does not move them.
    #[arg(long, conflicts_with = "room")]
    new_room: bool,

    /// Plain line-by-line output instead of the full interface. Useful when a
    /// full-screen UI would get in the way, such as piping a session to a file.
    #[arg(long)]
    plain: bool,
}

// Returning `Result` from `main` would render the error with `Debug`, turning
// a multi-line message into one line of escape sequences. These are read by
// people, so they are printed rather than returned.
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

// Not `#[tokio::main]`. Cursive's event loop blocks whichever thread it runs
// on, so it takes the main thread and the runtime is built by hand to live
// alongside it.
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let runtime = tokio::runtime::Runtime::new()?;

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    // Kept so startup problems can be reported through the same stream the UI
    // reads. Printing them would be useless: cursive clears the screen the
    // moment it starts, taking any earlier output with it.
    let startup = events_tx.clone();

    let (keypair, identity_notes) = resolve_identity(args.ephemeral, args.identity)?;
    for note in identity_notes {
        let _ = startup.send(note);
    }

    let (room_code, room_notes) = resolve_room(args.room, args.new_room)?;
    for note in room_notes {
        let _ = startup.send(note);
    }

    let room_path = room::default_path().ok();
    let launcher = Launcher::new(
        runtime.handle().clone(),
        keypair,
        room_code,
        room_path,
        events_tx,
    );

    // An empty name is what asks for the setup screen. Anything else given on
    // the command line prefills it.
    let startup = Startup {
        name: args.name.unwrap_or_default(),
        listen: args.listen,
        connect: args.connect,
        // Prefilled so it can be read off and shared, or replaced with a code
        // a friend has sent.
        room: launcher.room_display(),
    };

    if args.plain {
        if startup.name.trim().is_empty() {
            return Err("--plain needs --name: it has no setup screen to ask on".into());
        }
        let node = launcher.start(&startup)?;
        runtime.block_on(plain(node, events_rx));
    } else {
        ui::run(launcher, events_rx, startup);
    }

    Ok(())
}

/// Work out which keypair to run with, and what to tell the user about it.
///
/// A failure here stops the program rather than falling back to a fresh
/// keypair. Silently running under a different identity would be the worst
/// outcome available: everyone who had verified your fingerprint would see it
/// change, which is exactly the signal that means "someone is impersonating
/// them" -- so a routine disk problem would look identical to an attack.
fn resolve_identity(
    ephemeral: bool,
    explicit: Option<PathBuf>,
) -> Result<(Option<ee2e_chat::crypto::Keypair>, Vec<Event>), Box<dyn std::error::Error>> {
    if ephemeral {
        return Ok((
            None,
            vec![Event::Notice(
                "using a throwaway identity; your fingerprint will not match earlier sessions"
                    .to_string(),
            )],
        ));
    }

    let path = match explicit {
        Some(path) => path,
        None => identity::default_path()?,
    };

    let loaded = identity::load_or_create(&path).map_err(|e| {
        format!("{e}\n\nFix the file, point --identity somewhere else, or run with --ephemeral.")
    })?;

    let mut notes: Vec<Event> = loaded.notes.into_iter().map(Event::Warning).collect();
    notes.push(Event::Notice(if loaded.created {
        format!("new identity saved to {}", path.display())
    } else {
        format!("identity loaded from {}", path.display())
    }));

    Ok((Some(loaded.keypair), notes))
}

/// Decide which room to run in.
///
/// A room is always created when none exists, rather than running without one.
/// An open room that anybody who finds the port can join is not a state anyone
/// would choose deliberately, so it is not one that can be reached by
/// forgetting a flag.
fn resolve_room(
    given: Option<String>,
    new_room: bool,
) -> Result<(RoomCode, Vec<Event>), Box<dyn std::error::Error>> {
    let path = room::default_path()?;

    if let Some(text) = given {
        let code = RoomCode::parse(&text)
            .map_err(|e| format!("{e}\n\nA room code looks like TIO-K7M2-9QRX-4BNP-W8LZ."))?;
        // Remembered, so it only has to be typed once.
        room::save(&path, &code)?;
        return Ok((
            code.clone(),
            vec![Event::Notice(format!("room {}", code.display()))],
        ));
    }

    if new_room {
        let code = RoomCode::generate();
        room::save(&path, &code)?;
        return Ok((
            code.clone(),
            vec![
                Event::Notice(format!("new room {}", code.display())),
                Event::Notice("share that code with the people you want in it".to_string()),
            ],
        ));
    }

    let loaded = room::load_or_create(&path)?;
    let mut notes: Vec<Event> = loaded.notes.into_iter().map(Event::Warning).collect();

    notes.push(Event::Notice(format!("room {}", loaded.code.display())));
    if loaded.created {
        notes.push(Event::Notice(
            "share that code with the people you want in it".to_string(),
        ));
    }

    Ok((loaded.code, notes))
}

/// Line-by-line mode: the same event stream, printed instead of drawn.
async fn plain(node: Node, mut events: mpsc::UnboundedReceiver<Event>) {
    println!("you are {} · {}", node.name(), node.fingerprint());
    println!("type a message and press enter · ctrl-d to quit");

    let typing = node.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            typing.say(&line);
        }
    });

    while let Some(event) = events.recv().await {
        match event {
            Event::Listening(addr) => println!("listening on {addr}"),
            Event::Message {
                from, fingerprint, content,
            } => println!("<{from} {}> {content}", &fingerprint[..9]),
            Event::PeerJoined { name, fingerprint } => println!("* {name} joined · {fingerprint}"),
            Event::PeerLeft { name, fingerprint } => println!("* {name} left · {fingerprint}"),
            Event::NameConflict {
                name,
                existing,
                incoming,
                impersonating_you,
            } => {
                if impersonating_you {
                    println!("! someone joined using YOUR name, {name}");
                    println!("! you: {existing}  them: {incoming}");
                } else {
                    println!("! two peers are calling themselves {name}: {existing}, {incoming}");
                    println!("! check fingerprints before trusting either");
                }
            }
            Event::Notice(text) => println!("  {text}"),
            Event::Warning(text) => eprintln!("! {text}"),
        }
    }
}
