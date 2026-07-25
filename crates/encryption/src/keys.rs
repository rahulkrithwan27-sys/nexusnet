//! Key material and nonce sequencing.
//!
//! ## The two ways to destroy an AEAD
//!
//! Authenticated encryption is unforgiving in exactly two places, and both are
//! handled here rather than left to callers:
//!
//! 1. **Nonce reuse.** Encrypting two different messages with the same key and
//!    nonce does not merely leak the plaintexts — for the constructions used
//!    here it also leaks the authentication key, letting an attacker forge
//!    arbitrary messages. [`NonceSequence`] therefore counts, never repeats, and
//!    refuses to continue when the counter is exhausted rather than wrapping.
//!
//! 2. **Key material outliving its use.** A key sitting in freed memory can be
//!    recovered from a core dump or a reused page. [`Key`] zeroes itself on
//!    drop.
//!
//! Neither is a hypothetical. Nonce reuse in particular has broken real
//! deployments repeatedly, which is why the API here makes a nonce something you
//! are *given* rather than something you choose.

use std::fmt;

use zeroize::Zeroize;

use crate::error::{Error, Result};

/// The size of a symmetric key, in bytes.
///
/// Both supported ciphers take 256-bit keys.
pub const KEY_LEN: usize = 32;

/// The size of a nonce, in bytes.
///
/// Both supported ciphers take 96-bit nonces.
pub const NONCE_LEN: usize = 12;

/// The size of an authentication tag, in bytes.
pub const TAG_LEN: usize = 16;

/// A symmetric key.
///
/// The bytes are zeroed when the key is dropped, and neither [`Debug`] nor
/// [`Display`](fmt::Display) reveal them — a key logged by accident is a key
/// disclosed.
#[derive(Clone)]
pub struct Key {
    bytes: [u8; KEY_LEN],
}

impl Drop for Key {
    /// Zeroes the key material.
    ///
    /// Written by hand rather than derived: the derive macro is a separate
    /// proc-macro crate, and a cryptographic component is exactly where a
    /// smaller dependency surface is worth a few lines of code.
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl Key {
    /// Creates a key from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self { bytes }
    }

    /// Creates a key from a slice.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidKeyLength`] if the slice is not [`KEY_LEN`]
    /// bytes. Silently padding or truncating would produce a key that works
    /// locally and fails to interoperate, or worse, one with far less entropy
    /// than it appears to have.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let bytes: [u8; KEY_LEN] = bytes.try_into().map_err(|_| Error::InvalidKeyLength {
            expected: KEY_LEN,
            actual: bytes.len(),
        })?;

        Ok(Self { bytes })
    }

    /// Generates a key from the operating system's random source.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RandomSource`] if the system source fails. This is not
    /// recoverable by retrying, and generating a key from a weaker source would
    /// defeat the purpose entirely.
    pub fn generate() -> Result<Self> {
        use rand_core::{OsRng, RngCore};

        let mut bytes = [0_u8; KEY_LEN];
        OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|error| Error::RandomSource {
                reason: error.to_string(),
            })?;

        Ok(Self { bytes })
    }

    /// Returns the key bytes.
    ///
    /// Handle the result carefully: copying it into a longer-lived buffer
    /// defeats the zeroing this type performs.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.bytes
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately opaque: a key that reaches a log file is compromised.
        f.write_str("Key(<redacted>)")
    }
}

impl PartialEq for Key {
    /// Compares in constant time.
    ///
    /// A byte-by-byte comparison that returns early leaks how much of the key
    /// matched, which is enough to recover it one byte at a time.
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;

        self.bytes.ct_eq(&other.bytes).into()
    }
}

impl Eq for Key {}

/// Which direction a key protects.
///
/// Each direction gets its own key, derived from the same shared secret. This
/// prevents reflection: without it, an attacker could replay a message the
/// client sent back to the client, and it would decrypt and authenticate as
/// though the server had sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Direction {
    /// Traffic from the client to the server.
    ClientToServer,
    /// Traffic from the server to the client.
    ServerToClient,
}

impl Direction {
    /// Returns the opposite direction.
    #[must_use]
    pub const fn peer(self) -> Self {
        match self {
            Self::ClientToServer => Self::ServerToClient,
            Self::ServerToClient => Self::ClientToServer,
        }
    }

