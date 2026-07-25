//! Black-box integration tests for `nexusnet-core`.
//!
//! These tests exercise only the public API, the way a downstream consumer
//! would. White-box unit tests for private helpers live alongside the code in
//! `src/`.

use std::time::Duration;

use nexusnet_core::{Engine, EngineConfig, EngineState, Error, LogFormat, LogLevel};

#[test]
fn engine_drives_through_full_lifecycle() {
    let engine = Engine::builder()
        .name("integration")
        .build()
        .expect("configuration is valid");

    assert_eq!(engine.state(), EngineState::Created);
    engine.start().expect("engine starts");
    assert_eq!(engine.state(), EngineState::Running);
    engine.shutdown().expect("engine shuts down");
    assert_eq!(engine.state(), EngineState::Stopped);
}

#[test]
fn with_config_matches_builder() {
    let config = EngineConfig::builder()
        .name("via-config")
        .log_level(LogLevel::Warn)
        .log_format(LogFormat::Compact)
        .shutdown_timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let engine = Engine::with_config(config.clone()).unwrap();
    assert_eq!(engine.config(), &config);
}

#[test]
fn invalid_configuration_surfaces_as_error() {
    let result = Engine::builder().name("").build();
    assert!(matches!(
        result,
        Err(Error::InvalidConfig { field: "name", .. })
    ));
}

#[test]
fn default_engine_builds_and_reports_version() {
    let engine = Engine::new().expect("default engine builds");
    assert_eq!(engine.config().name, "nexusnet");
    assert!(nexusnet_core::version_string().contains(nexusnet_core::VERSION));
}

/// Environment overrides mutate process-global state, so every assertion that
/// touches `NEXUSNET_*` variables lives in this single test to avoid races with
/// the parallel test runner.
#[test]
fn environment_overrides_are_applied() {
    // SAFETY (edition 2021): `set_var`/`remove_var` are safe here; this is the
    // only test that reads or writes these variables.
    std::env::set_var("NEXUSNET_NAME", "from-env");
    std::env::set_var("NEXUSNET_LOG_LEVEL", "debug");
    std::env::set_var("NEXUSNET_WORKER_THREADS", "3");
    std::env::set_var("NEXUSNET_SHUTDOWN_TIMEOUT_SECS", "12");
    std::env::set_var("NEXUSNET_METRICS_ENABLED", "yes");

    let engine = Engine::builder()
        .name("will-be-overridden")
        .apply_env_overrides(true)
        .build()
        .expect("environment values are valid");

    let config = engine.config();
    assert_eq!(config.name, "from-env");
    assert_eq!(config.log_level, LogLevel::Debug);
    assert_eq!(config.worker_threads, Some(3));
    assert_eq!(config.shutdown_timeout, Duration::from_secs(12));
    assert!(config.metrics_enabled);

    // A bad value must surface as a typed error.
    std::env::set_var("NEXUSNET_WORKER_THREADS", "not-a-number");
    let err = EngineConfig::default()
        .with_env_overrides()
        .expect_err("invalid worker thread count must fail");
    assert!(matches!(err, Error::InvalidEnvVar { .. }));

    for key in [
        "NEXUSNET_NAME",
        "NEXUSNET_LOG_LEVEL",
        "NEXUSNET_WORKER_THREADS",
        "NEXUSNET_SHUTDOWN_TIMEOUT_SECS",
        "NEXUSNET_METRICS_ENABLED",
    ] {
        std::env::remove_var(key);
    }
}
