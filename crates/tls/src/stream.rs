//! Establishing TLS sessions and binding them to NexusNet's own encryption.

use std::sync::Arc;

use nexusnet_encryption::{Direction, Key, KEY_LEN};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ServerConfig};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{Error, Result};

/// A TLS-protected stream from the client side.
pub type ClientStream<S> = tokio_rustls::client::TlsStream<S>;

/// A TLS-protected stream from the server side.
pub type ServerStream<S> = tokio_rustls::server::TlsStream<S>;

/// Facts about an established session, for logging and verification.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionInfo {
    /// The negotiated protocol version, such as `TLSv1_3`.
    pub protocol_version: String,
    /// The negotiated cipher suite.
    pub cipher_suite: String,
    /// The ALPN protocol agreed, if any.
    pub alpn: Option<String>,
}

impl SessionInfo {
    /// Returns `true` if the session negotiated TLS 1.3.
    ///
    /// Worth asserting in production: a silent downgrade to 1.2 is exactly what
    /// a downgrade attack looks like.
    #[must_use]
    pub fn is_tls13(&self) -> bool {
        self.protocol_version.contains("1_3") || self.protocol_version.contains("1.3")
    }
}

/// Reads session facts from a `rustls` connection.
fn describe(common: &rustls::CommonState) -> SessionInfo {
    SessionInfo {
        protocol_version: common
            .protocol_version()
            .map_or_else(|| "unknown".to_owned(), |version| format!("{version:?}")),
        cipher_suite: common.negotiated_cipher_suite().map_or_else(
            || "unknown".to_owned(),
            |suite| format!("{:?}", suite.suite()),
        ),
        alpn: common
            .alpn_protocol()
            .map(|protocol| String::from_utf8_lossy(protocol).into_owned()),
    }
}

/// Derives a NexusNet session key from an established TLS session.
///
/// This is the piece that closes the gap `nexusnet-encryption` documented. TLS
/// performs an *authenticated* key exchange — the certificate proves who the
/// peer is — and RFC 5705 keying material export produces fresh secrets bound
/// to that specific session. Keys derived this way inherit TLS's authentication
/// rather than assuming a secret was shared by other means.
///
/// Because the exported material is bound to the handshake transcript, a
/// man-in-the-middle who terminated TLS separately with each side cannot make
/// both sides derive the same key.
///
/// # Errors
///
/// Returns [`Error::KeyExport`] if the session cannot export material, which
/// happens if the handshake has not completed.
/// The bound is on the concrete connection rather than `CommonState`, because
/// keying-material export lives on `ConnectionCommon` — `CommonState` carries
/// only the descriptive session facts. In rustls 0.23 the method takes the
/// output buffer by value and returns it filled.
fn export_key<D>(connection: &rustls::ConnectionCommon<D>, direction: Direction) -> Result<Key> {
    let material: [u8; KEY_LEN] = connection
        .export_keying_material(
            [0_u8; KEY_LEN],
            b"nexusnet v1 session key",
            Some(direction.label()),
        )
        .map_err(Error::KeyExport)?;

    Ok(Key::from_bytes(material))
}

/// Accepts inbound TLS connections.
///
/// # Examples
///
/// ```no_run
/// # use std::sync::Arc;
/// # use nexusnet_tls::{TlsAcceptor, TlsConfigBuilder, load_certificates, load_private_key};
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let certificates = load_certificates("server.crt")?;
/// let key = load_private_key("server.key")?;
/// let config = TlsConfigBuilder::new().build_server(certificates, key)?;
///
/// let acceptor = TlsAcceptor::new(config);
/// let listener = tokio::net::TcpListener::bind("127.0.0.1:8443").await?;
///
/// let (socket, _peer) = listener.accept().await?;
/// let stream = acceptor.accept(socket).await?;
///
/// // The stream implements AsyncRead + AsyncWrite, so it drops straight into
/// // `nexusnet_transport::Connection`.
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct TlsAcceptor {
    inner: tokio_rustls::TlsAcceptor,
}

impl std::fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsAcceptor").finish_non_exhaustive()
    }
}

impl TlsAcceptor {
    /// Creates an acceptor from a server configuration.
    #[must_use]
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            inner: tokio_rustls::TlsAcceptor::from(config),
        }
    }

    /// Performs the server side of a handshake.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Handshake`] if the handshake fails, which includes a
    /// client presenting an unacceptable certificate or speaking a protocol
    /// this configuration refuses.
    pub async fn accept<S>(&self, stream: S) -> Result<ServerStream<S>>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let stream = self.inner.accept(stream).await.map_err(Error::Handshake)?;

        let info = session_info_server(&stream);
        tracing::debug!(
            version = %info.protocol_version,
            suite = %info.cipher_suite,
            "TLS session established"
        );

        Ok(stream)
    }
}

/// Establishes outbound TLS connections.
#[derive(Clone)]
pub struct TlsConnector {
    inner: tokio_rustls::TlsConnector,
}

impl std::fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConnector").finish_non_exhaustive()
    }
}

impl TlsConnector {
    /// Creates a connector from a client configuration.
    #[must_use]
    pub fn new(config: Arc<ClientConfig>) -> Self {
        Self {
            inner: tokio_rustls::TlsConnector::from(config),
        }
    }

    /// Performs the client side of a handshake against `domain`.
    ///
    /// The domain is what the server's certificate is checked against, so it
    /// must be the name you intended to reach — not one taken from the
    /// connection itself, which an attacker controls.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidServerName`] if the domain does not parse, or
    /// [`Error::Handshake`] if verification fails.
    pub async fn connect<S>(&self, domain: &str, stream: S) -> Result<ClientStream<S>>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let server_name =
            ServerName::try_from(domain.to_owned()).map_err(|_| Error::InvalidServerName {
                name: domain.to_owned(),
            })?;

        let stream = self
            .inner
            .connect(server_name, stream)
            .await
            .map_err(Error::Handshake)?;

        Ok(stream)
    }
}

/// Returns session facts for a server-side stream.
#[must_use]
pub fn session_info_server<S>(stream: &ServerStream<S>) -> SessionInfo {
    describe(stream.get_ref().1)
}

/// Returns session facts for a client-side stream.
#[must_use]
pub fn session_info_client<S>(stream: &ClientStream<S>) -> SessionInfo {
    describe(stream.get_ref().1)
}

/// Derives a NexusNet key from a server-side session.
///
/// # Errors
///
/// Returns [`Error::KeyExport`] if the handshake has not completed.
pub fn export_key_server<S>(stream: &ServerStream<S>, direction: Direction) -> Result<Key> {
    export_key(stream.get_ref().1, direction)
}

/// Derives a NexusNet key from a client-side session.
///
/// # Errors
///
/// Returns [`Error::KeyExport`] if the handshake has not completed.
pub fn export_key_client<S>(stream: &ClientStream<S>, direction: Direction) -> Result<Key> {
    export_key(stream.get_ref().1, direction)
}
