# nexusnet-tls

TLS 1.3 transport security and authenticated key exchange for NexusNet.

Requires **Rust 1.85** (the modern TLS stack needs edition 2024). This is
confined to this crate; the rest of the workspace builds on 1.75.

## What it does

TLS closes the gap `nexusnet-encryption` leaves open: that layer protects a
session once both peers share a secret, but says nothing about how they came to
share one. TLS supplies the authenticated key exchange — the certificate proves
who the peer is — and the two layers are bound via RFC 5705 keying material
export, so an interceptor terminating TLS separately with each side cannot make
both derive the same key.

## Defaults

- **TLS 1.3 only** unless `allow_tls12` is set.
- **Certificate verification is mandatory** for clients — no switch disables it.
- **`ring`** crypto provider, so builds need no C toolchain.

## Mutual TLS

By default the server authenticates to the client. For service-to-service links
where both ends must prove identity, `build_server_with_client_auth` requires
each client to present a certificate chaining to a trust store you supply, and
`build_client_with_cert` presents one. A client with no certificate, or one from
an untrusted authority, is refused at the handshake; both are covered by tests.
An empty client-trust store is rejected at build time rather than silently
accepting no one.

## Testing

```bash
cargo test -p nexusnet-tls
```

The security-relevant tests are the rejections: an untrusted server certificate,
a mismatched hostname, a client with no certificate, and a client with an
untrusted certificate all fail the handshake. A passing round trip proves
nothing on its own; a passing rejection does.

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
