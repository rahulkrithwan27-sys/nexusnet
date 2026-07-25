//! # nexusnet-serializer
//!
//! Payload serialization for NexusNet: converting application types to and
//! from the bytes carried in a frame payload.
//!
//! This crate is a thin, uniform layer over `serde`. It does not decide *what*
//! to send; it decides how a value becomes bytes, and it makes that choice
//! explicit and negotiable via [`Format`].
//!
//! ## Formats
//!
//! | Format | Feature | Use it for |
//! | ------ | ------- | ---------- |
//! | [`Format::MessagePack`] | `msgpack` (default) | The wire default: compact, binary, schema-free. |
//! | [`Format::Json`] | `json` (default) | Debugging, logs, and interop with HTTP tooling. |
//!
//! MessagePack is the default because it is materially smaller than JSON for
//! the same value and costs no build tooling. JSON is kept for the cases where
//! a human or an external system has to read the payload.
//!
//! ## Example
//!
//! ```
//! # #[cfg(all(feature = "msgpack", feature = "json"))] {
//! use nexusnet_serializer::{decode, encode, Format};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, PartialEq, Serialize, Deserialize)]
//! struct Telemetry {
//!     node: String,
//!     rtt_micros: u32,
//! }
//!
//! let value = Telemetry { node: "edge-1".to_owned(), rtt_micros: 8_400 };
//!
//! let bytes = encode(Format::MessagePack, &value).expect("value serializes");
//! let restored: Telemetry =
//!     decode(Format::MessagePack, &bytes).expect("value deserializes");
//! assert_eq!(restored, value);
//!
//! // The same value in JSON is larger, but readable.
//! let json = encode(Format::Json, &value).expect("value serializes");
//! assert!(json.len() > bytes.len());
//! # }
//! ```
//!
//! ## Size limits
//!
//! [`decode`] accepts any input length. When decoding untrusted bytes, prefer
//! [`decode_with_limit`], which rejects oversized input before handing it to a
//! deserializer that might allocate in proportion to it.
#![cfg_attr(docsrs, feature(doc_cfg))]

// A serializer with no formats compiled in can encode nothing. Fail loudly at
// build time rather than shipping a crate whose every call returns
// `FormatUnavailable`.
#[cfg(not(any(feature = "msgpack", feature = "json")))]
compile_error!(
    "nexusnet-serializer needs at least one format feature enabled: `msgpack` (default), `json`, or both"
);

use std::fmt;
use std::result::Result as StdResult;
use std::str::FromStr;

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// A specialized [`Result`](std::result::Result) for serialization operations.
pub type Result<T> = StdResult<T, Error>;

/// The default maximum accepted by [`decode_with_limit`]: 16 MiB.
///
/// This mirrors the protocol crate's default maximum payload length, so a
/// payload that passed frame decoding is not rejected here by surprise.
pub const DEFAULT_MAX_DECODE_LEN: usize = 16 * 1024 * 1024;

/// An error produced while serializing or deserializing a payload.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A value could not be serialized into the requested format.
    #[error("failed to serialize as {format}: {reason}")]
    Serialize {
        /// The format that was attempted.
        format: Format,
        /// The underlying reason reported by the format implementation.
        reason: String,
    },

    /// Bytes could not be deserialized into the requested type.
    #[error("failed to deserialize {format}: {reason}")]
    Deserialize {
        /// The format that was attempted.
        format: Format,
        /// The underlying reason reported by the format implementation.
        reason: String,
    },

    /// The input exceeded the configured decode limit.
    #[error("encoded payload of {len} bytes exceeds maximum {max}")]
    TooLarge {
        /// The length of the offered input.
        len: usize,
        /// The configured maximum.
        max: usize,
    },

    /// A format identifier was not recognized.
    #[error("unknown serialization format: {value}")]
    UnknownFormat {
        /// The unrecognized identifier.
        value: String,
    },

    /// The requested format exists but was not compiled into this build.
    #[error("format {format} is not enabled; rebuild with its cargo feature")]
    FormatUnavailable {
        /// The format that is compiled out.
        format: Format,
    },
}

/// A payload serialization format.
///
/// The discriminants are stable and travel on the wire during content
/// negotiation, so they must not be reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(u8)]
pub enum Format {
    /// Compact binary encoding. The default for NexusNet payloads.
    MessagePack = 0x01,
    /// Human-readable text encoding, for debugging and interop.
    Json = 0x02,
}

impl Format {
    /// The format used when a peer expresses no preference.
    pub const DEFAULT: Self = Self::MessagePack;

