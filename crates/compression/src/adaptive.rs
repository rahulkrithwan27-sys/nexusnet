//! Adaptive compression policy.
//!
//! Compressing unconditionally is a mistake on a real network. Two cases lose:
//!
//! * **Small payloads.** Every codec adds framing overhead. Gzip's header and
//!   trailer alone exceed the size of a short control message, so the "compressed"
//!   form is bigger than the original.
//! * **Already-compressed payloads.** Ciphertext, JPEG, and Zstd output are
//!   effectively random to a second compressor. It burns CPU and returns
//!   something slightly larger.
//!
//! [`Compressor`] applies a policy that skips both cases and, critically,
//! verifies the result actually shrank before accepting it.

use bytes::Bytes;

use crate::algorithm::{Algorithm, Level, Result};
use crate::codec;

/// The default minimum payload size worth compressing.
///
/// Below this, codec overhead reliably outweighs any saving.
pub const DEFAULT_MIN_SIZE: usize = 128;

/// The default maximum accepted compression ratio.
///
/// A result must be at most this fraction of the original to be kept. `0.95`
/// means "must save at least 5%", which filters out payloads that are already
/// compressed while still accepting genuinely useful gains.
pub const DEFAULT_MAX_RATIO: f64 = 0.95;

/// The default decompression output limit: 64 MiB.
pub const DEFAULT_MAX_OUTPUT: usize = 64 * 1024 * 1024;

/// Why a payload was left uncompressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// The payload was shorter than the configured minimum.
    TooSmall,
    /// Compression ran but did not shrink the payload enough to be worth it.
    NotWorthwhile,
    /// The configured algorithm was [`Algorithm::None`].
    Disabled,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::TooSmall => "payload below minimum size",
            Self::NotWorthwhile => "compression did not shrink the payload enough",
            Self::Disabled => "compression disabled",
        };
        f.write_str(text)
    }
}

/// The result of an adaptive compression attempt.
///
/// Map this onto the protocol's compressed flag: set the flag for
/// [`Outcome::Compressed`], leave it clear for [`Outcome::Skipped`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The payload was compressed and is smaller than the original.
    Compressed {
        /// The algorithm used, which the peer needs in order to decompress.
        algorithm: Algorithm,
        /// The compressed bytes.
        data: Bytes,
        /// The original, uncompressed length.
        original_len: usize,
    },
    /// The payload was left as-is.
    Skipped {
        /// Why compression was skipped.
        reason: SkipReason,
        /// The original bytes, unchanged.
        data: Bytes,
    },
}

impl Outcome {
    /// Returns the bytes to transmit, compressed or not.
    #[must_use]
    pub fn data(&self) -> &Bytes {
        match self {
            Self::Compressed { data, .. } | Self::Skipped { data, .. } => data,
        }
    }

    /// Consumes the outcome, returning the bytes to transmit.
    #[must_use]
    pub fn into_data(self) -> Bytes {
        match self {
            Self::Compressed { data, .. } | Self::Skipped { data, .. } => data,
        }
    }

    /// Returns `true` if compression was applied.
    ///
    /// This is exactly the value the protocol's compressed flag should take.
    #[must_use]
    pub const fn is_compressed(&self) -> bool {
        matches!(self, Self::Compressed { .. })
    }

    /// Returns the algorithm used, or [`Algorithm::None`] if skipped.
    #[must_use]
    pub const fn algorithm(&self) -> Algorithm {
        match self {
            Self::Compressed { algorithm, .. } => *algorithm,
            Self::Skipped { .. } => Algorithm::None,
        }
    }

    /// Returns the compressed size divided by the original size.
    ///
    /// Returns `1.0` for a skipped payload, and for an empty input, since
    /// nothing was saved.
    #[must_use]
    pub fn ratio(&self) -> f64 {
        match self {
            Self::Compressed {
                data, original_len, ..
            } => {
                if *original_len == 0 {
                    1.0
                } else {
                    data.len() as f64 / *original_len as f64
                }
            }
            Self::Skipped { .. } => 1.0,
        }
    }

    /// Returns the bytes saved relative to the original.
    #[must_use]
    pub fn bytes_saved(&self) -> usize {
        match self {
            Self::Compressed {
                data, original_len, ..
            } => original_len.saturating_sub(data.len()),
            Self::Skipped { .. } => 0,
        }
    }
}