    /// Returns the HKDF info string that separates this direction's key.
    #[must_use]
    pub const fn label(self) -> &'static [u8] {
        match self {
            Self::ClientToServer => b"nexusnet v1 client-to-server",
            Self::ServerToClient => b"nexusnet v1 server-to-client",
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ClientToServer => "client-to-server",
            Self::ServerToClient => "server-to-client",
        };
        f.write_str(name)
    }
}

/// A nonce, produced only by a [`NonceSequence`].
///
/// There is deliberately no way to construct one from arbitrary bytes on the
/// sending side. A nonce a caller chooses is a nonce a caller can repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nonce {
    bytes: [u8; NONCE_LEN],
}

impl Nonce {
    /// Returns the nonce bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; NONCE_LEN] {
        &self.bytes
    }

    /// Reconstructs the nonce a peer used, for decryption only.
    ///
    /// Receiving is safe: the nonce arrives with the message, and using the
    /// wrong one simply fails to authenticate.
    #[must_use]
    pub const fn from_counter(counter: u64) -> Self {
        Self::of(counter)
    }

    /// Builds the nonce for a counter value.
    ///
    /// The counter occupies the low 8 bytes big-endian; the leading 4 are zero.
    /// Any injective mapping would do — what matters is that distinct counters
    /// give distinct nonces.
    const fn of(counter: u64) -> Self {
        let c = counter.to_be_bytes();
        Self {
            bytes: [0, 0, 0, 0, c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]],
        }
    }
}

/// Issues never-repeating nonces for one key.
///
/// # Examples
///
/// ```
/// use nexusnet_encryption::NonceSequence;
///
/// let mut sequence = NonceSequence::new();
///
/// let first = sequence.next_nonce().expect("not exhausted");
/// let second = sequence.next_nonce().expect("not exhausted");
///
/// assert_ne!(first, second, "a nonce must never repeat under one key");
/// assert_eq!(sequence.counter(), 2);
/// ```
#[derive(Debug, Clone, Default)]
pub struct NonceSequence {
    counter: u64,
    exhausted: bool,
}

/// How many messages one key may protect before it must be rotated.
///
/// Far below the counter's actual range. The limit exists so rotation happens
/// on a schedule rather than at the cliff edge, and so a bug that burns nonces
/// unexpectedly fast surfaces as a clean error rather than a wrap.
pub const MAX_MESSAGES_PER_KEY: u64 = 1 << 48;

