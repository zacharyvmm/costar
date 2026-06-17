//! Multi-node World — shared virtual time, deterministic links, and
//! multi-machine traces.
//!
//! A [`World`] owns several [`Machine`] instances connected by
//! deterministic [`Link`] FIFO channels and [`CanBus`] broadcast buses.
//! All machines share the same virtual clock and advance in lockstep
//! to the earliest deadline across all machines, links, buses, and
//! pending events.

pub mod board;
pub mod canbus;
pub mod link;
pub mod machine;
pub mod plant;
pub mod scenario;
pub mod world;

pub use board::{BoardConfig, BoardError};
pub use canbus::CanBus;
pub use link::Link;
pub use machine::Machine;
pub use plant::EnvironmentModel;
pub use scenario::{Scenario, ScenarioError, ScenarioResult};
pub use world::{FaultAction, World};
