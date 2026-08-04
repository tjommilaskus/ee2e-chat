# Implementation Plan — P2P E2EE Terminal Chat

Supersedes `~/docs/superpowers/plans/2026-08-04-e2ee-chat-implementation.md`,
which targeted the abandoned server-relay architecture.

**Goal:** a serverless terminal chat where every node connects directly to
every other, discovers the room by gossip, and encrypts each message
end-to-end. See `docs/design.md` for architecture and rationale.

## A note on how this plan is written

Stages 2 and 3 are specified step by step with real code. Stages 4 onward are
specified by *interface and behaviour* rather than line-by-line code.

That is deliberate. The previous plan contained detailed code written from
memory for the `crypto_box` API, and essentially none of it compiled — wrong
type names, wrong method names, a feature flag that was never enabled, and a
nonce size that disagreed with the rest of the plan. Fixing it took longer than
writing it would have from scratch. Detailed code is only worth committing to a
plan when it can be verified soon after being written; beyond that horizon,
interfaces and decisions carry the real value and invented code is a liability.

Each stage is expanded into steps immediately before it is implemented.

## Global constraints

- Rust 1.94 stable, edition 2024
- Every module outside `node.rs`/`ui.rs`/`main.rs` stays free of I/O so it can
  be unit-tested without sockets
- Anything parsing network input returns `Result`; no panics on remote data
- Peers are keyed by public key, never by name
- Messages are in-memory only

---

## Stage 1 — Message types and cryptography ✅ COMPLETE

`Message`, `NetworkMessage`, and an authenticated `crypto` module with 13
passing tests covering impersonation, reflection, tampering, third-party
decryption, and malformed input.

Carried forward as debt into Stage 2:

- `NetworkMessage` is a relay-era type. Direct connections establish sender
  identity during the handshake, so its outer `from` field is obsolete and it
  is replaced by `Frame::Chat`.
- `crypto` has no fingerprint function yet.

---

## Stage 2 — Wire protocol

**Files:** create `src/protocol.rs`, modify `src/crypto.rs`, `src/lib.rs`,
delete `NetworkMessage` from `src/messages.rs`

**Produces:**
- `crypto::fingerprint(public_key: &[u8]) -> String`
- `protocol::Frame` — `Hello`, `Peers`, `Chat`
- `protocol::PeerInfo`
- `Frame::to_line(&self) -> Result<String, ProtocolError>`
- `Frame::from_line(&str) -> Result<Frame, ProtocolError>`
- `protocol::MAX_FRAME_BYTES`

No I/O in this stage. Framing is applied to a socket in Stage 4.

- [ ] **Step 1: Add `fingerprint` to `crypto.rs`**

```rust
/// A short, human-comparable rendering of a public key, for out-of-band
/// verification. Truncating SHA-256 to 64 bits is adequate here: an attacker
/// must find a *second preimage* of a specific displayed fingerprint, and the
/// value is compared by humans who would not read more digits anyway.
pub fn fingerprint(public_key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ee2e-chat v1 fingerprint");
    hasher.update(public_key);
    let digest = hasher.finalize();

    digest[..8]
        .chunks(2)
        .map(|pair| format!("{:02X}{:02X}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join("-")
}
```

- [ ] **Step 2: Write failing tests for `fingerprint`**

Cover: stable across calls for the same key, differs between keys, and matches
the `XXXX-XXXX-XXXX-XXXX` shape.

- [ ] **Step 3: Run tests, confirm they fail, implement, confirm they pass**

- [ ] **Step 4: Write failing tests for `Frame` round-tripping**

Each variant survives `to_line` → `from_line`. Plus the properties that matter
for framing correctness:

- an encoded frame contains exactly one `\n`, at the very end — including when
  the message body itself contains newline characters, which `serde_json`
  escapes rather than emitting raw
- malformed JSON returns `Err`, never panics
- a line over `MAX_FRAME_BYTES` returns `Err`

- [ ] **Step 5: Implement `protocol.rs`**

`Frame` as a `#[serde(tag = "type")]` enum so the JSON is self-describing and
readable while debugging.

- [ ] **Step 6: Delete `NetworkMessage`, update `lib.rs`, run the full suite**

- [ ] **Step 7: Commit**

---

## Stage 3 — Peer registry

**Files:** create `src/peers.rs`

Pure in-memory state with no I/O, which makes the trickiest logic in the
project — TOFU conflicts and simultaneous-dial resolution — testable without
opening a socket.

**Produces:**