    /// Returns the stable wire discriminant.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parses a format from its wire discriminant.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownFormat`] if the byte is not a defined format.
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::MessagePack),
            0x02 => Ok(Self::Json),
            other => Err(Error::UnknownFormat {
                value: format!("{other:#04x}"),
            }),
        }
    }

    /// Returns `true` if this format is compiled into the current build.
    #[must_use]
    pub const fn is_available(self) -> bool {
        match self {
            Self::MessagePack => cfg!(feature = "msgpack"),
            Self::Json => cfg!(feature = "json"),
        }
    }

    /// Returns `true` if the format produces binary (non-textual) output.
    #[must_use]
    pub const fn is_binary(self) -> bool {
        matches!(self, Self::MessagePack)
    }

    /// Returns the conventional media type for this format.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::MessagePack => "application/msgpack",
            Self::Json => "application/json",
        }
    }

    /// Returns every format this build was compiled with, most preferred first.
    #[must_use]
    pub fn available() -> Vec<Self> {
        [Self::MessagePack, Self::Json]
            .into_iter()
            .filter(|f| f.is_available())
            .collect()
    }

    /// Returns an error unless this format is compiled in.
    fn ensure_available(self) -> Result<()> {
        if self.is_available() {
            Ok(())
        } else {
            Err(Error::FormatUnavailable { format: self })
        }
    }
}

impl Default for Format {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::MessagePack => "msgpack",
            Self::Json => "json",
        };
        f.write_str(name)
    }
}

impl FromStr for Format {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "msgpack" | "messagepack" | "application/msgpack" => Ok(Self::MessagePack),
            "json" | "application/json" => Ok(Self::Json),
            other => Err(Error::UnknownFormat {
                value: other.to_owned(),
            }),
        }
    }
}

/// Serializes `value` using `format`.
///
/// # Errors
///
/// Returns [`Error::FormatUnavailable`] if the format was compiled out, or
/// [`Error::Serialize`] if the value cannot be represented in that format.
pub fn encode<T>(format: Format, value: &T) -> Result<Bytes>
where
    T: Serialize + ?Sized,
{
    format.ensure_available()?;

    match format {
        #[cfg(feature = "msgpack")]
        Format::MessagePack => rmp_serde::to_vec_named(value)
            .map(Bytes::from)
            .map_err(|e| Error::Serialize {
                format,
                reason: e.to_string(),
            }),
        #[cfg(feature = "json")]
        Format::Json => serde_json::to_vec(value)
            .map(Bytes::from)
            .map_err(|e| Error::Serialize {
                format,
                reason: e.to_string(),
            }),
        #[allow(unreachable_patterns)]
        other => Err(Error::FormatUnavailable { format: other }),
    }
}

/// Deserializes a value of type `T` from `bytes` using `format`.
///
/// # Errors
///
/// Returns [`Error::FormatUnavailable`] if the format was compiled out, or
/// [`Error::Deserialize`] if the bytes are not a valid encoding of `T`.
pub fn decode<T>(format: Format, bytes: &[u8]) -> Result<T>
where
    T: DeserializeOwned,
{
    format.ensure_available()?;

    match format {
        #[cfg(feature = "msgpack")]
        Format::MessagePack => rmp_serde::from_slice(bytes).map_err(|e| Error::Deserialize {
            format,
            reason: e.to_string(),
        }),
        #[cfg(feature = "json")]
        Format::Json => serde_json::from_slice(bytes).map_err(|e| Error::Deserialize {
            format,
            reason: e.to_string(),
        }),
        #[allow(unreachable_patterns)]
        other => Err(Error::FormatUnavailable { format: other }),
    }
}

/// Deserializes a value, rejecting input longer than `max_len` first.
///
/// Prefer this over [`decode`] for untrusted input: a deserializer may allocate
/// in proportion to its input, so the length check must happen before parsing
/// rather than after.
///
/// # Errors
///
/// Returns [`Error::TooLarge`] if `bytes` exceeds `max_len`, plus any error
/// from [`decode`].
pub fn decode_with_limit<T>(format: Format, bytes: &[u8], max_len: usize) -> Result<T>
where
    T: DeserializeOwned,
{
    if bytes.len() > max_len {
        return Err(Error::TooLarge {
            len: bytes.len(),
            max: max_len,
        });
    }

    decode(format, bytes)
}

