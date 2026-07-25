# Cross-crate tests

End-to-end tests that span multiple crates live here in later phases (for
example, a client/server round-trip over a real transport).

Per-crate integration tests live in each crate's own `tests/` directory, e.g.
[`crates/core/tests/`](../crates/core/tests/).
