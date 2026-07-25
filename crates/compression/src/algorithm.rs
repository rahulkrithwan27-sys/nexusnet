//! Compression algorithms, levels, and errors.

use std::fmt;
use std::result::Result as StdResult;
use std::str::FromStr;

/// A specialized [`Result`](std::result::Result) for compression operations.
pub type Result<T> = StdResult<T, Error>;

/// An error produced while compressing or decompressing a payload.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Compression failed inside the codec.
    #[error("failed to compress with {algorithm}: {reason}")]
    Compress {
        /// The algorithm that was attempted.
        algorithm: Algorithm,
        /// The underlying reason reported by the codec.
        reason: String,
    },

    /// Decompression failed, usually because the input is corrupt or truncated.
    #[error("failed to decompress {algorithm}: {reason}")]
    Decompress {
        /// The algorithm that was attempted.
        algorithm: Algorithm,
        /// The underlying reason reported by the codec.
        reason: String,
    },

    /// Decompressed output exceeded the caller's limit.
    ///
    /// This is the defense against a decompression bomb: a small input that
    /// expands to an enormous output. The limit is enforced *during*
    /// decompression, so the oversized data is never fully materialized.
    #[error("decompressed output exceeds maximum of {max} bytes")]
    OutputTooLarge {
        /// The configured maximum output length.
        max: usize,
    },

    /// An algorithm identifier was not recognized.
    #[error("unknown compression algorithm: {value}")]
    UnknownAlgorithm {
        /// The unrecognized identifier.
        value: String,
    },

    /// The requested algorithm exists but was not compiled into this build.
    #[error("algorithm {algorithm} is not enabled; rebuild with its cargo feature")]
    AlgorithmUnavailable {
        /// The algorithm that is compiled out.
        algorithm: Algorithm,
    },
}

/// A compression algorithm.
///
/// The discriminants are stable and travel on the wire so a peer knows how to
/// decompress what it received; they must not be reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(u8)]
pub enum Algorithm {
    /// No compression. Always available.
    None = 0x00,
    /// DEFLATE in a gzip container. Ubiquitous and widely interoperable.
    Gzip = 0x01,
    /// Raw DEFLATE, without the gzip header and trailer.
    Deflate = 0x02,
    /// Brotli. Best ratio of the pure-Rust codecs, slower to compress.
    Brotli = 0x03,
    /// Zstandard. Best speed-to-ratio balance; requires a C toolchain.
    Zstd = 0x04,
}

impl Algorithm {
    /// The algorithm used when a peer expresses no preference.
    pub const DEFAULT: Self = Self::Gzip;

    /// Returns the stable wire discriminant.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parses an algorithm from its wire discriminant.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownAlgorithm`] if the byte is not a defined
    /// algorithm.
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0x00 => Ok(Self::None),
            0x01 => Ok(Self::Gzip),
            0x02 => Ok(Self::Deflate),
            0x03 => Ok(Self::Brotli),
            0x04 => Ok(Self::Zstd),
            other => Err(Error::UnknownAlgorithm {
                value: format!("{other:#04x}"),
            }),
        }
    }

    /// Returns `true` if this algorithm is compiled into the current build.
    #[must_use]
    pub const fn is_available(self) -> bool {
        match self {
            Self::None => true,
            Self::Gzip | Self::Deflate => cfg!(feature = "gzip"),
            Self::Brotli => cfg!(feature = "brotli"),
            Self::Zstd => cfg!(feature = "zstd"),
        }
    }

    /// Returns `true` if this algorithm needs a C toolchain to build.
    ///
    /// Only [`Algorithm::Zstd`] does; the rest are pure Rust, which keeps the
    /// default build working everywhere including WebAssembly.
    #[must_use]
    pub const fn requires_c_toolchain(self) -> bool {
        matches!(self, Self::Zstd)
    }

    /// Returns every algorithm this build was compiled with.
    #[must_use]
    pub fn available() -> Vec<Self> {
        [
            Self::None,
            Self::Gzip,
            Self::Deflate,
            Self::Brotli,
            Self::Zstd,
        ]
        .into_iter()
        .filter(|a| a.is_available())
        .collect()
    }

    /// Returns an error unless this algorithm is compiled in.
    pub(crate) fn ensure_available(self) -> Result<()> {
        if self.is_available() {
            Ok(())
        } else {
            Err(Error::AlgorithmUnavailable { algorithm: self })
        }
    }
}

impl Default for Algorithm {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::None => "none",
            Self::Gzip => "gzip",
            Self::Deflate => "deflate",
            Self::Brotli => "brotli",
            Self::Zstd => "zstd",
        };
        f.write_str(name)
    }
}

