//! # nexusnet-compression
//!
//! Pluggable compression codecs for NexusNet with adaptive and streaming support.
//!
//! ## Planned responsibilities
//!
//! * Zstd, Gzip, and Brotli codecs
//! * Adaptive codec and level selection
//! * Streaming (chunked) compression
//! * Built-in compression benchmarking
//!
//! ## Status
//!
//! This crate is workspace scaffolding established in Phase 1. Its public API is
//! implemented in Phase 2. It currently exposes no items so that it compiles
//! cleanly under the workspace's strict lint policy while the surrounding
//! architecture is built out.
#![cfg_attr(docsrs, feature(doc_cfg))]
