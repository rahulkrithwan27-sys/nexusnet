//! End-to-end test of the `tls` feature: a framed connection over real TLS.
//!
//! Exercises `nexusnet-transport`'s `tls` feature together with
//! `nexusnet-tls`. Lives in this crate because it needs both, and the 1.85
//! toolchain the TLS stack requires.

use nexusnet_protocol::{Frame, FrameType};
use nexusnet_tls::TlsConfigBuilder;
use nexusnet_transport::tls::{connect_tls, TlsListener};
use nexusnet_transport::TransportConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::RootCertStore;
use tokio::net::TcpListener;

fn certificate_for(name: &str) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let generated =
        rcgen::generate_simple_self_signed(vec![name.to_owned()]).expect("generates a certificate");
    let certificate = CertificateDer::from(generated.cert.der().to_vec());
    let key =
        PrivateKeyDer::try_from(generated.key_pair.serialize_der()).expect("the key is valid");
    (vec![certificate], key)
}

fn roots_trusting(certificates: &[CertificateDer<'static>]) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    for certificate in certificates {
        roots.add(certificate.clone()).expect("well formed");
    }
    roots
}

#[tokio::test]
async fn a_framed_connection_works_over_tls() {
    let (certificates, key) = certificate_for("localhost");

    let server_config = TlsConfigBuilder::new()
        .build_server(certificates.clone(), key)
        .expect("builds server config");
    let client_config = TlsConfigBuilder::new()
        .build_client_with_roots(roots_trusting(&certificates))
        .expect("builds client config");

    let tcp = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = tcp.local_addr().expect("has an address");

    let listener = TlsListener::new(server_config, TransportConfig::default());

    // Server: accept one TLS connection, echo one frame.
    let server = tokio::spawn(async move {
        let (socket, _peer) = tcp.accept().await.expect("accepts TCP");
        let mut connection = listener
            .accept(socket)
            .await
            .expect("TLS handshake succeeds");

        let frame = connection
            .recv()
            .await
            .expect("receives")
            .expect("a frame arrives");
        connection.send(&frame).await.expect("echoes");
    });

    // Client: connect over TLS, send a frame, read it back.
    let mut connection = connect_tls(
        address,
        "localhost",
        client_config,
        TransportConfig::default(),
    )
    .await
    .expect("TLS handshake succeeds");

    let payload = bytes::Bytes::from_static(b"secured payload");
    let frame = Frame::new(FrameType::Data, 1, payload.clone()).expect("builds a frame");
    connection.send(&frame).await.expect("sends");

    let echoed = connection
        .recv()
        .await
        .expect("receives")
        .expect("a frame arrives");
    assert_eq!(
        echoed.payload(),
        &payload,
        "the payload survives the TLS round trip"
    );

    server.await.expect("the server task completes");
}

#[tokio::test]
async fn an_untrusted_server_is_rejected_by_the_transport() {
    // The security property, at the transport layer: a client that does not
    // trust the server's certificate must fail to connect, so an intercepted
    // connection never carries a single frame.
    let (server_certificates, key) = certificate_for("localhost");
    let (other_certificates, _) = certificate_for("localhost");

    let server_config = TlsConfigBuilder::new()
        .build_server(server_certificates, key)
        .expect("builds");
    let client_config = TlsConfigBuilder::new()
        .build_client_with_roots(roots_trusting(&other_certificates))
        .expect("builds");

    let tcp = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = tcp.local_addr().expect("has an address");

    let listener = TlsListener::new(server_config, TransportConfig::default());
    tokio::spawn(async move {
        if let Ok((socket, _)) = tcp.accept().await {
            let _ = listener.accept(socket).await;
        }
    });

    let result = connect_tls(
        address,
        "localhost",
        client_config,
        TransportConfig::default(),
    )
    .await;

    assert!(
        result.is_err(),
        "a framed connection must never form over an unverified TLS session"
    );
}
