//! Authenticated encryption.
//!
//! Both supported ciphers are AEAD constructions: they encrypt and authenticate
//! in one pass, and they authenticate *associated data* that travels in the
//! clear. That second property is what stops an attacker moving a valid
//! ciphertext to a different stream or connection — the header is bound to the
//! payload even though it is not encrypted.

use std::fmt;

use aes_gcm::Aes256Gcm;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;

use crate::error::{Error, Result};
use crate::keys::{Key, Nonce, KEY_LEN, TAG_LEN};

/// Which authenticated cipher to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(u8)]
pub enum Cipher {
    /// `ChaCha20-Poly1305`.
    ///
    /// The default. Fast and constant-time in software on any CPU, which
    /// matters because AES without hardware support is both slower and harder
    /// to implement without timing leaks.
    ChaCha20Poly1305 = 0x01,
    /// `AES-256-GCM`.
    ///
    /// Faster where the CPU provides AES instructions, and often required for
    /// compliance reasons.
    Aes256Gcm = 0x02,
}

impl Cipher {
    /// The cipher used when nothing else is specified.
    pub const DEFAULT: Self = Self::ChaCha20Poly1305;

    /// Every supported cipher.
    pub const ALL: [Self; 2] = [Self::ChaCha20Poly1305, Self::Aes256Gcm];

    /// Returns the wire discriminant.
    #[must_use]
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// Returns the cipher for a wire discriminant.
    #[must_use]
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            0x01 => Some(Self::ChaCha20Poly1305),
            0x02 => Some(Self::Aes256Gcm),
            _ => None,
        }
    }

    /// Returns the key length in bytes.
    #[must_use]
    pub const fn key_len(self) -> usize {
        KEY_LEN
    }

    /// Returns the authentication tag length in bytes.
    #[must_use]
    pub const fn tag_len(self) -> usize {
        TAG_LEN
    }

    /// Returns how many bytes protecting a payload of `len` will produce.
    #[must_use]
    pub const fn ciphertext_len(self, len: usize) -> usize {
        len + TAG_LEN
    }
}

impl Default for Cipher {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for Cipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ChaCha20Poly1305 => "ChaCha20-Poly1305",
            Self::Aes256Gcm => "AES-256-GCM",
        };
        f.write_str(name)
    }
}

/// Encrypts and decrypts with one key.
///
/// # Examples
///
/// ```
/// use nexusnet_encryption::{Cipher, Key, NonceSequence, Sealer};
///
/// let key = Key::from_bytes([42; 32]);
/// let sealer = Sealer::new(Cipher::default(), key);
/// let mut nonces = NonceSequence::new();
///
/// let nonce = nonces.next_nonce()?;
/// let sealed = sealer.seal(nonce, b"the payload", b"the header")?;
///
/// // The ciphertext reveals nothing and carries a tag.
/// assert_ne!(sealed, b"the payload");
/// assert_eq!(sealed.len(), b"the payload".len() + 16);
///
/// let opened = sealer.open(nonce, &sealed, b"the header")?;
/// assert_eq!(opened, b"the payload");
/// # Ok::<(), nexusnet_encryption::Error>(())
/// ```
pub struct Sealer {
    cipher: Cipher,
    key: Key,
}

impl Sealer {
    /// Creates a sealer for `cipher` using `key`.
    #[must_use]
    pub const fn new(cipher: Cipher, key: Key) -> Self {
        Self { cipher, key }
    }

    /// Returns the cipher in use.
    #[must_use]
    pub const fn cipher(&self) -> Cipher {
        self.cipher
    }

