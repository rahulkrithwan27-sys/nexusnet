# nexusnet-optimizer

Adaptive optimization for NexusNet: measuring the network and turning those
measurements into sending decisions.

## What's here

- **`NetworkOptimizer`** — the entry point: observes conditions, grades the
  link, and returns a complete `OptimizationPlan`.
- **`NetworkQuality`** — a coarse grade derived from bandwidth, latency, and
  loss.
- **`BandwidthEstimator`**, **`RttEstimator`**, **`LossEstimator`** — smoothed
  estimates of each dimension.
- **`CompressionStrategy`**, **`CacheStrategy`**, **`DeltaSyncStrategy`** — the
  individual decisions, usable on their own.
- **`OptimizationMetrics`** — samples seen, quality transitions, and current
  estimates.
- **`Optimizer`** — the narrower predecessor, producing a `Recommendation` from
  bandwidth and latency alone. Retained for callers that don't measure loss.

## The network optimizer

```rust
use std::time::Duration;
use nexusnet_optimizer::NetworkOptimizer;

let mut optimizer = NetworkOptimizer::new();

for _ in 0..20 {
    optimizer.record_delivery(32 * 1024, Duration::from_secs(1));
    optimizer.record_rtt(Duration::from_millis(300));
    optimizer.record_loss(95, 5);
}

let plan = optimizer.plan();
assert!(plan.quality.is_degraded());
assert!(plan.compression.enabled);
assert!(plan.delta_sync.enabled);
```

One `plan()` call answers every adaptive question: payload size, compression
level, cache capacity and TTL, whether to send deltas, retry timeout, and bytes
in flight.

## Quality detection: worst dimension wins

A connection with 100 MiB/s of bandwidth and 20% packet loss is not a good
connection, and averaging those facts flatters it. `NetworkQuality` grades each
dimension separately and reports the **worst**, because the failing dimension is
what the application actually experiences.

Two details that matter:

- **An unmeasured dimension never counts against the link.** A fresh connection
  that has observed no loss isn't graded as lossy — it's graded on what's known.
- **A thin loss sample is ignored.** One loss out of three packets is a 33%
  ratio and means nothing, so loss only contributes once at least 20 packets
  have been observed.

Loss is graded harshly on purpose: every lost packet costs a retransmission
*and* a round trip of delay, so a few percent hurts more than the raw number
suggests.

## The strategies

**Compression** intensifies as the link degrades — the trade is time, and a slow
link can afford far more CPU per byte than a fast one. Above `Excellent`, it's
disabled entirely: most payloads transmit in less time than compressing them
takes.

**Caching** spends more memory and tolerates staler data as the link worsens,
since re-fetching gets expensive.

**Delta sync** activates only from `Fair` down. On a fast link the bookkeeping
and the risk of a stale base — which costs a whole extra round trip — outweigh
the bytes saved. A computed delta is also rejected if it isn't meaningfully
smaller than the original; a delta saving 5% still costs both peers the work.

## Advice, not action

Nothing here sends, compresses, or caches anything, and the crate depends on no
other NexusNet crate. It reads measurements and returns values, which keeps the
policy testable in isolation and the mechanism crates independent of it.

`OptimizationPlan::confident` reports whether the figures rest on enough
measurement to act on. Acting hard on one or two samples is how an adaptive
system starts oscillating.

## Advice, not action

Nothing here sends data or reaches into another subsystem. `Optimizer` returns
recommendations a caller may apply or ignore. That keeps the policy testable on
its own and leaves the mechanism crates — transport, compression, scheduler —
with no dependency on it.

```rust
use std::time::Duration;
use nexusnet_optimizer::{CompressionAdvice, Optimizer};

let mut optimizer = Optimizer::new();
for _ in 0..10 {
    optimizer.record_delivery(32 * 1024, Duration::from_secs(1));
    optimizer.record_rtt(Duration::from_millis(300));
}

let advice = optimizer.recommend();
assert_eq!(advice.compression, CompressionAdvice::Maximum);
```

## How the decisions are made

**Compression level follows bandwidth, not payload content.** The trade is
time: compressing is worth it exactly when the transmission time saved exceeds
the CPU time spent. Below 256 KiB/s, compress hard — the CPU is repaid many
times over. Above 32 MiB/s, don't bother; most payloads transmit in less time
than compressing them takes.

**Payload size follows the bandwidth-delay product.** Aiming at roughly an
eighth of it keeps several payloads in flight rather than one large one, so loss
is cheap to recover and latency stays low. Clamped to 1 KiB–1 MiB.

**Retry timeout uses the Jacobson/Karels algorithm**, the same one TCP uses:
smoothed RTT plus four times its mean deviation. Using the average alone
produces a timeout that fires constantly on a jittery link — the variation term
is what makes it usable. Bounded to 200 ms–60 s.

## Confidence

`Recommendation::confident` reports whether the figures rest on enough
measurement to trust. Acting on one or two samples is how an adaptive system
starts oscillating; when `confident` is false the values are defaults rather
than conclusions.

Estimators use exponentially weighted moving averages rather than a window mean:
constant memory, smooth response, and old conditions forgotten automatically. A
single outlier is damped (tested), while a *sustained* change is tracked
(also tested).

## Testing

```bash
cargo test -p nexusnet-optimizer
```

## Status

Implemented in **Phase 7**. Congestion prediction and predictive scheduling are
still to come. See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
