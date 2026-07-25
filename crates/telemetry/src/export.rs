//! Exporting metrics for external systems.
//!
//! Two formats cover the common cases: the Prometheus text exposition format,
//! which most scrapers understand, and JSON, which dashboards and ad-hoc tools
//! prefer.
//!
//! Both are produced from a [`MetricsRegistry`] snapshot without any I/O, so a
//! caller decides where the bytes go — an HTTP response, a file, a log line.

use std::fmt::Write as _;

use crate::registry::{MetricValue, MetricsRegistry};

/// Renders metrics in the Prometheus text exposition format.
///
/// Names are sanitized to the character set Prometheus permits, and histograms
/// are emitted as a count, a sum, and quantile series — the shape a scraper
/// expects rather than an invented one.
///
/// # Examples
///
/// ```
/// use nexusnet_telemetry::{prometheus, MetricsRegistry};
///
/// let mut registry = MetricsRegistry::new();
/// registry.counter("frames_sent", "Frames written").increment(42);
///
/// let text = prometheus(&registry);
/// assert!(text.contains("# TYPE frames_sent counter"));
/// assert!(text.contains("frames_sent 42"));
/// ```
#[must_use]
pub fn prometheus(registry: &MetricsRegistry) -> String {
    let mut output = String::new();

    for sample in registry.samples() {
        let name = sanitize(&sample.name);

        // `writeln!` to a String cannot fail, so the results are discarded
        // deliberately rather than unwrapped.
        let _ = writeln!(output, "# HELP {name} {}", escape_help(&sample.description));
        let _ = writeln!(output, "# TYPE {name} {}", sample.value.kind());

        match &sample.value {
            MetricValue::Counter(value) => {
                let _ = writeln!(output, "{name} {value}");
            }
            MetricValue::Gauge(value) => {
                let _ = writeln!(output, "{name} {value}");
            }
            MetricValue::Histogram(summary) => {
                let _ = writeln!(output, "{name}_count {}", summary.count);

                let sum_seconds = summary
                    .mean
                    .map_or(0.0, |mean| mean.as_secs_f64() * summary.count as f64);
                let _ = writeln!(output, "{name}_sum {sum_seconds}");

                for (quantile, value) in [
                    ("0.5", summary.p50),
                    ("0.9", summary.p90),
                    ("0.99", summary.p99),
                ] {
                    if let Some(value) = value {
                        let _ = writeln!(
                            output,
                            "{name}{{quantile=\"{quantile}\"}} {}",
                            value.as_secs_f64()
                        );
                    }
                }
            }
        }
    }

    output
}

/// Renders metrics as a JSON object keyed by metric name.
///
/// Hand-rolled rather than pulling in a serializer: the shape is small and
/// fixed, and this crate is a leaf that other things depend on.
///
/// # Examples
///
/// ```
/// use nexusnet_telemetry::{json, MetricsRegistry};
///
/// let mut registry = MetricsRegistry::new();
/// registry.gauge("connections_open", "Open connections").set(3.0);
///
/// let text = json(&registry);
/// assert!(text.contains("\"connections_open\""));
/// assert!(text.contains("\"kind\":\"gauge\""));
/// ```
#[must_use]
pub fn json(registry: &MetricsRegistry) -> String {
    let mut output = String::from("{");

    for (index, sample) in registry.samples().iter().enumerate() {
        if index > 0 {
            output.push(',');
        }

        let _ = write!(
            output,
            "\"{}\":{{\"kind\":\"{}\",\"description\":\"{}\"",
            escape_json(&sample.name),
            sample.value.kind(),
            escape_json(&sample.description)
        );

        match &sample.value {
            MetricValue::Counter(value) => {
                let _ = write!(output, ",\"value\":{value}");
            }
            MetricValue::Gauge(value) => {
                let _ = write!(output, ",\"value\":{value}");
            }
            MetricValue::Histogram(summary) => {
                let _ = write!(output, ",\"count\":{}", summary.count);

                for (key, value) in [
                    ("p50", summary.p50),
                    ("p90", summary.p90),
                    ("p99", summary.p99),
                    ("min", summary.min),
                    ("max", summary.max),
                    ("mean", summary.mean),
                ] {
                    if let Some(value) = value {
                        let _ = write!(output, ",\"{key}\":{}", value.as_secs_f64());
                    }
                }
            }
        }

        output.push('}');
    }

    output.push('}');
    output
}

/// Replaces characters Prometheus does not permit in a metric name.
fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // A name may not begin with a digit.
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }

    if out.is_empty() {
        out.push('_');
    }

    out
}

/// Escapes the characters that would break a HELP line.
fn escape_help(text: &str) -> String {
    text.replace('\\', r"\\").replace('\n', "\\n")
}

