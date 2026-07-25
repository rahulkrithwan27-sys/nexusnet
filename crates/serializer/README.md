# nexusnet-serializer

Payload serialization for NexusNet: converting application types to and from
the bytes carried in a frame payload.

A thin, uniform layer over `serde`. It doesn't decide *what* to send; it decides
how a value becomes bytes, and makes that choice explicit and negotiable.

## Formats

| Format | Feature | Use it for |
| ------ | ------- | ---------- |
| MessagePack | `msgpack` (default) | The wire default: compact, binary, schema-free. |
| JSON | `json` (default) | Debugging, logs, and interop with HTTP tooling. |

MessagePack is the default because it's materially smaller than JSON for the
same value and needs no build tooling. JSON stays for cases where a human or an
external system has to read the payload.

Protobuf is deliberately **not** here yet. It wants `.proto` files, a `build.rs`,
and a codegen step, and committing to that before the message shapes are settled
is backwards. Adding it later is clean — `prost`'s derive macros don't require
`protoc`; only compiling `.proto` files does.

## Usage

```rust
use nexusnet_serializer::{decode, encode, Format};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Telemetry { node: String, rtt_micros: u32 }

let value = Telemetry { node: "edge-1".to_owned(), rtt_micros: 8_400 };
let bytes = encode(Format::MessagePack, &value)?;
let restored: Telemetry = decode(Format::MessagePack, &bytes)?;
assert_eq!(restored, value);
# Ok::<(), nexusnet_serializer::Error>(())
```

## Untrusted input

`decode` accepts any length. For bytes off the network, use `decode_with_limit`:
a deserializer may allocate in proportion to its input, so the length check has
to happen *before* parsing, not after.

## Feature gating

Formats are independently selectable, and `Format::is_available` reports what
the current build supports so `negotiate` never selects a compiled-out format.
Building with **no** format feature is a compile error rather than a crate whose
every call fails at runtime.

```bash
cargo build -p nexusnet-serializer --no-default-features --features msgpack
```

## Testing & benchmarks

```bash
cargo test  -p nexusnet-serializer
cargo bench -p nexusnet-serializer
```

Benchmarks compare encode/decode cost across formats and report encoded sizes,
so the compactness trade-off is measured rather than assumed.

## Status

Implemented in **Phase 2**. See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
