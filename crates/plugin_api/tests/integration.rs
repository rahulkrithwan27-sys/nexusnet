//! Integration tests: plugins moving through their whole lifecycle.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use nexusnet_plugin_api::{
    Action, ApiVersion, Error, Interceptor, InterceptorChain, Plugin, PluginContext,
    PluginMetadata, PluginRegistry, PluginState, Result,
};

/// A plugin that records its lifecycle transitions.
struct Recorder {
    name: String,
    loads: Arc<AtomicUsize>,
    unloads: Arc<AtomicUsize>,
    fail_on_load: bool,
    api_version: ApiVersion,
}

impl Recorder {
    fn new(name: &str, loads: Arc<AtomicUsize>, unloads: Arc<AtomicUsize>) -> Self {
        Self {
            name: name.to_owned(),
            loads,
            unloads,
            fail_on_load: false,
            api_version: nexusnet_plugin_api::CURRENT_API_VERSION,
        }
    }

    fn failing(mut self) -> Self {
        self.fail_on_load = true;
        self
    }

    fn targeting(mut self, api_version: ApiVersion) -> Self {
        self.api_version = api_version;
        self
    }
}

impl Plugin for Recorder {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata::new(&self.name, "1.0.0").with_api_version(self.api_version)
    }

    fn on_load(&mut self, context: &PluginContext) -> Result<()> {
        if self.fail_on_load {
            return Err(Error::Configuration {
                key: "mode".to_owned(),
                reason: "deliberately unusable".to_owned(),
            });
        }

        // Prove the context reaches the plugin.
        if let Some(value) = context.get("greeting") {
            assert_eq!(value, "hello");
        }

        self.loads.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn on_unload(&mut self) -> Result<()> {
        self.unloads.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn a_plugin_moves_through_its_whole_lifecycle() {
    let loads = Arc::new(AtomicUsize::new(0));
    let unloads = Arc::new(AtomicUsize::new(0));

    let mut registry = PluginRegistry::new();
    registry
        .register(Box::new(Recorder::new(
            "audit",
            Arc::clone(&loads),
            Arc::clone(&unloads),
        )))
        .expect("registers");

    assert_eq!(registry.state("audit"), Some(PluginState::Registered));
    assert_eq!(loads.load(Ordering::SeqCst), 0, "registering must not load");

    let context = PluginContext::new().with_setting("greeting", "hello");
    registry.load("audit", &context).expect("loads");

    assert_eq!(registry.state("audit"), Some(PluginState::Active));
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(registry.stats().active, 1);

    registry.unload("audit").expect("unloads");
    assert_eq!(unloads.load(Ordering::SeqCst), 1);
    assert!(!registry.contains("audit"), "unloading removes the plugin");
}

#[test]
fn one_failing_plugin_does_not_stop_the_others() {
    let loads = Arc::new(AtomicUsize::new(0));
    let unloads = Arc::new(AtomicUsize::new(0));

    let mut registry = PluginRegistry::new();
    for name in ["alpha", "beta"] {
        registry
            .register(Box::new(Recorder::new(
                name,
                Arc::clone(&loads),
                Arc::clone(&unloads),
            )))
            .expect("registers");
    }
    registry
        .register(Box::new(
            Recorder::new("broken", Arc::clone(&loads), Arc::clone(&unloads)).failing(),
        ))
        .expect("registers");

    let failures = registry.load_all(&PluginContext::new());

    assert_eq!(failures.len(), 1, "exactly one plugin should have failed");
    assert!(failures[0].to_string().contains("broken"));

    assert_eq!(
        loads.load(Ordering::SeqCst),
        2,
        "a misconfigured optional plugin must not take down the others"
    );

    let stats = registry.stats();
    assert_eq!(stats.active, 2);
    assert_eq!(stats.failed, 1);
    assert_eq!(registry.state("broken"), Some(PluginState::Failed));
}

#[test]
fn an_incompatible_plugin_is_refused_at_registration() {
    let loads = Arc::new(AtomicUsize::new(0));
    let unloads = Arc::new(AtomicUsize::new(0));

    let mut registry = PluginRegistry::with_api_version(ApiVersion::new(1, 2));

    // Built against a newer minor: may use extension points we lack.
    let error = registry
        .register(Box::new(
            Recorder::new("futuristic", Arc::clone(&loads), Arc::clone(&unloads))
                .targeting(ApiVersion::new(1, 9)),
        ))
        .expect_err("must be refused");

    assert!(matches!(error, Error::IncompatibleApi { .. }));
    assert!(error.is_permanent(), "retrying could never help");
    assert!(registry.is_empty(), "a refused plugin must not be retained");

    // Built against an older minor: uses only what has always existed.
    registry
        .register(Box::new(
            Recorder::new("venerable", loads, unloads).targeting(ApiVersion::new(1, 0)),
        ))
        .expect("an older minor is safe");
    assert_eq!(registry.len(), 1);
}

#[test]
fn duplicate_names_are_refused() {
    let loads = Arc::new(AtomicUsize::new(0));
    let unloads = Arc::new(AtomicUsize::new(0));

    let mut registry = PluginRegistry::new();
    registry
        .register(Box::new(Recorder::new(
            "audit",
            Arc::clone(&loads),
            Arc::clone(&unloads),
        )))
        .expect("registers");

    let error = registry
        .register(Box::new(Recorder::new("audit", loads, unloads)))
        .expect_err("the name is taken");

    assert!(matches!(error, Error::DuplicateName { .. }));
    assert_eq!(registry.len(), 1, "the original must survive");
    assert_eq!(registry.stats().rejected, 1);
}

#[test]
fn unloading_everything_reports_per_plugin_results() {
    let loads = Arc::new(AtomicUsize::new(0));
    let unloads = Arc::new(AtomicUsize::new(0));

    let mut registry = PluginRegistry::new();
    for name in ["alpha", "beta", "gamma"] {
        registry
            .register(Box::new(Recorder::new(
                name,
                Arc::clone(&loads),
                Arc::clone(&unloads),
            )))
            .expect("registers");
    }
    registry.load_all(&PluginContext::new());

    let failures = registry.unload_all();

    assert!(failures.is_empty());
    assert_eq!(unloads.load(Ordering::SeqCst), 3);
    assert!(registry.is_empty());
    assert!(registry.stats().is_empty_registry());
}

/// Compresses outbound by trivially escaping, and reverses it inbound.
struct Wrapper {
    marker: u8,
    priority: i32,
    name: String,
}

impl Wrapper {
    fn new(marker: u8, priority: i32) -> Self {
        Self {
            marker,
            priority,
            name: format!("wrapper-{marker}"),
        }
    }
}

impl Interceptor for Wrapper {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn on_outbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
        payload.insert(0, self.marker);
        payload.push(self.marker);
        Ok(Action::Continue)
    }

    fn on_inbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
        if payload.first() != Some(&self.marker) || payload.last() != Some(&self.marker) {
            return Err(Error::Interceptor {
                name: self.name.clone(),
                reason: "the payload was not wrapped by this interceptor".to_owned(),
            });
        }

        payload.remove(0);
        payload.pop();
        Ok(Action::Continue)
    }
}

