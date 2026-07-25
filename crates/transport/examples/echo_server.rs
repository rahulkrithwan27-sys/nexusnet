//! A complete NexusNet echo server and client.
//!
//! Demonstrates the whole stack end to end: an engine lifecycle, a server with
//! a connection cap and graceful shutdown, framed connections, and a client
//! that reconnects with backoff.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p nexusnet-transport --example echo_server
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use nexusnet_core::{Engine, LogLevel};
use nexusnet_protocol::{Frame, FrameType};
use nexusnet_transport::{
    connect_with_retry, ReconnectPolicy, Server, ServerConfig, TcpConnection, TransportConfig,
};

/// Echoes every frame back to the peer until it hangs up.
async fn echo(mut connection: TcpConnection, peer: SocketAddr) {
    println!("  [server] {peer} connected");

    loop {
        match connection.recv().await {
            Ok(Some(frame)) => {
                let payload = String::from_utf8_lossy(frame.payload()).to_string();
                println!(
                    "  [server] received {payload:?} on stream {}",
                    frame.header().stream_id
                );

                if connection.send(&frame).await.is_err() {
                    break;
                }
            }
            Ok(None) => {
                println!("  [server] {peer} disconnected cleanly");
                break;
            }
            Err(error) => {
                eprintln!("  [server] {peer} failed: {error}");
                break;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The engine owns configuration and lifecycle; the server binds to it.
    let engine = Engine::builder()
        .name("echo-server")
        .log_level(LogLevel::Info)
        .install_logging(true)
        .shutdown_timeout(Duration::from_secs(5))
        .build()?;

    println!("engine: {} ({})", engine.config().name, engine.state());

    let config = ServerConfig::default().with_max_connections(64);
    let server = Server::bind("127.0.0.1:0", config).await?;
    let address = server.local_addr()?;
    let handle = server.handle();

    println!("listening on {address}\n");

    let server_task = tokio::spawn(server.serve(engine, echo));

    // A client that tolerates the server not being ready yet.
    let policy = ReconnectPolicy::new()
        .with_max_attempts(Some(5))
        .with_initial_delay(Duration::from_millis(50));
    let mut client = connect_with_retry(address, TransportConfig::default(), policy).await?;

    for (index, message) in ["hello", "multiplexed", "world"].into_iter().enumerate() {
        let frame = Frame::new(
            FrameType::Data,
            u32::try_from(index + 1)?,
            Bytes::from(message),
        )?;

        client.send(&frame).await?;
        let echoed = client.recv().await?.ok_or("server closed early")?;
        println!(
            "  [client] echoed {:?}",
            String::from_utf8_lossy(echoed.payload())
        );
    }

    client.shutdown().await?;
    drop(client);

    // Stop the server; in-flight connections get the grace period.
    handle.shutdown();
    let stats = server_task.await??;

    println!(
        "\nserver stopped: {} accepted, {} rejected, peak {} concurrent",
        stats.accepted, stats.rejected, stats.peak_active
    );

    Ok(())
}
