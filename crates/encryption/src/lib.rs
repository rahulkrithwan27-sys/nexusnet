//! # nexusnet-encryption
//!
//! Cryptographic primitives and secure handshakes for NexusNet.
//!
//! ## Planned responsibilities
//!
//! * ChaCha20-Poly1305 and AES-GCM
//! * TLS integration
//! * Key rotation and nonce management
//! * Secure handshake orchestration
//!
//! ## Status
//!
//! This crate is workspace scaffolding established in Phase 1. Its public API is
//! implemented in Phase 4. It currently exposes no items so that it compiles
//! cleanly under the workspace's strict lint policy while the surrounding
//! architecture is built out.
#![cfg_attr(docsrs, feature(doc_cfg))]
