//! Structured tracing collector for `malcolm` events.
//!
//! `MalcolmLayer` captures `tracing` events whose target is `malcolm` and
//! stores them as [`FaultEvent`] values for tests and post-mortem assertions.

use std::fmt;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use malcolm_core::types::FaultEvent;

/// Layer that records `malcolm` tracing events as structured fault events.
#[derive(Clone, Default)]
pub struct MalcolmLayer {
    events: Arc<Mutex<Vec<FaultEvent>>>,
}

impl MalcolmLayer {
    /// Create an empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the collected events.
    #[must_use]
    pub fn events(&self) -> Vec<FaultEvent> {
        self.events
            .lock()
            .map_or_else(|_| Vec::new(), |events| events.clone())
    }

    /// Remove all captured events.
    pub fn clear(&self) {
        if let Ok(mut events) = self.events.lock() {
            events.clear();
        }
    }
}

#[derive(Default)]
struct EventFields {
    fault_type: Option<String>,
    node_id: Option<String>,
    seed: Option<u64>,
    intensity: Option<f64>,
    dry_run: Option<bool>,
    timestamp_ms: Option<u64>,
}

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");

        match field.name() {
            "fault_type" | "malcolm.fault_type" => self.fault_type = Some(trim_quotes(rendered)),
            "node_id" | "malcolm.node_id" => self.node_id = Some(trim_quotes(rendered)),
            "seed" | "malcolm.seed" => self.seed = rendered.parse::<u64>().ok(),
            "intensity" | "malcolm.intensity" => self.intensity = rendered.parse::<f64>().ok(),
            "dry_run" | "malcolm.dry_run" => self.dry_run = rendered.parse::<bool>().ok(),
            "timestamp_ms" | "malcolm.timestamp_ms" => {
                self.timestamp_ms = rendered.parse::<u64>().ok();
            }
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "fault_type" | "malcolm.fault_type" => self.fault_type = Some(value.to_owned()),
            "node_id" | "malcolm.node_id" => self.node_id = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "seed" | "malcolm.seed" => self.seed = Some(value),
            "timestamp_ms" | "malcolm.timestamp_ms" => self.timestamp_ms = Some(value),
            _ => {}
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        match field.name() {
            "intensity" | "malcolm.intensity" => self.intensity = Some(value),
            _ => {}
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        match field.name() {
            "dry_run" | "malcolm.dry_run" => self.dry_run = Some(value),
            _ => {}
        }
    }
}

impl<S> Layer<S> for MalcolmLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "malcolm" {
            return;
        }

        let mut fields = EventFields::default();
        event.record(&mut fields);

        let Some(fault_type) = fields.fault_type else {
            return;
        };

        let event = FaultEvent {
            fault_type,
            node_id: fields.node_id.unwrap_or_default(),
            seed: fields.seed.unwrap_or_default(),
            intensity: fields.intensity.unwrap_or_default(),
            dry_run: fields.dry_run.unwrap_or(false),
            timestamp_ms: fields.timestamp_ms.unwrap_or_default(),
        };

        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

fn trim_quotes(value: String) -> String {
    let trimmed = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_owned);

    trimmed.unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn captures_malcolm_target_events() -> Result<(), std::io::Error> {
        let layer = MalcolmLayer::new();
        let subscriber = tracing_subscriber::registry().with(layer.clone());

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "malcolm",
                fault_type = "packet_loss",
                node_id = "node-1",
                seed = 7_u64,
                intensity = 0.8_f64,
                dry_run = false,
                timestamp_ms = 12_u64,
                "packet loss injected",
            );
        });

        let events = layer.events();
        assert_eq!(events.len(), 1);
        let event = events
            .first()
            .ok_or_else(|| std::io::Error::other("expected one captured event"))?;
        assert_eq!(event.fault_type, "packet_loss");
        assert_eq!(event.node_id, "node-1");
        assert_eq!(event.seed, 7);
        assert!(!event.dry_run);

        Ok(())
    }
}
