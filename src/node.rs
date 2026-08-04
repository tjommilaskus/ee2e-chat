//! The networking layer: one symmetric node that both listens and dials.
//!
//! Every connection is owned by exactly one task, which both reads from the
//! socket and writes to it, selecting between the two. Anyone wanting to send
//! to a peer clones that peer's `mpsc::Sender` from the shared state and
//! queues a frame, so nothing outside this module ever touches a socket.
//!
//! That single-task ownership is what makes shutdown simple: dropping a peer's
//! sender ends its task, which drops the `Framed`, which closes the socket. It
//! is also how a superseded connection is retired, since replacing the entry in
//! the connection map drops the sender that the losing task is waiting on.

use crate::crypto::{self, Keypair, KEY_LEN};
use crate::peers::{Admission, Peer, PeerRegistry, Rejection};
use crate::protocol::{Frame, ProtocolError, MAX_FRAME_BYTES};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::codec::{Framed, LinesCodec};

/// How long a peer has to complete the handshake before being dropped. Without
/// this, a connection that opens and then says nothing occupies a task forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Frames that may queue for one peer before sending blocks. Bounded on
/// purpose: a peer that stops reading must slow us down rather than grow our
/// memory without limit.
const SEND_QUEUE_DEPTH: usize = 64;

const MAX_NAME_LEN: usize = 32;

/// Something worth telling the user about. Stage 5 prints these; the cursive
/// UI will consume the same stream unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Listening(SocketAddr),
    PeerJoined {
        name: String,
        fingerprint: String,
    },
    PeerLeft {
        name: String,
        fingerprint: String,
    },
    /// Two different keys are presenting the same display name.
    NameConflict {
        name: String,
        existing: String,
        incoming: String,
    },
    Notice(String),
    Warning(String),
}

#[derive(Debug)]
pub enum ConnError {
    Io(std::io::Error),
    Protocol(ProtocolError),
    /// The line exceeded the codec's limit.
    Oversized,
    HandshakeTimeout,
    ClosedDuringHandshake,
    ExpectedHello,
    InvalidIdentity(&'static str),
    Rejected(Rejection),
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnError::Io(e) => write!(f, "io error: {e}"),
            ConnError::Protocol(e) => write!(f, "{e}"),
            ConnError::Oversized => write!(f, "peer sent an oversized line"),
            ConnError::HandshakeTimeout => write!(f, "peer did not complete the handshake in time"),
            ConnError::ClosedDuringHandshake => write!(f, "peer closed during the handshake"),
            ConnError::ExpectedHello => write!(f, "peer's first frame was not a Hello"),
            ConnError::InvalidIdentity(why) => write!(f, "peer presented an invalid identity: {why}"),
            ConnError::Rejected(Rejection::SelfConnection) => write!(f, "connected to ourselves"),
            ConnError::Rejected(Rejection::DuplicateConnection) => {
                write!(f, "already connected to this peer")
            }
        }
    }
}

impl std::error::Error for ConnError {}

impl From<std::io::Error> for ConnError {
    fn from(e: std::io::Error) -> Self {
        ConnError::Io(e)
    }
}

impl From<ProtocolError> for ConnError {
    fn from(e: ProtocolError) -> Self {
        ConnError::Protocol(e)
    }
}

impl From<tokio_util::codec::LinesCodecError> for ConnError {
    fn from(e: tokio_util::codec::LinesCodecError) -> Self {
        match e {
            tokio_util::codec::LinesCodecError::MaxLineLengthExceeded => ConnError::Oversized,
            tokio_util::codec::LinesCodecError::Io(e) => ConnError::Io(e),
        }
    }
}

pub struct NodeConfig {
    pub name: String,
    pub listen: SocketAddr,
}

/// Registry and live connections under one lock, so there is no ordering to get
/// wrong between them and no window where they disagree.
struct State {
    registry: PeerRegistry,
    connections: HashMap<Vec<u8>, mpsc::Sender<Frame>>,
}

struct Inner {
    name: String,
    identity: Keypair,
    listen_port: u16,
    state: Mutex<State>,
    events: mpsc::UnboundedSender<Event>,
}

/// A handle to the running node. Cheap to clone; every clone refers to the same
/// node.
#[derive(Clone)]
pub struct Node {
    inner: Arc<Inner>,
}

