//! Black-box integration tests for the compression crate.
//!
//! These run against every codec compiled into the current build, so the same
//! suite validates a pure-Rust build and one with Zstd enabled.

use nexusnet_compression::{
    compress, decompress, Algorithm, Compressor, Error, Level, Outcome, SkipReason,
};

/// Text-like data with realistic redundancy, rather than a single repeated byte
/// (which every codec crushes and which would hide real differences).
fn compressible(size: usize) -> Vec<u8> {
    let sentence = b"the quick brown fox jumps over the lazy dog; ";
    sentence.iter().copied().cycle().take(size).collect()
}

/// Pseudo-random bytes standing in for already-compressed or encrypted data.
/// A simple LCG keeps this deterministic without a dependency.
fn incompressible(size: usize) -> Vec<u8> {
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    (0..size)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            u8::try_from(state >> 56).unwrap_or(0)
        })
        .collect()
}

/// Every codec available in this build, excluding the no-op.
fn real_algorithms() -> Vec<Algorithm> {
    Algorithm::available()
        .into_iter()
        .filter(|a| *a != Algorithm::None)
        .collect()
}

/// A real codec that exists in this build.
///
/// `Algorithm::default()` is gzip, which is absent from a brotli-only or
/// zstd-only build, so tests that just need "some working codec" use this.
fn any_algorithm() -> Algorithm {
    real_algorithms()
        .first()
        .copied()
        .expect("the crate requires at least one codec feature")
}

#[test]
fn every_available_codec_round_trips() {
    let original = compressible(16_384);

    for algorithm in Algorithm::available() {
        let packed = compress(algorithm, Level::BALANCED, &original)
            .unwrap_or_else(|e| panic!("{algorithm} compresses: {e}"));
        let restored = decompress(algorithm, &packed, 1 << 20)
            .unwrap_or_else(|e| panic!("{algorithm} decompresses: {e}"));

        assert_eq!(restored.as_ref(), original.as_slice(), "{algorithm}");
    }
}

#[test]
fn every_codec_round_trips_at_every_level() {
    let original = compressible(4096);

    for algorithm in real_algorithms() {
        for level in [Level::FAST, Level::BALANCED, Level::BEST] {
            let packed = compress(algorithm, level, &original)
                .unwrap_or_else(|e| panic!("{algorithm} at level {level}: {e}"));
            let restored = decompress(algorithm, &packed, 1 << 20)
                .unwrap_or_else(|e| panic!("{algorithm} at level {level}: {e}"));

            assert_eq!(
                restored.as_ref(),
                original.as_slice(),
                "{algorithm}/{level}"
            );
        }
    }
}

#[test]
fn empty_and_tiny_inputs_round_trip() {
    for algorithm in Algorithm::available() {
        for original in [b"".as_slice(), b"x".as_slice(), b"hello".as_slice()] {
            let packed = compress(algorithm, Level::BALANCED, original)
                .unwrap_or_else(|e| panic!("{algorithm}: {e}"));
            let restored = decompress(algorithm, &packed, 1 << 16)
                .unwrap_or_else(|e| panic!("{algorithm}: {e}"));

            assert_eq!(restored.as_ref(), original, "{algorithm}");
        }
    }
}

#[test]
fn codecs_actually_shrink_compressible_data() {
    let original = compressible(32_768);

    for algorithm in real_algorithms() {
        let packed =
            compress(algorithm, Level::BALANCED, &original).expect("compressible data compresses");
        assert!(
            packed.len() < original.len() / 4,
            "{algorithm} produced {} bytes from {}",
            packed.len(),
            original.len()
        );
    }
}

#[test]
fn corrupt_input_is_an_error_not_a_panic() {
    for algorithm in real_algorithms() {
        let packed =
            compress(algorithm, Level::BALANCED, &compressible(2048)).expect("data compresses");

        let mut corrupted = packed.to_vec();
        let midpoint = corrupted.len() / 2;
        corrupted[midpoint] ^= 0xFF;
        corrupted.truncate(corrupted.len() - 1);

        // Must not panic; a wrong answer is unacceptable but an error is fine.
        let result = decompress(algorithm, &corrupted, 1 << 20);
        if let Ok(output) = result {
            assert_ne!(
                output.as_ref(),
                compressible(2048).as_slice(),
                "{algorithm} silently accepted corrupted input"
            );
        }
    }
}