    /// Encrypts `plaintext`, authenticating `associated_data`.
    ///
    /// The associated data is not encrypted but is covered by the tag, so it
    /// cannot be altered without detection. Pass the frame header here: it
    /// binds the ciphertext to its stream and position, so a valid payload
    /// cannot be replayed onto a different stream.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DecryptionFailed`] only from [`open`](Self::open);
    /// encryption fails only if the cipher rejects the input, which for these
    /// constructions means an implausibly large payload.
    pub fn seal(&self, nonce: Nonce, plaintext: &[u8], associated_data: &[u8]) -> Result<Vec<u8>> {
        let payload = Payload {
            msg: plaintext,
            aad: associated_data,
        };

        match self.cipher {
            Cipher::ChaCha20Poly1305 => {
                let cipher =
                    ChaCha20Poly1305::new_from_slice(self.key.as_bytes()).map_err(|_| {
                        Error::InvalidKeyLength {
                            expected: KEY_LEN,
                            actual: self.key.as_bytes().len(),
                        }
                    })?;
                cipher
                    .encrypt(nonce.as_bytes().into(), payload)
                    .map_err(|_| Error::DecryptionFailed)
            }
            Cipher::Aes256Gcm => {
                let cipher = Aes256Gcm::new_from_slice(self.key.as_bytes()).map_err(|_| {
                    Error::InvalidKeyLength {
                        expected: KEY_LEN,
                        actual: self.key.as_bytes().len(),
                    }
                })?;
                cipher
                    .encrypt(nonce.as_bytes().into(), payload)
                    .map_err(|_| Error::DecryptionFailed)
            }
        }
    }

    /// Decrypts `ciphertext`, verifying `associated_data`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CiphertextTooShort`] if the input cannot contain a tag,
    /// and [`Error::DecryptionFailed`] if authentication fails for any reason —
    /// wrong key, wrong nonce, altered ciphertext, or altered associated data.
    /// The reasons are deliberately indistinguishable: telling them apart gives
    /// an attacker an oracle.
    pub fn open(&self, nonce: Nonce, ciphertext: &[u8], associated_data: &[u8]) -> Result<Vec<u8>> {
        if ciphertext.len() < TAG_LEN {
            return Err(Error::CiphertextTooShort {
                len: ciphertext.len(),
                tag_len: TAG_LEN,
            });
        }

        let payload = Payload {
            msg: ciphertext,
            aad: associated_data,
        };

        match self.cipher {
            Cipher::ChaCha20Poly1305 => {
                let cipher = ChaCha20Poly1305::new_from_slice(self.key.as_bytes())
                    .map_err(|_| Error::DecryptionFailed)?;
                cipher
                    .decrypt(nonce.as_bytes().into(), payload)
                    .map_err(|_| Error::DecryptionFailed)
            }
            Cipher::Aes256Gcm => {
                let cipher = Aes256Gcm::new_from_slice(self.key.as_bytes())
                    .map_err(|_| Error::DecryptionFailed)?;
                cipher
                    .decrypt(nonce.as_bytes().into(), payload)
                    .map_err(|_| Error::DecryptionFailed)
            }
        }
    }
}

