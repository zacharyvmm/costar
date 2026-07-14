//! Deterministic trace sink for golden-trace tests.
//!
//! Every deterministic run emits a `Vec<TraceEvent>` that can be compared
//! across host platforms and used as an expected trace in regression tests.
//!
//! The sink also exposes a C-compatible `sim_trace_u32` helper for
//! lightweight counter tracing from guest firmware.

use crate::{error::SimErrorCode, event_queue::EventId, time::Tick};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;

/// A single trace event recorded during a deterministic simulation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "event")]
pub enum TraceEvent {
    /// An event was placed on the queue.
    EventScheduled {
        /// Virtual time of the scheduling operation.
        at: Tick,
        /// New event id.
        id: EventId,
        /// Priority.
        priority: u16,
        /// Event label.
        label: &'static str,
        /// Absolute virtual timestamp it is scheduled for.
        target_at: Tick,
    },
    /// An event was dispatched from the queue.
    EventDispatched {
        /// Current virtual time.
        at: Tick,
        /// Event id.
        id: EventId,
        /// Event label.
        label: &'static str,
    },
    /// An event was cancelled.
    EventCancelled {
        /// Current virtual time.
        at: Tick,
        /// Cancelled event id.
        id: EventId,
    },
    /// A simulated task was resumed.
    TaskResume {
        /// Virtual time.
        at: Tick,
        /// Task id.
        task: u64,
        /// Human-readable resume reason.
        reason: &'static str,
    },
    /// A simulated task yielded.
    TaskYield {
        /// Virtual time.
        at: Tick,
        /// Task id.
        task: u64,
        /// Human-readable yield reason.
        reason: &'static str,
    },
    /// A virtual interrupt was raised.
    InterruptRaised {
        /// Virtual time.
        at: Tick,
        /// Interrupt number.
        irq: u32,
    },
    /// A virtual interrupt was delivered to the guest.
    InterruptDelivered {
        /// Virtual time.
        at: Tick,
        /// Interrupt number.
        irq: u32,
    },
    /// A packet was received on a virtual network device.
    PacketRx {
        /// Virtual time.
        at: Tick,
        /// Packet length in bytes.
        len: usize,
    },
    /// A packet was transmitted from a virtual network device.
    PacketTx {
        /// Virtual time.
        at: Tick,
        /// Packet length in bytes.
        len: usize,
    },
    /// A fatal simulator error occurred.
    Fatal {
        /// Virtual time.
        at: Tick,
        /// Stable error code.
        code: SimErrorCode,
    },
    /// A user-defined u32 data point (for C `sim_trace_u32`).
    UserU32 {
        /// Virtual time.
        at: Tick,
        /// Label.
        label: &'static str,
        /// Value.
        value: u32,
    },
    /// A task was created and registered with a human-readable name.
    ///
    /// This event is emitted by `sim_create_task` so that trace analysis
    /// tools can resolve task IDs to names.  Golden traces may include
    /// these events; post-processing tools use them for symbolication.
    TaskCreated {
        /// Virtual time at creation.
        at: Tick,
        /// Task id.
        task: u64,
        /// Human-readable task name.
        name: &'static str,
    },
    /// A CAN frame was transmitted onto a broadcast bus.
    CanTx {
        /// Virtual time.
        at: Tick,
        /// Sender machine ID.
        sender: u64,
        /// CAN frame identifier.
        id: u32,
        /// Payload length in bytes.
        len: usize,
    },
    /// A CAN frame was received from a broadcast bus.
    CanRx {
        /// Virtual time.
        at: Tick,
        /// Receiver machine ID.
        receiver: u64,
        /// CAN frame identifier.
        id: u32,
        /// Payload length in bytes.
        len: usize,
    },
    /// A CAN frame was dropped by fault injection.
    CanDrop {
        /// Virtual time.
        at: Tick,
        /// CAN frame identifier that was dropped.
        id: u32,
    },
    /// A CAN frame was delayed by fault injection.
    CanDelay {
        /// Virtual time.
        at: Tick,
        /// CAN frame identifier.
        id: u32,
        /// Extra delay in virtual-time ticks.
        extra_ticks: Tick,
    },
}