impl FromStr for Algorithm {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "none" | "identity" => Ok(Self::None),
            "gzip" | "gz" => Ok(Self::Gzip),
            "deflate" => Ok(Self::Deflate),
            "brotli" | "br" => Ok(Self::Brotli),
            "zstd" | "zstandard" => Ok(Self::Zstd),
            other => Err(Error::UnknownAlgorithm {
                value: other.to_owned(),
            }),
        }
    }
}

/// A normalized compression level.
///
/// Codecs disagree about what their level numbers mean: gzip accepts 0–9,
/// Brotli 0–11, and Zstd roughly 1–22. [`Level`] is an abstract 0–100 scale
/// that each backend maps onto its own range, so callers can express intent
/// ("fast", "small") without memorizing three different scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(u8);

impl Level {
    /// Prioritizes speed over ratio.
    pub const FAST: Self = Self(15);
    /// A balanced default suitable for most traffic.
    pub const BALANCED: Self = Self(50);
    /// Prioritizes ratio over speed.
    pub const BEST: Self = Self(90);

    /// Creates a level from a 0–100 scale, clamping out-of-range input.
    ///
    /// Clamping rather than erroring keeps configuration forgiving: a level of
    /// 150 means "as small as possible", which is unambiguous.
    #[must_use]
    pub const fn new(value: u8) -> Self {
        Self(if value > 100 { 100 } else { value })
    }

    /// Returns the abstract level on the 0–100 scale.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Maps this level onto an inclusive backend range.
    ///
    /// Uses `u32` arithmetic internally so the multiplication cannot overflow.
    pub(crate) const fn scale_to(self, min: i32, max: i32) -> i32 {
        let span = max - min;
        // self.0 is 0..=100, so this product is at most 100 * span.
        min + (span * self.0 as i32) / 100
    }
}

impl Default for Level {
    fn default() -> Self {
        Self::BALANCED
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminants_are_stable() {
        assert_eq!(Algorithm::None.as_u8(), 0x00);
        assert_eq!(Algorithm::Gzip.as_u8(), 0x01);
        assert_eq!(Algorithm::Deflate.as_u8(), 0x02);
        assert_eq!(Algorithm::Brotli.as_u8(), 0x03);
        assert_eq!(Algorithm::Zstd.as_u8(), 0x04);
    }

    #[test]
    fn algorithms_roundtrip_through_u8() {
        for algorithm in [
            Algorithm::None,
            Algorithm::Gzip,
            Algorithm::Deflate,
            Algorithm::Brotli,
            Algorithm::Zstd,
        ] {
            assert_eq!(
                Algorithm::from_u8(algorithm.as_u8()).expect("known algorithm"),
                algorithm
            );
        }
    }

    #[test]
    fn unknown_discriminant_is_rejected() {
        assert!(matches!(
            Algorithm::from_u8(0x7F),
            Err(Error::UnknownAlgorithm { .. })
        ));
    }

    #[test]
    fn algorithms_parse_from_names_and_aliases() {
        assert_eq!("GZIP".parse::<Algorithm>().expect("known"), Algorithm::Gzip);
        assert_eq!("br".parse::<Algorithm>().expect("known"), Algorithm::Brotli);
        assert_eq!(
            "identity".parse::<Algorithm>().expect("known"),
            Algorithm::None
        );
        assert!("lzma".parse::<Algorithm>().is_err());
    }

    #[test]
    fn none_is_always_available() {
        assert!(Algorithm::None.is_available());
        assert!(Algorithm::available().contains(&Algorithm::None));
    }

    #[test]
    fn only_zstd_needs_a_c_toolchain() {
        assert!(Algorithm::Zstd.requires_c_toolchain());
        for algorithm in [
            Algorithm::None,
            Algorithm::Gzip,
            Algorithm::Deflate,
            Algorithm::Brotli,
        ] {
            assert!(!algorithm.requires_c_toolchain());
        }
    }

    #[test]
    fn levels_clamp_to_the_scale() {
        assert_eq!(Level::new(200).get(), 100);
        assert_eq!(Level::new(0).get(), 0);
        assert_eq!(Level::default(), Level::BALANCED);
        assert!(Level::FAST < Level::BALANCED);
        assert!(Level::BALANCED < Level::BEST);
    }

    #[test]
    fn level_scaling_covers_backend_ranges() {
        assert_eq!(Level::new(0).scale_to(0, 9), 0);
        assert_eq!(Level::new(100).scale_to(0, 9), 9);
        assert_eq!(Level::new(100).scale_to(1, 22), 22);
        assert_eq!(Level::new(0).scale_to(1, 22), 1);

        // Intermediate levels stay inside the range.
        for raw in 0..=100_u8 {
            let scaled = Level::new(raw).scale_to(0, 11);
            assert!((0..=11).contains(&scaled), "level {raw} mapped to {scaled}");
        }
    }
}
