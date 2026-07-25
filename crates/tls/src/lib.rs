//! # nexusnet-tls
//!
//! TLS 1.3 transport security and authenticated key exchange for NexusNet.
//!
//! ## The gap this closes
//!
//! [`nexusnet_encryption`] protects a session once both peers share a secret.
//! It says nothing about how they came to share one — and that is the part an
//! attacker attacks. Without an authenticated exchange, an interceptor
//! negotiates a perfectly good encrypted session with each side separately and
//! reads everything. The cryptography is sound and the system is still broken.
//!
//! TLS supplies the missing half: the certificate proves *who* the peer is,
//! and the handshake establishes a secret bound to that identity.
//!
//! ## Binding the two layers
//!
//! [`export_key_client`] and [`export_key_server`] derive NexusNet session keys
//! from the completed TLS session using RFC 5705 keying material export. Keys
//! obtained this way inherit TLS's authentication, and because the material is
//! bound to the handshake transcript, an interceptor terminating TLS separately
//! with each side cannot make both sides derive the same key.
//!
//! ## Defaults
//!
//! * **TLS 1.3 only** unless [`TlsConfigBuilder::allow_tls12`] is set.
//! * **Certificate verification is mandatory** for clients. There is no switch
//!   to disable it; such switches invariably reach production.
//! * **`ring` rather than `aws-lc-rs`**, so builds need no C toolchain.
//!
//! ## Minimum supported Rust version
//!
//! This crate requires **Rust 1.85**, above the workspace's 1.75. The modern TLS
//! stack requires edition 2024. The requirement is confined here so the rest of
//! NexusNet remains buildable on 1.75.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod config;
mod error;
mod stream;

pub use crate::config::{load_certificates, load_private_key, TlsConfigBuilder, NEXUSNET_ALPN};
pub use crate::error::{Error, Result};
pub use crate::stream::{
    export_key_client, export_key_server, session_info_client, session_info_server, ClientStream,
    ServerStream, SessionInfo, TlsAcceptor, TlsConnector,
};
