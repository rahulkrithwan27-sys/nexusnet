# Contributing to NexusNet

Thanks for your interest in improving NexusNet! This document explains how to get
set up and what we expect from a contribution.

## Getting started

1. Install the toolchain. With `rustup` installed, the pinned toolchain in
   [`rust-toolchain.toml`](rust-toolchain.toml) is selected automatically,
   including the `rustfmt` and `clippy` components.
2. Fork and clone the repository.
3. Build and test:

   ```bash
   cargo build --workspace
   cargo test --workspace
   ```

## Before you open a pull request

Every change must pass the same gates CI enforces:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

The MSRV is **1.75**; do not use APIs newer than that without raising the MSRV in
a dedicated change.

## Coding standards

- **No panics on the normal path.** Return `Result` and add a typed variant to
  `nexusnet_core::Error` (or a crate-local error) rather than calling `unwrap`,
  `expect`, or `panic!`. `unwrap`/`expect` are acceptable only in tests,
  benchmarks, and examples.
- **Document every public item.** `missing_docs` is denied in CI. Functions that
  return `Result` need an `# Errors` section; anything that can panic needs a
  `# Panics` section.
- **Prefer zero-copy and avoid needless allocation** on hot paths.
- **Keep the architecture explicit.** If you change a public API or a
  cross-crate boundary, explain the trade-offs in the PR description.
- Add unit tests next to the code, integration tests under the crate's `tests/`,
  and a Criterion benchmark for any performance-sensitive path.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/), e.g.
`feat(core): add graceful shutdown timeout`. Keep the subject under ~72
characters and explain the "why" in the body when it isn't obvious.

## Reporting bugs and requesting features

Use the GitHub issue templates. For anything security-related, follow
[`SECURITY.md`](SECURITY.md) instead of opening a public issue.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
