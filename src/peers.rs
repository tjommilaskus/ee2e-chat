//! Who we are connected to.
//!
//! Peers are keyed by **public key**, never by name. A name is whatever its
//! owner typed at startup and carries no authority: two peers may choose the
//! same one, and a peer may choose the name of someone already present. Only
//! the key identifies anybody, so only the key is allowed to index them.
//!
//! This module holds no sockets and no channels. Every decision here is a pure
//! function of the registry's contents, which is what makes the awkward parts
//! -- collision resolution in particular -- testable without a network.

use crate::crypto;
use crate::protocol::PeerInfo;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub public_key: Vec<u8>,
    /// Where this peer accepts connections, so we can pass it on via gossip.
    pub listen_addr: String,
    pub fingerprint: String,
}

impl Peer {
    pub fn new(name: String, public_key: Vec<u8>, listen_addr: String) -> Self {
        let fingerprint = crypto::fingerprint(&public_key);
        Peer {
            name,
            public_key,
            listen_addr,
            fingerprint,
        }
    }

    pub fn from_info(info: PeerInfo) -> Self {
        Peer::new(info.name, info.public_key, info.listen_addr)
    }

    pub fn to_info(&self) -> PeerInfo {
        PeerInfo {
            name: self.name.clone(),
            public_key: self.public_key.clone(),
            listen_addr: self.listen_addr.clone(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    /// Accepted. The caller announces the join.
    ///
    /// `name_conflict` carries the fingerprint of a peer already using this
    /// display name, when there is one, so the UI can warn instead of
    /// presenting two different people under one name.
    Admitted { name_conflict: Option<String> },

    /// Accepted, superseding an earlier connection to the same peer. The caller
    /// closes the previous connection and stays quiet -- this peer already
    /// appears in the room, so announcing the join again would be wrong.
    Superseded,

    /// Not accepted. The caller closes this connection.
    Rejected(Rejection),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rejection {
    /// Our own key. Happens as a matter of course once gossip echoes our entry
    /// back to us, and also if a user points `--connect` at themselves.
    SelfConnection,
    /// A second connection to a peer we already hold, which lost the tiebreak.
    DuplicateConnection,
}

pub struct PeerRegistry {
    me: Vec<u8>,
    peers: HashMap<Vec<u8>, Peer>,
}

impl PeerRegistry {
    pub fn new(my_public_key: Vec<u8>) -> Self {
        PeerRegistry {
            me: my_public_key,
            peers: HashMap::new(),
        }
    }

    /// Decide what to do with a connection whose handshake just completed.
    ///
    /// `inbound` says whether the peer dialled us; it is only consulted when
    /// two connections exist for one peer.
    pub fn admit(&mut self, peer: Peer, inbound: bool) -> Admission {
        if peer.public_key == self.me {
            return Admission::Rejected(Rejection::SelfConnection);
        }

        if self.peers.contains_key(&peer.public_key) {
            // Two connections for one peer: a simultaneous dial. Exactly one
            // must go, and both nodes have to choose the same one.
            return if self.connection_wins(&peer.public_key, inbound) {
                self.peers.insert(peer.public_key.clone(), peer);
                Admission::Superseded
            } else {
                Admission::Rejected(Rejection::DuplicateConnection)
            };
        }

        let name_conflict = self
            .peers
            .values()
            .find(|existing| existing.name == peer.name)
            .map(|existing| existing.fingerprint.clone());

        self.peers.insert(peer.public_key.clone(), peer);
        Admission::Admitted { name_conflict }
    }

    /// Which of two connections to the same peer survives.
    ///
    /// The surviving connection is the one dialled by the peer with the smaller
    /// public key. Both nodes know both keys once the handshake is done, so
    /// each reaches this answer alone and they always agree -- no negotiation,
    /// and no round trip during which a third connection could appear.
    ///
    /// Deliberately not enforced before dialling, even though gossip carries
    /// public keys and a node could simply decline to dial anyone below itself.
    /// That would strand any pair reachable in only one direction, which is the
    /// ordinary situation when one peer has forwarded a port and the other has
    /// not. Letting both dial and discarding the loser connects whenever any
    /// direction works.
    fn connection_wins(&self, their_key: &[u8], inbound: bool) -> bool {
        let (dialer, other) = if inbound {
            (their_key, self.me.as_slice())
        } else {
            (self.me.as_slice(), their_key)
        };
        dialer < other
    }

    pub fn remove(&mut self, public_key: &[u8]) {
        self.peers.remove(public_key);
    }

    pub fn get(&self, public_key: &[u8]) -> Option<&Peer> {
        self.peers.get(public_key)
    }

    pub fn all(&self) -> impl Iterator<Item = &Peer> {
        self.peers.values()
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Everyone in `gossiped` we hold no connection to and should dial.
    ///
    /// Deduplicated, because nothing stops a peer listing the same entry twice,
    /// and a duplicate here becomes two redundant connections.
    pub fn undialed(&self, gossiped: &[PeerInfo]) -> Vec<PeerInfo> {
        let mut seen: HashSet<&[u8]> = HashSet::new();

        gossiped
            .iter()
            .filter(|info| info.public_key != self.me)
            .filter(|info| !self.peers.contains_key(&info.public_key))
            .filter(|info| seen.insert(&info.public_key))
            .cloned()
            .collect()
    }
}
