//! A minimal TLS 1.3 echo server, for inspecting the handshake with external
//! tools like `openssl s_client`, `nmap`, or `mitmproxy`.
//!
//! It generates a self-signed certificate for `localhost` on startup and writes
//! it to `/tmp/nexusnet-server.crt`, so a client can be told to trust it:
//!
//! ```text
//! cargo run -p nexusnet-tls --example tls_server
//! # in another terminal:
//! openssl s_client -connect localhost:8443 -tls1_3 -CAfile /tmp/nexusnet-server.crt
//! openssl s_client -connect localhost:8443 -tls1_1   # must be refused
//! ```

use std::io::Write as _;

use nexusnet_tls::{TlsAcceptor, TlsConfigBuilder};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A throwaway self-signed certificate for localhost.
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let certificate = CertificateDer::from(generated.cert.der().to_vec());
    let key = PrivateKeyDer::try_from(generated.key_pair.serialize_der())?;

    // Write the certificate out so external clients can trust it.
    let cert_pem = generated.cert.pem();
    std::fs::File::create("/tmp/nexusnet-server.crt")?.write_all(cert_pem.as_bytes())?;
    println!("wrote certificate to /tmp/nexusnet-server.crt");

    let config = TlsConfigBuilder::new().build_server(vec![certificate], key)?;
    let acceptor = TlsAcceptor::new(config);

    let listener = TcpListener::bind("127.0.0.1:8443").await?;
    println!("TLS 1.3 echo server listening on 127.0.0.1:8443");
    println!("(TLS 1.2 and below will be refused; press Ctrl+C to stop)");

    loop {
        let (socket, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();

        tokio::spawn(async move {
            match acceptor.accept(socket).await {
                Ok(mut stream) => {
                    println!("  handshake succeeded with {peer}");
                    let mut buffer = vec![0_u8; 1024];
                    while let Ok(read) = stream.read(&mut buffer).await {
                        if read == 0 {
                            break;
                        }
                        if stream.write_all(&buffer[..read]).await.is_err() {
                            break;
                        }
                    }
                }
                Err(error) => {
                    // This is what you want to see when a tool tries to connect
                    // with the wrong protocol or an untrusted certificate.
                    println!("  handshake REJECTED from {peer}: {error}");
                }
            }
        });
    }
}
