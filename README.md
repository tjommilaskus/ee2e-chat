# TIO CHAT

End-to-end encrypted terminal chat with no server. Every participant runs the
same program, nodes connect directly to one another, and the rest of the room
is discovered by gossip.

```
┌──────────────────────────────────────────────────────────────┐
│           ⌐ TIO CHAT ¬  User: Sue  ⌐ 16:24:59 ¬              │
└──────────────────────────────────────────────────────────────┘
┌─────────────────────────┤ Messages ├─────────────────────────┐
│[you are Sue · 29C5-2015-DE08-4502]                           │
│[Bob joined the chat · 4DC2-D703-300C-4017]                   │
│┌[16:24:56]                                                   │
│└─ Bob ▶ hi Sue                                               │
└──────────────────────────────────────────────────────────────┘
┌─────────────────────────┤ Message ├──────────────────────────┐
│______________________________________________________________│
└──────────────────────────────────────────────────────────────┘
```

## Installing

```
./install.sh
```

Builds from source and installs to `~/.local/bin`, so it never needs root.
`--prefix DIR` puts it elsewhere and `--uninstall` removes it again. Rust is
the only requirement, and the script says how to get it if it is missing.

To build without installing anything:

```
cargo build --release && ./target/release/ee2e-chat
```

## Running it

Start it with no arguments and it asks who you are and who to connect to:

```
┌────────────────┤ ⌐ TIO CHAT ¬ ├────────────────┐
│ Your name                                      │
│ Sue___________________________________________ │
│                                                │
│ Listen on                                      │
│ 0.0.0.0:9999__________________________________ │
│                                                │
│ Connect to a peer  (blank to wait for others)  │
│ 127.0.0.1:9001________________________________ │
│                                                │
│                               <Connect> <Quit> │
└────────────────────────────────────────────────┘
```

Leave *Connect to* blank on the first machine and let the others dial it.

Everything on that screen can be passed on the command line instead, which
skips it entirely:

```
ee2e-chat --name alice --listen 0.0.0.0:9001
ee2e-chat --name bob   --listen 0.0.0.0:9002 --connect 192.168.1.42:9001
ee2e-chat --name carol --listen 0.0.0.0:9003 --connect 192.168.1.43:9002
```

Carol only needs to know *one* address. Gossip introduces her to alice, and the
three connect directly to each other. Anything given partially — `--listen`
without `--name`, say — prefills the setup screen rather than being ignored.

| | |
|---|---|
| `Enter` | send |
| `PgUp` / `PgDn` | scroll; reaching the bottom resumes following |
| `Esc` / `Ctrl+C` | quit |
| `/peers` | who is here, with fingerprints |
| `/clear` | empty the window |
| `/help` `/quit` | |

`--plain` gives line-by-line output instead of the full interface, which is
easier to pipe somewhere. It has no setup screen to ask on, so it needs
`--name`.

## Your identity

Your keypair is stored at `$XDG_CONFIG_HOME/ee2e-chat/identity` (usually
`~/.config/ee2e-chat/identity`), created on first run and readable only by you.
It is what keeps your fingerprint the same from one session to the next, so
someone who verified it last week still recognises you today.

- `--identity PATH` keeps it somewhere else, which is also how to run two
  distinct identities on one machine.
- `--ephemeral` uses a throwaway key instead. Your fingerprint will match no
  previous session, so nobody can recognise you.

If the file is unreadable or damaged the program stops rather than quietly
generating a replacement: a changed fingerprint is exactly the signal that
means *someone is impersonating them*, so a disk problem must never be able to
imitate an attack. Delete the file to start over deliberately.

## Verifying who you are talking to

Encryption alone does not tell you *whose* key you are encrypting to. On first
contact an attacker positioned between two peers could hand each of them its
own key and read everything, and the mathematics would be perfect throughout.

The defence is to compare fingerprints out of band — read them aloud, or send
them over a channel the attacker does not control:

```
/peers
   alice (you) · 29C5-2015-DE08-4502
   bob         · 4DC2-D703-300C-4017
```

If bob's screen shows the same two values, nobody is in the middle. This is the
same mechanism as Signal's safety numbers and SSH's host key prompt.

Display names carry no authority whatsoever — anyone can choose any name. If
two identities appear under one name, or someone takes yours, the interface
says so and shows both fingerprints. **The fingerprint is the identity.**

## How it works

Every node is symmetric: it listens, it dials, and it holds one long-lived
X25519 keypair. Because the mesh is full — every peer connected directly to
every other — no message is ever forwarded. A node encrypts each message
separately for each recipient and writes it straight down that connection.
Gossip is used only to discover peers, never to route messages.

| | |
|---|---|
| `crypto.rs` | key generation, seal/open, fingerprints |
| `messages.rs` | the plaintext message type |
| `protocol.rs` | wire frames and newline framing |
| `peers.rs` | peer registry, name clashes, dial collisions |
| `identity.rs` | reading and writing the stored keypair |
| `node.rs` | listening, dialling, handshakes, gossip |
| `ui.rs` | the interface |

The first four touch neither disk nor network, which is what makes the awkward
parts — collision resolution in particular — testable without opening a socket.

### Cryptography

X25519 key agreement between the sender's long-term secret and the recipient's
public key, hashed with SHA-256 into an XChaCha20-Poly1305 key. This is NaCl's
`crypto_box` construction.

- **Authenticated.** The sender's long-term key is an input, so a ciphertext
  that opens correctly could only have been produced by its holder.
- **Direction-bound.** The sender's public key is authenticated as associated
  data. Both directions of a pair derive the same shared secret, so without
  this an attacker could reflect a message back at its author and have it
  appear to come from the peer.
- **Deniable.** Either party could have produced any message, so neither can
  prove authorship to anyone else. Deliberate, and what chat wants.

## Limitations

Stated plainly rather than left to be discovered.

- **Peers must be able to reach each other.** Two hosts behind separate home
  NATs cannot open a TCP connection, and fixing that needs STUN/TURN
  infrastructure — servers, which is the thing this design removes. Works on a
  LAN, over a VPN such as Tailscale, or with one peer port-forwarded.
- **No record of who you have met.** Identities persist, but nothing remembers
  which fingerprint belonged to which name last time, so a familiar name
  appearing with a new key passes unremarked. Verification has to be repeated
  by hand each time you care.
- **No forward secrecy.** A pair's shared secret comes from long-term keys, so
  compromising one later exposes past messages.
- **Metadata is not hidden.** Peers see who is present and when messages are
  sent. Only content is protected.
- **Messages are not stored.** They live in memory for the session and are gone
  when you quit. Only the keypair is written to disk.

This was written to learn how the pieces fit together. It has not been audited,
and the limitations above are the ones that are *known*.

## Development

```
cargo test      # 111 tests
cargo clippy --all-targets
```

`docs/design.md` covers the architecture and why it is peer-to-peer;
`docs/plan.md` tracks the stages it was built in.
