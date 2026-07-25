//! Criterion benchmarks for the NexusNet wire format.
//!
//! These measure the hot paths a transport hits per frame: header encoding,
//! full-frame encode/decode, and incremental decoding from a chunked stream.
//! Throughput is reported in bytes so results are comparable across payload
//! sizes. Run with `cargo bench -p nexusnet-protocol`.
//!
//! The `criterion_group!` macro expands to a public function without a doc
//! comment, so `missing_docs` is allowed for this benchmark-only target.
#![allow(missing_docs)]

use std::hint::black_box;

use bytes::{Bytes, BytesMut};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexusnet_protocol::{Decoder, Encoder, Frame, FrameType};

/// Payload sizes spanning a control frame, a typical message, and a bulk chunk.
const PAYLOAD_SIZES: [usize; 4] = [0, 64, 1024, 65536];

fn payload(size: usize) -> Bytes {
    Bytes::from(vec![0xA5_u8; size])
}

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_encode");

    for size in PAYLOAD_SIZES {
        let frame = Frame::new(FrameType::Data, 1, payload(size)).expect("payload fits in u32");
        let encoded_len = frame.encoded_len();
        group.throughput(Throughput::Bytes(encoded_len as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &frame, |b, frame| {
            let mut buf = BytesMut::with_capacity(encoded_len);
            b.iter(|| {
                buf.clear();
                frame.encode_into(black_box(&mut buf));
                black_box(buf.len())
            });
        });
    }

    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_decode");

    for size in PAYLOAD_SIZES {
        let frame = Frame::new(FrameType::Data, 1, payload(size)).expect("payload fits in u32");
        let encoded = frame.encode();
        group.throughput(Throughput::Bytes(encoded.len() as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &encoded, |b, encoded| {
            b.iter(|| {
                let (decoded, consumed) =
                    Frame::decode(black_box(encoded)).expect("benchmark input is a valid frame");
                black_box((decoded, consumed))
            });
        });
    }

    group.finish();
}

/// Decodes a batch of frames delivered in fixed-size chunks, which is the
/// realistic transport path: reads rarely align to frame boundaries.
fn bench_stream_decode(c: &mut Criterion) {
    const FRAME_COUNT: usize = 64;
    const CHUNK: usize = 1500; // Roughly one Ethernet MTU.

    let mut encoder = Encoder::new();
    for i in 0..FRAME_COUNT {
        let frame = Frame::new(FrameType::Data, i as u32, payload(256)).expect("payload fits");
        encoder.encode(&frame);
    }
    let wire = encoder.take();

    let mut group = c.benchmark_group("stream_decode");
    group.throughput(Throughput::Bytes(wire.len() as u64));

    group.bench_function("chunked_1500b", |b| {
        b.iter(|| {
            let mut decoder = Decoder::new();
            let mut count = 0_usize;

            for chunk in wire.chunks(CHUNK) {
                decoder.push(black_box(chunk));
                while let Some(frame) = decoder.next_frame().expect("stream stays valid") {
                    black_box(&frame);
                    count += 1;
                }
            }

            debug_assert_eq!(count, FRAME_COUNT);
            black_box(count)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_encode, bench_decode, bench_stream_decode);
criterion_main!(benches);
