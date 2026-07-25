# nexusnet-plugin-api

Extension points for NexusNet: traits a third party implements, and a registry
that loads them.

## What's here

- **`Plugin`** — the lifecycle trait every plugin implements.
- **`Interceptor` / `InterceptorChain`** — the data-path extension point, where
  payloads can be observed or transformed.
- **`PluginRegistry`** — registration, loading, unloading.
- **`ApiVersion`** — the compatibility check that decides what may load.

```rust
use nexusnet_plugin_api::{Plugin, PluginContext, PluginMetadata, PluginRegistry};

struct Audit;

impl Plugin for Audit {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata::new("audit", "0.1.0")
    }
}

let mut registry = PluginRegistry::new();
registry.register(Box::new(Audit))?;
registry.load_all(&PluginContext::new());

assert_eq!(registry.stats().active, 1);
# Ok::<(), nexusnet_plugin_api::Error>(())
```

## Versioning is the whole point

A plugin is written against a particular set of extension points. When those
change, a plugin built against the old shape misbehaves at runtime, in whatever
way the mismatch happens to manifest — the hardest kind of bug to attribute.

`ApiVersion` makes the check explicit: majors must match exactly, and a plugin
may target an **older** minor than the host but never a newer one. An older minor
uses only extension points that have always existed; a newer one may use points
this host doesn't have. Registration is refused outright, and the error reports
`is_permanent()` so a caller knows retrying can't help.

## Ordering, and why inbound runs backwards

`InterceptorChain` runs interceptors in priority order outbound and in
**reverse** order inbound.

That symmetry is what lets a transforming pair work. Something that compresses
on the way out must be the last to touch the payload outbound and the first to
touch it inbound — otherwise it would try to decompress bytes another
interceptor had already altered. Getting this backwards is the classic
middleware bug, so there's a test with three nested wrappers asserting a round
trip is lossless.

## Failure isolation

One plugin failing to load doesn't stop the others. `load_all` returns a `Vec`
of failures rather than bailing at the first, and the failed plugin is marked
`Failed` and never called again. A misconfigured optional plugin shouldn't take
down a process that would otherwise run fine.

Unloading is the mirror image: an error during a plugin's own cleanup is
reported, but the plugin is removed regardless — otherwise a failing plugin
could never be got rid of.

## No dynamic loading

Plugins are ordinary Rust values compiled into the binary. This crate does
**not** load shared libraries at runtime, and that's deliberate: Rust has no
stable ABI, so a plugin compiled by a different compiler version — or with
different flags — can produce undefined behaviour when its types cross the
boundary.

The version check catches an *API* mismatch. Nothing in Rust can catch an *ABI*
mismatch. A C-ABI surface under `sdk/` is the safe route to runtime loading,
because C's ABI is stable in a way Rust's is not.

## Testing

```bash
cargo test -p nexusnet-plugin-api
```

## Status

Implemented in **Phase 8**. SDKs and the dashboard remain. See
[`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
