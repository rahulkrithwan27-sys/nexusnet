//! Black-box integration tests using real loopback sockets.
//!
//! The unit tests exercise framing over in-memory pipes; these verify the same
//! behavior survives an actual kernel socket, where reads fragment
//! unpredictably and close semantics are real.

use std::time::Duration;

use bytes::Bytes;
use nexusnet_protocol::{Frame, FrameFlags, FrameType};
use nexusnet_transport::{tcp, Error, TcpListener, TransportConfig, UdpEndpoint};

fn data_frame(stream_id: u32, payload: &[u8]) -> Frame {
    Frame::new(FrameType::Data, stream_id, Bytes::copy_from_slice(payload))
        .expect("payload fits in u32")
}

#[tokio::test]
async fn tcp_echoes_a_frame_over_a_real_socket() {
    let config = TransportConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0", config)
        .await
        .expect("binds to a free port");
    let address = listener.local_addr().expect("has a local address");

    let server = tokio::spawn(async move {
        let (mut connection, _peer) = listener.accept().await.expect("accepts");
        while let Some(frame) = connection.recv().await.expect("no transport error") {
            connection.send(&frame).await.expect("echoes");
        }
    });

    let mut client = tcp::connect(address, config).await.expect("connects");
    let sent = data_frame(1, b"round trip");
    client.send(&sent).await.expect("sends");

    let echoed = client
        .recv()
        .await
        .expect("no transport error")
        .expect("the echo arrives");
    assert_eq!(echoed, sent);

    client.shutdown().await.expect("shuts down");
    server.await.expect("the server task completes");
}

#[tokio::test]
async fn tcp_preserves_order_across_many_frames() {
    const COUNT: u32 = 500;

    let config = TransportConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0", config)
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    let server = tokio::spawn(async move {
        let (mut connection, _peer) = listener.accept().await.expect("accepts");
        let mut seen = Vec::new();

        while let Some(frame) = connection.recv().await.expect("no transport error") {
            seen.push(frame.header().stream_id);
        }
        seen
    });

    let mut client = tcp::connect(address, config).await.expect("connects");

    // Payloads vary in size so frame boundaries do not align with reads.
    for i in 0..COUNT {
        let payload = vec![b'x'; (i as usize % 997) + 1];
        client.send(&data_frame(i, &payload)).await.expect("sends");
    }
    client.shutdown().await.expect("shuts down");

    let seen = server.await.expect("the server task completes");
    assert_eq!(seen.len(), COUNT as usize);
    assert!(
        seen.iter().copied().eq(0..COUNT),
        "frames must arrive in order"
    );
}

#[tokio::test]
async fn tcp_batched_send_arrives_intact() {
    let config = TransportConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0", config)
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    let frames: Vec<Frame> = (0..64).map(|i| data_frame(i, b"batched payload")).collect();
    let expected = frames.clone();

    let server = tokio::spawn(async move {
        let (mut connection, _peer) = listener.accept().await.expect("accepts");
        let mut received = Vec::new();
        while let Some(frame) = connection.recv().await.expect("no transport error") {
            received.push(frame);
        }
        received
    });

    let mut client = tcp::connect(address, config).await.expect("connects");
    client.send_all(&frames).await.expect("batch sends");
    client.shutdown().await.expect("shuts down");

    let received = server.await.expect("the server task completes");
    assert_eq!(received, expected);
}

#[tokio::test]
async fn tcp_flags_survive_the_round_trip() {
    let config = TransportConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0", config)
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    let server = tokio::spawn(async move {
        let (mut connection, _peer) = listener.accept().await.expect("accepts");
        connection
            .recv()
            .await
            .expect("no transport error")
            .expect("a frame arrives")
    });

    let mut client = tcp::connect(address, config).await.expect("connects");
    let sent = data_frame(7, b"compressed and final")
        .with_flags(FrameFlags::COMPRESSED | FrameFlags::END_OF_STREAM);
    client.send(&sent).await.expect("sends");

    let received = server.await.expect("the server task completes");
    assert!(received.header().flags.contains(FrameFlags::COMPRESSED));
    assert!(received.header().flags.contains(FrameFlags::END_OF_STREAM));
}

#[tokio::test]
async fn tcp_reports_a_clean_close_as_end_of_stream() {
    let config = TransportConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0", config)
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    let server = tokio::spawn(async move {
        let (mut connection, _peer) = listener.accept().await.expect("accepts");
        connection.recv().await
    });

    let client = tcp::connect(address, config).await.expect("connects");
    drop(client); // Close without sending anything.

    let result = server.await.expect("the server task completes");
    assert!(
        matches!(result, Ok(None)),
        "a clean close must be end-of-stream, got {result:?}"
    );
}

