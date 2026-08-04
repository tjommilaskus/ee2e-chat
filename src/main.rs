// The modules live in the library crate (src/lib.rs). Declaring them again here
// with `mod` would compile a second, incompatible copy of every type into this
// binary, so this file only ever `use`s them.

use clap::Parser;
use ee2e_chat::node::{Event, Node, NodeConfig};
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (node, bound) = Node::start(
        NodeConfig {
            name: args.name,
            listen: args.listen,
        },
        events_tx,
    )
    .await?;

    println!("you are {} · {}", node.name(), node.fingerprint());
    println!("listening on {bound}");

    if let Some(target) = args.connect {
        // Resolved rather than parsed, so a hostname works as well as an IP.
        let addr = lookup_host(&target)
            .await
            .map_err(|e| format!("could not resolve {target}: {e}"))?
            .next()
            .ok_or_else(|| format!("{target} resolved to no addresses"))?;

        match node.connect(addr).await {
            Ok(()) => println!("dialling {addr}"),
            Err(e) => eprintln!("could not reach {addr}: {e}"),
        }
    }

    while let Some(event) = events_rx.recv().await {
        match event {
            Event::Listening(_) => {}
            Event::PeerJoined { name, fingerprint } => {
                println!("* {name} joined · {fingerprint}")
            }
            Event::PeerLeft { name, fingerprint } => {
                println!("* {name} left · {fingerprint}")
            }
            Event::NameConflict {
                name,
                existing,
                incoming,
            } => {
                println!("! two peers are calling themselves {name}: {existing} and {incoming}");
                println!("! check fingerprints before trusting either");
            }
            Event::Notice(text) => println!("  {text}"),
            Event::Warning(text) => eprintln!("! {text}"),
        }
    }

    Ok(())
}
