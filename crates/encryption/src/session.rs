//! Session keys, replay protection, and the encrypted-message API.

use std::collections::VecDeque;

use hkdf::Hkdf;
use sha2::Sha256;

use crate::cipher::{Cipher, Sealer};
use crate::error::{Error, Result};
use crate::keys::{Direction, Key, Nonce, NonceSequence, KEY_LEN};

/// Derives a directional key from a shared secret.
///
/// Uses HKDF-SHA256 with a per-direction info string, so the two directions get
/// independent keys from the same secret. Deriving one key and using it both
/// ways would let an attacker reflect a message back at its sender, where it
/// would decrypt and authenticate as though the peer had sent it.
///
/// # Errors
///
/// Returns [`Error::KeyDerivation`] if expansion fails, which for a 32-byte
/// output cannot happen in practice but is reported rather than unwrapped.
///
/// # Examples
///
/// ```
/// use nexusnet_encryption::{derive_key, Direction};
///
/// let outbound = derive_key(b"shared secret", b"session-42", Direction::ClientToServer)?;
/// let inbound = derive_key(b"shared secret", b"session-42", Direction::ServerToClient)?;
///
/// assert_ne!(outbound, inbound, "each direction gets its own key");
/// # Ok::<(), nexusnet_encryption::Error>(())
/// ```
pub fn derive_key(shared_secret: &[u8], salt: &[u8], direction: Direction) -> Result<Key> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), shared_secret);

    let mut bytes = [0_u8; KEY_LEN];
    hkdf.expand(direction.label(), &mut bytes)
        .map_err(|error| Error::KeyDerivation {
            reason: error.to_string(),
        })?;

    Ok(Key::from_bytes(bytes))
}

/// How many message counters behind the highest seen are still accepted.
///
/// A datagram transport reorders, so requiring strictly increasing counters
/// would drop legitimate traffic. The window accepts recent stragglers while
/// still refusing anything already seen.
pub const REPLAY_WINDOW: u64 = 64;

/// Rejects replayed and unacceptably old messages.
///
/// Authentication proves a message was created by the key holder. It says
/// nothing about *when* — an attacker who records a valid message can send it
/// again, and it will authenticate perfectly. Only counter tracking stops that.
#[derive(Debug, Clone, Default)]
pub struct ReplayFilter {
    highest: u64,
    seen: VecDeque<u64>,
    started: bool,
    rejected: u64,
}

impl ReplayFilter {
    /// Creates an empty filter.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            highest: 0,
            seen: VecDeque::new(),
            started: false,
            rejected: 0,
        }
    }

    /// Returns the highest counter accepted so far.
    #[must_use]
    pub const fn highest(&self) -> u64 {
        self.highest
    }

    /// Returns how many messages have been rejected.
    #[must_use]
    pub const fn rejected(&self) -> u64 {
        self.rejected
    }

    /// Accepts `counter` if it has not been seen and is not too old.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Replay`] for a duplicate or a counter older than the
    /// window.
    pub fn accept(&mut self, counter: u64) -> Result<()> {
        if !self.started {
            self.started = true;
            self.highest = counter;
            self.seen.push_back(counter);
            return Ok(());
        }

        if self.seen.contains(&counter) {
            self.rejected += 1;
            return Err(Error::Replay { counter });
        }

        if counter + REPLAY_WINDOW < self.highest {
            // Too old to prove it is not a replay: the record of whether it was
            // seen has already been discarded.
            self.rejected += 1;
            return Err(Error::Replay { counter });
        }

        self.highest = self.highest.max(counter);
        self.seen.push_back(counter);

        while self.seen.len() > REPLAY_WINDOW as usize {
            self.seen.pop_front();
        }

        Ok(())
    }
}

/// A message ready for the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SealedMessage {
    /// The counter that produced the nonce, needed by the receiver.
    pub counter: u64,
    /// The ciphertext, including its authentication tag.
    pub ciphertext: Vec<u8>,
}

impl SealedMessage {
    /// Returns the total bytes this message puts on the wire.
    ///
    /// The counter travels as 8 bytes alongside the ciphertext.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        self.ciphertext.len() + 8
    }
}

/// Protects one direction of a connection.
///
/// # Examples
///
/// ```
/// use nexusnet_encryption::{Cipher, Direction, SessionCrypto};
///
/// // Both peers derive from the same secret; directions keep them apart.
/// let mut client = SessionCrypto::new(b"shared secret", b"session-1", Direction::ClientToServer, Cipher::default())?;
/// let mut server = SessionCrypto::new(b"shared secret", b"session-1", Direction::ClientToServer, Cipher::default())?;
///
/// let sealed = client.seal(b"hello", b"header")?;
/// let opened = server.open(&sealed, b"header")?;
///
/// assert_eq!(opened, b"hello");
/// # Ok::<(), nexusnet_encryption::Error>(())
/// ```
#[derive(Debug)]
pub struct SessionCrypto {
    sealer: Sealer,
    nonces: NonceSequence,
    replay: ReplayFilter,
    direction: Direction,
    messages_sealed: u64,
    messages_opened: u64,
}

