//! # nexusnet-plugin-api
//!
//! Extension points for NexusNet: traits a third party implements, and a
//! registry that loads them.
//!
//! ## What's here
//!
//! * [`Plugin`] — the lifecycle trait every plugin implements.
//! * [`Interceptor`] and [`InterceptorChain`] — the data-path extension point,
//!   where payloads can be observed or transformed.
//! * [`PluginRegistry`] — registration, loading, and unloading.
//! * [`ApiVersion`] — the compatibility check that decides what may load.
//!
//! ## Example
//!
//! ```
//! use nexusnet_plugin_api::{
//!     Action, Interceptor, Plugin, PluginContext, PluginMetadata, PluginRegistry, Result,
//! };
//!
//! struct Audit {
//!     frames: std::sync::atomic::AtomicU64,
//! }
//!
//! impl Plugin for Audit {
//!     fn metadata(&self) -> PluginMetadata {
//!         PluginMetadata::new("audit", "0.1.0").with_description("Counts frames")
//!     }
//! }
//!
//! impl Interceptor for Audit {
//!     fn name(&self) -> &str {
//!         "audit"
//!     }
//!
//!     fn on_outbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
//!         self.frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
//!         let _ = payload;
//!         Ok(Action::Continue)
//!     }
//! }
//!
//! let mut registry = PluginRegistry::new();
//! registry.register(Box::new(Audit { frames: Default::default() }))?;
//!
//! let failures = registry.load_all(&PluginContext::new());
//! assert!(failures.is_empty());
//! assert_eq!(registry.stats().active, 1);
//! # Ok::<(), nexusnet_plugin_api::Error>(())
//! ```
//!
//! ## Versioning is the whole point
//!
//! A plugin is written against a particular set of extension points. When those
//! change, a plugin built against the old shape misbehaves at runtime, in
//! whatever way the mismatch happens to manifest — which is the hardest kind of
//! bug to attribute. [`ApiVersion`] makes the check explicit: majors must match,
//! and a plugin may target an older minor than the host but never a newer one.
//!
//! ## Ordering, and why inbound runs backwards
//!
//! [`InterceptorChain`] runs interceptors in priority order outbound and in
//! **reverse** order inbound. That symmetry is what lets a transforming pair
//! work: something that compresses on the way out must be the last to touch the
//! payload outbound and the first to touch it inbound, or it would try to
//! decompress bytes another interceptor had already altered. Getting this
//! backwards is the classic middleware bug, so there is a test for it.
//!
//! ## No dynamic loading
//!
//! Plugins are ordinary Rust values compiled into the binary; this crate does
//! not load shared libraries. Rust has no stable ABI, so a plugin compiled by a
//! different compiler version can produce undefined behaviour when its types
//! cross the boundary. The version check catches an *API* mismatch; nothing in
//! Rust can catch an *ABI* mismatch. A C-ABI surface under `sdk/` is the safe
//! route to runtime loading, because C's ABI is stable in a way Rust's is not.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod error;
mod metadata;
mod plugin;
mod registry;

pub use crate::error::{Error, Result};
pub use crate::metadata::{ApiVersion, PluginMetadata, CURRENT_API_VERSION};
pub use crate::plugin::{
    Action, Interceptor, InterceptorChain, Plugin, PluginContext, PluginState,
};
pub use crate::registry::{PluginInfo, PluginRegistry, RegistryStats};
