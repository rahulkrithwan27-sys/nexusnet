//! Black-box integration tests exercising the protocol crate's public API.
//!
//! These deliberately use only re-exported items, so they fail if the public
//! surface regresses even when the internals still compile.

use bytes::Bytes;
use nexusnet_protocol::{
    negotiate, Decoder, Encoder, Error, Frame, FrameFlags, FrameType, ProtocolVersion, HEADER_LEN,
    PROTOCOL_VERSION, SUPPORTED_VERSIONS,
};

#[test]
fn a_conversation_round_trips_through_the_codecs() {
    let mut encoder = Encoder::new();

    let handshake =
        Frame::new(FrameType::Handshake, 0, Bytes::from_static(b"v1")).expect("payload fits");
    let body = Frame::new(FrameType::Data, 1, Bytes::from_static(b"request body"))
        .expect("payload fits")
        .with_flags(FrameFlags::COMPRESSED);
    let close = Frame::new(FrameType::Close, 1, Bytes::new())
        .expect("payload fits")
        .with_flags(FrameFlags::END_OF_STREAM);

    for frame in [&handshake, &body, &close] {
        encoder.encode(frame);
    }
    let wire = encoder.take();

    let mut decoder = Decoder::new();
    decoder.push(&wire);

    assert_eq!(decoder.next_frame(), Ok(Some(handshake)));

    let decoded_body = decoder
        .next_frame()
        .expect("stream is valid")
        .expect("body frame is complete");
    assert_eq!(decoded_body.payload().as_ref(), b"request body");
    assert!(decoded_body.header().flags.contains(FrameFlags::COMPRESSED));
    assert_eq!(decoded_body.header().version, PROTOCOL_VERSION);

    assert_eq!(decoder.next_frame(), Ok(Some(close)));
    assert_eq!(decoder.next_frame(), Ok(None));
    assert!(decoder.is_empty());
}

#[test]
fn frames_survive_arbitrary_chunk_boundaries() {
    let mut encoder = Encoder::new();
    for i in 0..16_u32 {
        let payload = Bytes::from(vec![u8::try_from(i % 256).expect("modulo fits in u8"); 100]);
        encoder.encode(&Frame::new(FrameType::Data, i, payload).expect("payload fits"));
    }
    let wire = encoder.take();

    // Prime chunk size, so boundaries land inside headers and payloads alike.
    let mut decoder = Decoder::new();
    let mut decoded = Vec::new();
    for chunk in wire.chunks(7) {
        decoder.push(chunk);
        while let Some(frame) = decoder.next_frame().expect("stream stays valid") {
            decoded.push(frame);
        }
    }

    assert_eq!(decoded.len(), 16);
    assert!(decoder.is_empty());
    for (i, frame) in decoded.iter().enumerate() {
        assert_eq!(frame.header().stream_id, u32::try_from(i).expect("fits"));
        assert_eq!(frame.payload().len(), 100);
    }
}

#[test]
fn a_hostile_length_is_rejected_before_allocation() {
    // Claim a 4 GiB payload while sending only a header.
    let mut decoder = Decoder::with_max_payload_len(4096);
    let mut header = Vec::new();
    header.extend_from_slice(&nexusnet_protocol::MAGIC.to_be_bytes());
    header.extend_from_slice(&[PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor]);
    header.push(FrameType::Data.as_u8());
    header.push(FrameFlags::NONE.bits());
    header.extend_from_slice(&0_u16.to_be_bytes());
    header.extend_from_slice(&1_u32.to_be_bytes());
    header.extend_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(header.len(), HEADER_LEN);

    decoder.push(&header);
    assert_eq!(
        decoder.next_frame(),
        Err(Error::PayloadTooLarge {
            len: u32::MAX,
            max: 4096,
        })
    );
}

#[test]
fn truncated_streams_never_yield_a_partial_frame() {
    let frame = Frame::new(
        FrameType::Data,
        1,
        Bytes::from_static(b"incomplete transmission"),
    )
    .expect("payload fits");
    let encoded = frame.encode();

    let mut decoder = Decoder::new();
    decoder.push(&encoded[..encoded.len() - 1]);

    assert_eq!(decoder.next_frame(), Ok(None));
    assert!(!decoder.is_empty(), "partial data stays buffered");
}

#[test]
fn version_negotiation_agrees_on_a_shared_version() {
    let modern = [ProtocolVersion::new(1, 0), ProtocolVersion::new(1, 4)];
    let legacy = [ProtocolVersion::new(1, 0)];

    assert_eq!(negotiate(&modern, &legacy), Ok(ProtocolVersion::new(1, 0)));
    assert_eq!(
        negotiate(SUPPORTED_VERSIONS, SUPPORTED_VERSIONS),
        Ok(PROTOCOL_VERSION)
    );
    assert_eq!(
        negotiate(SUPPORTED_VERSIONS, &[ProtocolVersion::new(9, 0)]),
        Err(Error::NoCommonVersion)
    );
}
