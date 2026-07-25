//! # nexusnet-encryption
//!
//! Authenticated encryption for NexusNet.
//!
//! ## What's here
//!
//! * [`SessionCrypto`] — the intended entry point: derives a directional key,
//!   seals and opens messages, and rejects replays.
//! * [`Sealer`] and [`Cipher`] — the AEAD layer, if you need it directly.
//! * [`Key`], [`NonceSequence`] — key material that zeroes itself, and nonces
//!   that cannot repeat.
//!
//! ## Example
//!
//! ```
//! use nexusnet_encryption::{Cipher, Direction, SessionCrypto};
//!
//! let mut sender = SessionCrypto::new(
//!     b"shared secret", b"session-1", Direction::ClientToServer, Cipher::default(),
//! )?;
//! let mut receiver = SessionCrypto::new(
//!     b"shared secret", b"session-1", Direction::ClientToServer, Cipher::default(),
//! )?;
//!
//! let sealed = sender.seal(b"attack at dawn", b"frame-header")?;
//! assert_ne!(sealed.ciphertext, b"attack at dawn");
//!
//! assert_eq!(receiver.open(&sealed, b"frame-header")?, b"attack at dawn");
//!
//! // The same message a second time is a replay, and is refused.
//! assert!(receiver.open(&sealed, b"frame-header").is_err());
//! # Ok::<(), nexusnet_encryption::Error>(())
//! ```
//!
//! ## Four things this crate takes out of the caller's hands
//!
//! Cryptography fails in specific, well-known ways. Each of these has broken
//! real deployments, so none of them is left as an exercise:
//!
//! **Nonce reuse.** Encrypting two messages with one key and nonce leaks the
//! authentication key, not merely the plaintexts. Nonces come only from
//! [`NonceSequence`], which counts and refuses to wrap — sending stops before a
//! nonce repeats.
//!
//! **Reflection.** Each direction gets its own key, derived from the shared
//! secret with a direction-specific label. Without that, a recorded message
//! replayed back at its sender would authenticate as though the peer had sent
//! it.
//!
//! **Replay.** Authentication proves *who* wrote a message, never *when*.
//! [`ReplayFilter`] tracks counters over a window, so reordering is tolerated
//! but a duplicate is refused. Authentication is verified before the filter is
//! touched, so a forgery cannot poison it into dropping genuine traffic.
//!
//! **Error oracles.** Decryption failures are indistinguishable from one
//! another. Reporting *why* a message failed — bad tag, bad padding, unexpected
//! nonce — hands an attacker an oracle, and that class of leak has broken
//! protocols repeatedly.
//!
//! ## What this crate does not do
//!
//! There is **no key exchange here**. [`SessionCrypto`] starts from a shared
//! secret that something else established. Establishing it over an untrusted
//! network needs an authenticated exchange, which arrives with the QUIC and TLS
//! work; until then, this layer protects sessions whose secret came from
//! elsewhere.
//!
//! Nor is this a substitute for TLS on a public network. It is the primitive
//! layer NexusNet's own protocol is built on.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod cipher;
mod error;
mod keys;
mod session;

pub use crate::cipher::{Cipher, Sealer};
pub use crate::error::{Error, Result};
pub use crate::keys::{
    Direction, Key, Nonce, NonceSequence, KEY_LEN, MAX_MESSAGES_PER_KEY, NONCE_LEN, TAG_LEN,
};
pub use crate::session::{derive_key, ReplayFilter, SealedMessage, SessionCrypto, REPLAY_WINDOW};