impl Node {
    /// Bind the listener, start accepting, and return the address actually
    /// bound -- which is the only way to learn the port when asked for zero.
    pub async fn start(
        config: NodeConfig,
        events: mpsc::UnboundedSender<Event>,
    ) -> std::io::Result<(Node, SocketAddr)> {
        let listener = TcpListener::bind(config.listen).await?;
        let bound = listener.local_addr()?;

        let identity = crypto::generate_keypair();
        let registry = PeerRegistry::new(identity.public_key.clone());

        let node = Node {
            inner: Arc::new(Inner {
                name: config.name,
                identity,
                listen_port: bound.port(),
                state: Mutex::new(State {
                    registry,
                    connections: HashMap::new(),
                }),
                events,
            }),
        };

        let accepting = node.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, from)) => {
                        let node = accepting.clone();
                        tokio::spawn(async move {
                            node.serve(stream, from.ip(), true).await;
                        });
                    }
                    Err(e) => {
                        accepting.emit(Event::Warning(format!("accept failed: {e}")));
                        // A persistent accept failure would otherwise spin.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });

        node.emit(Event::Listening(bound));
        Ok((node, bound))
    }

    /// Dial a peer. Returns once the socket is open; the handshake and
    /// everything after it continue in the background.
    pub async fn connect(&self, addr: SocketAddr) -> std::io::Result<()> {
        let stream = TcpStream::connect(addr).await?;
        let node = self.clone();
        tokio::spawn(async move {
            node.serve(stream, addr.ip(), false).await;
        });
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn public_key(&self) -> &[u8] {
        &self.inner.identity.public_key
    }

    pub fn fingerprint(&self) -> String {
        crypto::fingerprint(&self.inner.identity.public_key)
    }

    pub fn listen_port(&self) -> u16 {
        self.inner.listen_port
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.inner
            .state
            .lock()
            .unwrap()
            .registry
            .all()
            .cloned()
            .collect()
    }

    fn emit(&self, event: Event) {
        // Unbounded, so emitting never awaits and can be done from anywhere. A
        // send only fails once the consumer is gone, which means shutdown.
        let _ = self.inner.events.send(event);
    }

    /// Drive one connection from handshake to close, reporting why it ended.
    async fn serve(&self, stream: TcpStream, peer_ip: IpAddr, inbound: bool) {
        let direction = if inbound { "from" } else { "to" };
        if let Err(e) = self.run_connection(stream, peer_ip, inbound).await {
            match e {
                // Losing a tiebreak is the system working, not a fault.
                ConnError::Rejected(Rejection::DuplicateConnection) => {}
                other => self.emit(Event::Warning(format!(
                    "connection {direction} {peer_ip} ended: {other}"
                ))),
            }
        }
    }

    async fn run_connection(
        &self,
        stream: TcpStream,
        peer_ip: IpAddr,
        inbound: bool,
    ) -> Result<(), ConnError> {
        let codec = LinesCodec::new_with_max_length(MAX_FRAME_BYTES);
        let mut framed = Framed::new(stream, codec);

        let peer = tokio::time::timeout(HANDSHAKE_TIMEOUT, self.handshake(&mut framed, peer_ip))
            .await
            .map_err(|_| ConnError::HandshakeTimeout)??;

        let (tx, mut rx) = mpsc::channel::<Frame>(SEND_QUEUE_DEPTH);
        // Kept so cleanup can tell whether the registered connection is still
        // ours, or whether a later one superseded us.
        let mine = tx.clone();

        let admission = {
            let mut state = self.inner.state.lock().unwrap();
            let admission = state.registry.admit(peer.clone(), inbound);
            if !matches!(admission, Admission::Rejected(_)) {
                // On Superseded this drops the previous sender, which ends the
                // task holding the losing connection.
                state.connections.insert(peer.public_key.clone(), tx);
            }
            admission
        };

        match admission {
            Admission::Rejected(reason) => return Err(ConnError::Rejected(reason)),
            Admission::Superseded => {}
            Admission::Admitted { name_conflict } => {
                if let Some(existing) = name_conflict {
                    self.emit(Event::NameConflict {
                        name: peer.name.clone(),
                        existing,
                        incoming: peer.fingerprint.clone(),
                    });
                }
                self.emit(Event::PeerJoined {
                    name: peer.name.clone(),
                    fingerprint: peer.fingerprint.clone(),
                });
            }
        }

        let result = self.pump(&mut framed, &mut rx, &peer).await;
        self.disconnect(&peer, &mine);
        result
    }

    /// Read frames from the peer and write frames queued for it, until either
    /// side stops.
    async fn pump(
        &self,
        framed: &mut Framed<TcpStream, LinesCodec>,
        rx: &mut mpsc::Receiver<Frame>,
        peer: &Peer,
    ) -> Result<(), ConnError> {
        loop {
            tokio::select! {
                incoming = framed.next() => {
                    match incoming {
                        Some(Ok(line)) => self.handle_frame(peer, &line)?,
                        Some(Err(e)) => return Err(e.into()),
                        // Peer closed cleanly.
                        None => return Ok(()),
                    }
                }
                outgoing = rx.recv() => {
                    match outgoing {
                        Some(frame) => framed.send(frame.encode()?).await?,
                        // Our sender was dropped: we were superseded, or the
                        // node is shutting down.
                        None => return Ok(()),
                    }
                }
            }
        }
    }

    fn handle_frame(&self, peer: &Peer, line: &str) -> Result<(), ConnError> {
        match Frame::decode(line)? {
            // A second Hello is a protocol violation: identity is settled once.
            Frame::Hello { .. } => Err(ConnError::ExpectedHello),
            Frame::Peers { .. } => {
                self.emit(Event::Notice(format!(
                    "ignoring gossip from {} (not implemented yet)",
                    peer.name
                )));
                Ok(())
            }
            Frame::Chat { .. } => {
                self.emit(Event::Notice(format!(
                    "ignoring message from {} (not implemented yet)",
                    peer.name
                )));
                Ok(())
            }
        }
    }

    /// Retire this connection, but only if it is still the registered one.
    ///
    /// A superseded task also reaches here, and must not evict the connection
    /// that replaced it.
    fn disconnect(&self, peer: &Peer, mine: &mpsc::Sender<Frame>) {
        let still_ours = {
            let mut state = self.inner.state.lock().unwrap();
            let ours = state
                .connections
                .get(&peer.public_key)
                .is_some_and(|current| current.same_channel(mine));

            if ours {
                state.connections.remove(&peer.public_key);
                state.registry.remove(&peer.public_key);
            }
            ours
        };

        if still_ours {
            self.emit(Event::PeerLeft {
                name: peer.name.clone(),
                fingerprint: peer.fingerprint.clone(),
            });
        }
    }

    /// Exchange identities. Both sides send first and then read, so neither
    /// waits on the other to go first.
    async fn handshake(
        &self,
        framed: &mut Framed<TcpStream, LinesCodec>,
        peer_ip: IpAddr,
    ) -> Result<Peer, ConnError> {
        let hello = Frame::Hello {
            name: self.inner.name.clone(),
            public_key: self.inner.identity.public_key.clone(),
            listen_port: self.inner.listen_port,
        };
        framed.send(hello.encode()?).await?;

        let line = framed
            .next()
            .await
            .ok_or(ConnError::ClosedDuringHandshake)??;

        let Frame::Hello {
            name,
            public_key,
            listen_port,
        } = Frame::decode(&line)?
        else {
            return Err(ConnError::ExpectedHello);
        };

        // Validated here so that malformed identities cannot reach the crypto
        // or the registry, which both assume a well-formed key.
        if public_key.len() != KEY_LEN {
            return Err(ConnError::InvalidIdentity("public key is not 32 bytes"));
        }
        if name.trim().is_empty() {
            return Err(ConnError::InvalidIdentity("name is empty"));
        }
        if name.chars().count() > MAX_NAME_LEN {
            return Err(ConnError::InvalidIdentity("name is too long"));
        }
        if listen_port == 0 {
            return Err(ConnError::InvalidIdentity("listen port is zero"));
        }

        // The peer's own view of its address is unreliable behind NAT, so the
        // address recorded is the one this connection demonstrably came from,
        // paired with the port it advertises.
        let listen_addr = SocketAddr::new(peer_ip, listen_port).to_string();

        Ok(Peer::new(name, public_key, listen_addr))
    }
}