impl SessionCrypto {
    /// Creates crypto state for one direction of a connection.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyDerivation`] if the key cannot be derived.
    pub fn new(
        shared_secret: &[u8],
        salt: &[u8],
        direction: Direction,
        cipher: Cipher,
    ) -> Result<Self> {
        let key = derive_key(shared_secret, salt, direction)?;

        Ok(Self {
            sealer: Sealer::new(cipher, key),
            nonces: NonceSequence::new(),
            replay: ReplayFilter::new(),
            direction,
            messages_sealed: 0,
            messages_opened: 0,
        })
    }

    /// Returns the direction this state protects.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Returns the cipher in use.
    #[must_use]
    pub const fn cipher(&self) -> Cipher {
        self.sealer.cipher()
    }

    /// Returns how many messages have been sealed.
    #[must_use]
    pub const fn messages_sealed(&self) -> u64 {
        self.messages_sealed
    }

    /// Returns how many messages have been opened.
    #[must_use]
    pub const fn messages_opened(&self) -> u64 {
        self.messages_opened
    }

    /// Returns how many messages were rejected as replays.
    #[must_use]
    pub const fn replays_rejected(&self) -> u64 {
        self.replay.rejected()
    }

    /// Returns `true` when the key should be rotated.
    #[must_use]
    pub const fn should_rotate(&self) -> bool {
        self.nonces.should_rotate()
    }

    /// Encrypts a message.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NonceExhausted`] once the key has protected as many
    /// messages as it safely can.
    pub fn seal(&mut self, plaintext: &[u8], associated_data: &[u8]) -> Result<SealedMessage> {
        let counter = self.nonces.counter();
        let nonce = self.nonces.next_nonce()?;
        let ciphertext = self.sealer.seal(nonce, plaintext, associated_data)?;

        self.messages_sealed += 1;

        Ok(SealedMessage {
            counter,
            ciphertext,
        })
    }

    /// Decrypts a message, rejecting replays.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DecryptionFailed`] if the message is not authentic, or
    /// [`Error::Replay`] if it has been seen before. Authentication is checked
    /// **first**: the replay filter must not be updated by a message that
    /// cannot be proven genuine, or an attacker could poison it with forged
    /// counters and cause legitimate messages to be dropped.
    pub fn open(&mut self, message: &SealedMessage, associated_data: &[u8]) -> Result<Vec<u8>> {
        let nonce = Nonce::from_counter(message.counter);
        let plaintext = self
            .sealer
            .open(nonce, &message.ciphertext, associated_data)?;

        self.replay.accept(message.counter)?;
        self.messages_opened += 1;

        Ok(plaintext)
    }

