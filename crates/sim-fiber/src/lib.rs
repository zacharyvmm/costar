//! # sim-fiber
//!
//! Stackful coroutine runtime that hosts simulated RTOS tasks.
//!
//! Uses `corosensei` for stack switching and exposes a safe-ish Rust
//! wrapper around task creation, resume, suspend, exit, and fault handling.
//!
//! This crate owns:
//! * Fiber creation and lifecycle (task.rs)
//! * Thread-local active yielder (tls.rs)
//! * Panic boundary between Rust and C guest code
//! * Yield/Resume reason enums (yield_reason.rs)
//! * Minimum stack size enforcement

#![warn(missing_docs)]

mod task;
mod tls;
pub mod yield_reason;

pub use task::{Fiber, TaskId, TaskState, MIN_HOST_COROUTINE_STACK};
pub use tls::{has_active_fiber, suspend_active_fiber};
pub use yield_reason::{ResumeReason, YieldReason};