impl NonceSequence {
    /// Creates a sequence starting at zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            counter: 0,
            exhausted: false,
        }
    }

    /// Returns how many nonces have been issued.
    #[must_use]
    pub const fn counter(&self) -> u64 {
        self.counter
    }

    /// Returns `true` once the sequence refuses to issue more.
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Returns `true` when the key should be rotated soon.
    ///
    /// True from three quarters of the budget, leaving room to rotate in an
    /// orderly way rather than in a panic.
    #[must_use]
    pub const fn should_rotate(&self) -> bool {
        self.counter >= (MAX_MESSAGES_PER_KEY / 4) * 3
    }

    /// Issues the next nonce.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NonceExhausted`] once the budget is spent. Failing to
    /// send is a bad outcome; reusing a nonce is a catastrophic one, so this
    /// refuses rather than wraps.
    pub fn next_nonce(&mut self) -> Result<Nonce> {
        if self.exhausted || self.counter >= MAX_MESSAGES_PER_KEY {
            self.exhausted = true;
            return Err(Error::NonceExhausted {
                messages: self.counter,
            });
        }

        let nonce = Nonce::of(self.counter);
        self.counter += 1;

        Ok(nonce)
    }

    /// Resets the sequence, as after installing a fresh key.
    ///
    /// Calling this without also changing the key reuses every nonce, so it is
    /// only correct as part of rotation.
    pub fn reset(&mut self) {
        self.counter = 0;
        self.exhausted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_through_bytes() {
        let key = Key::from_bytes([3; KEY_LEN]);
        assert_eq!(key.as_bytes(), &[3; KEY_LEN]);

        let from_slice = Key::from_slice(&[3; KEY_LEN]).expect("correct length");
        assert_eq!(key, from_slice);
    }

    #[test]
    fn a_wrong_length_key_is_refused() {
        let error = Key::from_slice(&[0; 16]).expect_err("too short");

        assert!(
            matches!(
                error,
                Error::InvalidKeyLength {
                    expected: 32,
                    actual: 16
                }
            ),
            "padding silently would produce a key with half the entropy it claims"
        );
        assert!(Key::from_slice(&[0; 64]).is_err(), "too long is also wrong");
    }

    #[test]
    fn generated_keys_differ() {
        let first = Key::generate().expect("system randomness works");
        let second = Key::generate().expect("system randomness works");

        assert_ne!(first, second, "two generated keys must not collide");
        assert_ne!(
            first.as_bytes(),
            &[0; KEY_LEN],
            "an all-zero key means the random source failed silently"
        );
    }

    #[test]
    fn keys_do_not_leak_through_debug() {
        let key = Key::from_bytes([0xAB; KEY_LEN]);
        let rendered = format!("{key:?}");

        assert!(!rendered.contains("ab"), "a logged key is a disclosed key");
        assert!(!rendered.contains("171"));
        assert_eq!(rendered, "Key(<redacted>)");
    }

    #[test]
    fn directions_are_distinct_and_reversible() {
        assert_eq!(Direction::ClientToServer.peer(), Direction::ServerToClient);
        assert_eq!(
            Direction::ServerToClient.peer().peer(),
            Direction::ServerToClient
        );
        assert_ne!(
            Direction::ClientToServer.label(),
            Direction::ServerToClient.label(),
            "identical labels would derive identical keys and allow reflection"
        );
    }

    #[test]
    fn nonces_never_repeat() {
        let mut sequence = NonceSequence::new();
        let mut seen = std::collections::HashSet::new();

        for _ in 0..10_000 {
            let nonce = sequence.next_nonce().expect("not exhausted");
            assert!(
                seen.insert(*nonce.as_bytes()),
                "a repeated nonce leaks the authentication key"
            );
        }
    }

    #[test]
    fn nonces_track_the_counter() {
        let mut sequence = NonceSequence::new();

        let first = sequence.next_nonce().expect("not exhausted");
        assert_eq!(first, Nonce::from_counter(0));
        assert_eq!(sequence.counter(), 1);

        let second = sequence.next_nonce().expect("not exhausted");
        assert_eq!(second, Nonce::from_counter(1));
    }

    #[test]
    fn an_exhausted_sequence_refuses_rather_than_wrapping() {
        let mut sequence = NonceSequence::new();
        sequence.counter = MAX_MESSAGES_PER_KEY;

        let error = sequence.next_nonce().expect_err("budget spent");
        assert!(matches!(error, Error::NonceExhausted { .. }));
        assert!(sequence.is_exhausted());

        // And it stays refused; no retry can make it safe.
        assert!(sequence.next_nonce().is_err());
    }

    #[test]
    fn rotation_is_advised_before_the_cliff() {
        let mut sequence = NonceSequence::new();
        assert!(!sequence.should_rotate());

        sequence.counter = (MAX_MESSAGES_PER_KEY / 4) * 3;
        assert!(
            sequence.should_rotate(),
            "rotation should be advised with budget to spare"
        );
        assert!(
            !sequence.is_exhausted(),
            "and well before sending becomes impossible"
        );
    }

    #[test]
    fn resetting_restarts_the_sequence() {
        let mut sequence = NonceSequence::new();
        for _ in 0..100 {
            sequence.next_nonce().expect("not exhausted");
        }

        sequence.reset();
        assert_eq!(sequence.counter(), 0);
        assert_eq!(
            sequence.next_nonce().expect("not exhausted"),
            Nonce::from_counter(0)
        );
    }

    #[test]
    fn key_comparison_is_constant_time() {
        // Correctness is what a test can check; the timing property comes from
        // `subtle`. Verify at least that comparison behaves as expected.
        let key = Key::from_bytes([1; KEY_LEN]);
        let same = Key::from_bytes([1; KEY_LEN]);

        let mut differing = [1_u8; KEY_LEN];
        differing[KEY_LEN - 1] = 2;

        assert_eq!(key, same);
        assert_ne!(key, Key::from_bytes(differing));
    }
}
