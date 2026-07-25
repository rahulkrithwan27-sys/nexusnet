//! Errors raised by the TLS layer.

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Something went wrong establishing or using a TLS session.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The TLS handshake failed.
    ///
    /// This is the error that matters most: it means the peer could not be
    /// authenticated, the protocol could not be agreed, or the connection was
    /// tampered with. Treat it as hostile until proven otherwise.
    #[error("TLS handshake failed: {0}")]
    Handshake(#[source] std::io::Error),

    /// The certificate or key could not be read.
    #[error("could not read {kind} from {path}: {source}")]
    CertificateLoad {
        /// What was being loaded.
        kind: &'static str,
        /// The path attempted.
        path: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// The file contained no usable certificate or key.
    #[error("{path} contains no {kind}")]
    NoCertificate {
        /// What was expected.
        kind: &'static str,
        /// The path inspected.
        path: String,
    },

    /// `rustls` rejected the configuration.
    #[error("invalid TLS configuration: {0}")]
    Configuration(#[source] rustls::Error),

    /// The server name is not a valid DNS name.
    ///
    /// Rejected rather than coerced: connecting to a name that does not parse
    /// means certificate verification cannot be meaningful.
    #[error("'{name}' is not a valid server name")]
    InvalidServerName {
        /// The name supplied.
        name: String,
    },

    /// Exporting keying material failed.
    #[error("could not export keying material: {0}")]
    KeyExport(#[source] rustls::Error),

    /// An I/O error unrelated to the handshake.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Returns `true` if the failure suggests an attack rather than a
    /// misconfiguration.
    ///
    /// A failed handshake is the signal worth alerting on: it is what an
    /// interception attempt looks like from the inside.
    #[must_use]
    pub const fn is_security_relevant(&self) -> bool {
        matches!(self, Self::Handshake(_) | Self::InvalidServerName { .. })
    }
}
