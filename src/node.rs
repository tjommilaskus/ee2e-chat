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
use crate::messages::Message;
use crate::peers::{Admission, NameClash, Peer, PeerRegistry, Rejection};
use crate::protocol::{Frame, PeerInfo, ProtocolError, MAX_FRAME_BYTES};
use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
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

/// Most entries honoured from one gossip frame.
///
/// Gossip is the one place a peer can influence where we open connections, so
/// it is capped. Nothing in a gossip entry is trusted beyond deciding whether
/// to dial: the handshake establishes the real identity, so a forged key only
/// ever wastes one connection attempt.
const MAX_GOSSIP_PEERS: usize = 64;

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
    /// Two identities are presenting the same display name.
    NameConflict {
        name: String,
        /// Fingerprint of whoever already answered to the name -- ours when
        /// `impersonating_you` is set.
        existing: String,
        incoming: String,
        /// The name taken is our own.
        impersonating_you: bool,
    },
    /// A message, already decrypted and attributed. Also emitted for our own
    /// messages, so a UI that clears its input line can still show what was
    /// sent.
    Message {
        from: String,
        fingerprint: String,
        content: String,
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
    /// Peers with a dial already in flight.
    ///
    /// A dialled peer is not in the registry until its handshake finishes, so
    /// without this a second gossip frame arriving in that window would dial
    /// them again. The tiebreak would resolve the duplicate, but by replacing
    /// the live connection -- so the mesh would churn while it settled.
    dialing: HashSet<Vec<u8>>,
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
        let registry = PeerRegistry::new(identity.public_key.clone(), config.name.clone());

        let node = Node {
            inner: Arc::new(Inner {
                name: config.name,
                identity,
                listen_port: bound.port(),
                state: Mutex::new(State {
                    registry,
                    connections: HashMap::new(),
                    dialing: HashSet::new(),
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

    /// Encrypt `text` separately for every connected peer and queue it.
    ///
    /// Deliberately not `async`. Cursive's event handlers are synchronous, so
    /// an async send would need a runtime handle at every call site; and
    /// awaiting a full queue would let one unresponsive peer stall messages to
    /// everyone else. Queuing is best-effort instead, and a peer whose queue is
    /// full is reported rather than waited on.
    pub fn say(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        let message = Message::new(self.inner.name.clone(), text.to_string());
        let plaintext = match message.to_json() {
            Ok(json) => json,
            Err(e) => {
                self.emit(Event::Warning(format!("could not serialise message: {e}")));
                return;
            }
        };

        // Snapshot under the lock and encrypt outside it. Sealing is real CPU
        // work, once per recipient, and no connection should be blocked while
        // it happens.
        let targets: Vec<(Peer, mpsc::Sender<Frame>)> = {
            let state = self.inner.state.lock().unwrap();
            state
                .registry
                .all()
                .filter_map(|peer| {
                    state
                        .connections
                        .get(&peer.public_key)
                        .map(|tx| (peer.clone(), tx.clone()))
                })
                .collect()
        };

        if targets.is_empty() {
            self.emit(Event::Warning("nobody is connected; message not sent".to_string()));
            return;
        }

        // One ciphertext per recipient: each pair has its own shared secret, so
        // there is nothing to reuse between them.
        for (peer, tx) in targets {
            let sealed = crypto::seal(
                plaintext.as_bytes(),
                &peer.public_key,
                &self.inner.identity.secret_key,
            );

            match sealed {
                Ok((ciphertext, nonce)) => {
                    if tx.try_send(Frame::Chat { ciphertext, nonce }).is_err() {
                        self.emit(Event::Warning(format!(
                            "{} is not keeping up; message to them was dropped",
                            peer.name
                        )));
                    }
                }
                Err(e) => self.emit(Event::Warning(format!(
                    "could not encrypt for {}: {e}",
                    peer.name
                ))),
            }
        }

        self.emit(Event::Message {
            from: self.inner.name.clone(),
            fingerprint: self.fingerprint(),
            content: text.to_string(),
        });
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
                if let Some(clash) = name_conflict {
                    let (existing, impersonating_you) = match clash {
                        NameClash::WithPeer { fingerprint } => (fingerprint, false),
                        NameClash::WithYou => (self.fingerprint(), true),
                    };
                    self.emit(Event::NameConflict {
                        name: peer.name.clone(),
                        existing,
                        incoming: peer.fingerprint.clone(),
                        impersonating_you,
                    });
                }
                self.emit(Event::PeerJoined {
                    name: peer.name.clone(),
                    fingerprint: peer.fingerprint.clone(),
                });
                // Only on a genuine admission. Doing it for a superseded
                // connection would announce a membership change that did not
                // happen, and keep the room gossiping about itself.
                self.announce_peers();
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
            Frame::Peers { peers } => {
                self.handle_gossip(peer, peers);
                Ok(())
            }
            Frame::Chat { ciphertext, nonce } => {
                self.receive_chat(peer, &ciphertext, &nonce);
                Ok(())
            }
        }
    }

    /// Tell everyone who we are connected to.
    ///
    /// Sent after each new peer is admitted, which both introduces the newcomer
    /// to the room and hands the newcomer the room. It terminates because an
    /// admission only happens once per peer: when nobody new is being admitted,
    /// nothing is being sent.
    fn announce_peers(&self) {
        let (frame, targets) = {
            let state = self.inner.state.lock().unwrap();
            let peers: Vec<PeerInfo> = state.registry.all().map(|peer| peer.to_info()).collect();
            let targets: Vec<_> = state.connections.values().cloned().collect();
            (Frame::Peers { peers }, targets)
        };

        for tx in targets {
            // Best-effort: a peer too backed up to accept a peer list will get
            // the next one.
            let _ = tx.try_send(frame.clone());
        }
    }

    /// Dial anyone in a gossip frame we are not already connected to.
    fn handle_gossip(&self, from: &Peer, mut peers: Vec<PeerInfo>) {
        if peers.len() > MAX_GOSSIP_PEERS {
            self.emit(Event::Warning(format!(
                "{} gossiped {} peers; only the first {MAX_GOSSIP_PEERS} were considered",
                from.name,
                peers.len()
            )));
            peers.truncate(MAX_GOSSIP_PEERS);
        }

        // Entries that could never produce a usable connection are discarded
        // here rather than wasting a dial on them.
        peers.retain(|info| {
            info.public_key.len() == KEY_LEN
                && !info.name.trim().is_empty()
                && info.listen_addr.parse::<SocketAddr>().is_ok()
        });

        let to_dial = {
            let mut state = self.inner.state.lock().unwrap();
            let candidates = state.registry.undialed(&peers);

            // Marked as in-flight in the same critical section that selected
            // them, so two gossip frames arriving at once cannot both pick the
            // same peer.
            let mut chosen = Vec::new();
            for info in candidates {
                if state.dialing.insert(info.public_key.clone()) {
                    chosen.push(info);
                }
            }
            chosen
        };

        for info in to_dial {
            let Ok(addr) = info.listen_addr.parse::<SocketAddr>() else {
                self.finished_dialing(&info.public_key);
                continue;
            };

            let node = self.clone();
            tokio::spawn(async move {
                match TcpStream::connect(addr).await {
                    Ok(stream) => node.serve(stream, addr.ip(), false).await,
                    Err(e) => node.emit(Event::Warning(format!("could not reach {addr}: {e}"))),
                }
                // Cleared however the attempt ended, so a peer that drops out
                // can be dialled again when it is next gossiped.
                node.finished_dialing(&info.public_key);
            });
        }
    }

    fn finished_dialing(&self, public_key: &[u8]) {
        self.inner.state.lock().unwrap().dialing.remove(public_key);
    }

    /// Decrypt and attribute an incoming message.
    ///
    /// A message that will not open is reported and skipped rather than being
    /// treated as fatal: dropping the connection on bad input would hand any
    /// peer an easy way to disconnect us.
    fn receive_chat(&self, peer: &Peer, ciphertext: &[u8], nonce: &[u8; crypto::NONCE_LEN]) {
        // Opening against the peer's public key is what proves authorship. Only
        // the holder of that key could have produced something that opens here.
        let plaintext = match crypto::open(
            ciphertext,
            nonce,
            &self.inner.identity.secret_key,
            &peer.public_key,
        ) {
            Ok(plaintext) => plaintext,
            Err(e) => {
                self.emit(Event::Warning(format!(
                    "could not decrypt a message from {}: {e}",
                    peer.name
                )));
                return;
            }
        };

        let json = match std::str::from_utf8(&plaintext) {
            Ok(json) => json,
            Err(_) => {
                self.emit(Event::Warning(format!(
                    "{} sent a message that is not valid UTF-8",
                    peer.name
                )));
                return;
            }
        };

        let message = match Message::from_json(json) {
            Ok(message) => message,
            Err(e) => {
                self.emit(Event::Warning(format!(
                    "{} sent a malformed message: {e}",
                    peer.name
                )));
                return;
            }
        };

        // The name inside the ciphertext is authenticated, so no third party
        // can have altered it -- but the peer itself can still write someone
        // else's name there. Identity belongs to the handshake, so a
        // disagreement means this peer is trying to appear as somebody else.
        if message.from != peer.name {
            self.emit(Event::Warning(format!(
                "{} ({}) sent a message signed '{}'; discarded",
                peer.name, peer.fingerprint, message.from
            )));
            return;
        }

        self.emit(Event::Message {
            from: peer.name.clone(),
            fingerprint: peer.fingerprint.clone(),
            content: message.content,
        });
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
