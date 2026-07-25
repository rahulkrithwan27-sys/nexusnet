//! Errors raised by the encryption layer.

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Something went wrong protecting or unprotecting a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Decryption failed.
    ///
    /// Deliberately carries no detail. Distinguishing "the tag was wrong" from
    /// "the padding was wrong" from "the nonce was unexpected" gives an
    /// attacker an oracle, and oracles of exactly this kind have broken real
    /// protocols. To a caller there is only one fact worth knowing: this
    /// message is not authentic.
    #[error("decryption failed: the message is not authentic")]
    DecryptionFailed,

    /// A key was not the expected length.
    #[error("expected a {expected}-byte key, got {actual} bytes")]
    InvalidKeyLength {
        /// The required length.
        expected: usize,
        /// The length supplied.
        actual: usize,
    },

    /// A nonce was not the expected length.
    #[error("expected a {expected}-byte nonce, got {actual} bytes")]
    InvalidNonceLength {
        /// The required length.
        expected: usize,
        /// The length supplied.
        actual: usize,
    },

    /// The ciphertext is too short to contain an authentication tag.
    #[error("ciphertext of {len} bytes is shorter than the {tag_len}-byte tag")]
    CiphertextTooShort {
        /// The length received.
        len: usize,
        /// The tag length required.
        tag_len: usize,
    },

    /// The key has protected as many messages as it safely can.
    ///
    /// Sending stops rather than reusing a nonce, which would leak the
    /// authentication key.
    #[error("nonce space exhausted after {messages} messages: rotate the key")]
    NonceExhausted {
        /// How many messages this key protected.
        messages: u64,
    },

    /// A replayed or badly out-of-order message was rejected.
    #[error("message {counter} was rejected as a replay")]
    Replay {
        /// The counter carried by the offending message.
        counter: u64,
    },

    /// The system random source failed.
    #[error("the system random source failed: {reason}")]
    RandomSource {
        /// What the source reported.
        reason: String,
    },

    /// Key derivation failed.
    #[error("key derivation failed: {reason}")]
    KeyDerivation {
        /// What went wrong.
        reason: String,
    },
}

impl Error {
    /// Returns `true` if the error indicates tampering rather than a mistake.
    ///
    /// Lets a caller distinguish an attack in progress — which usually warrants
    /// closing the connection — from a local misconfiguration.
    #[must_use]
    pub const fn indicates_tampering(&self) -> bool {
        matches!(self, Self::DecryptionFailed | Self::Replay { .. })
    }
}
