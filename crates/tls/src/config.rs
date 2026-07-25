//! Building client and server TLS configurations.
//!
//! ## Defaults that matter
//!
//! * **TLS 1.3 only by default.** Older versions carry cipher suites and
//!   renegotiation behaviour with a long history of attacks. TLS 1.2 is
//!   available behind [`TlsConfigBuilder::allow_tls12`] for interoperability,
//!   but it is opt-in rather than silently accepted.
//! * **Certificate verification is always on for clients.** There is no
//!   convenience switch to disable it. A client that skips verification is
//!   trivially intercepted, and such switches invariably end up in production.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};

use crate::error::{Error, Result};

/// Loads a certificate chain from a PEM file.
///
/// # Errors
///
/// Returns [`Error::CertificateLoad`] if the file cannot be read, or
/// [`Error::NoCertificate`] if it contains no certificate.
pub fn load_certificates(path: impl AsRef<Path>) -> Result<Vec<CertificateDer<'static>>> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|source| Error::CertificateLoad {
        kind: "certificate",
        path: path.display().to_string(),
        source,
    })?;

    let mut reader = BufReader::new(file);
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| Error::CertificateLoad {
            kind: "certificate",
            path: path.display().to_string(),
            source,
        })?;

    if certificates.is_empty() {
        return Err(Error::NoCertificate {
            kind: "certificate",
            path: path.display().to_string(),
        });
    }

    Ok(certificates)
}

/// Loads a private key from a PEM file.
///
/// # Errors
///
/// Returns [`Error::CertificateLoad`] if the file cannot be read, or
/// [`Error::NoCertificate`] if it contains no private key.
pub fn load_private_key(path: impl AsRef<Path>) -> Result<PrivateKeyDer<'static>> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|source| Error::CertificateLoad {
        kind: "private key",
        path: path.display().to_string(),
        source,
    })?;

    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|source| Error::CertificateLoad {
            kind: "private key",
            path: path.display().to_string(),
            source,
        })?
        .ok_or_else(|| Error::NoCertificate {
            kind: "private key",
            path: path.display().to_string(),
        })
}

/// Builds TLS configurations.
#[derive(Debug, Clone, Default)]
pub struct TlsConfigBuilder {
    allow_tls12: bool,
    alpn: Vec<Vec<u8>>,
}

/// The ALPN identifier NexusNet advertises.
///
/// Advertising a protocol lets a peer reject a connection that is speaking
/// something else, rather than discovering the mismatch after the handshake.
pub const NEXUSNET_ALPN: &[u8] = b"nexusnet/1";

impl TlsConfigBuilder {
    /// Creates a builder with TLS 1.3 only.
    #[must_use]
    pub fn new() -> Self {
        Self {
            allow_tls12: false,
            alpn: vec![NEXUSNET_ALPN.to_vec()],
        }
    }

    /// Also accepts TLS 1.2.
    ///
    /// Only for interoperability with peers that cannot do 1.3. TLS 1.2 permits
    /// cipher suites and constructions with a long record of attacks, so it is
    /// deliberately opt-in.
    #[must_use]
    pub const fn allow_tls12(mut self, allow: bool) -> Self {
        self.allow_tls12 = allow;
        self
    }

    /// Replaces the advertised ALPN protocols.
    #[must_use]
    pub fn with_alpn(mut self, protocols: Vec<Vec<u8>>) -> Self {
        self.alpn = protocols;
        self
    }

    /// Returns the protocol versions this builder permits.
    ///
    /// Returned by value: a `&[&TLS13]` slice would borrow a temporary array
    /// that does not outlive the call.
    fn versions(&self) -> Vec<&'static rustls::SupportedProtocolVersion> {
        if self.allow_tls12 {
            rustls::ALL_VERSIONS.to_vec()
        } else {
            vec![&rustls::version::TLS13]
        }
    }

    /// Builds a server configuration from a certificate chain and key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Configuration`] if `rustls` rejects the certificate or
    /// key — most often because they do not match each other.
    pub fn build_server(
        &self,
        certificates: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Arc<ServerConfig>> {
        let provider = Arc::new(ring::default_provider());

        let mut config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&self.versions())
            .map_err(Error::Configuration)?
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .map_err(Error::Configuration)?;

        config.alpn_protocols.clone_from(&self.alpn);

        Ok(Arc::new(config))
    }

    /// Builds a client configuration trusting the system root certificates.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Configuration`] if `rustls` rejects the configuration.
    pub fn build_client(&self) -> Result<Arc<ClientConfig>> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        self.build_client_with_roots(roots)
    }

    /// Builds a client configuration trusting a specific set of roots.
    ///
    /// Use this to pin a private certificate authority. Note there is no option
    /// to skip verification: a client that does not verify is trivially
    /// intercepted, and every such switch eventually reaches production.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Configuration`] if `rustls` rejects the configuration.
    pub fn build_client_with_roots(&self, roots: RootCertStore) -> Result<Arc<ClientConfig>> {
        let provider = Arc::new(ring::default_provider());

        let mut config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&self.versions())
            .map_err(Error::Configuration)?
            .with_root_certificates(roots)
            .with_no_client_auth();

        config.alpn_protocols.clone_from(&self.alpn);

        Ok(Arc::new(config))
    }

    /// Builds a server that **requires** clients to present a trusted
    /// certificate.
    ///
    /// This is mutual TLS. With [`build_server`](Self::build_server) only the
    /// server is authenticated; here each connecting client must also present a
    /// certificate that chains to `client_roots`, and a client that presents
    /// none — or an untrusted one — is refused at the handshake. Use it for
    /// service-to-service links where both ends must prove who they are.
    ///
    /// `client_roots` is the set of certificate authorities whose clients are
    /// accepted. It is usually a private CA, not the public web roots: you want
    /// to admit *your* clients, not anyone holding a valid public certificate.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Configuration`] if the verifier cannot be built (for
    /// example, from an empty root store, which would accept no one) or the
    /// certificate and key are rejected.
    pub fn build_server_with_client_auth(
        &self,
        certificates: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        client_roots: RootCertStore,
    ) -> Result<Arc<ServerConfig>> {
        let provider = Arc::new(ring::default_provider());

        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(client_roots))
            .build()
            .map_err(|error| Error::Configuration(rustls::Error::General(error.to_string())))?;

        let mut config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&self.versions())
            .map_err(Error::Configuration)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certificates, key)
            .map_err(Error::Configuration)?;

        config.alpn_protocols.clone_from(&self.alpn);

        Ok(Arc::new(config))
    }

    /// Builds a client that presents a certificate for mutual TLS.
    ///
    /// The counterpart to
    /// [`build_server_with_client_auth`](Self::build_server_with_client_auth).
    /// The client verifies the server against `roots` as usual, and in addition
    /// presents `certificates` and `key` to prove its own identity when the
    /// server asks.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Configuration`] if the configuration is rejected — most
    /// often because the certificate and key do not match.
    pub fn build_client_with_cert(
        &self,
        roots: RootCertStore,
        certificates: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Arc<ClientConfig>> {
        let provider = Arc::new(ring::default_provider());

        let mut config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&self.versions())
            .map_err(Error::Configuration)?
            .with_root_certificates(roots)
            .with_client_auth_cert(certificates, key)
            .map_err(Error::Configuration)?;

        config.alpn_protocols.clone_from(&self.alpn);

        Ok(Arc::new(config))
    }
}
