//! Plugin registration and lifecycle management.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::metadata::{ApiVersion, PluginMetadata, CURRENT_API_VERSION};
use crate::plugin::{Plugin, PluginContext, PluginState};

/// A registered plugin and its current state.
struct Entry {
    plugin: Box<dyn Plugin>,
    metadata: PluginMetadata,
    state: PluginState,
}

/// A summary of one registered plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PluginInfo {
    /// What the plugin declares about itself.
    pub metadata: PluginMetadata,
    /// Where it is in its lifecycle.
    pub state: PluginState,
}

/// A snapshot of registry activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct RegistryStats {
    /// Plugins currently registered.
    pub registered: usize,
    /// Plugins loaded and running.
    pub active: usize,
    /// Plugins that failed to load.
    pub failed: usize,
    /// Registrations refused.
    pub rejected: u64,
}

impl RegistryStats {
    /// Returns `true` if nothing is registered at all.
    ///
    /// Useful as a shutdown assertion: every plugin unloaded and removed.
    #[must_use]
    pub const fn is_empty_registry(&self) -> bool {
        self.registered == 0
    }

    /// Returns `true` if any plugin failed to load.
    #[must_use]
    pub const fn has_failures(&self) -> bool {
        self.failed > 0
    }
}

/// Holds and manages plugins.
///
/// # Dynamic loading, and why there is none
///
/// This registry takes plugins as ordinary Rust values, compiled into the
/// binary. It does not load shared libraries at runtime. That is a deliberate
/// limitation: Rust has no stable ABI, so a plugin compiled by a different
/// compiler version — or with different flags — can produce undefined behaviour
/// when its types cross the boundary. The version check here catches a mismatch
/// in the *API*; nothing can catch a mismatch in the ABI.
///
/// A future C-ABI surface under `sdk/` is the safe route to runtime loading,
/// since C's ABI is stable in a way Rust's is not.
///
/// # Examples
///
/// ```
/// use nexusnet_plugin_api::{Plugin, PluginContext, PluginMetadata, PluginRegistry, Result};
///
/// struct Audit;
///
/// impl Plugin for Audit {
///     fn metadata(&self) -> PluginMetadata {
///         PluginMetadata::new("audit", "0.1.0")
///     }
/// }
///
/// let mut registry = PluginRegistry::new();
/// registry.register(Box::new(Audit))?;
/// registry.load_all(&PluginContext::new());
///
/// assert_eq!(registry.stats().active, 1);
/// # Ok::<(), nexusnet_plugin_api::Error>(())
/// ```
pub struct PluginRegistry {
    plugins: BTreeMap<String, Entry>,
    api_version: ApiVersion,
    rejected: u64,
}

impl PluginRegistry {
    /// Creates an empty registry advertising the current API version.
    #[must_use]
    pub fn new() -> Self {
        Self {
            plugins: BTreeMap::new(),
            api_version: CURRENT_API_VERSION,
            rejected: 0,
        }
    }

    /// Creates a registry advertising a specific API version.
    ///
    /// Useful for testing compatibility handling without waiting for the real
    /// API version to move.
    #[must_use]
    pub fn with_api_version(api_version: ApiVersion) -> Self {
        Self {
            plugins: BTreeMap::new(),
            api_version,
            rejected: 0,
        }
    }

    /// Returns the API version this registry provides.
    #[must_use]
    pub const fn api_version(&self) -> ApiVersion {
        self.api_version
    }

    /// Returns how many plugins are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Returns `true` if nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Registers a plugin without loading it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidName`] for an unusable name,
    /// [`Error::DuplicateName`] if the name is taken, or
    /// [`Error::IncompatibleApi`] if the plugin targets an API this registry
    /// cannot honour.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) -> Result<()> {
        let metadata = plugin.metadata();

        if !metadata.has_valid_name() {
            self.rejected += 1;
            return Err(Error::InvalidName {
                name: metadata.name,
            });
        }

        if !metadata.api_version.is_compatible_with(self.api_version) {
            self.rejected += 1;
            return Err(Error::IncompatibleApi {
                name: metadata.name,
                required: metadata.api_version,
                provided: self.api_version,
            });
        }

        if self.plugins.contains_key(&metadata.name) {
            self.rejected += 1;
            return Err(Error::DuplicateName {
                name: metadata.name,
            });
        }

