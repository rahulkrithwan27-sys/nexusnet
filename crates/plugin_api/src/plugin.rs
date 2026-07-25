//! The plugin trait, its lifecycle, and the data-path extension point.

use std::collections::BTreeMap;
use std::fmt;

use crate::error::{Error, Result};
use crate::metadata::PluginMetadata;

/// Configuration handed to a plugin when it loads.
///
/// Deliberately a flat string map rather than a typed structure: the host
/// cannot know what settings a third-party plugin needs, and a typed interface
/// would have to change every time one was added — precisely the API churn the
/// version check exists to police.
#[derive(Debug, Clone, Default)]
pub struct PluginContext {
    settings: BTreeMap<String, String>,
}

impl PluginContext {
    /// Creates an empty context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a setting.
    #[must_use]
    pub fn with_setting(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.settings.insert(key.into(), value.into());
        self
    }

    /// Returns a setting, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.settings.get(key).map(String::as_str)
    }

    /// Returns a setting parsed as `T`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Configuration`] if the key is absent or the value does
    /// not parse. A plugin that silently accepted a malformed setting would be
    /// far harder to diagnose than one that refused to load.
    pub fn parse<T>(&self, key: &str) -> Result<T>
    where
        T: std::str::FromStr,
        T::Err: fmt::Display,
    {
        let raw = self.get(key).ok_or_else(|| Error::Configuration {
            key: key.to_owned(),
            reason: "the setting is not present".to_owned(),
        })?;

        raw.parse::<T>().map_err(|error| Error::Configuration {
            key: key.to_owned(),
            reason: error.to_string(),
        })
    }

    /// Returns how many settings are present.
    #[must_use]
    pub fn len(&self) -> usize {
        self.settings.len()
    }

    /// Returns `true` if no setting is present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty()
    }

    /// Returns every setting key, in stable order.
    #[must_use]
    pub fn keys(&self) -> Vec<&str> {
        self.settings.keys().map(String::as_str).collect()
    }
}

/// Where a plugin is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PluginState {
    /// Registered but not yet loaded.
    Registered,
    /// Loaded and active.
    Active,
    /// Unloaded; will receive no further calls.
    Unloaded,
    /// Loading failed. The plugin is inert and will not be called.
    Failed,
}

impl PluginState {
    /// Returns `true` if the plugin should receive calls.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

impl fmt::Display for PluginState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Registered => "registered",
            Self::Active => "active",
            Self::Unloaded => "unloaded",
            Self::Failed => "failed",
        };
        f.write_str(name)
    }
}

/// A NexusNet plugin.
///
/// Implementors must be `Send + Sync` because the framework is multi-threaded
/// and a plugin may be called from any task.
///
/// # Examples
///
/// ```
/// use nexusnet_plugin_api::{Plugin, PluginContext, PluginMetadata, Result};
///
/// struct Audit {
///     loaded: bool,
/// }
///
/// impl Plugin for Audit {
///     fn metadata(&self) -> PluginMetadata {
///         PluginMetadata::new("audit", "0.1.0").with_description("Counts frames")
///     }
///
///     fn on_load(&mut self, _context: &PluginContext) -> Result<()> {
///         self.loaded = true;
///         Ok(())
///     }
/// }
/// ```
pub trait Plugin: Send + Sync + 'static {
    /// Returns what this plugin declares about itself.
    ///
    /// Called before loading, so it must not depend on any state established
    /// during [`on_load`](Plugin::on_load).
    fn metadata(&self) -> PluginMetadata;

    /// Called once when the plugin is loaded.
    ///
    /// # Errors
    ///
    /// Returning an error marks the plugin [`Failed`](PluginState::Failed); it
    /// is not called again. Failing here is the right response to
    /// misconfiguration, since the alternative is failing later on the data
    /// path where it is far harder to attribute.
    fn on_load(&mut self, context: &PluginContext) -> Result<()> {
        let _ = context;
        Ok(())
    }

    /// Called once when the plugin is unloaded.
    ///
    /// # Errors
    ///
    /// An error is reported but does not prevent unloading: a plugin that
    /// cannot clean up must still be removed, or a failing plugin could not be
    /// got rid of.
    fn on_unload(&mut self) -> Result<()> {
        Ok(())
    }
}

/// What an interceptor decided about a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Action {
    /// Pass the payload to the next interceptor.
    Continue,
    /// Drop the payload; no further interceptor runs.
    ///
    /// Used by filters and access control. The caller is told the payload was
    /// dropped rather than silently receiving nothing.
    Drop,
}

impl Action {
    /// Returns `true` if processing should continue.
    #[must_use]
    pub const fn is_continue(self) -> bool {
        matches!(self, Self::Continue)
    }
}

