# nexusnet-telemetry

Metrics collection and export for NexusNet.

## What's here

- **`MetricsRegistry`** — named counters, gauges, and histograms.
- **`prometheus()`** and **`json()`** — rendering a registry for external
  systems.

```rust
use std::time::Duration;
use nexusnet_telemetry::{prometheus, MetricsRegistry};

let mut registry = MetricsRegistry::new();
registry.counter("frames_sent", "Frames written to the wire").increment(120);
registry.gauge("connections_open", "Currently open connections").set(4.0);
registry.histogram("request_latency", "End-to-end latency")
        .record(Duration::from_millis(35));

let exposition = prometheus(&registry);
assert!(exposition.contains("# TYPE frames_sent counter"));
```

## Three instruments, chosen deliberately

A **counter** only increases. Exporters rely on that monotonicity to compute
rates between scrapes, so a counter that could decrease would silently produce
wrong graphs — `increment` saturates rather than wrapping for the same reason.

A **gauge** moves both ways. Non-finite values are ignored: a `NaN` gauge
poisons every aggregate downstream of it.

A **histogram** records a distribution, because an average latency hides the
tail users actually notice.

## Stable output

Metrics are stored sorted by name, so two exports of unchanged state are
byte-identical. Unstable ordering makes exports impossible to diff and breaks
naive change detection.

## Escaping

Both exporters escape their output properly — newlines in Prometheus help text,
quotes and control characters in JSON. An unescaped tab in a description would
otherwise produce invalid JSON, which is the kind of bug that only appears once
someone writes an unusual description.

## No I/O

Exporters return a `String`. Where those bytes go — an HTTP response, a file, a
log line — is the caller's decision, which keeps this crate free of any server
or runtime dependency.

## Status

Implemented in **Phase 6**. Distributed tracing integration is still to come.
See [`docs/roadmap.md`](../../docs/roadmap.md).

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
