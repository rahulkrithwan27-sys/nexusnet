//! # nexusnet-cache
//!
//! Smart caching layer for NexusNet with delta synchronization and deduplication.
//!
//! ## Planned responsibilities
//!
//! * LRU and TTL caches
//! * Delta synchronization
//! * Object deduplication
//! * Memory-aware and on-disk tiers
//!
//! ## Status
//!
//! This crate is workspace scaffolding established in Phase 1. Its public API is
//! implemented in Phase 5. It currently exposes no items so that it compiles
//! cleanly under the workspace's strict lint policy while the surrounding
//! architecture is built out.
#![cfg_attr(docsrs, feature(doc_cfg))]
