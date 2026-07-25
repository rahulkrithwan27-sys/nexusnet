//! Integration tests for the server and its engine lifecycle binding.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use nexusnet_core::{Engine, EngineState};
use nexusnet_protocol::{Frame, FrameType};
use nexusnet_transport::{tcp, Server, ServerConfig, TcpConnection, TransportConfig};

fn frame(payload: &'static [u8]) -> Frame {
    Frame::new(FrameType::Data, 1, Bytes::from_static(payload)).expect("payload fits in u32")
}

/// An echo handler, the common shape of a connection handler.
async fn echo(mut connection: TcpConnection, _peer: SocketAddr) {
    while let Ok(Some(frame)) = connection.recv().await {
        if connection.send(&frame).await.is_err() {
            break;
        }
    }
}

#[tokio::test]
async fn the_server_serves_and_reports_stats() {
    let engine = Engine::builder().name("test").build().expect("builds");
    let server = Server::bind("127.0.0.1:0", ServerConfig::default())
        .await
        .expect("binds");
    let address = server.local_addr().expect("has an address");
    let handle = server.handle();

    let task = tokio::spawn(server.serve(engine, echo));

    let mut client = tcp::connect(address, TransportConfig::default())
        .await
        .expect("connects");
    client.send(&frame(b"hello")).await.expect("sends");
    assert_eq!(
        client
            .recv()
            .await
            .expect("no error")
            .expect("echo")
            .payload()
            .as_ref(),
        b"hello"
    );
    drop(client);

    handle.shutdown();
    let stats = task.await.expect("task completes").expect("serves cleanly");

    assert_eq!(stats.accepted, 1);
    assert_eq!(stats.rejected, 0);
    assert_eq!(stats.peak_active, 1);
}

#[tokio::test]
async fn the_engine_lifecycle_brackets_the_server() {
    let engine = Engine::builder().name("lifecycle").build().expect("builds");
    assert_eq!(engine.state(), EngineState::Created);

    let server = Server::bind("127.0.0.1:0", ServerConfig::default())
        .await
        .expect("binds");
    let handle = server.handle();

    // The engine handle is cheap to clone and shares state, so the test can
    // observe what the server does to it.
    let observer = engine.clone();
    let task = tokio::spawn(server.serve(engine, echo));

    // Give the server a moment to start the engine.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(observer.is_running(), "serving should start the engine");

    handle.shutdown();
    task.await.expect("task completes").expect("serves cleanly");

    assert_eq!(
        observer.state(),
        EngineState::Stopped,
        "the server should shut the engine down when it stops"
    );
}

#[tokio::test]
async fn an_already_running_engine_is_accepted() {
    let engine = Engine::builder().name("running").build().expect("builds");
    engine.start().expect("starts");

    let server = Server::bind("127.0.0.1:0", ServerConfig::default())
        .await
        .expect("binds");
    let handle = server.handle();
    let observer = engine.clone();

    let task = tokio::spawn(server.serve(engine, echo));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(observer.is_running());

    handle.shutdown();
    task.await.expect("task completes").expect("serves cleanly");
}

#[tokio::test]
async fn a_shut_down_engine_is_rejected() {
    let engine = Engine::builder().name("stopped").build().expect("builds");
    engine.start().expect("starts");
    engine.shutdown().expect("shuts down");

    let server = Server::bind("127.0.0.1:0", ServerConfig::default())
        .await
        .expect("binds");

    let result = server.serve(engine, echo).await;
    assert!(
        result.is_err(),
        "serving with a stopped engine must fail rather than silently proceed"
    );
}

#[tokio::test]
async fn several_clients_are_served_concurrently() {
    let engine = Engine::builder()
        .name("concurrent")
        .build()
        .expect("builds");
    let server = Server::bind("127.0.0.1:0", ServerConfig::default())
        .await
        .expect("binds");
    let address = server.local_addr().expect("has an address");
    let handle = server.handle();

    let task = tokio::spawn(server.serve(engine, echo));

    let mut clients = Vec::new();
    for _ in 0..8 {
        let mut client = tcp::connect(address, TransportConfig::default())
            .await
            .expect("connects");
        client.send(&frame(b"concurrent")).await.expect("sends");
        clients.push(client);
    }

    for client in &mut clients {
        let echoed = client.recv().await.expect("no error").expect("echo");
        assert_eq!(echoed.payload().as_ref(), b"concurrent");
    }

    assert_eq!(handle.stats().accepted, 8);
    assert_eq!(handle.stats().peak_active, 8);

    drop(clients);
    handle.shutdown();
    let stats = task.await.expect("task completes").expect("serves cleanly");
    assert_eq!(stats.accepted, 8);
}