#[test]
fn a_decompression_bomb_is_rejected() {
    // A megabyte of zeros compresses to a tiny payload that would expand back.
    let bomb_source = vec![0_u8; 1024 * 1024];

    for algorithm in real_algorithms() {
        let packed = compress(algorithm, Level::BEST, &bomb_source).expect("zeros compress well");

        assert!(
            packed.len() < 8192,
            "{algorithm} should compress zeros aggressively"
        );

        // Refuse to expand it into a small buffer.
        let err = decompress(algorithm, &packed, 4096)
            .expect_err("{algorithm} must refuse to exceed the limit");
        assert!(
            matches!(err, Error::OutputTooLarge { max: 4096 }),
            "{algorithm} gave {err:?}"
        );

        // The same payload is fine when the limit accommodates it.
        let restored =
            decompress(algorithm, &packed, 2 * 1024 * 1024).expect("fits within a larger limit");
        assert_eq!(restored.len(), bomb_source.len());
    }
}

#[test]
fn adaptive_skips_small_payloads() {
    let compressor = Compressor::new(any_algorithm());
    let outcome = compressor.compress(b"ack").expect("no codec failure");

    assert!(!outcome.is_compressed());
    assert_eq!(outcome.algorithm(), Algorithm::None);
    assert_eq!(outcome.bytes_saved(), 0);
    assert!(matches!(
        outcome,
        Outcome::Skipped {
            reason: SkipReason::TooSmall,
            ..
        }
    ));
}

#[test]
fn adaptive_skips_incompressible_payloads() {
    let random = incompressible(8192);

    for algorithm in real_algorithms() {
        let compressor = Compressor::new(algorithm);
        let outcome = compressor.compress(&random).expect("no codec failure");

        assert!(
            !outcome.is_compressed(),
            "{algorithm} should decline random data, got ratio {}",
            outcome.ratio()
        );
        assert_eq!(outcome.data().as_ref(), random.as_slice());
    }
}

#[test]
fn adaptive_compresses_worthwhile_payloads() {
    let original = compressible(16_384);

    for algorithm in real_algorithms() {
        let compressor = Compressor::new(algorithm);
        let outcome = compressor.compress(&original).expect("no codec failure");

        assert!(outcome.is_compressed(), "{algorithm} should compress text");
        assert_eq!(outcome.algorithm(), algorithm);
        assert!(
            outcome.ratio() < 0.5,
            "{algorithm} ratio {}",
            outcome.ratio()
        );
        assert!(outcome.bytes_saved() > 0);

        let restored = compressor.restore(&outcome).expect("round-trips");
        assert_eq!(restored.as_ref(), original.as_slice(), "{algorithm}");
    }
}

#[test]
fn skipped_outcomes_still_restore() {
    let compressor = Compressor::new(any_algorithm());
    let tiny = b"short";

    let outcome = compressor.compress(tiny).expect("no codec failure");
    let restored = compressor.restore(&outcome).expect("restores");

    assert_eq!(restored.as_ref(), tiny);
}

#[test]
fn algorithm_none_passes_data_through() {
    let compressor = Compressor::new(Algorithm::None);
    let original = compressible(4096);

    let outcome = compressor.compress(&original).expect("no codec failure");
    assert!(matches!(
        outcome,
        Outcome::Skipped {
            reason: SkipReason::Disabled,
            ..
        }
    ));
    assert_eq!(outcome.data().as_ref(), original.as_slice());
}

#[test]
fn policy_thresholds_are_configurable() {
    let original = compressible(256);

    // Default policy compresses a 256-byte payload.
    let permissive = Compressor::new(any_algorithm());
    assert!(permissive.compress(&original).expect("ok").is_compressed());

    // Raising the minimum size makes the same payload skip.
    let strict = Compressor::new(any_algorithm()).with_min_size(1024);
    assert!(!strict.compress(&original).expect("ok").is_compressed());

    // Demanding an impossible ratio also skips.
    let impossible = Compressor::new(any_algorithm()).with_max_ratio(0.0);
    assert!(!impossible.compress(&original).expect("ok").is_compressed());
}

#[test]
fn compressor_enforces_its_output_limit() {
    let compressor = Compressor::new(any_algorithm()).with_max_output(1024);
    let original = compressible(64 * 1024);

    let outcome = compressor.compress(&original).expect("compresses");
    assert!(outcome.is_compressed());

    let err = compressor
        .restore(&outcome)
        .expect_err("restoring exceeds the configured limit");
    assert!(matches!(err, Error::OutputTooLarge { max: 1024 }));
}

#[test]
fn unavailable_algorithms_report_clearly() {
    for algorithm in [
        Algorithm::Gzip,
        Algorithm::Deflate,
        Algorithm::Brotli,
        Algorithm::Zstd,
    ] {
        if algorithm.is_available() {
            continue;
        }

        let err = compress(algorithm, Level::BALANCED, b"data")
            .expect_err("compiled-out algorithm must fail");
        assert!(matches!(err, Error::AlgorithmUnavailable { .. }));
    }
}
