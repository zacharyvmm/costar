//! Multi-node World — shared virtual time, deterministic links, and
//! multi-machine traces.
//!
//! A [`World`] owns several [`Machine`] instances connected by
//! deterministic [`Link`] FIFO channels.  All machines share the same
//! virtual clock and advance in lockstep to the earliest deadline
//! across all machines, links, and pending events.

pub mod link;
pub mod machine;
pub mod world;

pub use link::Link;
pub use machine::Machine;
pub use world::World;