/// A Trace v2 record — richer identity + causality for the product data model.
///
/// This is **opt-in**: the [`World`](../../sim_world/struct.World.html) only
/// populates it when trace v2 is explicitly enabled, on a separate sink, so the
/// default human/golden trace output is completely unchanged. See the dogfood
/// plan's "Make Trace v2 the Product Data Model".
///
/// This foundation covers CAN message-delivery edges: every transmit→receive
/// path carries a shared [`correlation_id`](Self::correlation_id), and each
/// record carries explicit [`source`](Self::source) and
/// [`destination`](Self::destination) component identity. Further event types
/// and the full field set can be layered on additively.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceV2 {
    /// Monotonic per-record id (stable within a run).
    pub trace_id: u64,
    /// Shared id linking a transmit to all of its receive edges.
    pub correlation_id: u64,
    /// For a *forwarded* frame (e.g. a gateway bridging one bus to another),
    /// the correlation id of the frame that caused the forward — links child
    /// causality back to its parent. `0` for an original (non-forwarded) frame.
    pub parent_id: u64,
    /// Virtual time of this record (delivery time for an `rx` edge).
    pub virtual_time: Tick,
    /// Primary machine of this event: the receiver for an `rx` edge, the sender
    /// for a `tx` edge.
    pub machine_id: u64,
    /// Human-readable name of [`machine_id`](Self::machine_id) (empty if the
    /// machine has no known name).
    pub machine_name: String,
    /// Component (device) id the event relates to — for CAN, the controller id.
    pub component_id: u32,
    /// Component type, e.g. `"can_controller"`.
    pub component_type: String,
    /// Port identity within the component. Reserved for typed-port topology;
    /// empty for CAN broadcast.
    pub port_id: String,
    /// Event class, e.g. `"can_frame"`.
    pub event_type: String,
    /// Direction, e.g. `"rx"` (a delivery edge) or `"tx"`.
    pub direction: String,
    /// Bus or link identity the frame travelled on.
    pub bus_or_link_id: String,
    /// Protocol message id (CAN id).
    pub message_id: u32,
    /// Short hex summary of the payload (up to 8 bytes) for GUI/AI inspection.
    pub payload_summary: String,
    /// Task that produced the event, if known. Reserved for task-level events;
    /// `0` for bus-delivery edges.
    pub task_id: u64,
    /// RTOS backend of the machine, if known. Reserved; empty for bus edges.
    pub rtos: String,
    /// Source component (sender machine id).
    pub source: u64,
    /// Destination component (receiver machine id).
    pub destination: u64,
    /// Payload length in bytes.
    pub len: usize,
}

impl TraceV2 {
    /// A compact lowercase-hex summary of a payload (first 8 bytes, then `…`).
    pub fn hex_summary(data: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        for b in data.iter().take(8) {
            let _ = write!(s, "{b:02x}");
        }
        if data.len() > 8 {
            s.push('\u{2026}');
        }
        s
    }
}

impl TraceV2 {
    /// Serialize to a single-line JSON object (for JSONL output).
    pub fn to_json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Regenerate the legacy human trace line from a v2 record — demonstrates
    /// that the old human/JSONL output can be produced from trace v2.
    pub fn to_human_line(&self) -> String {
        match self.direction.as_str() {
            "rx" => format!(
                "{:>12} can-rx receiver={} id={:#06x} len={}",
                self.virtual_time, self.destination, self.message_id, self.len
            ),
            _ => format!(
                "{:>12} can-tx sender={} id={:#06x} len={}",
                self.virtual_time, self.source, self.message_id, self.len
            ),
        }
    }
}

