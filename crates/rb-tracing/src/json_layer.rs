use opentelemetry::trace::TraceContextExt as _;
use serde_json::{Map, Value};
use std::{fmt, io::Write, sync::Mutex};
use tracing::{Event, Subscriber};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

// ── Timestamp ────────────────────────────────────────────────────────────────

/// Converts days since the Unix epoch (1970-01-01) to (year, month, day) in
/// the proleptic Gregorian calendar.  Algorithm by Howard E. Hinnant
/// (<https://howardhinnant.github.io/date_algorithms.html>, "`civil_from_days`").
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Returns the current UTC time formatted as RFC 3339 with nanosecond precision,
/// e.g. `2024-06-01T12:34:56.123456789Z`.
fn timestamp_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = i64::try_from(d.as_secs()).unwrap_or_default();
    let nanos = d.subsec_nanos();

    let days = total_secs / 86_400;
    let sod = total_secs % 86_400;
    let h = sod / 3600;
    let mi = (sod % 3600) / 60;
    let s = sod % 60;

    let (year, month, day) = civil_from_days(days);

    if nanos == 0 {
        format!("{year:04}-{month:02}-{day:02}T{h:02}:{mi:02}:{s:02}Z")
    } else {
        let ns = format!("{nanos:09}");
        let trimmed = ns.trim_end_matches('0');
        format!("{year:04}-{month:02}-{day:02}T{h:02}:{mi:02}:{s:02}.{trimmed}Z")
    }
}

// ── Field visitor ─────────────────────────────────────────────────────────────

struct JsonVisitor<'a>(&'a mut Map<String, Value>);

impl tracing::field::Visit for JsonVisitor<'_> {
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_i128(&mut self, field: &tracing::field::Field, value: i128) {
        self.0.insert(field.name().to_owned(), value.to_string().into());
    }
    fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
        self.0.insert(field.name().to_owned(), value.to_string().into());
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.0.insert(field.name().to_owned(), value.to_string().into());
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.0
            .insert(field.name().to_owned(), format!("{value:?}").into());
    }
}

// ── Layer ─────────────────────────────────────────────────────────────────────

/// Emits one compact JSON line per [`tracing`] event.
///
/// Each line is guaranteed to contain:
/// - `timestamp` — RFC 3339 UTC with nanosecond precision
/// - `level`     — `TRACE`, `DEBUG`, `INFO`, `WARN`, or `ERROR`
/// - `target`    — module-path target from the event metadata
/// - `trace_id`  — W3C 32-hex-char trace ID (empty string when no OpenTelemetry span is active)
/// - `span_id`   — W3C 16-hex-char span ID (empty string when no OpenTelemetry span is active)
/// - `fields`    — map of all structured event fields, including `message`
///
/// When the event occurs inside a [`tracing`] span the line also contains:
/// - `span`  — name of the innermost active span
/// - `spans` — ordered `[outermost, …, innermost]` list of span names
///
/// `trace_id` and `span_id` are populated only when an OpenTelemetry tracer
/// provider has been registered (e.g. via [`rb_tracing::init`]).
pub struct StructuredJsonLayer {
    writer: Mutex<Box<dyn Write + Send>>,
}

impl StructuredJsonLayer {
    /// Writes JSON lines to `stdout`.
    #[must_use]
    pub fn stdout() -> Self {
        Self {
            writer: Mutex::new(Box::new(std::io::stdout())),
        }
    }

