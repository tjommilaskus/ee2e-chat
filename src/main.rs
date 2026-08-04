// The modules live in the library crate (src/lib.rs). Declaring them again here
// with `mod` would compile a second, incompatible copy of every type into this
// binary, so this file only ever `use`s them.

use clap::Parser;
use ee2e_chat::node::{Event, Node, NodeConfig};
use ee2e_chat::ui;
use std::net::SocketAddr;
use tokio::net::lookup_host;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "ee2e-chat", about = "End-to-end encrypted peer-to-peer terminal chat")]
struct Args {
    /// Display name shown to other peers. Carries no authority -- peers are
    /// identified by key fingerprint.
    #[arg(long)]
    name: String,

    /// Address to accept connections on.
    #[arg(long, default_value = "0.0.0.0:9999")]
    listen: SocketAddr,

    /// A peer to dial on startup. The rest of the room is discovered from it.
    #[arg(long)]
    connect: Option<String>,

    /// Plain line-by-line output instead of the full interface. Useful when a
    /// full-screen UI would get in the way, such as piping a session to a file.
    #[arg(long)]
    plain: bool,
}

// Not `#[tokio::main]`. Cursive's event loop blocks whichever thread it runs
// on, so it takes the main thread and the runtime is built by hand to live
// alongside it.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let runtime = tokio::runtime::Runtime::new()?;

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (node, bound) = runtime.block_on(Node::start(
        NodeConfig {
            name: args.name,
            listen: args.listen,
        },
        events_tx,
    ))?;

    if let Some(target) = args.connect {
        // Resolved rather than parsed, so a hostname works as well as an IP.
        let dialled = runtime.block_on(async {
            let addr = lookup_host(&target)
                .await
                .map_err(|e| format!("could not resolve {target}: {e}"))?
                .next()
                .ok_or_else(|| format!("{target} resolved to no addresses"))?;
            node.connect(addr)
                .await
                .map_err(|e| format!("could not reach {addr}: {e}"))?;
            Ok::<_, String>(addr)
        });

        if let Err(e) = dialled {
            eprintln!("{e}");
        }
    }

    if args.plain {
        runtime.block_on(plain(node, bound, events_rx));
    } else {
        ui::run(node, events_rx, runtime.handle().clone());
    }

    Ok(())
}

/// Line-by-line mode: the same event stream, printed instead of drawn.
async fn plain(node: Node, bound: SocketAddr, mut events: mpsc::UnboundedReceiver<Event>) {
    println!("you are {} · {}", node.name(), node.fingerprint());
    println!("listening on {bound}");
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
            Event::Listening(_) => {}
            Event::Message {
                from, fingerprint, content,
            } => println!("<{from} {}> {content}", &fingerprint[..9]),
            Event::PeerJoined { name, fingerprint } => println!("* {name} joined · {fingerprint}"),
            Event::PeerLeft { name, fingerprint } => println!("* {name} left · {fingerprint}"),
            Event::NameConflict {
                name, existing, incoming,
            } => {
                println!("! two peers are calling themselves {name}: {existing} and {incoming}");
                println!("! check fingerprints before trusting either");
            }
            Event::Notice(text) => println!("  {text}"),
            Event::Warning(text) => eprintln!("! {text}"),
        }
    }
}