impl fmt::Debug for Sealer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The key must not appear, even indirectly.
        f.debug_struct("Sealer")
            .field("cipher", &self.cipher)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::NonceSequence;

    fn sealer(cipher: Cipher) -> Sealer {
        Sealer::new(cipher, Key::from_bytes([9; KEY_LEN]))
    }

    #[test]
    fn every_cipher_round_trips() {
        for cipher in Cipher::ALL {
            let sealer = sealer(cipher);
            let mut nonces = NonceSequence::new();
            let nonce = nonces.next_nonce().expect("fresh");

            let sealed = sealer
                .seal(nonce, b"attack at dawn", b"header")
                .expect("seals");
            let opened = sealer.open(nonce, &sealed, b"header").expect("opens");

            assert_eq!(opened, b"attack at dawn", "{cipher} failed to round trip");
        }
    }

    #[test]
    fn ciphertext_does_not_reveal_plaintext() {
        let sealer = sealer(Cipher::default());
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");

        let plaintext = b"aaaaaaaaaaaaaaaaaaaaaaaa";
        let sealed = sealer.seal(nonce, plaintext, b"").expect("seals");

        assert_ne!(&sealed[..plaintext.len()], &plaintext[..]);
        assert_eq!(sealed.len(), plaintext.len() + TAG_LEN);
    }

    #[test]
    fn the_same_plaintext_encrypts_differently_each_time() {
        let sealer = sealer(Cipher::default());
        let mut nonces = NonceSequence::new();

        let first = sealer
            .seal(nonces.next_nonce().expect("fresh"), b"identical", b"")
            .expect("seals");
        let second = sealer
            .seal(nonces.next_nonce().expect("fresh"), b"identical", b"")
            .expect("seals");

        assert_ne!(
            first, second,
            "identical ciphertexts would reveal that two messages match"
        );
    }

    #[test]
    fn a_tampered_ciphertext_is_rejected() {
        let sealer = sealer(Cipher::default());
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");

        let mut sealed = sealer.seal(nonce, b"transfer 100", b"").expect("seals");
        sealed[2] ^= 0x01;

        let error = sealer.open(nonce, &sealed, b"").expect_err("tampered");
        assert_eq!(error, Error::DecryptionFailed);
        assert!(error.indicates_tampering());
    }

    #[test]
    fn a_tampered_tag_is_rejected() {
        let sealer = sealer(Cipher::default());
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");

        let mut sealed = sealer.seal(nonce, b"payload", b"").expect("seals");
        let last = sealed.len() - 1;
        sealed[last] ^= 0x80;

        assert!(sealer.open(nonce, &sealed, b"").is_err());
    }

    #[test]
    fn altered_associated_data_is_rejected() {
        let sealer = sealer(Cipher::default());
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");

        let sealed = sealer.seal(nonce, b"payload", b"stream=1").expect("seals");

        assert!(
            sealer.open(nonce, &sealed, b"stream=2").is_err(),
            "binding the header is what stops a payload being moved between streams"
        );
        assert!(sealer.open(nonce, &sealed, b"").is_err());
    }

    #[test]
    fn the_wrong_nonce_fails_to_open() {
        let sealer = sealer(Cipher::default());
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");
        let other = nonces.next_nonce().expect("fresh");

        let sealed = sealer.seal(nonce, b"payload", b"").expect("seals");
        assert!(sealer.open(other, &sealed, b"").is_err());
    }

    #[test]
    fn the_wrong_key_fails_to_open() {
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");

        let sealed = sealer(Cipher::default())
            .seal(nonce, b"payload", b"")
            .expect("seals");

        let other = Sealer::new(Cipher::default(), Key::from_bytes([8; KEY_LEN]));
        assert!(other.open(nonce, &sealed, b"").is_err());
    }

    #[test]
    fn ciphers_do_not_interoperate() {
        let key = Key::from_bytes([5; KEY_LEN]);
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");

        let sealed = Sealer::new(Cipher::ChaCha20Poly1305, key.clone())
            .seal(nonce, b"payload", b"")
            .expect("seals");

        let wrong = Sealer::new(Cipher::Aes256Gcm, key);
        assert!(
            wrong.open(nonce, &sealed, b"").is_err(),
            "a cipher mismatch must fail cleanly, not produce garbage"
        );
    }

    #[test]
    fn a_truncated_ciphertext_is_rejected_early() {
        let sealer = sealer(Cipher::default());
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");

        let error = sealer.open(nonce, &[0; 4], b"").expect_err("too short");
        assert!(matches!(error, Error::CiphertextTooShort { len: 4, .. }));
    }

    #[test]
    fn an_empty_payload_is_still_authenticated() {
        let sealer = sealer(Cipher::default());
        let mut nonces = NonceSequence::new();
        let nonce = nonces.next_nonce().expect("fresh");

        let sealed = sealer.seal(nonce, b"", b"header").expect("seals");
        assert_eq!(sealed.len(), TAG_LEN, "an empty message is all tag");

        assert_eq!(sealer.open(nonce, &sealed, b"header").expect("opens"), b"");
        assert!(sealer.open(nonce, &sealed, b"other").is_err());
    }

    #[test]
    fn wire_discriminants_round_trip() {
        for cipher in Cipher::ALL {
            assert_eq!(Cipher::from_id(cipher.id()), Some(cipher));
        }
        assert_eq!(Cipher::from_id(0xFF), None);
    }

    #[test]
    fn ciphertext_length_is_predictable() {
        let cipher = Cipher::default();
        assert_eq!(cipher.ciphertext_len(0), TAG_LEN);
        assert_eq!(cipher.ciphertext_len(100), 100 + TAG_LEN);
    }

    #[test]
    fn a_sealer_does_not_leak_its_key_through_debug() {
        let rendered = format!("{:?}", sealer(Cipher::default()));
        assert!(!rendered.contains('9'));
        assert!(rendered.contains("Sealer"));
    }
}