impl fmt::Display for TraceEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceEvent::EventScheduled {
                at,
                id,
                priority,
                label,
                target_at,
            } => {
                write!(
                    f,
                    "{at:>12} schedule id={id} pri={priority} \"{label}\" target={target_at}"
                )
            }
            TraceEvent::EventDispatched { at, id, label } => {
                write!(f, "{at:>12} dispatch id={id} \"{label}\"")
            }
            TraceEvent::EventCancelled { at, id } => {
                write!(f, "{at:>12} cancel id={id}")
            }
            TraceEvent::TaskResume { at, task, reason } => {
                write!(f, "{at:>12} task-resume id={task} reason={reason}")
            }
            TraceEvent::TaskYield { at, task, reason } => {
                write!(f, "{at:>12} task-yield id={task} reason={reason}")
            }
            TraceEvent::InterruptRaised { at, irq } => {
                write!(f, "{at:>12} irq-raised irq={irq}")
            }
            TraceEvent::InterruptDelivered { at, irq } => {
                write!(f, "{at:>12} irq-delivered irq={irq}")
            }
            TraceEvent::PacketRx { at, len } => {
                write!(f, "{at:>12} pkt-rx len={len}")
            }
            TraceEvent::PacketTx { at, len } => {
                write!(f, "{at:>12} pkt-tx len={len}")
            }
            TraceEvent::Fatal { at, code } => {
                write!(f, "{at:>12} FATAL code={code:?}")
            }
            TraceEvent::UserU32 { at, label, value } => {
                write!(f, "{at:>12} user-u32 \"{label}\" = {value}")
            }
            TraceEvent::TaskCreated { at, task, name } => {
                write!(f, "{at:>12} task-created id={task} name=\"{name}\"")
            }
            TraceEvent::CanTx {
                at,
                sender,
                id,
                len,
            } => {
                write!(f, "{at:>12} can-tx sender={sender} id={id:#06x} len={len}")
            }
            TraceEvent::CanRx {
                at,
                receiver,
                id,
                len,
            } => {
                write!(
                    f,
                    "{at:>12} can-rx receiver={receiver} id={id:#06x} len={len}"
                )
            }
            TraceEvent::CanDrop { at, id } => {
                write!(f, "{at:>12} can-drop id={id:#06x}")
            }
            TraceEvent::CanDelay {
                at,
                id,
                extra_ticks,
            } => {
                write!(
                    f,
                    "{at:>12} can-delay id={id:#06x} extra_ticks={extra_ticks}"
                )
            }
        }
    }
}

/// A growable trace buffer.
#[derive(Debug, Clone)]
pub struct TraceSink {
    /// The recorded events.
    pub events: Vec<TraceEvent>,
}

impl TraceSink {
    /// Create an empty trace sink.
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Record an event into the trace.
    pub fn record(&mut self, event: TraceEvent) {
        self.events.push(event);
    }

    /// Convenience: record an event dispatch.
    pub fn event_dispatch(&mut self, now: Tick, id: EventId, label: &'static str) {
        self.record(TraceEvent::EventDispatched { at: now, id, label });
    }

    /// Convenience: record an event schedule.
    pub fn event_scheduled(
        &mut self,
        at: Tick,
        id: EventId,
        priority: u16,
        label: &'static str,
        target_at: Tick,
    ) {
        self.record(TraceEvent::EventScheduled {
            at,
            id,
            priority,
            label,
            target_at,
        });
    }

    /// Convenience: record an event cancellation.
    pub fn event_cancelled(&mut self, at: Tick, id: EventId) {
        self.record(TraceEvent::EventCancelled { at, id });
    }

    /// All recorded events, in order.
    pub fn events(&self) -> &[TraceEvent] {
        &self.events
    }