    /// Writes JSON lines to `stderr`.
    #[must_use]
    pub fn stderr() -> Self {
        Self {
            writer: Mutex::new(Box::new(std::io::stderr())),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_writer(w: impl Write + Send + 'static) -> Self {
        Self {
            writer: Mutex::new(Box::new(w)),
        }
    }
}

impl<S> Layer<S> for StructuredJsonLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();

        // OpenTelemetry span context — read from the current tracing span's extensions
        // via OpenTelemetrySpanExt, NOT from opentelemetry::Context::current().
        // tracing-opentelemetry 0.29 does not call Context::attach() in on_enter,
        // so the thread-local OTel context is never populated during request handling.
        let otel_ctx = tracing::Span::current().context();
        let otel_span = otel_ctx.span();
        let sc = otel_span.span_context();
        let (trace_id, span_id) = if sc.is_valid() {
            (sc.trace_id().to_string(), sc.span_id().to_string())
        } else {
            (String::new(), String::new())
        };

        // tracing span ancestry: outermost → innermost.
        let (span_name, span_list): (Option<String>, Vec<String>) =
            if let Some(span_ref) = ctx.lookup_current() {
                let name = span_ref.metadata().name().to_owned();
                let mut ancestors: Vec<String> = Vec::new();
                let mut current = span_ref.parent();
                while let Some(s) = current {
                    ancestors.push(s.metadata().name().to_owned());
                    current = s.parent();
                }
                ancestors.reverse();
                ancestors.push(name.clone());
                (Some(name), ancestors)
            } else {
                (None, Vec::new())
            };

        // Collect structured event fields.
        let mut fields: Map<String, Value> = Map::new();
        event.record(&mut JsonVisitor(&mut fields));

        // Build the JSON record.
        let mut record: Map<String, Value> = Map::with_capacity(9);
        record.insert("timestamp".into(), timestamp_now().into());
        record.insert("level".into(), meta.level().to_string().into());
        record.insert("target".into(), meta.target().into());
        record.insert("trace_id".into(), trace_id.into());
        record.insert("span_id".into(), span_id.into());
        if let Some(name) = span_name {
            record.insert("span".into(), name.into());
            record.insert(
                "spans".into(),
                Value::Array(span_list.into_iter().map(Value::String).collect()),
            );
        }
        record.insert("fields".into(), Value::Object(fields));

        if let Ok(line) = serde_json::to_string(&record) {
            let mut w = self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = writeln!(w, "{line}");
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{layer::SubscriberExt as _, Registry};

    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture() -> (StructuredJsonLayer, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let layer = StructuredJsonLayer::with_writer(BufWriter(Arc::clone(&buf)));
        (layer, buf)
    }

    fn first_line(buf: &[u8]) -> serde_json::Value {
        let text = std::str::from_utf8(buf).unwrap();
        serde_json::from_str(text.lines().next().unwrap()).unwrap()
    }

    #[test]
    fn json_line_has_required_fields() {
        let (layer, buf) = capture();
        let sub = Registry::default().with(layer);
        tracing::dispatcher::with_default(&tracing::Dispatch::new(sub), || {
            tracing::info!(answer = 42, "hello");
        });

        let v = first_line(&buf.lock().unwrap());
        // Required structural fields
        assert!(v["timestamp"].is_string(), "timestamp missing");
        assert_eq!(v["level"], "INFO");
        assert!(v["target"].is_string(), "target missing");
        assert!(v["trace_id"].is_string(), "trace_id missing");
        assert!(v["span_id"].is_string(), "span_id missing");
        // Event fields
        assert_eq!(v["fields"]["message"], "hello");
        assert_eq!(v["fields"]["answer"], 42);
    }

    #[test]
    fn json_timestamp_is_rfc3339() {
        let (layer, buf) = capture();
        let sub = Registry::default().with(layer);
        tracing::dispatcher::with_default(&tracing::Dispatch::new(sub), || {
            tracing::debug!("ts test");
        });

        let v = first_line(&buf.lock().unwrap());
        let ts = v["timestamp"].as_str().unwrap();
        // Must end with Z and contain T separator
        assert!(ts.contains('T'), "timestamp not ISO 8601: {ts}");
        assert!(ts.ends_with('Z'), "timestamp not UTC: {ts}");
    }

    #[test]
    fn json_includes_span_context() {
        let (layer, buf) = capture();
        let sub = Registry::default().with(layer);
        tracing::dispatcher::with_default(&tracing::Dispatch::new(sub), || {
            let _span = tracing::info_span!("outer").entered();
            let _inner = tracing::info_span!("inner").entered();
            tracing::warn!("inside spans");
        });

        let v = first_line(&buf.lock().unwrap());
        assert_eq!(v["span"], "inner", "innermost span name wrong");
        let spans = v["spans"].as_array().unwrap();
        assert_eq!(spans[0], "outer");
        assert_eq!(spans[1], "inner");
    }

    #[test]
    fn json_no_span_omits_span_fields() {
        let (layer, buf) = capture();
        let sub = Registry::default().with(layer);
        tracing::dispatcher::with_default(&tracing::Dispatch::new(sub), || {
            tracing::error!("no span here");
        });

        let v = first_line(&buf.lock().unwrap());
        assert!(v.get("span").is_none(), "span field should be absent outside a span");
        assert!(v.get("spans").is_none(), "spans field should be absent outside a span");
    }

    #[test]
    fn json_level_strings() {
        let (layer, buf) = capture();
        let sub = Registry::default()
            .with(tracing_subscriber::filter::LevelFilter::TRACE)
            .with(layer);
        tracing::dispatcher::with_default(&tracing::Dispatch::new(sub), || {
            tracing::trace!("t");
            tracing::debug!("d");
            tracing::info!("i");
            tracing::warn!("w");
            tracing::error!("e");
        });

        let output = buf.lock().unwrap();
        let text = std::str::from_utf8(&output).unwrap();
        let levels: Vec<String> = text
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["level"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(levels, ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"]);
    }

    #[test]
    fn civil_from_days_epoch() {
        // Day 0 == 1970-01-01
        let (y, m, d) = civil_from_days(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn civil_from_days_known_date() {
        // 2024-06-01 is day 19875 since Unix epoch
        // Calculated: (2024-1970)*365 + leap days + Jan(31)+Feb(29)+Mar(31)+Apr(30)+May(31) = 19875
        let days = {
            let mut d: i64 = 0;
            for y in 1970..2024_i64 {
                d += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                    366
                } else {
                    365
                };
            }
            d += 31 + 29 + 31 + 30 + 31; // Jan–May 2024 (leap year)
            d
        };
        let (y, m, day) = civil_from_days(days);
        assert_eq!((y, m, day), (2024, 6, 1));
    }
}
