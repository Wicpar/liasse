//! Server-Sent-Events line framing — composing and parsing the `event:`/`data:`/
//! `id:`/`retry:` text form, with no I/O of its own.
//!
//! The connector's downstream stream is SSE (AGENTS.md: no WebSockets): the SSE
//! `id:` carries the frontier token, giving `Last-Event-ID` §12.2 resume for free.
//! This module knows nothing of frames or transport — it is the pure text codec
//! between a [`SseEvent`] and the bytes on the wire. The frame it wraps in `data:`
//! is produced by [`crate::codec`]; who reads and writes those bytes is the
//! transport binding's concern.
//!
//! This is a per-event line codec: each [`SseEvent`] carries its own fields, so a
//! parsed event reflects exactly the lines that produced it. SSE's persistence of
//! the last event id across events (for `Last-Event-ID` on reconnect) is the
//! transport's concern, not this codec's.

/// One Server-Sent Event: an optional type, an optional id (the frontier token in
/// this connector), an optional reconnection delay, and a data payload (the JSON
/// frame, which may span lines).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// The `event:` type, naming the kind of event.
    pub event: Option<String>,
    /// The `id:` value — the frontier token, echoed as `Last-Event-ID` on resume.
    pub id: Option<String>,
    /// The `data:` payload, un-prefixed and with interior newlines preserved.
    pub data: String,
    /// The `retry:` reconnection delay in milliseconds.
    pub retry: Option<u64>,
}

impl SseEvent {
    /// A data-only event carrying `data`.
    #[must_use]
    pub fn data(data: impl Into<String>) -> Self {
        Self { data: data.into(), ..Self::default() }
    }

    /// Set the event id (the frontier token).
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Set the event type.
    #[must_use]
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }

    /// Render this event to its SSE text form, terminated by the blank line that
    /// dispatches it. `id`/`event`/`retry` become single lines; `data` becomes one
    /// `data:` line per interior line so any payload round-trips.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::new();
        if let Some(id) = &self.id {
            push_field(&mut out, "id", id);
        }
        if let Some(event) = &self.event {
            push_field(&mut out, "event", event);
        }
        if let Some(retry) = self.retry {
            push_field(&mut out, "retry", &retry.to_string());
        }
        for line in self.data.split('\n') {
            push_field(&mut out, "data", line);
        }
        out.push('\n');
        out
    }

    /// Render a whole stream of events to SSE text.
    #[must_use]
    pub fn encode_stream(events: &[Self]) -> String {
        events.iter().map(Self::encode).collect()
    }

    /// Parse an SSE text stream into the events it dispatches. Lenient by the SSE
    /// definition: comment lines (`:`…) and unknown fields are ignored, and a
    /// field group with no `data` dispatches nothing. It never fails and never
    /// panics — arbitrary text yields some (possibly empty) set of events.
    #[must_use]
    pub fn parse_stream(input: &str) -> Vec<Self> {
        let mut events = Vec::new();
        let mut pending = Pending::default();
        for line in normalize(input).split('\n') {
            if line.is_empty() {
                pending.dispatch(&mut events);
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line, ""),
            };
            pending.field(field, value);
        }
        // A trailing group without a terminating blank line still dispatches.
        pending.dispatch(&mut events);
        events
    }
}

/// Accumulated fields for the event currently being parsed.
#[derive(Default)]
struct Pending {
    event: Option<String>,
    id: Option<String>,
    data: String,
    has_data: bool,
    retry: Option<u64>,
}

impl Pending {
    /// Fold one `field: value` line into the pending event.
    fn field(&mut self, field: &str, value: &str) {
        match field {
            "event" => self.event = Some(value.to_owned()),
            "id" => self.id = Some(value.to_owned()),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
                self.has_data = true;
            }
            "retry" => {
                if let Ok(delay) = value.parse::<u64>() {
                    self.retry = Some(delay);
                }
            }
            _ => {}
        }
    }

    /// Emit the pending event if it carried data, then reset for the next one. A
    /// group with no `data` dispatches nothing (per the SSE definition).
    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        if self.has_data {
            let data = self.data.strip_suffix('\n').unwrap_or(&self.data).to_owned();
            events.push(SseEvent {
                event: self.event.take(),
                id: self.id.take(),
                data,
                retry: self.retry.take(),
            });
        }
        *self = Self::default();
    }
}

/// Append one `field: value` line (SSE inserts a space after the colon).
fn push_field(out: &mut String, field: &str, value: &str) {
    out.push_str(field);
    out.push_str(": ");
    out.push_str(value);
    out.push('\n');
}

/// Collapse SSE's three accepted line terminators (`\r\n`, `\r`, `\n`) to `\n`.
fn normalize(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}
