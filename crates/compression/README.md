# nexusnet-compression

Payload compression for NexusNet, with an adaptive policy that declines to
compress when compressing wouldn't help.

## Algorithms

| Algorithm | Feature | Toolchain | Notes |
| --------- | ------- | --------- | ----- |
| Gzip | `gzip` (default) | Pure Rust | Ubiquitous, widely interoperable. |
| Deflate | `gzip` (default) | Pure Rust | Gzip without the container overhead. |
| Brotli | `brotli` (default) | Pure Rust | Best ratio of the pure-Rust codecs. |
| Zstd | `zstd` (opt-in) | **Requires C** | Best speed-to-ratio balance. |

The default feature set is deliberately pure Rust, so the crate builds anywhere
`cargo` does — including WebAssembly and minimal CI images — with no C compiler.
Zstd is excellent but binds to a C library, so it's opt-in:

```toml
nexusnet-compression = { version = "0.1", features = ["zstd"] }
```

Building with **no** codec feature is a compile error rather than a crate whose
every call fails at runtime.

## Adaptive compression

Compressing unconditionally loses in two common cases:

- **Small payloads.** Codec framing overhead exceeds the saving. Gzip's header
  and trailer alone are larger than a short control message.
- **Already-compressed payloads.** Ciphertext, JPEG, and Zstd output are
  effectively random to a second compressor: CPU burned, nothing saved.

`Compressor` skips both, and decides by **measuring rather than guessing** —
payloads above the size threshold are actually compressed, and the result is
kept only if it beat the configured ratio. That handles incompressible data
correctly without needing to detect its type.

```rust
use nexusnet_compression::{Algorithm, Compressor};

let compressor = Compressor::new(Algorithm::Gzip);
let outcome = compressor.compress(&vec![b'x'; 8192])?;

assert!(outcome.is_compressed());
let restored = compressor.restore(&outcome)?;
# Ok::<(), nexusnet_compression::Error>(())
```

`Outcome::is_compressed()` maps directly onto the protocol's `FrameFlags::COMPRESSED`
bit, and `Outcome::algorithm()` tells the peer how to reverse it.

## Compression levels

Codecs disagree about level numbering — gzip 0–9, Brotli 0–11, Zstd 1–22.
`Level` is an abstract 0–100 scale each backend maps onto its own range, so you
express intent (`Level::FAST`, `BALANCED`, `BEST`) without memorizing three
scales.

## Decompression limits

Decompression always enforces a maximum output size, **during** decompression
rather than after. This is the defense against a decompression bomb: a few
kilobytes that would expand to gigabytes is rejected before that output is ever
materialized. There's an explicit test for it against every codec.

## Measured results

64 KiB of HTTP-like text, on the default `BALANCED` level:

| Codec | Compressed size | Ratio | Compress | Decompress |
| ----- | --------------- | ----- | -------- | ---------- |
| gzip | 340 B | 0.52% | 465 MiB/s | 2.4 GiB/s |
| deflate | 322 B | 0.49% | 478 MiB/s | 2.7 GiB/s |
| brotli | 85 B | 0.13% | 124 MiB/s | 1.85 GiB/s |
| zstd | 94 B | 0.14% | 70 MiB/s | 2.07 GiB/s |

Highly repetitive input, so ratios are unusually good; treat them as relative.
Brotli and Zstd clearly beat the DEFLATE family on size, at real compression
cost. Decompression is fast everywhere, which is the right trade for traffic
compressed once and read many times.

## Testing & benchmarks

```bash
cargo test  -p nexusnet-compression
cargo test  -p nexusnet-compression --all-features   # includes zstd
cargo bench -p nexusnet-compression
```

## Status

Implemented in **Phase 2**. See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
