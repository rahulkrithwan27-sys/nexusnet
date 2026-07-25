# nexusnet-scheduler

Traffic scheduling for NexusNet: deciding what to send, how fast, and in what
order.

## What's here

- **`PriorityQueue`** — weighted fair queueing across five priority classes.
- **`TokenBucket`** — rate limiting and shaping with a burst allowance.
- **`FlowController`** — per-stream credit windows, which remove head-of-line
  blocking from a multiplexed session.

## Weighted, not strict

Strict priority is the obvious scheduling design and the wrong one: a steady
stream of urgent traffic starves everything below it, and background work that
never runs becomes an outage that looks like a mystery.

`PriorityQueue` uses **deficit round robin**. Each class earns credit in
proportion to its weight (background 1, low 2, normal 4, high 8, critical 16)
and spends it to dequeue. Urgent traffic still goes first within a round, but
every class makes progress and the ratio is a configured number rather than an
emergent accident. A test saturates critical and background simultaneously and
asserts background is still served, at roughly the 16:1 ratio the weights imply.

Capacity is **per class**, so a flood of background work cannot consume the
space critical traffic will need.

## Rate limiting

```rust
use std::time::{Duration, Instant};
use nexusnet_scheduler::TokenBucket;

let start = Instant::now();
let mut bucket = TokenBucket::new_at(1000.0, 2000, start);   // 1000/s, burst 2000

assert!(bucket.try_consume_at(2000, start));
assert!(!bucket.try_consume_at(1, start));
assert!(bucket.try_consume_at(1000, start + Duration::from_secs(1)));
```

Every method has an `_at` variant taking an explicit `Instant`, so rate-limit
behavior is tested by driving the clock rather than sleeping — deterministic
instead of approximate.

## Flow control

This is the fix for the limitation flagged in `nexusnet-transport`'s
multiplexer. Without per-stream flow control a session either has no
backpressure (a slow consumer makes the sender buffer without bound) or applies
it connection-wide (a slow consumer stalls every stream). Both are wrong.

A credit window makes backpressure **per stream**: a receiver advertises how
many bytes it will accept, and the sender may send only that much. One stalled
consumer exhausts only its own window; every other stream keeps flowing. HTTP/2
and QUIC do exactly this, for exactly this reason.

`SendWindow` and `ReceiveWindow` are deliberately distinct types — conflating
the two directions is the classic flow-control bug. A peer that overruns its
window gets `FlowError::WindowExceeded`, which is a protocol violation rather
than congestion: close the connection.

Window updates are emitted once half the window has been consumed. Updating per
byte would flood the connection with bookkeeping; waiting until empty would
stall the sender.

## Testing

```bash
cargo test -p nexusnet-scheduler
```

## Status

Implemented in **Phase 5**. See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