/// Selects the first format that both peers support.
///
/// `preferred` is the local preference order; `remote` is the set the peer
/// advertised. Formats compiled out of this build are never selected.
///
/// # Errors
///
/// Returns [`Error::UnknownFormat`] if there is no mutually supported format.
pub fn negotiate(preferred: &[Format], remote: &[Format]) -> Result<Format> {
    preferred
        .iter()
        .copied()
        .find(|f| f.is_available() && remote.contains(f))
        .ok_or_else(|| Error::UnknownFormat {
            value: "no mutually supported format".to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        count: u32,
        ratio: f64,
        tags: Vec<String>,
        optional: Option<u8>,
    }

    fn sample() -> Sample {
        Sample {
            name: "edge-node".to_owned(),
            count: 42,
            ratio: 0.75,
            tags: vec!["alpha".to_owned(), "beta".to_owned()],
            optional: None,
        }
    }

    #[test]
    fn format_discriminants_are_stable() {
        assert_eq!(Format::MessagePack.as_u8(), 0x01);
        assert_eq!(Format::Json.as_u8(), 0x02);
    }

    #[test]
    fn format_roundtrips_through_u8() {
        for format in [Format::MessagePack, Format::Json] {
            assert_eq!(
                Format::from_u8(format.as_u8()).expect("known format"),
                format
            );
        }
    }

    #[test]
    fn unknown_discriminant_is_rejected() {
        assert!(matches!(
            Format::from_u8(0xFE),
            Err(Error::UnknownFormat { .. })
        ));
    }

    #[test]
    fn format_parses_case_insensitively() {
        assert_eq!(
            "MsgPack".parse::<Format>().expect("known format"),
            Format::MessagePack
        );
        assert_eq!(
            "application/json".parse::<Format>().expect("known format"),
            Format::Json
        );
        assert!("yaml".parse::<Format>().is_err());
    }

    #[test]
    fn default_format_is_messagepack() {
        assert_eq!(Format::default(), Format::MessagePack);
        assert!(Format::MessagePack.is_binary());
        assert!(!Format::Json.is_binary());
    }

    #[cfg(feature = "msgpack")]
    #[test]
    fn messagepack_roundtrips() {
        let value = sample();
        let bytes = encode(Format::MessagePack, &value).expect("value serializes");
        let restored: Sample = decode(Format::MessagePack, &bytes).expect("value deserializes");
        assert_eq!(restored, value);
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_roundtrips() {
        let value = sample();
        let bytes = encode(Format::Json, &value).expect("value serializes");
        let restored: Sample = decode(Format::Json, &bytes).expect("value deserializes");
        assert_eq!(restored, value);
    }

    #[cfg(all(feature = "msgpack", feature = "json"))]
    #[test]
    fn messagepack_is_more_compact_than_json() {
        let value = sample();
        let packed = encode(Format::MessagePack, &value).expect("value serializes");
        let json = encode(Format::Json, &value).expect("value serializes");
        assert!(
            packed.len() < json.len(),
            "msgpack {} should be smaller than json {}",
            packed.len(),
            json.len()
        );
    }

    #[cfg(feature = "json")]
    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        let err = decode::<Sample>(Format::Json, b"{not json").expect_err("invalid json");
        assert!(matches!(err, Error::Deserialize { .. }));
    }

    #[cfg(feature = "msgpack")]
    #[test]
    fn truncated_msgpack_is_an_error() {
        let bytes = encode(Format::MessagePack, &sample()).expect("value serializes");
        let err = decode::<Sample>(Format::MessagePack, &bytes[..bytes.len() / 2])
            .expect_err("truncated input");
        assert!(matches!(err, Error::Deserialize { .. }));
    }

    #[test]
    fn oversized_input_is_rejected_before_parsing() {
        let err = decode_with_limit::<Sample>(Format::default(), &[0_u8; 128], 64)
            .expect_err("input is over the limit");
        assert!(matches!(err, Error::TooLarge { len: 128, max: 64 }));
    }

    #[test]
    fn available_formats_are_reported() {
        let available = Format::available();
        assert!(!available.is_empty());
        assert!(available.iter().all(|f| f.is_available()));
    }

    #[cfg(all(feature = "msgpack", feature = "json"))]
    #[test]
    fn negotiation_prefers_local_order() {
        let remote = vec![Format::Json, Format::MessagePack];
        let chosen =
            negotiate(&[Format::MessagePack, Format::Json], &remote).expect("shared format");
        assert_eq!(chosen, Format::MessagePack);
    }

    #[test]
    fn negotiation_never_selects_a_compiled_out_format() {
        // The peer offers everything; we may only pick what this build has.
        let remote = vec![Format::MessagePack, Format::Json];
        let chosen = negotiate(&[Format::MessagePack, Format::Json], &remote);

        match Format::available().first() {
            Some(&expected) => assert_eq!(chosen.expect("a shared format exists"), expected),
            None => assert!(chosen.is_err(), "a build with no formats cannot negotiate"),
        }
    }

    #[test]
    fn negotiation_fails_without_overlap() {
        assert!(negotiate(&[Format::MessagePack], &[]).is_err());
    }

    #[test]
    fn media_types_are_conventional() {
        assert_eq!(Format::MessagePack.media_type(), "application/msgpack");
        assert_eq!(Format::Json.media_type(), "application/json");
    }
}
