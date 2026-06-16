//! Deterministic trace sink for golden-trace tests.
//!
//! Every deterministic run emits a `Vec<TraceEvent>` that can be compared
//! across host platforms and used as an expected trace in regression tests.
//!
//! The sink also exposes a C-compatible `sim_trace_u32` helper for
//! lightweight counter tracing from guest firmware.

use crate::{error::SimErrorCode, event_queue::EventId, time::Tick};
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
}
