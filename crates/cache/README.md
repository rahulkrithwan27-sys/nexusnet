# nexusnet-cache

Caching and content deduplication for NexusNet.

## What's here

- **`LruCache`** — least-recently-used caching with optional per-entry expiry
  and byte-aware capacity.
- **`Deduplicator` / `DedupStore`** — content-addressed deduplication, so a
  payload that already crossed the wire is sent as a 16-byte digest instead of
  again in full.

## Two capacity limits, not one

Most caches bound entry *count*. That says nothing about memory when entry sizes
vary by four orders of magnitude — which network payloads do. Bounding bytes
alone lets a flood of tiny entries balloon the bookkeeping. `LruCache` enforces
both and evicts by least-recent use until each is satisfied.

```rust
use std::time::Duration;
use nexusnet_cache::LruCache;

let mut cache: LruCache<String, Vec<u8>> = LruCache::new(1024)
    .with_max_bytes(8 * 1024 * 1024)
    .with_default_ttl(Duration::from_secs(60));

cache.insert("session:42".to_owned(), b"payload".to_vec());
assert!(cache.get("session:42").is_some());
```

A single value larger than the byte budget is still stored — refusing it would
turn a cache into a silent failure — but it evicts everything else to fit.

Expiry is **lazy**: an expired entry costs nothing until it's looked at. Call
`purge_expired()` periodically if entries may go untouched for long.

## Deduplication

Sending content once and thereafter sending a digest is the largest bandwidth
saving available — larger than any compression ratio, because the best case
isn't "smaller" but "nothing at all".

```rust
use bytes::Bytes;
use nexusnet_cache::Deduplicator;

let mut dedup = Deduplicator::new(1024);
let payload = Bytes::from(vec![b'x'; 4096]);

let first = dedup.offer(payload.clone());   // Content: send the bytes
let second = dedup.offer(payload);          // Cached: send 16 bytes
assert_eq!(second.wire_len(), 16);
```

Payloads under 64 bytes are never deduplicated, since a digest wouldn't be
smaller. If the receiver's `DedupStore` doesn't hold a referenced digest — after
a restart, say — `resolve` returns `None` so the sender knows to resend rather
than silently serving nothing.

### Security note

The digest is a fast 128-bit non-cryptographic hash. That's appropriate when
both peers are trusted and the concern is accidental collision. **An attacker
who can choose payloads could construct a collision and cause the wrong content
to be served.** Before deduplicating across a trust boundary, switch to a
cryptographic digest — that belongs with Phase 4, where a hash primitive is
already a dependency.

## Performance

Measured on the bundled benchmarks:

| Operation | Cost |
| --------- | ---- |
| `get` (hit) | ~51 ns |
| `insert` (no eviction) | ~140 ns |
| `insert` (with eviction) | ~198 ns |
| digest | ~500 MiB/s |

Eviction uses an ordered index rather than scanning for the least-recently-used
entry, and expiry sweeps are skipped entirely when no entry carries a TTL.
Together those took the eviction path from 18.9 ms to 1.62 ms per 8192 inserts —
an 11.7x improvement over the naive implementation.

## Testing & benchmarks

```bash
cargo test  -p nexusnet-cache
cargo bench -p nexusnet-cache
```

## Status

Implemented in **Phase 5**. Delta synchronization and disk tiering are still to
come. See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