/// Escapes the characters that would break a JSON string.
fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());

    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn an_empty_registry_exports_cleanly() {
        let registry = MetricsRegistry::new();

        assert!(prometheus(&registry).is_empty());
        assert_eq!(json(&registry), "{}");
    }

    #[test]
    fn a_counter_exports_with_help_and_type() {
        let mut registry = MetricsRegistry::new();
        registry
            .counter("frames_sent", "Frames written")
            .increment(42);

        let text = prometheus(&registry);
        assert!(text.contains("# HELP frames_sent Frames written"));
        assert!(text.contains("# TYPE frames_sent counter"));
        assert!(text.contains("\nframes_sent 42\n") || text.contains("frames_sent 42"));
    }

    #[test]
    fn a_gauge_exports_its_reading() {
        let mut registry = MetricsRegistry::new();
        registry
            .gauge("connections_open", "Open connections")
            .set(7.0);

        let text = prometheus(&registry);
        assert!(text.contains("# TYPE connections_open gauge"));
        assert!(text.contains("connections_open 7"));
    }

    #[test]
    fn a_histogram_exports_count_sum_and_quantiles() {
        let mut registry = MetricsRegistry::new();
        for millis in [10, 20, 30, 40, 50] {
            registry
                .histogram("latency", "Request latency")
                .record(Duration::from_millis(millis));
        }

        let text = prometheus(&registry);
        assert!(text.contains("# TYPE latency histogram"));
        assert!(text.contains("latency_count 5"));
        assert!(text.contains("latency_sum"));
        assert!(text.contains("quantile=\"0.5\""));
        assert!(text.contains("quantile=\"0.99\""));
    }

    #[test]
    fn names_are_sanitized_for_prometheus() {
        let mut registry = MetricsRegistry::new();
        registry
            .counter("bytes/sent-total", "Odd name")
            .increment(1);

        let text = prometheus(&registry);
        assert!(
            text.contains("bytes_sent_total"),
            "illegal characters must be replaced, got: {text}"
        );
    }

    #[test]
    fn a_leading_digit_is_prefixed() {
        assert_eq!(sanitize("5xx_errors"), "_5xx_errors");
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("valid_name:9"), "valid_name:9");
    }

    #[test]
    fn help_text_is_escaped() {
        let mut registry = MetricsRegistry::new();
        registry.counter("weird", "Line one\nline two").increment(1);

        let text = prometheus(&registry);
        let help_line = text
            .lines()
            .find(|line| line.starts_with("# HELP"))
            .expect("a help line exists");

        assert!(
            !help_line.contains('\n'),
            "a raw newline would break the exposition format"
        );
        assert!(help_line.contains("\\n"));
    }

    #[test]
    fn json_includes_kind_and_value() {
        let mut registry = MetricsRegistry::new();
        registry.counter("errors", "Errors seen").increment(3);

        let text = json(&registry);
        assert!(text.starts_with('{') && text.ends_with('}'));
        assert!(text.contains("\"errors\""));
        assert!(text.contains("\"kind\":\"counter\""));
        assert!(text.contains("\"value\":3"));
    }

    #[test]
    fn json_separates_multiple_metrics() {
        let mut registry = MetricsRegistry::new();
        registry.counter("a", "First").increment(1);
        registry.counter("b", "Second").increment(2);

        let text = json(&registry);
        assert!(
            text.contains("},\""),
            "entries must be comma separated: {text}"
        );
    }

    #[test]
    fn json_escapes_quotes_and_control_characters() {
        let mut registry = MetricsRegistry::new();
        registry
            .counter("quoted", "He said \"hello\"\tand left")
            .increment(1);

        let text = json(&registry);
        assert!(text.contains("\\\""), "quotes must be escaped: {text}");
        assert!(text.contains("\\t"));
        assert!(!text.contains('\t'), "a raw tab would produce invalid JSON");
    }

    #[test]
    fn json_includes_histogram_percentiles() {
        let mut registry = MetricsRegistry::new();
        for millis in [10, 20, 30] {
            registry
                .histogram("latency", "Latency")
                .record(Duration::from_millis(millis));
        }

        let text = json(&registry);
        assert!(text.contains("\"count\":3"));
        assert!(text.contains("\"p50\":"));
        assert!(text.contains("\"max\":"));
    }

    #[test]
    fn export_order_is_stable() {
        let mut registry = MetricsRegistry::new();
        registry.counter("zebra", "Z").increment(1);
        registry.counter("alpha", "A").increment(1);

        let first = prometheus(&registry);
        let second = prometheus(&registry);

        assert_eq!(first, second);
        assert!(
            first.find("alpha").expect("present") < first.find("zebra").expect("present"),
            "stable ordering is what makes two exports diffable"
        );
    }
}
