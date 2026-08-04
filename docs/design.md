# E2EE Terminal Chat — Design

**Status:** approved · supersedes the earlier server-relay design
**Stack:** Rust 1.94 · tokio · cursive · serde_json · x25519-dalek · chacha20poly1305

## What this is

A retro terminal chat application where every participant runs the same
program. Nodes connect directly to one another, discover the rest of the room
by gossip, and encrypt every message end-to-end. There is no server.

## Why peer-to-peer

The original design used a relay server that also distributed public keys.
That makes the server a trusted third party: it can hand Alice its own key
while claiming the key is Bob's, do the same to Bob, and transparently read
everything. The encryption stays mathematically perfect and is bypassed
completely.

Removing the server removes that attack. Peers exchange keys directly and
verify them out of band, so no infrastructure has to be trusted.

## The constraint this imposes

Two hosts behind separate home NATs cannot open a TCP connection to each
other. Working around that requires STUN/TURN infrastructure, which would
reintroduce the servers we just removed. So this app requires that peers be
mutually reachable:

| Scenario | Works |
|---|---|
| Same LAN or WiFi | yes |
| Over a VPN (Tailscale, WireGuard) | yes |
| Internet with one peer port-forwarded | yes |
| Internet, both behind NAT, no setup | no |

This is accepted, not worked around. NAT traversal is explicitly out of scope.

## Architecture

Every node is symmetric — there are no client or server roles. A node:

- listens on a TCP port for inbound connections
- dials outbound to peers it learns about
- holds one long-lived X25519 identity keypair
- keeps an in-memory registry of connected peers
- renders a cursive UI

```
        alice ─────────── bob
           \             /
            \           /
             \         /
              +-carol-+
```

Because the mesh is *full* — every peer holds a direct connection to every
other peer — **no message forwarding is needed.** A node encrypts each message
separately for each peer and writes it straight down that peer's connection.
There is no relaying, no hop count, no duplicate suppression.

Gossip is used only to *discover* peers, never to route messages.

## Sender identity comes from the connection

In the relay design, `NetworkMessage` carried a `from: String` that the server
forwarded and the recipient had to trust. It was spoofable.

Direct connections remove the field entirely. Each connection completes a
handshake that establishes exactly one peer identity, so an incoming chat
frame's sender is already known — it cannot be claimed. The plaintext
`Message` still carries a `from` for display, but it now sits *inside* the
ciphertext, which means it is authenticated by the AEAD and cross-checked
against the connection's identity on arrival.

## Admission

Encryption settles what a peer can read. It says nothing about who may join, so
without something further, anyone able to reach the listening port becomes a
peer — tolerable only on a network where that is already impossible, which
rules out the port forwarding people will reasonably want to do.

A room therefore has a code, held by everyone in it. During the handshake each
side sends an HMAC over both public keys and a fresh nonce from each, keyed by
the code. The code itself never crosses the wire, and the proof is worthless on
any other connection: replaying it into a later one fails on the nonces, and
echoing it back at its sender fails because the two sides order the inputs
differently.

The code is generated at full entropy rather than chosen, so a plain hash
suffices to derive the key -- there is no dictionary to run against it.

One is created when none exists rather than running without. An open room is
not a state anyone would choose deliberately, so it is not one that can be
reached by forgetting a flag.

This answers "are you invited". Fingerprints answer "are you who you claim".
Both questions still need asking, and a leaked code makes the second one matter
more rather than less.

## Trust model

Keys are accepted on first use (TOFU) and each is rendered as a short
fingerprint in the UI:

```
bob joined  ·  4F2A-91C3-7E08-BD56
```

Users compare fingerprints out of band — read them aloud, send them over a
channel an attacker does not control. Matching fingerprints prove there is no
machine-in-the-middle. This is the same mechanism as Signal's safety numbers
and SSH's host key prompt.

Peers are tracked by **public key**, not by name. Names are user-chosen and
carry no authority; two peers may pick the same one, and the UI distinguishes
them by fingerprint. If a name reappears in a session bound to a different
key, the UI warns.

### Known limitations (v1)