        self.plugins.insert(
            metadata.name.clone(),
            Entry {
                plugin,
                metadata,
                state: PluginState::Registered,
            },
        );

        Ok(())
    }

    /// Loads one registered plugin.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the name is unknown, or
    /// [`Error::LoadFailed`] if the plugin refused. A plugin that fails to load
    /// is marked [`PluginState::Failed`] and never called again.
    pub fn load(&mut self, name: &str, context: &PluginContext) -> Result<()> {
        let entry = self.plugins.get_mut(name).ok_or_else(|| Error::NotFound {
            name: name.to_owned(),
        })?;

        match entry.plugin.on_load(context) {
            Ok(()) => {
                entry.state = PluginState::Active;
                Ok(())
            }
            Err(source) => {
                entry.state = PluginState::Failed;
                Err(Error::LoadFailed {
                    name: name.to_owned(),
                    reason: source.to_string(),
                })
            }
        }
    }

    /// Loads every registered plugin, returning the failures.
    ///
    /// One plugin failing does not prevent the others from loading: a
    /// misconfigured optional plugin should not take down a process that would
    /// otherwise run fine.
    pub fn load_all(&mut self, context: &PluginContext) -> Vec<Error> {
        let names: Vec<String> = self
            .plugins
            .iter()
            .filter(|(_, entry)| entry.state == PluginState::Registered)
            .map(|(name, _)| name.clone())
            .collect();

        let mut failures = Vec::new();
        for name in names {
            if let Err(error) = self.load(&name, context) {
                failures.push(error);
            }
        }

        failures
    }

    /// Unloads and removes a plugin.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the name is unknown. An error from the
    /// plugin's own cleanup is returned, but the plugin is removed regardless —
    /// otherwise a failing plugin could never be got rid of.
    pub fn unload(&mut self, name: &str) -> Result<()> {
        let mut entry = self.plugins.remove(name).ok_or_else(|| Error::NotFound {
            name: name.to_owned(),
        })?;

        let outcome = entry.plugin.on_unload();
        entry.state = PluginState::Unloaded;

        outcome.map_err(|source| Error::LoadFailed {
            name: name.to_owned(),
            reason: source.to_string(),
        })
    }

    /// Unloads every plugin, returning any errors raised during cleanup.
    pub fn unload_all(&mut self) -> Vec<Error> {
        let names: Vec<String> = self.plugins.keys().cloned().collect();

        let mut failures = Vec::new();
        for name in names {
            if let Err(error) = self.unload(&name) {
                failures.push(error);
            }
        }

        failures
    }

    /// Returns `true` if a plugin is registered under this name.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.plugins.contains_key(name)
    }

    /// Returns a plugin's state.
    #[must_use]
    pub fn state(&self, name: &str) -> Option<PluginState> {
        self.plugins.get(name).map(|entry| entry.state)
    }

    /// Returns a summary of one plugin.
    #[must_use]
    pub fn info(&self, name: &str) -> Option<PluginInfo> {
        self.plugins.get(name).map(|entry| PluginInfo {
            metadata: entry.metadata.clone(),
            state: entry.state,
        })
    }

    /// Returns every plugin, in stable name order.
    #[must_use]
    pub fn list(&self) -> Vec<PluginInfo> {
        self.plugins
            .values()
            .map(|entry| PluginInfo {
                metadata: entry.metadata.clone(),
                state: entry.state,
            })
            .collect()
    }

    /// Returns a snapshot of registry activity.
    #[must_use]
    pub fn stats(&self) -> RegistryStats {
        let active = self
            .plugins
            .values()
            .filter(|entry| entry.state.is_active())
            .count();
        let failed = self
            .plugins
            .values()
            .filter(|entry| entry.state == PluginState::Failed)
            .count();

        RegistryStats {
            registered: self.plugins.len(),
            active,
            failed,
            rejected: self.rejected,
        }
    }
}

impl Default for PluginRegistry {
    /// Creates a registry advertising the current API version.
    ///
    /// Written out rather than derived: `ApiVersion` has no meaningful default,
    /// and 0.0 would silently reject every plugin.
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("api_version", &self.api_version)
            .field("plugins", &self.plugins.keys().collect::<Vec<_>>())
            .finish()
    }
}