    /// Format the trace as a multi-line string.
    pub fn format(&self) -> String {
        self.events
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Serialize the trace as JSONL (one JSON object per line).
    ///
    /// Each line is a self-describing JSON object with an `"event"` field
    /// that identifies the variant.  Suitable for machine-parsing in CI
    /// tooling and for `costar trace diff`.
    pub fn to_jsonl(&self) -> String {
        self.events
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Write the trace as JSONL to the given writer.
    ///
    /// Returns the number of events written.
    pub fn write_jsonl<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<usize> {
        let mut count = 0;
        for event in &self.events {
            let line =
                serde_json::to_string(event).map_err(|e| std::io::Error::other(e.to_string()))?;
            writeln!(w, "{}", line)?;
            count += 1;
        }
        Ok(count)
    }

    /// Number of events recorded.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether no events have been recorded.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Clear all recorded events.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Build a symbol map from TaskCreated events.
    ///
    /// Scans the trace for `TaskCreated` events and returns a map from
    /// task ID to task name.  The most recent `TaskCreated` for a given
    /// ID wins (useful if tasks are created, destroyed, and re-created).
    pub fn task_symbols(&self) -> std::collections::BTreeMap<u64, &'static str> {
        let mut symbols = std::collections::BTreeMap::new();
        for event in &self.events {
            if let TraceEvent::TaskCreated { task, name, .. } = event {
                symbols.insert(*task, *name);
            }
        }
        symbols
    }

    /// Resolve a task ID to its human-readable name.
    ///
    /// Returns the name registered via `TaskCreated` events, or `None`
    /// if no `TaskCreated` event was recorded for this task.
    pub fn resolve_task_name(&self, task_id: u64) -> Option<&'static str> {
        // Walk backwards to find the most recent TaskCreated for this id.
        for event in self.events.iter().rev() {
            if let TraceEvent::TaskCreated { task, name, .. } = event {
                if *task == task_id {
                    return Some(*name);
                }
            }
        }
        None
    }

