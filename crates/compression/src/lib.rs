//! # nexusnet-compression
//!
//! Payload compression for NexusNet, with an adaptive policy that declines to
//! compress when compressing would not help.
//!
//! ## Algorithms
//!
//! | Algorithm | Feature | Toolchain | Notes |
//! | --------- | ------- | --------- | ----- |
//! | [`Algorithm::Gzip`] | `gzip` (default) | Pure Rust | Ubiquitous, widely interoperable. |
//! | [`Algorithm::Deflate`] | `gzip` (default) | Pure Rust | Gzip without the container overhead. |
//! | [`Algorithm::Brotli`] | `brotli` (default) | Pure Rust | Best ratio of the pure-Rust codecs. |
//! | [`Algorithm::Zstd`] | `zstd` (opt-in) | **Requires C** | Best speed-to-ratio balance. |
//!
//! The default feature set is deliberately pure Rust, so the crate builds
//! anywhere `cargo` does — including WebAssembly and minimal CI images — with
//! no C compiler. Zstd is excellent but binds to a C library, so it is opt-in:
//!
//! ```toml
//! nexusnet-compression = { version = "0.1", features = ["zstd"] }
//! ```
//!
//! ## Adaptive compression
//!
//! Compressing unconditionally loses on small payloads (codec overhead exceeds
//! the saving) and on already-compressed payloads such as ciphertext or JPEG
//! (which are incompressible, so the attempt burns CPU for nothing).
//! [`Compressor`] skips both, and decides by *measuring* rather than guessing:
//! it keeps a compressed result only if it actually shrank enough.
//!
//! ```
//! # #[cfg(feature = "gzip")] {
//! use nexusnet_compression::{Algorithm, Compressor};
//!
//! let compressor = Compressor::new(Algorithm::Gzip);
//!
//! let outcome = compressor.compress(&vec![b'x'; 8192]).expect("compresses");
//! assert!(outcome.is_compressed());
//!
//! // Round-trips back to the original bytes.
//! let restored = compressor.restore(&outcome).expect("restores");
//! assert_eq!(restored.len(), 8192);
//! # }
//! ```
//!
//! [`Outcome::is_compressed`] maps directly onto the protocol's compressed
//! frame flag, and [`Outcome::algorithm`] tells the peer how to reverse it.
//!
//! ## Decompression limits
//!
//! Decompression always enforces a maximum output size, and enforces it
//! *during* decompression rather than after. This is the defense against a
//! decompression bomb: a few kilobytes of input that would expand to gigabytes
//! is rejected before that output is ever materialized.
#![cfg_attr(docsrs, feature(doc_cfg))]

// Every codec is optional, but a compression crate that cannot compress is not
// useful. Fail at build time rather than returning `AlgorithmUnavailable` from
// every call.
#[cfg(not(any(feature = "gzip", feature = "brotli", feature = "zstd")))]
compile_error!(
    "nexusnet-compression needs at least one codec feature enabled: `gzip` (default), `brotli` (default), or `zstd`"
);

mod adaptive;
mod algorithm;
mod codec;

pub use crate::adaptive::{
    Compressor, Outcome, SkipReason, DEFAULT_MAX_OUTPUT, DEFAULT_MAX_RATIO, DEFAULT_MIN_SIZE,
};
pub use crate::algorithm::{Algorithm, Error, Level, Result};
pub use crate::codec::{compress, decompress};
