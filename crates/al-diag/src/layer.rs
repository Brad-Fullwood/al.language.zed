//! Custom tracing Layer that writes structured events to SQLite.
//!
//! Every tracing event becomes a row in the `events` table. Span open/close
//! timings go into `span_timings`. Both are indexed for fast queries.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tracing::field::{Field, Visit};
use tracing::span;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::writer::DiagStore;

/// A tracing layer that writes structured events to a SQLite database.
pub struct DiagLayer {
    store: Arc<DiagStore>,
}

impl DiagLayer {
    pub fn new(path: PathBuf) -> Self {
        let store = DiagStore::new(path).expect("failed to open diag database");
        Self {
            store: Arc::new(store),
        }
    }
}

/// Per-span data stored in the registry.
struct SpanData {
    name: &'static str,
    fields: serde_json::Map<String, serde_json::Value>,
    opened_at: Instant,
}

/// Visitor that collects field values into a JSON map.
struct JsonVisitor {
    fields: serde_json::Map<String, serde_json::Value>,
    message: Option<String>,
}

impl JsonVisitor {
    fn new() -> Self {
        Self {
            fields: serde_json::Map::new(),
            message: None,
        }
    }
}

impl Visit for JsonVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let name = field.name();
        if name == "message" {
            self.message = Some(format!("{:?}", value));
        } else {
            self.fields.insert(
                name.to_string(),
                serde_json::Value::String(format!("{:?}", value)),
            );
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let name = field.name();
        if name == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields.insert(
                name.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if let Some(n) = serde_json::Number::from_f64(value) {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::Number(n));
        }
    }
}

fn epoch_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

impl<S> Layer<S> for DiagLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let mut visitor = JsonVisitor::new();
        attrs.record(&mut visitor);

        let data = SpanData {
            name: attrs.metadata().name(),
            fields: visitor.fields,
            opened_at: Instant::now(),
        };

        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(data);
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = JsonVisitor::new();
        event.record(&mut visitor);

        // Collect span stack
        let mut spans = Vec::new();
        if let Some(scope) = ctx.event_span(event) {
            let mut current = Some(scope);
            while let Some(span) = current {
                if let Some(data) = span.extensions().get::<SpanData>() {
                    spans.push(data.name);
                }
                current = span.parent();
            }
        }
        spans.reverse();

        let meta = event.metadata();
        let spans_str = if spans.is_empty() {
            String::new()
        } else {
            spans.join(" > ")
        };

        let fields_json = if visitor.fields.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&serde_json::Value::Object(visitor.fields)).unwrap_or_default()
        };

        self.store.write_event(
            epoch_us(),
            meta.level().as_str(),
            meta.target(),
            &spans_str,
            visitor.message.as_deref().unwrap_or(""),
            &fields_json,
        );
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(&id) {
            if let Some(data) = span.extensions().get::<SpanData>() {
                let duration_us = data.opened_at.elapsed().as_micros() as u64;
                // Only log span close for spans that took >500µs (skip trivial ones)
                if duration_us > 500 {
                    let fields_json = if data.fields.is_empty() {
                        String::new()
                    } else {
                        serde_json::to_string(&serde_json::Value::Object(data.fields.clone()))
                            .unwrap_or_default()
                    };
                    self.store
                        .write_span_timing(epoch_us(), data.name, duration_us, &fields_json);
                }
            }
        }
    }
}