/// Observes or transforms payloads on the data path.
///
/// This is the extension point that makes plugins useful: encryption, custom
/// compression, auditing, and filtering are all payload transformations.
///
/// # Ordering
///
/// Interceptors run in priority order outbound, and in **reverse** order
/// inbound. That symmetry is what lets a transforming pair work: an interceptor
/// that compresses on the way out must be the last to see the payload outbound
/// and the first to see it inbound, or it would try to decompress something
/// another interceptor had already altered.
pub trait Interceptor: Send + Sync + 'static {
    /// Returns a name for diagnostics.
    fn name(&self) -> &str;

    /// Returns the ordering priority; lower runs earlier outbound.
    fn priority(&self) -> i32 {
        0
    }

    /// Processes a payload on its way out.
    ///
    /// # Errors
    ///
    /// Returning an error aborts the send and propagates to the caller.
    fn on_outbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
        let _ = payload;
        Ok(Action::Continue)
    }

    /// Processes a payload on its way in.
    ///
    /// # Errors
    ///
    /// Returning an error aborts delivery of that payload.
    fn on_inbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
        let _ = payload;
        Ok(Action::Continue)
    }
}

/// An ordered pipeline of interceptors.
///
/// # Examples
///
/// ```
/// use nexusnet_plugin_api::{Action, Interceptor, InterceptorChain, Result};
///
/// struct Exclaim;
///
/// impl Interceptor for Exclaim {
///     fn name(&self) -> &str {
///         "exclaim"
///     }
///
///     fn on_outbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
///         payload.push(b'!');
///         Ok(Action::Continue)
///     }
/// }
///
/// let mut chain = InterceptorChain::new();
/// chain.add(Box::new(Exclaim));
///
/// let mut payload = b"hello".to_vec();
/// assert!(chain.outbound(&mut payload)?.is_continue());
/// assert_eq!(payload, b"hello!");
/// # Ok::<(), nexusnet_plugin_api::Error>(())
/// ```
#[derive(Default)]
pub struct InterceptorChain {
    interceptors: Vec<Box<dyn Interceptor>>,
}

impl InterceptorChain {
    /// Creates an empty chain.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an interceptor, keeping the chain ordered by priority.
    ///
    /// Ties keep insertion order, so a caller registering two interceptors at
    /// the same priority gets a predictable result rather than an arbitrary
    /// one.
    pub fn add(&mut self, interceptor: Box<dyn Interceptor>) {
        let priority = interceptor.priority();
        let position = self
            .interceptors
            .iter()
            .position(|existing| existing.priority() > priority)
            .unwrap_or(self.interceptors.len());

        self.interceptors.insert(position, interceptor);
    }

    /// Returns how many interceptors are installed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.interceptors.len()
    }

    /// Returns `true` if the chain is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.interceptors.is_empty()
    }

    /// Returns the installed names, in outbound order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.interceptors.iter().map(|i| i.name()).collect()
    }

    /// Runs the chain over an outbound payload, in priority order.
    ///
    /// # Errors
    ///
    /// Propagates any interceptor's error, naming the interceptor responsible.
    pub fn outbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
        for interceptor in &self.interceptors {
            match interceptor.on_outbound(payload) {
                Ok(Action::Continue) => {}
                Ok(Action::Drop) => return Ok(Action::Drop),
                Err(source) => {
                    return Err(Error::Interceptor {
                        name: interceptor.name().to_owned(),
                        reason: source.to_string(),
                    })
                }
            }
        }

        Ok(Action::Continue)
    }

    /// Runs the chain over an inbound payload, in reverse priority order.
    ///
    /// # Errors
    ///
    /// Propagates any interceptor's error, naming the interceptor responsible.
    pub fn inbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
        for interceptor in self.interceptors.iter().rev() {
            match interceptor.on_inbound(payload) {
                Ok(Action::Continue) => {}
                Ok(Action::Drop) => return Ok(Action::Drop),
                Err(source) => {
                    return Err(Error::Interceptor {
                        name: interceptor.name().to_owned(),
                        reason: source.to_string(),
                    })
                }
            }
        }

        Ok(Action::Continue)
    }

    /// Removes every interceptor.
    pub fn clear(&mut self) {
        self.interceptors.clear();
    }
}

