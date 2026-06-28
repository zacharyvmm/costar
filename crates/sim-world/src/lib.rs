#![warn(missing_docs)]
//! Multi-node World — shared virtual time, deterministic links, and
//! multi-machine traces.
//!
//! A [`World`] owns several [`Machine`] instances connected by
//! deterministic [`Link`] FIFO channels and [`CanBus`] broadcast buses.
//! All machines share the same virtual clock and advance in lockstep
//! to the earliest deadline across all machines, links, buses, and
//! pending events.

use serde::{Deserialize, Serialize};

/// Which RTOS backend a machine uses.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum RtosBackend {
    /// FreeRTOS (default).
    #[default]
    #[serde(rename = "freertos")]
    FreeRtos,
    /// Zephyr (standalone test).
    Zephyr,
}

/// State of a simulation session (used by both JSON-RPC and gRPC servers).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// Session created, no scenario loaded yet.
    #[default]
    Idle,
    /// Scenario loaded and ready to run.
    Ready,
    /// Simulation is actively running.
    Running,
    /// Simulation paused (e.g., by gRPC timeline scrubbing).
    Paused,
    /// Simulation completed successfully.
    Done,
    /// Simulation encountered an error.
    Error,
}

impl SessionState {
    /// Human-readable label for logging / status display.
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Idle => "idle",
            SessionState::Ready => "ready",
            SessionState::Running => "running",
            SessionState::Paused => "paused",
            SessionState::Done => "done",
            SessionState::Error => "error",
        }
    }
}

pub mod board;
pub mod canbus;
pub mod firmware;
pub mod link;
pub mod machine;
pub mod plant;
pub mod scenario;
pub mod world;

pub use board::{BoardConfig, BoardError};
pub use canbus::CanBus;
pub use firmware::Firmware;
pub use link::Link;
pub use machine::Machine;
pub use plant::EnvironmentModel;
pub use scenario::{Scenario, ScenarioError, ScenarioResult};
pub use world::{BleInjection, FaultAction, World};
