# nexusnet-router

Route selection, load balancing, and health-aware failover.

## What's here

- **`Router`** — holds endpoints, picks one per request, withdraws those that
  fail.
- **`Strategy`** — round robin, weighted round robin, or least connections.
- **`HealthTracker`** — the circuit breaker behind automatic failover.

```rust
use std::time::Instant;
use nexusnet_router::{Router, Strategy};

let now = Instant::now();
let mut router: Router<&str> = Router::new(Strategy::RoundRobin);

let primary = router.add("10.0.0.1:9000");
let secondary = router.add("10.0.0.2:9000");

assert_eq!(router.select(now), Some(primary));

// Repeated failures withdraw an endpoint automatically.
for _ in 0..3 { router.record_failure(primary, now); }
assert_eq!(router.select(now), Some(secondary));
```

## Recovery is the hard part

Removing a failing endpoint is easy. Deciding when to put it **back** is what
goes wrong — return it too eagerly and traffic keeps hitting a broken server;
never return it and a transient blip permanently shrinks the pool.

The router uses a circuit breaker:

1. Consecutive failures withdraw the endpoint (`Unhealthy`).
2. After a cooldown it becomes `Recovering` — due a probe, but still refused
   ordinary traffic.
3. A **single** probe decides. Success (after the configured threshold) restores
   it; failure restarts the cooldown rather than retrying immediately.

One request is risked, not all of them. A recovering endpoint is probed *ahead*
of ordinary selection, because that probe is what restores capacity.

Isolated failures interrupted by successes never withdraw an endpoint — the
streak resets, so a flaky-but-working backend stays in the pool.

## Strategies

**Round robin** is fair when endpoints are equivalent and requests cost about
the same.

**Weighted round robin** serves each endpoint for as many turns as its weight —
use it when backends differ in capacity.

**Least connections** adapts to requests of uneven cost, which round robin
cannot: a backend stuck on a slow request stops attracting more work. Load is
normalized by weight, so a bigger backend accepts more before it looks equally
loaded. Ties break deterministically rather than by iteration order.

## Reporting exhaustion

`select()` returns `None` when every endpoint is withdrawn and none is due a
probe. That's deliberate: routing to a known-dead backend produces a slow
failure and wasted work, whereas an honest `None` lets the caller apply
backpressure or fail fast.

## Status

Implemented in **Phase 6**. See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