    /// Format the trace with symbolicated task names.
    ///
    /// For each `TaskResume` and `TaskYield` event, the task name
    /// (if known via `TaskCreated` events) is appended after the ID.
    pub fn format_symbolicated(&self) -> String {
        let symbols = self.task_symbols();
        self.events
            .iter()
            .map(|e| match e {
                TraceEvent::TaskResume { at, task, reason } => {
                    if let Some(name) = symbols.get(task) {
                        format!("{at:>12} task-resume id={task} name=\"{name}\" reason={reason}")
                    } else {
                        e.to_string()
                    }
                }
                TraceEvent::TaskYield { at, task, reason } => {
                    if let Some(name) = symbols.get(task) {
                        format!("{at:>12} task-yield id={task} name=\"{name}\" reason={reason}")
                    } else {
                        e.to_string()
                    }
                }
                _ => e.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Default for TraceSink {
    fn default() -> Self {
        Self::new()
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Online accumulator over a trace stream (Stage J1). Fed one normalized human
/// trace line at a time *before* any retention eviction, so the hash and
/// counts cover the complete run even when only the last N records are kept.
///
/// `normalized_fnv1a64` is the FNV-1a (64-bit) hash of the same normalized line
/// representation used by `microcar/dogfood/src/trace_hash.rs` (trailing
/// whitespace trimmed, blank lines dropped, lines joined with `\n`), so an
/// empty stream hashes to the FNV offset basis.
#[derive(Debug, Clone)]
pub struct TraceStats {
    /// Number of (non-blank) records observed.
    pub event_count: u64,
    /// Running FNV-1a hash of the normalized stream.
    pub normalized_fnv1a64: u64,
    /// First observed virtual time.
    pub first_virtual_time: Option<u64>,
    /// Last observed virtual time.
    pub last_virtual_time: Option<u64>,
    /// Count of per-stream virtual-time regressions.
    pub time_regressions: u64,
    /// CAN transmit counts by message id.
    pub can_tx_by_id: BTreeMap<u32, u64>,
    /// CAN receive counts by message id.
    pub can_rx_by_id: BTreeMap<u32, u64>,
    /// Dropped-frame counts by message id.
    pub dropped_by_id: BTreeMap<u32, u64>,
    /// Number of assertion failures noted.
    pub assertion_failures: u64,
    /// Records currently retained (all of them when unbounded).
    pub retained_records: usize,
    /// Records evicted from the retention ring.
    pub evicted_records: u64,

    wrote_any: bool,
    last_time_by_stream: BTreeMap<u64, u64>,
    retention: Option<usize>,
    ring: VecDeque<String>,
}

impl Default for TraceStats {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceStats {
    /// Unbounded accumulator (default; preserves byte-identical golden traces).
    pub fn new() -> Self {
        Self {
            event_count: 0,
            normalized_fnv1a64: FNV_OFFSET_BASIS,
            first_virtual_time: None,
            last_virtual_time: None,
            time_regressions: 0,
            can_tx_by_id: BTreeMap::new(),
            can_rx_by_id: BTreeMap::new(),
            dropped_by_id: BTreeMap::new(),
            assertion_failures: 0,
            retained_records: 0,
            evicted_records: 0,
            wrote_any: false,
            last_time_by_stream: BTreeMap::new(),
            retention: None,
            ring: VecDeque::new(),
        }
    }

    /// Accumulator that retains only the last `limit` records (soak mode).
    pub fn with_retention(limit: usize) -> Self {
        let mut s = Self::new();
        s.retention = Some(limit);
        s
    }

    /// The currently-retained trace lines, oldest first.
    pub fn retained_lines(&self) -> impl Iterator<Item = &str> {
        self.ring.iter().map(String::as_str)
    }

    /// Note an assertion failure (assertion state is tracked out of band).
    pub fn note_assertion_failure(&mut self) {
        self.assertion_failures += 1;
    }

    /// Record one trace line, updating the hash and stats before any eviction.
    pub fn record_line(&mut self, line: &str) {
        let norm = line.trim_end();
        if norm.is_empty() {
            return; // normalized representation drops blank lines
        }
        // FNV-1a over normalized lines joined by '\n'.
        if self.wrote_any {
            self.normalized_fnv1a64 ^= b'\n' as u64;
            self.normalized_fnv1a64 = self.normalized_fnv1a64.wrapping_mul(FNV_PRIME);
        }
        for &byte in norm.as_bytes() {
            self.normalized_fnv1a64 ^= byte as u64;
            self.normalized_fnv1a64 = self.normalized_fnv1a64.wrapping_mul(FNV_PRIME);
        }
        self.wrote_any = true;
        self.event_count += 1;

        let (stream, rest) = split_stream_prefix(norm);
        if let Some(t) = leading_virtual_time(rest) {
            self.first_virtual_time.get_or_insert(t);
            if let Some(&last) = self.last_time_by_stream.get(&stream) {
                if t < last {
                    self.time_regressions += 1;
                }
            }
            self.last_time_by_stream.insert(stream, t);
            self.last_virtual_time = Some(t);
        }

        if let Some(id) = can_frame_id(rest) {
            if rest.contains("can-tx") {
                *self.can_tx_by_id.entry(id).or_default() += 1;
            } else if rest.contains("can-rx") {
                *self.can_rx_by_id.entry(id).or_default() += 1;
            } else if rest.contains("can-drop") {
                *self.dropped_by_id.entry(id).or_default() += 1;
            }
        }

        match self.retention {
            Some(limit) => {
                self.ring.push_back(norm.to_string());
                while self.ring.len() > limit {
                    self.ring.pop_front();
                    self.evicted_records += 1;
                }
                self.retained_records = self.ring.len();
            }
            None => {
                self.retained_records = self.event_count as usize;
            }
        }
    }
}

/// Split a `[machine.N] rest…` prefix, returning `(stream_id, rest)`. Lines
/// without the prefix are stream 0.
fn split_stream_prefix(line: &str) -> (u64, &str) {
    if let Some(inner) = line.strip_prefix("[machine.") {
        if let Some(end) = inner.find(']') {
            let id = inner[..end].parse::<u64>().unwrap_or(0);
            return (id, inner[end + 1..].trim_start());
        }
    }
    (0, line)
}

/// The leading virtual-time token of an event body, if numeric.
fn leading_virtual_time(body: &str) -> Option<u64> {
    body.split_whitespace().next()?.parse().ok()
}

/// The CAN message id from an `… id=0xNNNN …` token, if present.
fn can_frame_id(body: &str) -> Option<u32> {
    let idx = body.find("id=0x")?;
    let hex = &body[idx + 5..];
    let end = hex
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(hex.len());
    u32::from_str_radix(&hex[..end], 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_event_jsonl_serialization() {
        // Each variant should serialize to a self-describing JSONL line.
        let ev = TraceEvent::TaskResume {
            at: 42,
            task: 1,
            reason: "scheduler",
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"event\":\"TaskResume\""));
        assert!(json.contains("\"at\":42"));
        assert!(json.contains("\"task\":1"));
        assert!(json.contains("\"reason\":\"scheduler\""));
    }

    #[test]
    fn test_trace_sink_jsonl_output() {
        let mut sink = TraceSink::new();
        sink.record(TraceEvent::TaskResume {
            at: 0,
            task: 1,
            reason: "start",
        });
        sink.record(TraceEvent::TaskYield {
            at: 1,
            task: 1,
            reason: "Cooperative",
        });
        sink.record(TraceEvent::UserU32 {
            at: 2,
            label: "counter",
            value: 42,
        });

        let jsonl = sink.to_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 3);

        // Each line is valid JSON.
        for line in &lines {
            let _: serde_json::Value =
                serde_json::from_str(line).expect("each JSONL line must be valid JSON");
        }

        // First line is TaskResume.
        assert!(lines[0].contains("\"event\":\"TaskResume\""));
        // Last line is UserU32.
        assert!(lines[2].contains("\"event\":\"UserU32\""));
    }

    #[test]
    fn test_jsonl_write_to_writer() {
        let mut sink = TraceSink::new();
        sink.record(TraceEvent::UserU32 {
            at: 100,
            label: "x",
            value: 0,
        });
        sink.record(TraceEvent::UserU32 {
            at: 200,
            label: "y",
            value: 1,
        });

        let mut buf = Vec::new();
        let count = sink.write_jsonl(&mut buf).unwrap();
        assert_eq!(count, 2);

        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"event\":\"UserU32\""));
        assert!(lines[1].contains("\"event\":\"UserU32\""));
    }

    #[test]
    fn test_jsonl_backward_compat_human_format() {
        // Human-readable format is unchanged by serde additions.
        let mut sink = TraceSink::new();
        sink.record(TraceEvent::TaskResume {
            at: 0,
            task: 1,
            reason: "start",
        });
        let human = sink.format();
        assert!(human.contains("task-resume"));
        assert!(human.contains("id=1"));
        // Golden trace format unchanged.
        assert!(!human.contains("\"event\""));
    }

    #[test]
    fn test_fatal_jsonl_serialization() {
        let ev = TraceEvent::Fatal {
            at: 999,
            code: SimErrorCode::PanicCrossedCAbi,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"event\":\"Fatal\""));
        assert!(json.contains("\"code\":\"PanicCrossedCAbi\""));
    }

    #[test]
    fn test_task_created_trace_event() {
        let ev = TraceEvent::TaskCreated {
            at: 0,
            task: 1,
            name: "Sender",
        };
        let formatted = ev.to_string();
        assert!(formatted.contains("task-created"));
        assert!(formatted.contains("id=1"));
        assert!(formatted.contains("Sender"));
    }

    #[test]
    fn test_task_symbol_resolution() {
        let mut sink = TraceSink::new();
        sink.record(TraceEvent::TaskCreated {
            at: 0,
            task: 1,
            name: "Sender",
        });
        sink.record(TraceEvent::TaskCreated {
            at: 0,
            task: 2,
            name: "Receiver",
        });
        sink.record(TraceEvent::TaskResume {
            at: 1,
            task: 1,
            reason: "scheduler",
        });
        sink.record(TraceEvent::TaskYield {
            at: 2,
            task: 2,
            reason: "Cooperative",
        });

        // resolve_task_name
        assert_eq!(sink.resolve_task_name(1), Some("Sender"));
        assert_eq!(sink.resolve_task_name(2), Some("Receiver"));
        assert_eq!(sink.resolve_task_name(99), None);

        // task_symbols
        let symbols = sink.task_symbols();
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols.get(&1), Some(&"Sender"));
        assert_eq!(symbols.get(&2), Some(&"Receiver"));

        // format_symbolicated
        let sym = sink.format_symbolicated();
        assert!(sym.contains("name=\"Sender\""));
        assert!(sym.contains("name=\"Receiver\""));
    }

    #[test]
    fn test_symbolicated_format_falls_back_to_id() {
        let mut sink = TraceSink::new();
        // No TaskCreated event for task 3.
        sink.record(TraceEvent::TaskResume {
            at: 0,
            task: 3,
            reason: "start",
        });
        let sym = sink.format_symbolicated();
        // Should still have the resume line, just without a name.
        assert!(sym.contains("task-resume id=3"));
        assert!(!sym.contains("name="));
    }

    fn batch_fnv(lines: &[&str]) -> u64 {
        let mut h = FNV_OFFSET_BASIS;
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                h ^= b'\n' as u64;
                h = h.wrapping_mul(FNV_PRIME);
            }
            for &b in l.as_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(FNV_PRIME);
            }
        }
        h
    }

    #[test]
    fn trace_stats_empty_is_offset_basis() {
        assert_eq!(TraceStats::new().normalized_fnv1a64, FNV_OFFSET_BASIS);
    }

    #[test]
    fn trace_stats_hash_matches_normalized_batch() {
        let mut s = TraceStats::new();
        s.record_line("line one   "); // trailing ws trimmed
        s.record_line(""); // blank dropped
        s.record_line("line two\t"); // trailing ws trimmed
        assert_eq!(s.event_count, 2);
        assert_eq!(
            s.normalized_fnv1a64,
            batch_fnv(&["line one", "line two"]),
            "incremental hash must match the batch normalized FNV"
        );
    }

    #[test]
    fn trace_stats_can_counts_and_time() {
        let mut s = TraceStats::new();
        s.record_line("[machine.1]  10 can-tx sender=1 id=0x0102 len=2");
        s.record_line("[machine.2]  10 can-rx receiver=2 id=0x0102 len=2");
        s.record_line("[machine.1]  20 can-drop id=0x0200");
        assert_eq!(s.can_tx_by_id.get(&0x102), Some(&1));
        assert_eq!(s.can_rx_by_id.get(&0x102), Some(&1));
        assert_eq!(s.dropped_by_id.get(&0x200), Some(&1));
        assert_eq!(s.first_virtual_time, Some(10));
        assert_eq!(s.last_virtual_time, Some(20));
    }

    #[test]
    fn trace_stats_time_regressions_per_stream() {
        let mut s = TraceStats::new();
        s.record_line("[machine.1] 10 x");
        s.record_line("[machine.1] 20 x"); // ok
        s.record_line("[machine.2] 5 x"); // different stream — not a regression
        s.record_line("[machine.1] 15 x"); // regression on stream 1 (15 < 20)
        assert_eq!(s.time_regressions, 1);
    }

    #[test]
    fn trace_stats_ring_eviction() {
        let mut s = TraceStats::with_retention(3);
        for i in 0..5 {
            s.record_line(&format!("[machine.0] {i} e"));
        }
        assert_eq!(s.event_count, 5);
        assert_eq!(s.retained_records, 3);
        assert_eq!(s.evicted_records, 2);
        let kept: Vec<&str> = s.retained_lines().collect();
        assert_eq!(
            kept,
            vec!["[machine.0] 2 e", "[machine.0] 3 e", "[machine.0] 4 e"]
        );
        // Hash still covers ALL 5 records (accumulated before eviction).
        assert_ne!(s.normalized_fnv1a64, FNV_OFFSET_BASIS);
    }
}
