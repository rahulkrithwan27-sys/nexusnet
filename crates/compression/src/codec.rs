//! Backend codec implementations.
//!
//! Each backend is behind a cargo feature. Decompression always enforces an
//! output limit *while* decompressing rather than after, so a decompression
//! bomb is stopped before its output is materialized.

use std::io::Read;

use bytes::Bytes;

use crate::algorithm::{Algorithm, Error, Level, Result};

/// Compresses `input` with `algorithm` at `level`.
///
/// # Errors
///
/// Returns [`Error::AlgorithmUnavailable`] if the algorithm was compiled out,
/// or [`Error::Compress`] if the codec fails.
pub fn compress(algorithm: Algorithm, level: Level, input: &[u8]) -> Result<Bytes> {
    algorithm.ensure_available()?;

    match algorithm {
        Algorithm::None => Ok(Bytes::copy_from_slice(input)),

        #[cfg(feature = "gzip")]
        Algorithm::Gzip => {
            use flate2::write::GzEncoder;
            use std::io::Write;

            let mut encoder = GzEncoder::new(
                Vec::with_capacity(input.len() / 2),
                flate2::Compression::new(gzip_level(level)),
            );
            encoder
                .write_all(input)
                .map_err(|e| compress_err(algorithm, &e))?;
            encoder
                .finish()
                .map(Bytes::from)
                .map_err(|e| compress_err(algorithm, &e))
        }

        #[cfg(feature = "gzip")]
        Algorithm::Deflate => {
            use flate2::write::DeflateEncoder;
            use std::io::Write;

            let mut encoder = DeflateEncoder::new(
                Vec::with_capacity(input.len() / 2),
                flate2::Compression::new(gzip_level(level)),
            );
            encoder
                .write_all(input)
                .map_err(|e| compress_err(algorithm, &e))?;
            encoder
                .finish()
                .map(Bytes::from)
                .map_err(|e| compress_err(algorithm, &e))
        }

        #[cfg(feature = "brotli")]
        Algorithm::Brotli => {
            use std::io::Write;

            let quality = brotli_quality(level);
            let mut output = Vec::with_capacity(input.len() / 2);
            {
                let mut writer = brotli::CompressorWriter::new(
                    &mut output,
                    BROTLI_BUFFER,
                    quality,
                    BROTLI_LGWIN,
                );
                writer
                    .write_all(input)
                    .map_err(|e| compress_err(algorithm, &e))?;
                writer.flush().map_err(|e| compress_err(algorithm, &e))?;
            }
            Ok(Bytes::from(output))
        }

        #[cfg(feature = "zstd")]
        Algorithm::Zstd => zstd::stream::encode_all(input, zstd_level(level))
            .map(Bytes::from)
            .map_err(|e| compress_err(algorithm, &e)),

        #[allow(unreachable_patterns)]
        other => Err(Error::AlgorithmUnavailable { algorithm: other }),
    }
}

/// Decompresses `input`, refusing to produce more than `max_output` bytes.
///
/// # Errors
///
/// Returns [`Error::AlgorithmUnavailable`] if the algorithm was compiled out,
/// [`Error::OutputTooLarge`] if the output would exceed `max_output`, or
/// [`Error::Decompress`] if the input is corrupt or truncated.
pub fn decompress(algorithm: Algorithm, input: &[u8], max_output: usize) -> Result<Bytes> {
    algorithm.ensure_available()?;

    match algorithm {
        Algorithm::None => {
            if input.len() > max_output {
                return Err(Error::OutputTooLarge { max: max_output });
            }
            Ok(Bytes::copy_from_slice(input))
        }

        #[cfg(feature = "gzip")]
        Algorithm::Gzip => read_limited(algorithm, flate2::read::GzDecoder::new(input), max_output),

        #[cfg(feature = "gzip")]
        Algorithm::Deflate => read_limited(
            algorithm,
            flate2::read::DeflateDecoder::new(input),
            max_output,
        ),

        #[cfg(feature = "brotli")]
        Algorithm::Brotli => read_limited(
            algorithm,
            brotli::Decompressor::new(input, BROTLI_BUFFER),
            max_output,
        ),

        #[cfg(feature = "zstd")]
        Algorithm::Zstd => {
            let decoder = zstd::stream::read::Decoder::new(input)
                .map_err(|e| decompress_err(algorithm, &e))?;
            read_limited(algorithm, decoder, max_output)
        }

        #[allow(unreachable_patterns)]
        other => Err(Error::AlgorithmUnavailable { algorithm: other }),
    }
}

/// Reads a decoder to completion, aborting if it would exceed `max_output`.
///
/// Reading `max_output + 1` bytes is what makes the limit safe: if the decoder
/// yields that many, the real output is definitely too large, and we stop
/// without ever allocating the full expansion.
#[allow(dead_code)] // Unused when every codec feature is disabled.
fn read_limited<R: Read>(algorithm: Algorithm, reader: R, max_output: usize) -> Result<Bytes> {
    let mut output = Vec::new();
    let mut limited = reader.take(max_output as u64 + 1);

    limited
        .read_to_end(&mut output)
        .map_err(|e| decompress_err(algorithm, &e))?;

    if output.len() > max_output {
        return Err(Error::OutputTooLarge { max: max_output });
    }

    Ok(Bytes::from(output))
}

#[allow(dead_code)]
fn compress_err(algorithm: Algorithm, error: &dyn std::fmt::Display) -> Error {
    Error::Compress {
        algorithm,
        reason: error.to_string(),
    }
}

#[allow(dead_code)]
fn decompress_err(algorithm: Algorithm, error: &dyn std::fmt::Display) -> Error {
    Error::Decompress {
        algorithm,
        reason: error.to_string(),
    }
}

/// Brotli's internal buffer size; 4 KiB is the value its own tools default to.
#[cfg(feature = "brotli")]
const BROTLI_BUFFER: usize = 4096;

/// Brotli window size (2^22 bytes). Larger windows find more distant matches at
/// the cost of memory; 22 is Brotli's own default.
#[cfg(feature = "brotli")]
const BROTLI_LGWIN: u32 = 22;

/// Maps the abstract level onto flate2's 0–9 range.
#[cfg(feature = "gzip")]
fn gzip_level(level: Level) -> u32 {
    u32::try_from(level.scale_to(0, 9)).unwrap_or(6)
}

/// Maps the abstract level onto Brotli's 0–11 quality range.
#[cfg(feature = "brotli")]
fn brotli_quality(level: Level) -> u32 {
    u32::try_from(level.scale_to(0, 11)).unwrap_or(5)
}

/// Maps the abstract level onto Zstd's 1–22 range.
#[cfg(feature = "zstd")]
fn zstd_level(level: Level) -> i32 {
    level.scale_to(1, 22)
}