#[test]
fn nested_transforms_unwind_correctly() {
    // The property that matters: outbound and inbound must be exact inverses,
    // which only holds because inbound runs in reverse order.
    let mut chain = InterceptorChain::new();
    chain.add(Box::new(Wrapper::new(1, 10)));
    chain.add(Box::new(Wrapper::new(2, 20)));
    chain.add(Box::new(Wrapper::new(3, 30)));

    let original = b"payload".to_vec();
    let mut payload = original.clone();

    chain.outbound(&mut payload).expect("wraps");
    assert_eq!(
        payload.first(),
        Some(&3),
        "the last interceptor wraps outermost"
    );
    assert!(payload.len() > original.len());

    chain.inbound(&mut payload).expect("unwraps");
    assert_eq!(
        payload, original,
        "a round trip through the chain must be lossless"
    );
}

#[test]
fn a_filtering_interceptor_drops_traffic() {
    struct Firewall;

    impl Interceptor for Firewall {
        fn name(&self) -> &str {
            "firewall"
        }

        fn priority(&self) -> i32 {
            -100
        }

        fn on_inbound(&self, payload: &mut Vec<u8>) -> Result<Action> {
            if payload.starts_with(b"BLOCK") {
                return Ok(Action::Drop);
            }
            Ok(Action::Continue)
        }
    }

    let counter = Arc::new(AtomicU64::new(0));

    struct Counting(Arc<AtomicU64>);

    impl Interceptor for Counting {
        fn name(&self) -> &str {
            "counting"
        }

        fn priority(&self) -> i32 {
            -200
        }

        fn on_inbound(&self, _payload: &mut Vec<u8>) -> Result<Action> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Action::Continue)
        }
    }

    let mut chain = InterceptorChain::new();
    chain.add(Box::new(Counting(Arc::clone(&counter))));
    chain.add(Box::new(Firewall));

    let mut allowed = b"ALLOW this".to_vec();
    assert_eq!(chain.inbound(&mut allowed).expect("runs"), Action::Continue);
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    let mut blocked = b"BLOCK this".to_vec();
    assert_eq!(chain.inbound(&mut blocked).expect("runs"), Action::Drop);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "an interceptor after the drop must not run"
    );
}
