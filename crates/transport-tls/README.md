# nexusnet-transport-tls

TLS-secured framed connections for NexusNet: [`nexusnet-transport`] running over
[`nexusnet-tls`].

Requires **Rust 1.85** (via `nexusnet-tls`).

## Why this crate exists

If `nexusnet-transport` depended on `nexusnet-tls` for an optional TLS feature
while `nexusnet-tls` depended on `nexusnet-transport`, the two would form a
publish cycle that crates.io cannot resolve. This crate holds the integration
instead — it depends on both, and nothing depends on it — so the two lower
crates stay independent and publishable.

## Usage

```rust
# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
use nexusnet_transport_tls::{connect_tls_default, TlsListener};
use nexusnet_transport::TransportConfig;

// Client: connect over TLS to a publicly-trusted server, get a framed connection.
let mut connection = connect_tls_default(
    "example.com:8443".parse()?,
    "example.com",
    TransportConfig::default(),
).await?;
# Ok(())
# }
```

Because `Connection<S>` is generic over any `AsyncRead + AsyncWrite` stream and
the TLS streams are exactly that, this is a thin convenience layer, not new
protocol machinery.

## Example

```bash
cargo run -p nexusnet-transport-tls --example echo
```

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).

[`nexusnet-transport`]: https://crates.io/crates/nexusnet-transport
[`nexusnet-tls`]: https://crates.io/crates/nexusnet-tls
