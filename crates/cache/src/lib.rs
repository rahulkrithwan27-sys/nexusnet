//! # nexusnet-cache
//!
//! Caching and deduplication for NexusNet.
//!
//! ## What's here
//!
//! * [`LruCache`] — least-recently-used caching with optional per-entry expiry
//!   and a byte-aware capacity, so a cache of variable-size network payloads can
//!   be bounded by memory rather than entry count alone.
//! * [`Deduplicator`] — content-addressed deduplication, so a payload that has
//!   already crossed the wire is sent as a short digest reference instead of
//!   again in full.
//!
//! ## Example
//!
//! ```
//! use std::time::Duration;
//! use nexusnet_cache::LruCache;
//!
//! let mut cache: LruCache<String, Vec<u8>> = LruCache::new(1024)
//!     .with_max_bytes(8 * 1024 * 1024)
//!     .with_default_ttl(Duration::from_secs(60));
//!
//! cache.insert("session:42".to_owned(), b"payload".to_vec());
//! assert_eq!(cache.get("session:42").map(Vec::len), Some(7));
//! ```
#![cfg_attr(docsrs, feature(doc_cfg))]

mod dedup;
mod lru;

pub use crate::dedup::{
    DedupStats, DedupStore, Deduplicator, Digest, Reference, DEFAULT_MIN_DEDUP_SIZE,
};
pub use crate::lru::{CacheStats, EvictionReason, LruCache, Weight};