    /// Installs a freshly derived key and restarts the nonce sequence.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyDerivation`] if the new key cannot be derived.
    pub fn rotate(&mut self, shared_secret: &[u8], salt: &[u8]) -> Result<()> {
        let key = derive_key(shared_secret, salt, self.direction)?;

        self.sealer = Sealer::new(self.sealer.cipher(), key);
        // Safe only because the key changed too: resetting alone would reuse
        // every nonce.
        self.nonces.reset();
        self.replay = ReplayFilter::new();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(direction: Direction) -> SessionCrypto {
        SessionCrypto::new(b"shared secret", b"salt", direction, Cipher::default())
            .expect("derives")
    }

    #[test]
    fn directions_derive_different_keys() {
        let outbound = derive_key(b"secret", b"salt", Direction::ClientToServer).expect("derives");
        let inbound = derive_key(b"secret", b"salt", Direction::ServerToClient).expect("derives");

        assert_ne!(
            outbound, inbound,
            "one key both ways would allow a message to be reflected at its sender"
        );
    }

    #[test]
    fn derivation_is_deterministic() {
        let first = derive_key(b"secret", b"salt", Direction::ClientToServer).expect("derives");
        let second = derive_key(b"secret", b"salt", Direction::ClientToServer).expect("derives");

        assert_eq!(first, second, "both peers must arrive at the same key");
    }

    #[test]
    fn a_different_salt_derives_a_different_key() {
        let first =
            derive_key(b"secret", b"session-1", Direction::ClientToServer).expect("derives");
        let second =
            derive_key(b"secret", b"session-2", Direction::ClientToServer).expect("derives");

        assert_ne!(
            first, second,
            "per-session salts keep one session's key useless against another"
        );
    }

    #[test]
    fn a_session_round_trips() {
        let mut sender = session(Direction::ClientToServer);
        let mut receiver = session(Direction::ClientToServer);

        let sealed = sender.seal(b"hello", b"header").expect("seals");
        let opened = receiver.open(&sealed, b"header").expect("opens");

        assert_eq!(opened, b"hello");
        assert_eq!(sender.messages_sealed(), 1);
        assert_eq!(receiver.messages_opened(), 1);
    }

    #[test]
    fn the_opposite_direction_cannot_decrypt() {
        let mut sender = session(Direction::ClientToServer);
        let mut wrong = session(Direction::ServerToClient);

        let sealed = sender.seal(b"hello", b"").expect("seals");
        assert!(
            wrong.open(&sealed, b"").is_err(),
            "reflection protection depends on this failing"
        );
    }

    #[test]
    fn a_replayed_message_is_rejected() {
        let mut sender = session(Direction::ClientToServer);
        let mut receiver = session(Direction::ClientToServer);

        let sealed = sender.seal(b"transfer 100", b"").expect("seals");

        assert!(receiver.open(&sealed, b"").is_ok());
        let error = receiver.open(&sealed, b"").expect_err("replayed");

        assert!(
            matches!(error, Error::Replay { .. }),
            "authentication alone cannot tell a replay from the original"
        );
        assert!(error.indicates_tampering());
        assert_eq!(receiver.replays_rejected(), 1);
    }

    #[test]
    fn reordered_messages_within_the_window_are_accepted() {
        let mut sender = session(Direction::ClientToServer);
        let mut receiver = session(Direction::ClientToServer);

        let messages: Vec<SealedMessage> = (0..10)
            .map(|i| {
                sender
                    .seal(format!("message {i}").as_bytes(), b"")
                    .expect("seals")
            })
            .collect();

        // Deliver out of order, as a datagram transport would.
        for index in [5, 2, 9, 0, 7, 1, 8, 3, 6, 4] {
            receiver
                .open(&messages[index], b"")
                .unwrap_or_else(|error| panic!("message {index} rejected: {error}"));
        }

        assert_eq!(receiver.messages_opened(), 10);
    }

    #[test]
    fn a_very_old_message_is_rejected() {
        let mut filter = ReplayFilter::new();

        filter.accept(1000).expect("first");
        let error = filter.accept(10).expect_err("far too old");

        assert!(
            matches!(error, Error::Replay { .. }),
            "beyond the window we cannot prove it is not a replay"
        );
    }

    #[test]
    fn a_forged_message_does_not_poison_the_replay_filter() {
        let mut sender = session(Direction::ClientToServer);
        let mut receiver = session(Direction::ClientToServer);

        // An attacker asserts a high counter with a bogus ciphertext.
        let forged = SealedMessage {
            counter: 5_000,
            ciphertext: vec![0; 32],
        };
        assert!(receiver.open(&forged, b"").is_err());

        // Genuine traffic must still be accepted: authentication is checked
        // before the filter is touched.
        let genuine = sender.seal(b"legitimate", b"").expect("seals");
        assert!(
            receiver.open(&genuine, b"").is_ok(),
            "a forgery must not be able to cause legitimate messages to drop"
        );
    }

    #[test]
    fn each_message_uses_a_fresh_counter() {
        let mut sender = session(Direction::ClientToServer);

        let first = sender.seal(b"a", b"").expect("seals");
        let second = sender.seal(b"b", b"").expect("seals");

        assert_eq!(first.counter, 0);
        assert_eq!(second.counter, 1);
    }

    #[test]
    fn rotation_installs_a_new_key_and_restarts_counters() {
        let mut sender = session(Direction::ClientToServer);
        let mut receiver = session(Direction::ClientToServer);

        for _ in 0..5 {
            let sealed = sender.seal(b"before", b"").expect("seals");
            receiver.open(&sealed, b"").expect("opens");
        }

        sender.rotate(b"new secret", b"salt-2").expect("rotates");
        receiver.rotate(b"new secret", b"salt-2").expect("rotates");

        let sealed = sender.seal(b"after", b"").expect("seals");
        assert_eq!(sealed.counter, 0, "the nonce sequence restarts");
        assert_eq!(receiver.open(&sealed, b"").expect("opens"), b"after");
    }

    #[test]
    fn a_message_from_before_rotation_no_longer_opens() {
        let mut sender = session(Direction::ClientToServer);
        let mut receiver = session(Direction::ClientToServer);

        let old = sender.seal(b"old traffic", b"").expect("seals");
        receiver.rotate(b"new secret", b"salt-2").expect("rotates");

        assert!(
            receiver.open(&old, b"").is_err(),
            "rotation should retire the old key, not merely supplement it"
        );
    }

    #[test]
    fn wire_length_accounts_for_the_counter() {
        let mut sender = session(Direction::ClientToServer);
        let sealed = sender.seal(b"payload", b"").expect("seals");

        assert_eq!(sealed.wire_len(), sealed.ciphertext.len() + 8);
        assert_eq!(sealed.ciphertext.len(), b"payload".len() + 16);
    }
}
