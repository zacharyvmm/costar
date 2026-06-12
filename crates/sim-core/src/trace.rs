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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl Default for TraceSink {
    fn default() -> Self {
        Self::new()
    }
}