```rust
pub struct Peer {
    pub name: String,
    pub public_key: Vec<u8>,
    pub listen_addr: String,
    pub fingerprint: String,
}

pub enum Admission {
    /// First time this key has been seen; caller should announce the join.
    New(Peer),
    /// Already connected on another connection; caller should drop this one.
    Duplicate,
    /// A different key is already using this display name.
    NameConflict { existing_fingerprint: String },
}

pub struct PeerRegistry { /* keyed by public key */ }

impl PeerRegistry {
    pub fn new(my_public_key: Vec<u8>) -> Self;
    pub fn admit(&mut self, peer: Peer, inbound: bool) -> Admission;
    pub fn remove(&mut self, public_key: &[u8]);
    pub fn get(&self, public_key: &[u8]) -> Option<&Peer>;
    pub fn all(&self) -> impl Iterator<Item = &Peer>;
    /// Peers we have learned about but hold no connection to.
    pub fn undialed(&self, known: &[PeerInfo]) -> Vec<PeerInfo>;
}
```

- [ ] **Step 1: Tests for basic admission** — add, look up, remove, list

- [ ] **Step 2: Tests for self-rejection** — a node must never admit its own
      public key, which happens naturally once gossip echoes our own entry back

- [ ] **Step 3: Tests for the simultaneous-dial tiebreak**

The rule from `docs/design.md`: the surviving connection is the one dialled by
the peer with the smaller public key. The test that matters constructs both
sides of one collision and asserts they reach *opposite* conclusions — exactly
one connection survives. Asserting each side in isolation would pass even if
both sides dropped, or both kept.

- [ ] **Step 4: Tests for TOFU name conflicts** — same name, different key,
      returns `NameConflict` carrying the existing fingerprint for display

- [ ] **Step 5: Tests for `undialed`** — filters out already-connected peers
      and ourselves

- [ ] **Step 6: Implement, run the suite, commit**

---

## Stage 4 — Transport: listen, dial, handshake

**Files:** create `src/node.rs`, add `tokio-util` (`codec` feature) and `clap`
(`derive` feature), rewrite `src/main.rs`

Two nodes connect and complete a handshake. No chat yet.

**Key decisions already made:**

- Framing uses `tokio_util::codec::{Framed, LinesCodec}` with
  `LinesCodec::new_with_max_length(MAX_FRAME_BYTES)`. Rolling this by hand with
  `read_until` cannot enforce the cap correctly — the buffer has already grown
  past the limit by the time it can be checked.
- The connection splits into a reader task and a writer task joined by an
  `mpsc` channel, so any part of the program can send to a peer by cloning that
  peer's sender.
- The handshake is symmetric: both sides send `Hello` immediately and wait for
  the peer's. Whoever dialled is irrelevant afterwards.

**Behaviour to verify:** two nodes reach mutual admission; a peer sending a
non-`Hello` first frame is dropped; a peer that disconnects is removed from the
registry; a malformed or oversized frame closes only that connection and leaves
the node running.

---

## Stage 5 — Encrypted chat, headless

Typed lines are sealed once per peer and written to each connection; inbound
`Chat` frames are opened using the connection's established identity. Output is
`println!` — the UI comes next.

**Milestone: two people can hold a real encrypted conversation.**

**Behaviour to verify:** a message arrives intact at every peer; the `from`
inside the decrypted `Message` matches the connection's identity, and a
mismatch is rejected rather than displayed; a frame that fails to open logs a
warning and does not kill the connection.

---

## Stage 6 — Cursive retro UI

Replaces `println!` with a green-on-blue cursive interface: scrolling messages,
input line, peer sidebar with fingerprints.

**The integration risk in this project.** Cursive runs a blocking event loop;
tokio runs async tasks. They communicate through cursive's `CbSink`, which
accepts callbacks from other threads and runs them on the UI thread. Expect the
cursive loop to own the main thread with the tokio runtime alongside it, rather
than the reverse.

---

## Stage 7 — Gossip

`Peers` frames after each handshake and on membership change; recipients dial
anything in `undialed()`. Three nodes started with a single `--connect` between
them converge to a full mesh.

**Behaviour to verify:** convergence from a chain bootstrap; no dial storms; no
duplicate connections once the Stage 3 tiebreak is exercised for real.

---

## Stage 8 — Polish

Fingerprint display and name-conflict warnings in the UI, clean disconnect
handling, `/quit` and `/peers` commands, sensible errors for an unreachable
`--connect`.

---

## Explicitly out of scope

NAT traversal · message persistence · forward secrecy · private one-to-one
messages within a room · identity persistence across runs (a natural next
project, and the change that would make fingerprint verification meaningful
between sessions).
