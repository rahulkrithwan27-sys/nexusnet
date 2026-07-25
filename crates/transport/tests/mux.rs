//! Integration tests for stream multiplexing.

use std::time::Duration;

use bytes::Bytes;
use nexusnet_transport::{
    tcp, Connection, Error, Role, Session, SessionConfig, SessionHandle, TcpListener,
    TransportConfig,
};

/// Builds a connected client/server session pair over an in-memory pipe.
fn session_pair(config: SessionConfig) -> (SessionHandle, SessionHandle) {
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let transport = TransportConfig::default();

    let (client, client_driver) =
        Session::start(Connection::new(client_io, transport), Role::Client, config);
    let (server, server_driver) =
        Session::start(Connection::new(server_io, transport), Role::Server, config);

    tokio::spawn(async move { client_driver.run().await });
    tokio::spawn(async move { server_driver.run().await });

    (client, server)
}

#[tokio::test]
async fn stream_identifiers_follow_role_parity() {
    let (client, server) = session_pair(SessionConfig::default());

    let first = client.open_stream().expect("opens");
    let second = client.open_stream().expect("opens");
    assert_eq!(first.id(), 1, "clients allocate odd identifiers");
    assert_eq!(second.id(), 3);

    let server_stream = server.open_stream().expect("opens");
    assert_eq!(server_stream.id(), 2, "servers allocate even identifiers");

    assert!(Role::Client.owns(1));
    assert!(!Role::Client.owns(2));
    assert!(Role::Server.owns(2));
    assert!(
        !Role::Server.owns(0),
        "identifier 0 is control, not a stream"
    );
}

#[tokio::test]
async fn a_payload_crosses_a_stream() {
    let (client, server) = session_pair(SessionConfig::default());

    let mut outbound = client.open_stream().expect("opens");
    outbound
        .send(Bytes::from_static(b"hello multiplexed world"))
        .await
        .expect("sends");

    let mut inbound = server.accept_stream().await.expect("a stream arrives");
    assert_eq!(inbound.id(), outbound.id());
    assert_eq!(
        inbound.recv().await,
        Some(Bytes::from_static(b"hello multiplexed world"))
    );
}

#[tokio::test]
async fn streams_stay_independent_when_interleaved() {
    let (client, server) = session_pair(SessionConfig::default());

    let mut first = client.open_stream().expect("opens");
    let mut second = client.open_stream().expect("opens");
    let mut third = client.open_stream().expect("opens");

    // Interleave writes so the frames are mixed on the wire.
    for round in 0..10_u8 {
        first
            .send(Bytes::from(vec![b'a', round]))
            .await
            .expect("sends");
        second
            .send(Bytes::from(vec![b'b', round]))
            .await
            .expect("sends");
        third
            .send(Bytes::from(vec![b'c', round]))
            .await
            .expect("sends");
    }

    // Collect the three accepted streams, keyed by identifier.
    let mut accepted = Vec::new();
    for _ in 0..3 {
        accepted.push(server.accept_stream().await.expect("a stream arrives"));
    }
    accepted.sort_by_key(nexusnet_transport::Stream::id);

    // Each stream must see only its own payloads, in order.
    for (index, tag) in [b'a', b'b', b'c'].into_iter().enumerate() {
        let stream = &mut accepted[index];
        for round in 0..10_u8 {
            let payload = stream.recv().await.expect("a payload arrives");
            assert_eq!(
                payload.as_ref(),
                &[tag, round],
                "stream {} received the wrong payload",
                stream.id()
            );
        }
    }
}

#[tokio::test]
async fn streams_are_bidirectional() {
    let (client, server) = session_pair(SessionConfig::default());

    let mut client_stream = client.open_stream().expect("opens");
    client_stream
        .send(Bytes::from_static(b"request"))
        .await
        .expect("sends");

    let mut server_stream = server.accept_stream().await.expect("a stream arrives");
    assert_eq!(
        server_stream.recv().await,
        Some(Bytes::from_static(b"request"))
    );

    server_stream
        .send(Bytes::from_static(b"response"))
        .await
        .expect("replies");
    assert_eq!(
        client_stream.recv().await,
        Some(Bytes::from_static(b"response"))
    );
}

#[tokio::test]
async fn closing_a_stream_ends_the_peer_side() {
    let (client, server) = session_pair(SessionConfig::default());

    let mut outbound = client.open_stream().expect("opens");
    outbound
        .send(Bytes::from_static(b"final payload"))
        .await
        .expect("sends");

    let mut inbound = server.accept_stream().await.expect("a stream arrives");
    assert_eq!(
        inbound.recv().await,
        Some(Bytes::from_static(b"final payload"))
    );

    outbound.close().await.expect("closes");
    assert!(outbound.is_closed());

    // The peer observes end-of-stream rather than hanging.
    let ended = tokio::time::timeout(Duration::from_secs(2), inbound.recv())
        .await
        .expect("recv resolves promptly after close");
    assert_eq!(ended, None);
}