impl fmt::Debug for InterceptorChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InterceptorChain")
            .field("interceptors", &self.names())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Appends a byte outbound and removes it inbound.
    struct Tag {
        byte: u8,
        priority: i32,
        name: String,
    }

    impl Tag {
        fn new(byte: u8, priority: i32) -> Self {
            Self {
                byte,
                priority,
                name: format!("tag-{}", byte as char),
            }
        }
    }

    impl Interceptor for Tag {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        fn on_outbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
            payload.push(self.byte);
            Ok(Action::Continue)
        }

        fn on_inbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
            assert_eq!(
                payload.pop(),
                Some(self.byte),
                "inbound order must mirror outbound order exactly"
            );
            Ok(Action::Continue)
        }
    }

    struct Blocker;

    impl Interceptor for Blocker {
        fn name(&self) -> &str {
            "blocker"
        }

        fn on_outbound(&self, _payload: &mut Vec<u8>) -> Result<Action> {
            Ok(Action::Drop)
        }
    }

    struct Failing;

    impl Interceptor for Failing {
        fn name(&self) -> &str {
            "failing"
        }

        fn on_outbound(&self, _payload: &mut Vec<u8>) -> Result<Action> {
            Err(Error::Interceptor {
                name: "failing".to_owned(),
                reason: "deliberate".to_owned(),
            })
        }
    }

    #[test]
    fn an_empty_context_reports_nothing() {
        let context = PluginContext::new();

        assert!(context.is_empty());
        assert_eq!(context.len(), 0);
        assert!(context.get("absent").is_none());
    }

    #[test]
    fn settings_are_retrievable() {
        let context = PluginContext::new()
            .with_setting("level", "9")
            .with_setting("mode", "fast");

        assert_eq!(context.get("level"), Some("9"));
        assert_eq!(context.len(), 2);
        assert_eq!(context.keys(), vec!["level", "mode"], "keys are ordered");
    }

    #[test]
    fn settings_parse_to_typed_values() {
        let context = PluginContext::new().with_setting("level", "9");

        assert_eq!(context.parse::<u32>("level").expect("parses"), 9);
    }

    #[test]
    fn a_missing_setting_is_an_error() {
        let context = PluginContext::new();
        let error = context.parse::<u32>("level").expect_err("absent");

        assert!(matches!(error, Error::Configuration { .. }));
    }

    #[test]
    fn a_malformed_setting_is_an_error() {
        let context = PluginContext::new().with_setting("level", "not-a-number");
        let error = context.parse::<u32>("level").expect_err("malformed");

        assert!(
            matches!(error, Error::Configuration { .. }),
            "silently defaulting would be far harder to diagnose"
        );
    }

    #[test]
    fn an_empty_chain_passes_payloads_through() {
        let chain = InterceptorChain::new();
        let mut payload = b"unchanged".to_vec();

        assert!(chain
            .outbound(&mut payload)
            .expect("succeeds")
            .is_continue());
        assert_eq!(payload, b"unchanged");
        assert!(chain.is_empty());
    }

    #[test]
    fn interceptors_run_in_priority_order() {
        let mut chain = InterceptorChain::new();
        // Added out of order deliberately.
        chain.add(Box::new(Tag::new(b'c', 30)));
        chain.add(Box::new(Tag::new(b'a', 10)));
        chain.add(Box::new(Tag::new(b'b', 20)));

        assert_eq!(chain.names(), vec!["tag-a", "tag-b", "tag-c"]);

        let mut payload = Vec::new();
        chain.outbound(&mut payload).expect("succeeds");
        assert_eq!(payload, b"abc");
    }

    #[test]
    fn inbound_runs_in_reverse_so_transforms_unwind() {
        let mut chain = InterceptorChain::new();
        chain.add(Box::new(Tag::new(b'a', 10)));
        chain.add(Box::new(Tag::new(b'b', 20)));
        chain.add(Box::new(Tag::new(b'c', 30)));

        let mut payload = Vec::new();
        chain.outbound(&mut payload).expect("succeeds");
        assert_eq!(payload, b"abc");

        // Each Tag asserts it sees its own byte; reverse order is what makes
        // that hold. Getting this backwards is the classic middleware bug.
        chain.inbound(&mut payload).expect("succeeds");
        assert!(payload.is_empty());
    }

    #[test]
    fn ties_preserve_insertion_order() {
        let mut chain = InterceptorChain::new();
        chain.add(Box::new(Tag::new(b'x', 5)));
        chain.add(Box::new(Tag::new(b'y', 5)));

        assert_eq!(chain.names(), vec!["tag-x", "tag-y"]);
    }

    #[test]
    fn dropping_stops_the_chain() {
        let mut chain = InterceptorChain::new();
        chain.add(Box::new(Blocker));
        chain.add(Box::new(Tag::new(b'z', 10)));

        let mut payload = Vec::new();
        let action = chain.outbound(&mut payload).expect("succeeds");

        assert_eq!(action, Action::Drop);
        assert!(
            payload.is_empty(),
            "an interceptor after the drop must not have run"
        );
    }

    #[test]
    fn an_error_names_the_interceptor_responsible() {
        let mut chain = InterceptorChain::new();
        chain.add(Box::new(Failing));

        let mut payload = Vec::new();
        let error = chain.outbound(&mut payload).expect_err("fails");

        assert!(
            error.to_string().contains("failing"),
            "the operator needs to know which plugin broke: {error}"
        );
    }

    #[test]
    fn a_chain_can_be_cleared() {
        let mut chain = InterceptorChain::new();
        chain.add(Box::new(Tag::new(b'a', 0)));
        assert_eq!(chain.len(), 1);

        chain.clear();
        assert!(chain.is_empty());
    }

    #[test]
    fn states_classify_correctly() {
        assert!(PluginState::Active.is_active());
        assert!(!PluginState::Failed.is_active());
        assert!(!PluginState::Unloaded.is_active());
        assert_eq!(PluginState::Registered.to_string(), "registered");
    }

    #[test]
    fn actions_classify_correctly() {
        assert!(Action::Continue.is_continue());
        assert!(!Action::Drop.is_continue());
    }
}
