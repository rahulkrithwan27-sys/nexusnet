//! # nexusnet-transport
//!
//! Transport-layer connectivity for NexusNet: TCP, UDP, QUIC, WebSocket, HTTP/2 and HTTP/3.
//!
//! ## Planned responsibilities
//!
//! * TCP, UDP, and QUIC endpoints
//! * WebSocket, HTTP/2, and HTTP/3 transports
//! * Connection pooling and automatic reconnect
//! * Stream multiplexing over a single connection
//!
//! ## Status
//!
//! This crate is workspace scaffolding established in Phase 1. Its public API is
//! implemented in Phase 3. It currently exposes no items so that it compiles
//! cleanly under the workspace's strict lint policy while the surrounding
//! architecture is built out.
#![cfg_attr(docsrs, feature(doc_cfg))]
