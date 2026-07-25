# nexusnet-encryption

Authenticated encryption for NexusNet.

## What's here

- **`SessionCrypto`** — the entry point: derives a directional key, seals and
  opens messages, rejects replays.
- **`Sealer` / `Cipher`** — the AEAD layer directly, if you need it.
- **`Key` / `NonceSequence`** — key material that zeroes itself, and nonces that
  cannot repeat.

```rust
use nexusnet_encryption::{Cipher, Direction, SessionCrypto};

let mut sender = SessionCrypto::new(
    b"shared secret", b"session-1", Direction::ClientToServer, Cipher::default(),
)?;
let mut receiver = SessionCrypto::new(
    b"shared secret", b"session-1", Direction::ClientToServer, Cipher::default(),
)?;

let sealed = sender.seal(b"attack at dawn", b"frame-header")?;
assert_eq!(receiver.open(&sealed, b"frame-header")?, b"attack at dawn");

// Sending the same message again is a replay, and is refused.
assert!(receiver.open(&sealed, b"frame-header").is_err());
# Ok::<(), nexusnet_encryption::Error>(())
```

## Four failures taken out of the caller's hands

Cryptography fails in specific, well-documented ways. Each of these has broken
real deployments, so none is left as an exercise:

**Nonce reuse.** Encrypting two messages under one key and nonce doesn't merely
leak the plaintexts — for these constructions it leaks the *authentication key*,
letting an attacker forge anything. Nonces come only from `NonceSequence`, which
counts and refuses to wrap. Sending stops before a nonce repeats: failing to
send is bad, reusing a nonce is catastrophic.

**Reflection.** Each direction derives its own key via HKDF with a
direction-specific label. Share one key both ways and an attacker can replay a
client's message back at the client, where it authenticates as though the server
sent it.

**Replay.** Authentication proves *who* wrote a message, never *when*. A
recorded message replays perfectly. `ReplayFilter` tracks counters over a
64-message window, tolerating the reordering a datagram transport produces while
refusing duplicates. Authentication is verified **before** the filter is
touched — otherwise a forged high counter could poison the window and cause
legitimate traffic to be dropped.

**Error oracles.** All decryption failures are indistinguishable. Reporting
*why* — bad tag, bad padding, unexpected nonce — hands an attacker an oracle,
and that class of leak has broken protocols repeatedly.

## Associated data binds the header

`seal` authenticates associated data that travels in the clear. Pass the frame
header: it binds the ciphertext to its stream and position, so a valid payload
can't be lifted onto a different stream. There's a test asserting a payload
sealed with `stream=1` fails to open as `stream=2`.

## Ciphers

`ChaCha20-Poly1305` is the default — fast and constant-time in software on any
CPU, which matters because AES without hardware support is both slower and
harder to implement without timing leaks. `AES-256-GCM` is available where the
CPU accelerates it or compliance requires it.

## What this does not do

**There is no key exchange here.** `SessionCrypto` starts from a shared secret
that something else established. Establishing one over an untrusted network
needs an authenticated exchange, which arrives with the QUIC/TLS work. Until
then this protects sessions whose secret came from elsewhere — and it is not a
substitute for TLS on a public network.

## Dependencies

Deliberately minimal for a security component: the RustCrypto AEAD and HKDF
crates, `subtle` for constant-time comparison, and `zeroize`. The `zeroize`
derive macro is *not* used — the `Drop` impl is four lines by hand, which is
worth avoiding an extra proc-macro crate in the supply chain.

## Testing

```bash
cargo test -p nexusnet-encryption
```

43 tests, including tamper detection on ciphertext and tag, associated-data
binding, cross-cipher rejection, replay and reordering, forgery not poisoning
the replay filter, and keys not leaking through `Debug`.

## Status

Implemented in **Phase 4**. Key exchange and QUIC remain. See
[`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