#[tokio::test]
async fn the_connection_limit_is_enforced() {
    let engine = Engine::builder().name("limited").build().expect("builds");
    let config = ServerConfig::default().with_max_connections(2);

    let server = Server::bind("127.0.0.1:0", config).await.expect("binds");
    let address = server.local_addr().expect("has an address");
    let handle = server.handle();

    let task = tokio::spawn(server.serve(engine, echo));

    // Two connections that stay open occupy the whole budget.
    let mut held = Vec::new();
    for _ in 0..2 {
        let mut client = tcp::connect(address, TransportConfig::default())
            .await
            .expect("connects");
        client.send(&frame(b"holding")).await.expect("sends");
        client.recv().await.expect("no error").expect("echo");
        held.push(client);
    }

    // A third is accepted by the OS but refused by the server.
    let mut extra = tcp::connect(address, TransportConfig::default())
        .await
        .expect("the OS accepts");
    let _ = extra.send(&frame(b"over the limit")).await;

    // The server closed it, so reading ends rather than echoing.
    let result = tokio::time::timeout(Duration::from_secs(2), extra.recv()).await;
    match result {
        Ok(Ok(None) | Err(_)) => {}
        other => panic!("a refused connection should close, got {other:?}"),
    }

    assert_eq!(handle.stats().rejected, 1);
    assert_eq!(handle.stats().active, 2);

    drop(held);
    handle.shutdown();
    let stats = task.await.expect("task completes").expect("serves cleanly");
    assert_eq!(stats.rejected, 1);
}

#[tokio::test]
async fn graceful_shutdown_waits_for_in_flight_work() {
    let finished = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&finished);

    let engine = Engine::builder().name("graceful").build().expect("builds");
    let config = ServerConfig::default().with_shutdown_timeout(Duration::from_secs(5));
    let server = Server::bind("127.0.0.1:0", config).await.expect("binds");
    let address = server.local_addr().expect("has an address");
    let handle = server.handle();

    // A handler that keeps working briefly after the connection arrives.
    let task = tokio::spawn(
        server.serve(engine, move |mut connection: TcpConnection, _peer| {
            let counter = Arc::clone(&counter);
            async move {
                if let Ok(Some(frame)) = connection.recv().await {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let _ = connection.send(&frame).await;
                }
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }),
    );

    let mut client = tcp::connect(address, TransportConfig::default())
        .await
        .expect("connects");
    client.send(&frame(b"slow work")).await.expect("sends");

    // Shut down while the handler is still working.
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.shutdown();

    let stats = task.await.expect("task completes").expect("serves cleanly");

    assert_eq!(
        finished.load(Ordering::SeqCst),
        1,
        "in-flight work should be allowed to finish"
    );
    assert_eq!(stats.abandoned, 0, "nothing should be abandoned");
}

#[tokio::test]
async fn the_grace_period_is_bounded() {
    let engine = Engine::builder().name("bounded").build().expect("builds");
    let config = ServerConfig::default().with_shutdown_timeout(Duration::from_millis(100));
    let server = Server::bind("127.0.0.1:0", config).await.expect("binds");
    let address = server.local_addr().expect("has an address");
    let handle = server.handle();

    // A handler that never finishes on its own.
    let task = tokio::spawn(server.serve(
        engine,
        |mut connection: TcpConnection, _peer| async move {
            let _ = connection.recv().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        },
    ));

    let mut client = tcp::connect(address, TransportConfig::default())
        .await
        .expect("connects");
    client.send(&frame(b"never finishes")).await.expect("sends");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = std::time::Instant::now();
    handle.shutdown();
    let stats = task.await.expect("task completes").expect("serves cleanly");
    let elapsed = started.elapsed();

    assert_eq!(stats.abandoned, 1, "the stuck connection is reported");
    assert!(
        elapsed < Duration::from_secs(5),
        "shutdown must not wait indefinitely, took {elapsed:?}"
    );
}

#[tokio::test]
async fn the_server_uses_the_engine_shutdown_timeout_by_default() {
    // No explicit server timeout, so the engine's value applies.
    let engine = Engine::builder()
        .name("inherited")
        .shutdown_timeout(Duration::from_millis(100))
        .build()
        .expect("builds");

    let server = Server::bind("127.0.0.1:0", ServerConfig::default())
        .await
        .expect("binds");
    let address = server.local_addr().expect("has an address");
    let handle = server.handle();

    let task = tokio::spawn(server.serve(
        engine,
        |mut connection: TcpConnection, _peer| async move {
            let _ = connection.recv().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        },
    ));

    let mut client = tcp::connect(address, TransportConfig::default())
        .await
        .expect("connects");
    client.send(&frame(b"stuck")).await.expect("sends");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = std::time::Instant::now();
    handle.shutdown();
    let stats = task.await.expect("task completes").expect("serves cleanly");

    assert_eq!(stats.abandoned, 1);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the engine's 100ms timeout should have applied"
    );
}
