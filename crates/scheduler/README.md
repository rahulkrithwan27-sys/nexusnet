# nexusnet-scheduler

Traffic scheduling for NexusNet: deciding what to send, how fast, and in what
order.

## What's here

- **`PacketScheduler`** — the entry point, tying the rest together.
- **`PriorityQueue`** — weighted fair queueing across five priority classes.
- **`TokenBucket`** — rate limiting with a burst allowance.
- **`TrafficShaper`** — an aggregate send rate with optional per-class
  reservations.
- **`RetryManager`** — retransmission scheduling with backed-off, jittered
  delays.
- **`FlowController`** — per-stream credit windows, which remove head-of-line
  blocking from a multiplexed session.
- **`SchedulerMetrics`** — a consistent snapshot of what the scheduler did.

## The packet scheduler

`PacketScheduler` owns the queue, the shaper, and the retry manager, and drives
them from a single `poll_at` call. A caller loops: poll, send what comes back,
report the outcome.

```rust
use std::time::Instant;
use nexusnet_scheduler::{Dispatch, PacketScheduler, Priority, SchedulerConfig};

let now = Instant::now();
let mut scheduler: PacketScheduler<&str> =
    PacketScheduler::new_at(SchedulerConfig::new().with_rate(1_000_000.0), now);

scheduler.enqueue(Priority::Background, 512, "bulk upload")?;
scheduler.enqueue(Priority::Critical, 64, "keepalive")?;

match scheduler.poll_at(now) {
    Dispatch::Send(packet) => {
        assert_eq!(*packet.payload(), "keepalive");   // urgent traffic first
        scheduler.acknowledge(packet.id());
    }
    Dispatch::Wait { delay } => { /* sleep for `delay`, then poll again */ }
    Dispatch::Idle => { /* nothing to do */ }
    _ => {}
}
# Ok::<(), nexusnet_scheduler::EnqueueError>(())
```

### No I/O, and an explicit clock

The scheduler sends nothing and owns no timer. It's a state machine driven by an
`Instant` the caller supplies, so rate limiting, backoff, and priority behaviour
under load are all tested deterministically rather than by sleeping and hoping.
The same type works from a Tokio task, a plain thread, or a simulation.

### Design decisions worth knowing

**A deferred packet keeps its place.** When the rate limiter holds a packet
back, it's parked on the scheduler rather than pushed back into the queue. It's
reconsidered first on the next poll, so a rate-limited packet can't lose its
place to traffic that arrives while it waits.

**An unsendable packet is dropped, not retried forever.** A payload larger than
the whole burst capacity can never be admitted — no amount of waiting helps — so
it's dropped and counted rather than left blocking everything behind it.

**`Wait` accounts for both deadlines.** The delay returned is the sooner of the
rate limit clearing and the next retry falling due, so sleeping on it wastes no
time and misses nothing.

**Retries re-enter the queue.** A retransmission competes fairly with fresh
traffic in its class rather than jumping ahead, which keeps one failing stream
from monopolising the link.

**Identifiers are never reused,** and a rejected enqueue doesn't consume one, so
gaps in the sequence always mean something real.

## Retry management

`RetryPolicy` applies exponential backoff with jitter; `RetryManager` holds
pending retries in a due-time-ordered heap, so releasing them is `O(log n)`
rather than a scan. Ties break by scheduling order, so a burst of failures
retries in the sequence it failed.

Jitter matters as much as the backoff: clients retrying on a fixed interval
synchronise and hammer a service exactly as it tries to recover. Delays are
uniform in `[d/2, d]`.

## Traffic shaping

`TrafficShaper` layers two limits over the token bucket: an aggregate cap, and
an optional reservation per priority class.

The reservation is the point. An aggregate limit alone is first-come,
first-served, so a large background upload can consume the entire budget and
leave a heartbeat queued behind it. A reserved class draws on its own bucket
first and only then competes for the shared one.

```rust
use nexusnet_scheduler::{Priority, TrafficShaper};

let shaper = TrafficShaper::new(1_000_000.0)          // 1 MB/s aggregate
    .with_reservation(Priority::Critical, 0.2);       // 20% reserved
```

Charging is all-or-nothing: a packet that must wait is charged nothing, so
partial deductions can't accumulate and stall a stream.

## Metrics

`SchedulerMetrics` is an immutable snapshot rather than a live reference, so a
caller computing several derived figures sees one consistent moment. It reports
counts for enqueued, dispatched, rejected, dropped, shaped, retried, and
acknowledged packets, bytes split between first sends and retransmissions, and
instantaneous pending/in-flight/awaiting-retry depths.

`retransmission_ratio()` is the health signal worth watching: a rising value
means the link is losing packets or the retry timeout is too aggressive.

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
