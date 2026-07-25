# nexusnet-optimizer

Adaptive optimization for NexusNet: measuring the network and turning those
measurements into sending decisions.

## What's here

- **`BandwidthEstimator`** — smoothed link capacity, plus the peak ever seen.
- **`RttEstimator`** — smoothed round-trip time, its variation, and a
  retransmission timeout.
- **`Optimizer`** — converts both into a `Recommendation`: payload size,
  compression level, retry timeout, and bytes in flight.

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
