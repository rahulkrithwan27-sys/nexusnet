# nexusnet-analytics

Measurement and statistics for NexusNet: what the network actually did.

## What's here

- **`Histogram`** — distributions with percentiles, in bounded memory.
- **`RateMeter`** — throughput, smoothed and lifetime-averaged.
- **`ConnectionStats`** — per-connection bytes, frames, errors, loss, latency,
  and jitter.

## Percentiles, not averages

A mean round-trip time of 40 ms is consistent with every request taking 40 ms,
and equally consistent with 95% taking 5 ms while the rest take 700 ms. Those
are very different links, and only the second generates complaints.

```rust
use std::time::Duration;
use nexusnet_analytics::Histogram;

let mut latency = Histogram::new();
for _ in 0..95 { latency.record(Duration::from_millis(5)); }
for _ in 0..5  { latency.record(Duration::from_millis(700)); }

assert!(latency.median().expect("samples") < Duration::from_millis(20));
assert!(latency.summary().has_heavy_tail());
```

## Bounded memory

`Histogram` uses 64 fixed logarithmic buckets rather than retaining samples, so
a process running for months uses the same memory as one that just started. The
cost is that percentiles are accurate to a bucket width (~26%) rather than
exact — the right trade when the question is whether the tail is 10 ms or
500 ms, not whether it's 412 ms or 415 ms. Min, max, and mean are tracked
exactly alongside.

The gap between mean and median is itself informative: a mean far above the
median means a heavy tail, which `has_heavy_tail()` reports directly.

## Jitter

`ConnectionStats` derives jitter from consecutive RTT measurements, so it
becomes available from the second sample onward. A steady link shows nearly
none; an alternating one shows large values — both are tested.

## An explicit clock

Every time-dependent type has `_at` constructors and methods taking an
`Instant`. Rates computed from a supplied clock are exact in tests rather than
depending on how long the test happened to take.

## Status

Implemented in **Phase 6**. See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
