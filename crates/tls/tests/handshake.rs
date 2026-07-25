//! Integration tests for the TLS layer.
//!
//! These are the tests that matter for security. A round trip proving data
//! flows is table stakes; what needs proving is that the *wrong* peer is
//! rejected, and that the two sides derive matching keys.

use nexusnet_encryption::Direction;
use nexusnet_tls::{
    export_key_client, export_key_server, session_info_client, TlsAcceptor, TlsConfigBuilder,
    TlsConnector,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::RootCertStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Generates a self-signed certificate for `name`.
fn certificate_for(name: &str) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let generated =
        rcgen::generate_simple_self_signed(vec![name.to_owned()]).expect("generates a certificate");

    let certificate = CertificateDer::from(generated.cert.der().to_vec());
    let key = PrivateKeyDer::try_from(generated.key_pair.serialize_der())
        .expect("the generated key is valid");

    (vec![certificate], key)
}

/// Builds a client root store trusting exactly `certificates`.
fn roots_trusting(certificates: &[CertificateDer<'static>]) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    for certificate in certificates {
        roots
            .add(certificate.clone())
            .expect("the certificate is well formed");
    }
    roots
}

/// Runs an echo server, returning its address.
async fn spawn_echo_server(acceptor: TlsAcceptor) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = listener.local_addr().expect("has an address");

    tokio::spawn(async move {
        while let Ok((socket, _peer)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    // A rejected handshake is the expected outcome in several
                    // of these tests.
                    return;
                };

                let mut buffer = vec![0_u8; 1024];
                while let Ok(read) = stream.read(&mut buffer).await {
                    if read == 0 || stream.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    address
}

#[tokio::test]
async fn a_trusted_certificate_completes_the_handshake() {
    let (certificates, key) = certificate_for("localhost");
    let server_config = TlsConfigBuilder::new()
        .build_server(certificates.clone(), key)
        .expect("builds");
    let client_config = TlsConfigBuilder::new()
        .build_client_with_roots(roots_trusting(&certificates))
        .expect("builds");

    let address = spawn_echo_server(TlsAcceptor::new(server_config)).await;
    let socket = TcpStream::connect(address).await.expect("connects");

    let mut stream = TlsConnector::new(client_config)
        .connect("localhost", socket)
        .await
        .expect("the handshake succeeds against a trusted certificate");

    stream.write_all(b"hello over TLS").await.expect("writes");
    let mut buffer = vec![0_u8; 64];
    let read = stream.read(&mut buffer).await.expect("reads");

    assert_eq!(&buffer[..read], b"hello over TLS");
}

#[tokio::test]
async fn an_untrusted_certificate_is_rejected() {
    // This is the test that matters: it is what stops interception.
    let (server_certificates, key) = certificate_for("localhost");
    let (other_certificates, _) = certificate_for("localhost");

    let server_config = TlsConfigBuilder::new()
        .build_server(server_certificates, key)
        .expect("builds");

    // The client trusts a *different* certificate for the same name — exactly
    // what an interceptor presents.
    let client_config = TlsConfigBuilder::new()
        .build_client_with_roots(roots_trusting(&other_certificates))
        .expect("builds");

    let address = spawn_echo_server(TlsAcceptor::new(server_config)).await;
    let socket = TcpStream::connect(address).await.expect("connects");

    let result = TlsConnector::new(client_config)
        .connect("localhost", socket)
        .await;

    assert!(
        result.is_err(),
        "an unverified certificate must fail the handshake, or interception is trivial"
    );
    assert!(
        result.unwrap_err().is_security_relevant(),
        "the failure should be flagged as security relevant"
    );
}

#[tokio::test]
async fn a_mismatched_hostname_is_rejected() {
    let (certificates, key) = certificate_for("intended.example");
    let server_config = TlsConfigBuilder::new()
        .build_server(certificates.clone(), key)
        .expect("builds");
    let client_config = TlsConfigBuilder::new()
        .build_client_with_roots(roots_trusting(&certificates))
        .expect("builds");

    let address = spawn_echo_server(TlsAcceptor::new(server_config)).await;
    let socket = TcpStream::connect(address).await.expect("connects");

    // The certificate is trusted, but it is not for this name.
    let result = TlsConnector::new(client_config)
        .connect("attacker.example", socket)
        .await;

    assert!(
        result.is_err(),
        "a trusted certificate for the wrong name must still be refused"
    );
}

#[tokio::test]
async fn the_session_negotiates_tls13_and_the_expected_alpn() {
    let (certificates, key) = certificate_for("localhost");
    let server_config = TlsConfigBuilder::new()
        .build_server(certificates.clone(), key)
        .expect("builds");
    let client_config = TlsConfigBuilder::new()
        .build_client_with_roots(roots_trusting(&certificates))
        .expect("builds");

    let address = spawn_echo_server(TlsAcceptor::new(server_config)).await;
    let socket = TcpStream::connect(address).await.expect("connects");

    let stream = TlsConnector::new(client_config)
        .connect("localhost", socket)
        .await
        .expect("handshake succeeds");

    let info = session_info_client(&stream);
    assert!(
        info.is_tls13(),
        "a silent downgrade is what a downgrade attack looks like; got {}",
        info.protocol_version
    );
    assert_eq!(info.alpn.as_deref(), Some("nexusnet/1"));
}

#[tokio::test]
async fn both_peers_derive_the_same_session_key() {
    // This is what binds TLS to nexusnet-encryption: the key comes from the
    // authenticated handshake rather than being assumed.
    let (certificates, key) = certificate_for("localhost");
    let server_config = TlsConfigBuilder::new()
        .build_server(certificates.clone(), key)
        .expect("builds");
    let client_config = TlsConfigBuilder::new()
        .build_client_with_roots(roots_trusting(&certificates))
        .expect("builds");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = listener.local_addr().expect("has an address");
    let acceptor = TlsAcceptor::new(server_config);

    let server = tokio::spawn(async move {
        let (socket, _peer) = listener.accept().await.expect("accepts");
        let stream = acceptor.accept(socket).await.expect("handshake succeeds");

        let outbound = export_key_server(&stream, Direction::ClientToServer).expect("exports");
        let inbound = export_key_server(&stream, Direction::ServerToClient).expect("exports");
        (outbound, inbound)
    });

    let socket = TcpStream::connect(address).await.expect("connects");
    let stream = TlsConnector::new(client_config)
        .connect("localhost", socket)
        .await
        .expect("handshake succeeds");

    let client_c2s = export_key_client(&stream, Direction::ClientToServer).expect("exports");
    let client_s2c = export_key_client(&stream, Direction::ServerToClient).expect("exports");

    let (server_c2s, server_s2c) = server.await.expect("the server task completes");

    assert_eq!(
        client_c2s, server_c2s,
        "both peers must derive the same client-to-server key"
    );
    assert_eq!(client_s2c, server_s2c);
    assert_ne!(
        client_c2s, client_s2c,
        "the two directions must still differ, or reflection is possible"
    );
}

// --- Mutual TLS -----------------------------------------------------------

/// Builds a client root store trusting exactly `certificates`, for verifying
/// client certificates on the server side.
fn client_roots_trusting(certificates: &[CertificateDer<'static>]) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    for certificate in certificates {
        roots.add(certificate.clone()).expect("well formed");
    }
    roots
}

/// Connects, then attempts a read/write.
///
/// In TLS 1.3 the client's `connect` can return `Ok` before the server has
/// verified the client certificate — the server's rejection arrives on the
/// first I/O, not during the handshake. A test that only checks `connect`
/// therefore misses a client-auth rejection. This drives one round of I/O so
/// the rejection actually surfaces.
async fn connect_and_probe(
    connector: TlsConnector,
    domain: &str,
    socket: TcpStream,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = connector.connect(domain, socket).await?;
    // Force the session to be used; a rejected client fails here.
    stream.write_all(b"probe").await?;
    let mut buffer = vec![0_u8; 16];
    // A clean rejection shows up as a read error or a zero-length read after
    // the server tears the connection down.
    let read = stream.read(&mut buffer).await?;
    if read == 0 {
        return Err("connection closed by server (client auth rejected)".into());
    }
    Ok(())
}

#[tokio::test]
async fn mutual_tls_succeeds_when_both_sides_are_trusted() {
    // Server identity, and a client identity the server will trust.
    let (server_certs, server_key) = certificate_for("localhost");
    let (client_certs, client_key) = certificate_for("client.internal");

    let server_config = TlsConfigBuilder::new()
        .build_server_with_client_auth(
            server_certs.clone(),
            server_key,
            client_roots_trusting(&client_certs),
        )
        .expect("builds a mutual-TLS server");

    let client_config = TlsConfigBuilder::new()
        .build_client_with_cert(roots_trusting(&server_certs), client_certs, client_key)
        .expect("builds a client that presents a certificate");

    let address = spawn_echo_server(TlsAcceptor::new(server_config)).await;
    let socket = TcpStream::connect(address).await.expect("connects");

    let mut stream = TlsConnector::new(client_config)
        .connect("localhost", socket)
        .await
        .expect("mutual handshake succeeds when both certificates are trusted");

    stream
        .write_all(b"mutually authenticated")
        .await
        .expect("writes");
    let mut buffer = vec![0_u8; 64];
    let read = stream.read(&mut buffer).await.expect("reads");
    assert_eq!(&buffer[..read], b"mutually authenticated");
}

#[tokio::test]
async fn mutual_tls_rejects_a_client_with_no_certificate() {
    // This is the security property of mutual TLS: a client that presents no
    // certificate must be turned away, even though it trusts the server.
    let (server_certs, server_key) = certificate_for("localhost");
    let (client_certs, _client_key) = certificate_for("client.internal");

    let server_config = TlsConfigBuilder::new()
        .build_server_with_client_auth(
            server_certs.clone(),
            server_key,
            client_roots_trusting(&client_certs),
        )
        .expect("builds");

    // An ordinary client: verifies the server, presents nothing.
    let client_config = TlsConfigBuilder::new()
        .build_client_with_roots(roots_trusting(&server_certs))
        .expect("builds");

    let address = spawn_echo_server(TlsAcceptor::new(server_config)).await;
    let socket = TcpStream::connect(address).await.expect("connects");

    let result = connect_and_probe(TlsConnector::new(client_config), "localhost", socket).await;

    assert!(
        result.is_err(),
        "a client presenting no certificate must be rejected by a mutual-TLS server"
    );
}

#[tokio::test]
async fn mutual_tls_rejects_a_client_with_an_untrusted_certificate() {
    // A client that presents a certificate from the wrong authority.
    let (server_certs, server_key) = certificate_for("localhost");
    let (trusted_client_certs, _) = certificate_for("client.internal");
    let (rogue_client_certs, rogue_client_key) = certificate_for("client.internal");

    let server_config = TlsConfigBuilder::new()
        .build_server_with_client_auth(
            server_certs.clone(),
            server_key,
            // The server trusts only the first client identity.
            client_roots_trusting(&trusted_client_certs),
        )
        .expect("builds");

    // The client presents a different, untrusted certificate.
    let client_config = TlsConfigBuilder::new()
        .build_client_with_cert(
            roots_trusting(&server_certs),
            rogue_client_certs,
            rogue_client_key,
        )
        .expect("builds");

    let address = spawn_echo_server(TlsAcceptor::new(server_config)).await;
    let socket = TcpStream::connect(address).await.expect("connects");

    let result = connect_and_probe(TlsConnector::new(client_config), "localhost", socket).await;

    assert!(
        result.is_err(),
        "a client certificate from an untrusted authority must be rejected"
    );
}

#[tokio::test]
async fn an_empty_client_root_store_is_refused_at_build_time() {
    // A verifier that trusts no one would reject every client. rustls refuses
    // to build it, which surfaces the misconfiguration immediately rather than
    // as a mysterious total outage.
    let (server_certs, server_key) = certificate_for("localhost");

    let result = TlsConfigBuilder::new().build_server_with_client_auth(
        server_certs,
        server_key,
        RootCertStore::empty(),
    );

    assert!(
        result.is_err(),
        "an empty client-trust store is a configuration error, caught at build time"
    );
}
