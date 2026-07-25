//! A framed echo client and server running over TLS, end to end.
//!
//! Run with the `tls` feature (requires Rust 1.85):
//!
//! ```text
//! cargo run -p nexusnet-transport-tls --example echo
//! ```
//!
//! It runs both ends in one process against a self-signed certificate, so it
//! needs no setup and demonstrates the whole path: TCP, TLS 1.3 handshake,
//! framed send and receive.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use nexusnet_protocol::{Frame, FrameType};
    use nexusnet_tls::TlsConfigBuilder;
    use nexusnet_transport::TransportConfig;
    use nexusnet_transport_tls::{connect_tls, TlsListener};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls::RootCertStore;
    use tokio::net::TcpListener;

    // A self-signed certificate for localhost.
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let certificate = CertificateDer::from(generated.cert.der().to_vec());
    let key = PrivateKeyDer::try_from(generated.key_pair.serialize_der())?;

    let server_config = TlsConfigBuilder::new().build_server(vec![certificate.clone()], key)?;

    let mut roots = RootCertStore::empty();
    roots.add(certificate)?;
    let client_config = TlsConfigBuilder::new().build_client_with_roots(roots)?;

    let tcp = TcpListener::bind("127.0.0.1:0").await?;
    let address = tcp.local_addr()?;
    println!("listening on {address}");

    let listener = TlsListener::new(server_config, TransportConfig::default());
    tokio::spawn(async move {
        if let Ok((socket, peer)) = tcp.accept().await {
            match listener.accept(socket).await {
                Ok(mut connection) => {
                    println!("[server] TLS handshake succeeded with {peer}");
                    while let Ok(Some(frame)) = connection.recv().await {
                        println!(
                            "[server] echoing {} bytes on stream {}",
                            frame.payload().len(),
                            frame.header().stream_id
                        );
                        if connection.send(&frame).await.is_err() {
                            break;
                        }
                    }
                }
                Err(error) => println!("[server] handshake rejected: {error}"),
            }
        }
    });

    let mut connection = connect_tls(
        address,
        "localhost",
        client_config,
        TransportConfig::default(),
    )
    .await?;
    println!("[client] TLS handshake succeeded");

    for message in ["hello", "over", "TLS"] {
        let frame = Frame::new(
            FrameType::Data,
            1,
            bytes::Bytes::copy_from_slice(message.as_bytes()),
        )?;
        connection.send(&frame).await?;
        let echoed = connection.recv().await?.expect("a frame comes back");
        println!(
            "[client] sent {:?}, echoed {:?}",
            message,
            String::from_utf8_lossy(echoed.payload())
        );
    }

    println!("done — a full framed exchange over TLS 1.3");
    Ok(())
}