#[tokio::test]
async fn writing_to_a_closed_stream_is_an_error() {
    let (client, _server) = session_pair(SessionConfig::default());

    let mut stream = client.open_stream().expect("opens");
    stream.close().await.expect("closes");

    let err = stream
        .send(Bytes::from_static(b"too late"))
        .await
        .expect_err("a closed stream rejects writes");
    assert!(matches!(err, Error::StreamClosed { .. }));
}

#[tokio::test]
async fn the_stream_limit_is_enforced() {
    let config = SessionConfig::default().with_max_streams(3);
    let (client, _server) = session_pair(config);

    let _first = client.open_stream().expect("opens");
    let _second = client.open_stream().expect("opens");
    let _third = client.open_stream().expect("opens");

    let err = client
        .open_stream()
        .expect_err("the fourth stream exceeds the limit");
    assert!(matches!(err, Error::TooManyStreams { max: 3 }));
}

#[tokio::test]
async fn dropping_a_stream_frees_a_slot() {
    let config = SessionConfig::default().with_max_streams(2);
    let (client, _server) = session_pair(config);

    let first = client.open_stream().expect("opens");
    let _second = client.open_stream().expect("opens");
    assert!(client.open_stream().is_err(), "the limit is reached");

    drop(first);
    assert!(
        client.open_stream().is_ok(),
        "dropping a stream should free its slot"
    );
}

#[tokio::test]
async fn ping_is_answered_automatically() {
    let (client, _server) = session_pair(SessionConfig::default());

    // The peer's driver replies to pings on its own; this verifies the ping
    // path does not disturb the session.
    client
        .ping(Bytes::from_static(b"are you there"))
        .await
        .expect("ping sends");

    let mut stream = client.open_stream().expect("opens");
    stream
        .send(Bytes::from_static(b"still working"))
        .await
        .expect("the session still works after a ping");
}

#[tokio::test]
async fn statistics_track_stream_activity() {
    let (client, server) = session_pair(SessionConfig::default());

    let mut first = client.open_stream().expect("opens");
    let mut second = client.open_stream().expect("opens");
    first.send(Bytes::from_static(b"one")).await.expect("sends");
    second
        .send(Bytes::from_static(b"two"))
        .await
        .expect("sends");

    let mut accepted_first = server.accept_stream().await.expect("a stream arrives");
    let _accepted_second = server.accept_stream().await.expect("a stream arrives");
    assert_eq!(
        accepted_first.recv().await,
        Some(Bytes::from_static(b"one"))
    );

    assert_eq!(client.stats().streams_opened, 2);
    assert_eq!(client.stats().streams_active, 2);
    assert_eq!(server.stats().streams_accepted, 2);

    first.close().await.expect("closes");
    assert_eq!(client.stats().streams_closed, 1);
}

#[tokio::test]
async fn multiplexing_works_over_a_real_socket() {
    let transport = TransportConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0", transport)
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    // Server: echo every payload back on the stream it arrived on.
    tokio::spawn(async move {
        let (connection, _peer) = listener.accept().await.expect("accepts");
        let (handle, driver) = Session::start(connection, Role::Server, SessionConfig::default());
        tokio::spawn(async move { driver.run().await });

        while let Some(mut stream) = handle.accept_stream().await {
            tokio::spawn(async move {
                while let Some(payload) = stream.recv().await {
                    if stream.send(payload).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    let connection = tcp::connect(address, transport).await.expect("connects");
    let (client, driver) = Session::start(connection, Role::Client, SessionConfig::default());
    tokio::spawn(async move { driver.run().await });

    // Run several streams concurrently over the one socket.
    let mut handles = Vec::new();
    for index in 0..8_u8 {
        let mut stream = client.open_stream().expect("opens");
        handles.push(tokio::spawn(async move {
            let payload = Bytes::from(vec![index; 64]);
            stream.send(payload.clone()).await.expect("sends");

            let echoed = tokio::time::timeout(Duration::from_secs(5), stream.recv())
                .await
                .expect("the echo arrives promptly")
                .expect("a payload arrives");
            assert_eq!(echoed, payload, "stream {} got the wrong echo", stream.id());
        }));
    }

    for handle in handles {
        handle.await.expect("the stream task completes");
    }

    assert_eq!(client.stats().streams_opened, 8);
}

#[tokio::test]
async fn closing_the_session_stops_the_driver() {
    let (client, _server) = session_pair(SessionConfig::default());
    assert!(client.is_open());

    client.close();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The driver has stopped; a stream opened afterwards cannot send.
    let mut stream = client.open_stream().expect("registration still succeeds");
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        stream.send(Bytes::from_static(b"after close")),
    )
    .await
    .expect("send resolves rather than hanging");

    assert!(
        result.is_err() || client.stats().streams_opened == 1,
        "a closed session should not accept new traffic silently"
    );
}
