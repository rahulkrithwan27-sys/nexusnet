# nexusnet-protocol

The NexusNet wire format: frame layout, framing, and version negotiation.

This crate is transport-agnostic. It turns frames into bytes and bytes back into
frames; it does not open sockets, compress, or encrypt. Those concerns live in
`nexusnet-transport`, `nexusnet-compression`, and `nexusnet-encryption`, which
describe their work using the flags defined here.

## Wire layout

A fixed 16-byte big-endian header precedes every payload:

```text
 0       1       2       3       4       5       6       7
+-------+-------+-------+-------+-------+-------+-------+-------+
|     magic     | major | minor |  type | flags |   reserved    |
+-------+-------+-------+-------+-------+-------+-------+-------+
|           stream_id           |          payload_len          |
+-------+-------+-------+-------+-------+-------+-------+-------+
|                          payload ...                          |
+---------------------------------------------------------------+
```

A fixed-width header means a reader always knows how many bytes it needs before
it can learn the payload length, which keeps the streaming decoder a simple
two-state machine.

## Usage

```rust
use bytes::Bytes;
use nexusnet_protocol::{Decoder, Encoder, Frame, FrameType};

let mut encoder = Encoder::new();
encoder.encode(&Frame::new(FrameType::Data, 1, Bytes::from_static(b"hello"))?);
let wire = encoder.take();

let mut decoder = Decoder::new();
decoder.push(&wire);
let frame = decoder.next_frame()?.expect("frame is complete");
assert_eq!(frame.payload().as_ref(), b"hello");
# Ok::<(), nexusnet_protocol::Error>(())
```

`Decoder` handles frames split across reads or batched into a single read —
push whatever bytes arrive and drain frames until `next_frame` returns
`Ok(None)`.

## Robustness

Decoding is defensive by default:

- Unknown frame types and undefined flag bits are **rejected**, not ignored, so
  a future flag is never silently misread by an older peer.
- Reserved header bits must be zero, keeping them assignable later.
- Payload lengths are bounds-checked **before** any payload memory is
  committed, so a hostile peer cannot induce a large allocation. The default cap
  is 16 MiB; set your own with `Decoder::with_max_payload_len`.

## Versioning

Peers are compatible when their **major** versions match. A newer minor version
may add frame types or flags but must not change existing meanings, so the
effective version of a connection is the minimum of both peers.

## Testing & benchmarks

```bash
cargo test  -p nexusnet-protocol
cargo bench -p nexusnet-protocol
```

Benchmarks cover header encoding, full-frame encode/decode across payload sizes,
and incremental decoding from an MTU-sized chunked stream.

## Status

Implemented in **Phase 2**. See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
