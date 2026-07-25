//! Integration tests for connection pooling and reconnection.

use std::time::Duration;

use bytes::Bytes;
use nexusnet_protocol::{Frame, FrameType};
use nexusnet_transport::{
    connect_with_retry, ConnectionPool, PoolConfig, ReconnectPolicy, TcpListener, TransportConfig,
};

fn frame(stream_id: u32) -> Frame {
    Frame::new(FrameType::Data, stream_id, Bytes::from_static(b"payload"))
        .expect("payload fits in u32")
}

/// Spawns an echo server and returns its address.
async fn echo_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0", TransportConfig::default())
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    tokio::spawn(async move {
        loop {
            let Ok((mut connection, _peer)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                while let Ok(Some(frame)) = connection.recv().await {
                    if connection.send(&frame).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    address
}

#[tokio::test]
async fn pool_reuses_a_connection() {
    let address = echo_server().await;
    let pool = ConnectionPool::new(address, PoolConfig::default());

    for _ in 0..5 {
        let mut connection = pool.get().await.expect("checkout succeeds");
        connection.send(&frame(1)).await.expect("sends");
        connection.recv().await.expect("no error").expect("echo");
    }

    let stats = pool.stats();
    assert_eq!(stats.created, 1, "only the first checkout should dial");
    assert_eq!(stats.reused, 4);
    assert_eq!(stats.idle, 1, "the connection returns to the pool");
    assert!((stats.reuse_ratio() - 0.8).abs() < 1e-9);
}

#[tokio::test]
async fn concurrent_checkouts_create_separate_connections() {
    let address = echo_server().await;
    let pool = ConnectionPool::new(address, PoolConfig::default());

    // Hold three connections at once, so none can be reused.
    let mut held = Vec::new();
    for i in 0..3 {
        let mut connection = pool.get().await.expect("checkout succeeds");
        connection.send(&frame(i)).await.expect("sends");
        connection.recv().await.expect("no error").expect("echo");
        held.push(connection);
    }

    assert_eq!(pool.stats().created, 3);
    assert_eq!(pool.stats().idle, 0);

    drop(held);
    assert_eq!(pool.stats().idle, 3, "all three return to the pool");

    // A later checkout reuses rather than dialing.
    let _reused = pool.get().await.expect("checkout succeeds");
    assert_eq!(pool.stats().created, 3);
    assert_eq!(pool.stats().reused, 1);
}

#[tokio::test]
async fn pool_respects_its_idle_capacity() {
    let address = echo_server().await;
    let config = PoolConfig::default().with_max_idle_connections(2);
    let pool = ConnectionPool::new(address, config);

    let mut held = Vec::new();
    for i in 0..5 {
        let mut connection = pool.get().await.expect("checkout succeeds");
        connection.send(&frame(i)).await.expect("sends");
        connection.recv().await.expect("no error").expect("echo");
        held.push(connection);
    }
    drop(held);

    let stats = pool.stats();
    assert_eq!(stats.idle, 2, "only two connections are retained");
    assert_eq!(stats.discarded, 3, "the rest are closed");
}

#[tokio::test]
async fn idle_connections_expire() {
    let address = echo_server().await;
    let config = PoolConfig::default().with_max_idle_time(Duration::from_millis(50));
    let pool = ConnectionPool::new(address, config);

    {
        let mut connection = pool.get().await.expect("checkout succeeds");
        connection.send(&frame(1)).await.expect("sends");
        connection.recv().await.expect("no error").expect("echo");
    }
    assert_eq!(pool.stats().idle, 1);

    tokio::time::sleep(Duration::from_millis(120)).await;

    // The stale connection is discarded and a fresh one dialed.
    let _connection = pool.get().await.expect("checkout succeeds");
    let stats = pool.stats();
    assert_eq!(stats.expired, 1, "the stale connection is dropped");
    assert_eq!(stats.created, 2, "a replacement is dialed");
    assert_eq!(stats.reused, 0);
}

#[tokio::test]
async fn a_closed_peer_connection_is_not_reused() {
    let listener = TcpListener::bind("127.0.0.1:0", TransportConfig::default())
        .await
        .expect("binds");
    let address = listener.local_addr().expect("has an address");

    // A server that accepts one frame, replies, then hangs up.
    tokio::spawn(async move {
        while let Ok((mut connection, _peer)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(Some(frame)) = connection.recv().await {
                    let _ = connection.send(&frame).await;
                }
                let _ = connection.shutdown().await;
            });
        }
    });

    let pool = ConnectionPool::new(address, PoolConfig::default());

    {
        let mut connection = pool.get().await.expect("checkout succeeds");
        connection.send(&frame(1)).await.expect("sends");
        connection.recv().await.expect("no error").expect("echo");

        // The peer has closed; observing end-of-stream marks it unusable.
        assert!(connection.recv().await.expect("clean eof").is_none());
        assert!(connection.is_broken(), "a closed peer must not be pooled");
    }

    assert_eq!(pool.stats().idle, 0, "the dead connection is not retained");
    assert_eq!(pool.stats().discarded, 1);
}

#[tokio::test]
async fn explicitly_discarded_connections_are_not_reused() {
    let address = echo_server().await;
    let pool = ConnectionPool::new(address, PoolConfig::default());

    {
        let mut connection = pool.get().await.expect("checkout succeeds");
        connection.send(&frame(1)).await.expect("sends");
        connection.recv().await.expect("no error").expect("echo");
        connection.discard();
    }

    assert_eq!(pool.stats().idle, 0);
    assert_eq!(pool.stats().discarded, 1);
}

#[tokio::test]
async fn clearing_the_pool_closes_idle_connections() {
    let address = echo_server().await;
    let pool = ConnectionPool::new(address, PoolConfig::default());

    let mut held = Vec::new();
    for i in 0..3 {
        held.push(pool.get().await.expect("checkout succeeds"));
        let _ = i;
    }
    drop(held);
    assert_eq!(pool.stats().idle, 3);

    pool.clear();
    assert_eq!(pool.stats().idle, 0);
    assert_eq!(pool.stats().discarded, 3);
}

#[tokio::test]
async fn retry_gives_up_after_the_configured_attempts() {
    // Reserved TEST-NET-1: connections are discarded, not refused.
    let policy = ReconnectPolicy::new()
        .with_max_attempts(Some(3))
        .with_initial_delay(Duration::from_millis(10))
        .with_max_delay(Duration::from_millis(30));
    let config = TransportConfig::default().with_connect_timeout(Duration::from_millis(50));

    let started = std::time::Instant::now();
    let result = connect_with_retry("192.0.2.1:9", config, policy).await;
    let elapsed = started.elapsed();

    assert!(result.is_err(), "an unreachable address must fail");
    assert!(
        elapsed < Duration::from_secs(5),
        "bounded retries should fail quickly, took {elapsed:?}"
    );
}

#[tokio::test]
async fn retry_succeeds_once_the_server_appears() {
    // Reserve a port, then release it so the first dial fails.
    let placeholder = TcpListener::bind("127.0.0.1:0", TransportConfig::default())
        .await
        .expect("binds");
    let address = placeholder.local_addr().expect("has an address");
    drop(placeholder);

    // Bring the real server up shortly after the client starts dialing.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(120)).await;
        if let Ok(listener) = TcpListener::bind(address, TransportConfig::default()).await {
            let _ = listener.accept().await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });

    let policy = ReconnectPolicy::new()
        .with_max_attempts(Some(20))
        .with_initial_delay(Duration::from_millis(20))
        .with_max_delay(Duration::from_millis(60));

    let result = connect_with_retry(address, TransportConfig::default(), policy).await;
    assert!(
        result.is_ok(),
        "retry should succeed once the server binds: {result:?}"
    );
}

#[tokio::test]
async fn a_never_retry_policy_fails_immediately() {
    let policy = ReconnectPolicy::never();
    let config = TransportConfig::default().with_connect_timeout(Duration::from_millis(100));

    let started = std::time::Instant::now();
    let result = connect_with_retry("192.0.2.1:9", config, policy).await;

    assert!(result.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "no retries means no backoff delay"
    );
}