#[tokio::test]
async fn tcp_rejects_a_frame_over_the_payload_limit() {
    let server_config = TransportConfig::default().with_max_payload_len(64);
    let client_config = TransportConfig::default();

    let listener = TcpListener::bind("127.0.0.1:0", server_config)
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    let server = tokio::spawn(async move {
        let (mut connection, _peer) = listener.accept().await.expect("accepts");
        connection.recv().await
    });

    let mut client = tcp::connect(address, client_config)
        .await
        .expect("connects");
    let oversized = vec![b'x'; 4096];
    let _ = client.send(&data_frame(1, &oversized)).await;

    let result = server.await.expect("the server task completes");
    match result {
        Err(err) => {
            assert!(matches!(err, Error::Protocol(_)), "got {err:?}");
            assert!(err.is_fatal());
        }
        other => panic!("expected a protocol error, got {other:?}"),
    }
}

#[tokio::test]
async fn tcp_connect_times_out_promptly() {
    // Reserved TEST-NET-1 address; packets are discarded rather than refused.
    let config = TransportConfig::default().with_connect_timeout(Duration::from_millis(150));

    let started = std::time::Instant::now();
    let result = tcp::connect("192.0.2.1:9", config).await;
    let elapsed = started.elapsed();

    match result {
        Err(Error::ConnectTimeout { .. }) => {
            assert!(
                elapsed < Duration::from_secs(3),
                "timeout should fire promptly, took {elapsed:?}"
            );
        }
        // Some sandboxed networks refuse immediately instead of blackholing;
        // that is still a correct, prompt failure.
        Err(Error::Io(_)) => {}
        other => panic!("expected a timeout or i/o error, got {other:?}"),
    }
}

#[tokio::test]
async fn tcp_serves_several_clients() {
    let config = TransportConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0", config)
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    let server = tokio::spawn(async move {
        for _ in 0..4 {
            let (mut connection, _peer) = listener.accept().await.expect("accepts");
            tokio::spawn(async move {
                while let Some(frame) = connection.recv().await.expect("no transport error") {
                    connection.send(&frame).await.expect("echoes");
                }
            });
        }
    });

    let mut clients = Vec::new();
    for i in 0..4_u32 {
        let mut client = tcp::connect(address, config).await.expect("connects");
        client
            .send(&data_frame(i, format!("client {i}").as_bytes()))
            .await
            .expect("sends");
        clients.push((i, client));
    }

    for (i, mut client) in clients {
        let echoed = client
            .recv()
            .await
            .expect("no transport error")
            .expect("the echo arrives");
        assert_eq!(echoed.header().stream_id, i);
    }

    server.await.expect("the server task completes");
}

#[tokio::test]
async fn udp_round_trips_a_frame() {
    let config = TransportConfig::default();

    let mut server = UdpEndpoint::bind("127.0.0.1:0", config)
        .await
        .expect("server binds");
    let server_address = server.local_addr().expect("has an address");

    let mut client = UdpEndpoint::bind("127.0.0.1:0", config)
        .await
        .expect("client binds");

    let sent = data_frame(3, b"datagram payload");
    client
        .send_to(&sent, server_address)
        .await
        .expect("sends the datagram");

    let (received, peer) = server.recv_from().await.expect("receives the datagram");
    assert_eq!(received, sent);
    assert_eq!(peer, client.local_addr().expect("client has an address"));

    // Reply to the observed peer address.
    let reply = data_frame(3, b"acknowledged");
    server.send_to(&reply, peer).await.expect("replies");

    let (echoed, _) = client.recv_from().await.expect("receives the reply");
    assert_eq!(echoed, reply);
}

#[tokio::test]
async fn udp_rejects_a_frame_larger_than_the_datagram_limit() {
    let config = TransportConfig::default().with_max_datagram(256);

    let endpoint = UdpEndpoint::bind("127.0.0.1:0", config)
        .await
        .expect("binds");
    let target = endpoint.local_addr().expect("has an address");

    let oversized = data_frame(1, &vec![b'x'; 1024]);
    let err = endpoint
        .send_to(&oversized, target)
        .await
        .expect_err("oversized datagram is rejected");

    assert!(matches!(err, Error::DatagramTooLarge { max: 256, .. }));
    assert!(!err.is_fatal(), "the endpoint remains usable");

    // The endpoint still works afterwards.
    let small = data_frame(1, b"fits");
    endpoint.send_to(&small, target).await.expect("still sends");
}

#[tokio::test]
async fn udp_rejects_a_malformed_datagram() {
    let config = TransportConfig::default();

    let mut server = UdpEndpoint::bind("127.0.0.1:0", config)
        .await
        .expect("binds");
    let server_address = server.local_addr().expect("has an address");

    // Send raw garbage that is not a valid frame.
    let raw = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("raw socket binds");
    raw.send_to(b"this is not a nexusnet frame at all", server_address)
        .await
        .expect("sends garbage");

    let err = server
        .recv_from()
        .await
        .expect_err("malformed datagram is rejected");
    assert!(matches!(err, Error::Protocol(_)), "got {err:?}");
}