Stated plainly rather than left implicit:

- **No record of who you have met.** The keypair persists across runs, so a
  fingerprint now identifies the same person over time. Nothing remembers which
  fingerprint went with which name previously, though, so a familiar name
  arriving under a new key is not flagged. An SSH-style known-hosts file would
  close this.
- **No forward secrecy.** A pair's shared secret is derived from long-term
  keys, so compromising one later exposes past messages. Fixing this needs a
  ratchet, which is well beyond scope.
- **Metadata is not hidden.** Peers see who is in the room and when messages
  are sent. Only content is protected.
- **No persistence.** Messages live in memory for the session only.

## Cryptography

Implemented in `src/crypto.rs`, complete and tested.

X25519 key agreement between the sender's long-term secret and the recipient's
public key, hashed with SHA-256 into an XChaCha20-Poly1305 key. This is NaCl's
`crypto_box` construction.

- **Authentication** — the sender's long-term key is an input, so a ciphertext
  that opens correctly could only have been produced by its holder.
- **Direction binding** — the sender's public key is authenticated as
  associated data. Both directions of a pair derive the same shared secret, so
  without this an attacker could reflect a message back at its author and have
  it appear to come from the peer.
- **Deniability** — either party could have produced any message, so neither
  can prove authorship to a third party. Deliberate, and what chat wants.
- **192-bit nonces** — the per-pair key is fixed, so randomly generated nonces
  need to be wide enough that collisions are not a concern.

## Wire protocol

Newline-delimited JSON over TCP. `serde_json` escapes newlines inside strings,
so a bare `\n` unambiguously terminates a frame.

TCP is a byte stream: a single `read()` may return half a frame, or two frames
at once. Framing is therefore mandatory, not optional. Reads are length-capped
so a peer cannot exhaust memory by sending an endless line.

```rust
#[serde(tag = "type")]
enum Frame {
    Hello { name: String, public_key: Vec<u8>, listen_addr: String },
    Peers { peers: Vec<PeerInfo> },
    Chat  { ciphertext: Vec<u8>, nonce: [u8; 24] },
}
```

- **Hello** — first frame in both directions on every connection. Carries the
  identity and the address at which this node accepts connections, so the peer
  can pass it on via gossip.
- **Peers** — sent after the handshake and whenever membership changes. The
  recipient dials any peer it does not already hold a connection to.
- **Chat** — one encrypted message, already sealed for this specific peer.

## Simultaneous dial

Once gossip is running, Alice and Bob will sometimes dial each other at the
same moment and end up with two connections for one pair. Resolved by a rule
both sides evaluate identically:

> The surviving connection is the one dialled by the peer with the
> lexicographically smaller public key.

Because both nodes know both keys after the handshake, they reach the same
conclusion without negotiating, and exactly one connection is dropped.

## Modules

| File | Responsibility |
|---|---|
| `crypto.rs` | Key generation, seal/open, fingerprints. No I/O. |
| `messages.rs` | The plaintext `Message` type. |
| `protocol.rs` | `Frame` enum, newline framing, encode/decode. No I/O. |
| `peers.rs` | Peer registry, TOFU checks, dial tiebreak. No I/O. |
| `node.rs` | Listener, dialer, handshake, connection lifecycle. |
| `ui.rs` | Cursive setup, retro theme, input handling. |
| `main.rs` | CLI parsing, wiring. |

The first four are pure logic and fully unit-testable without sockets, which
is where most of the behaviour lives and most of the bugs would otherwise
hide.

## Interface

```
ee2e-chat --name alice --listen 0.0.0.0:9999
ee2e-chat --name bob --listen 0.0.0.0:9998 --connect 192.168.1.42:9999
```

`--connect` bootstraps against one known peer; gossip discovers the rest.

## UI

Cursive with a green-on-blue retro palette: scrolling message view, input
line, and a peer sidebar showing names with fingerprints.

Cursive owns a blocking event loop while the network runs on tokio, so the two
communicate through cursive's `CbSink`, which lets async tasks post callbacks
into the UI thread. This boundary is the trickiest integration in the project
and is deliberately called out early.