/// An adaptive compressor.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "gzip")] {
/// use nexusnet_compression::{Algorithm, Compressor};
///
/// let compressor = Compressor::new(Algorithm::Gzip);
///
/// // Highly repetitive data compresses well.
/// let repetitive = vec![b'a'; 4096];
/// let outcome = compressor.compress(&repetitive).expect("compresses");
/// assert!(outcome.is_compressed());
/// assert!(outcome.ratio() < 0.1);
///
/// // A short payload is left alone: codec overhead would make it bigger.
/// let tiny = b"ack";
/// let outcome = compressor.compress(tiny).expect("no codec failure");
/// assert!(!outcome.is_compressed());
/// assert_eq!(outcome.data().as_ref(), tiny);
/// # }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Compressor {
    algorithm: Algorithm,
    level: Level,
    min_size: usize,
    max_ratio: f64,
    max_output: usize,
}

impl Compressor {
    /// Creates a compressor with default policy for `algorithm`.
    #[must_use]
    pub const fn new(algorithm: Algorithm) -> Self {
        Self {
            algorithm,
            level: Level::BALANCED,
            min_size: DEFAULT_MIN_SIZE,
            max_ratio: DEFAULT_MAX_RATIO,
            max_output: DEFAULT_MAX_OUTPUT,
        }
    }

    /// Sets the compression level.
    #[must_use]
    pub const fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Sets the minimum payload size worth compressing.
    #[must_use]
    pub const fn with_min_size(mut self, min_size: usize) -> Self {
        self.min_size = min_size;
        self
    }

    /// Sets the maximum accepted ratio of compressed to original size.
    ///
    /// Values are clamped to `0.0..=1.0`; a ratio above 1.0 would accept
    /// results that grew, which is never useful.
    #[must_use]
    pub fn with_max_ratio(mut self, max_ratio: f64) -> Self {
        self.max_ratio = max_ratio.clamp(0.0, 1.0);
        self
    }

    /// Sets the maximum output size accepted when decompressing.
    #[must_use]
    pub const fn with_max_output(mut self, max_output: usize) -> Self {
        self.max_output = max_output;
        self
    }

    /// Returns the configured algorithm.
    #[must_use]
    pub const fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Returns the configured level.
    #[must_use]
    pub const fn level(&self) -> Level {
        self.level
    }

    /// Compresses `input` if the policy says it is worthwhile.
    ///
    /// The decision is made by *measuring*, not guessing: payloads above the
    /// size threshold are actually compressed, and the result is kept only if
    /// it beats [`with_max_ratio`](Self::with_max_ratio). That correctly
    /// handles already-compressed data without needing to detect it.
    ///
    /// # Errors
    ///
    /// Returns an error only if the codec itself fails; a payload that simply
    /// is not worth compressing yields [`Outcome::Skipped`], not an error.
    pub fn compress(&self, input: &[u8]) -> Result<Outcome> {
        if self.algorithm == Algorithm::None {
            return Ok(Outcome::Skipped {
                reason: SkipReason::Disabled,
                data: Bytes::copy_from_slice(input),
            });
        }

        if input.len() < self.min_size {
            return Ok(Outcome::Skipped {
                reason: SkipReason::TooSmall,
                data: Bytes::copy_from_slice(input),
            });
        }

        let compressed = codec::compress(self.algorithm, self.level, input)?;

        let ratio = compressed.len() as f64 / input.len() as f64;
        if ratio > self.max_ratio {
            return Ok(Outcome::Skipped {
                reason: SkipReason::NotWorthwhile,
                data: Bytes::copy_from_slice(input),
            });
        }

        Ok(Outcome::Compressed {
            algorithm: self.algorithm,
            data: compressed,
            original_len: input.len(),
        })
    }

    /// Decompresses `input`, which was compressed with `algorithm`.
    ///
    /// Enforces the configured output limit, so a decompression bomb is
    /// rejected before its output is materialized.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OutputTooLarge`](crate::Error::OutputTooLarge) if the
    /// output would exceed the limit, or a decode error if the input is corrupt.
    pub fn decompress(&self, algorithm: Algorithm, input: &[u8]) -> Result<Bytes> {
        codec::decompress(algorithm, input, self.max_output)
    }

    /// Reverses an [`Outcome`], recovering the original bytes.
    ///
    /// # Errors
    ///
    /// See [`decompress`](Self::decompress).
    pub fn restore(&self, outcome: &Outcome) -> Result<Bytes> {
        match outcome {
            Outcome::Compressed {
                algorithm, data, ..
            } => self.decompress(*algorithm, data),
            Outcome::Skipped { data, .. } => Ok(data.clone()),
        }
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new(Algorithm::DEFAULT)
    }
}
