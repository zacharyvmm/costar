//! # sim-core
//!
//! Deterministic virtual-time event queue, trace sink, and run loop for the
//! costar — Cooperative Scheduler Testing And Runtime.
//!
//! The core owns:
//! * Virtual time (`Tick`).
//! * The deterministic min-heap event queue.
//! * The event dispatch loop.
//! * The trace sink used for golden-trace tests.
//!
//! The core never spawns host threads, never uses wall-clock time in
//! deterministic mode, and never calls into RTOS-specific code. It is a
//! pure Rust event engine that other layers build on.
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod event_queue;
pub mod run_loop;
pub mod time;
pub mod trace;

pub use config::SimConfig;
pub use error::{SimError, SimErrorCode, SimResult};
pub use event_queue::{EventId, EventQueue, QueueKey, ScheduledEvent};
pub use run_loop::{SimulatorContext, SimulatorCore};
pub use time::Tick;
pub use trace::{TraceEvent, TraceSink, TraceStats, TraceV2};
