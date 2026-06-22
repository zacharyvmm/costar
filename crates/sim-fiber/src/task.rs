//! Fiber task management.
//!
//! Every simulated RTOS task maps to one `Fiber`.  A fiber wraps a
//! `corosensei::Coroutine` and tracks metadata like task id, name, state,
//! and the configured vs actual stack sizes.

use std::fmt;

use corosensei::{Coroutine, CoroutineResult};
use sim_core::time::Tick;

use crate::tls::{self, SimYielder};
use crate::yield_reason::{ResumeReason, YieldReason};

/// Opaque task identifier.
pub type TaskId = u64;

/// The state of a simulated task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Task exists but has not started yet.
    Created,
    /// Task is actively running on the CPU.
    Running,
    /// Task is ready to run but not currently scheduled.
    Ready,
    /// Task is blocked waiting for a resource.
    Blocked,
    /// Task is sleeping until a virtual timestamp.
    Sleeping {
        /// The absolute virtual time when the task should wake.
        until: Tick,
    },
    /// Task is waiting for I/O.
    IoWaiting,
    /// Task has suspended itself (yielded).
    Suspended,
    /// Task has exited normally.
    Exited,
    /// Task faulted (panic, budget exceeded, etc.).
    Faulted,
}

/// A simulated task backed by a stackful coroutine.
pub struct Fiber {
    /// Unique task identifier.
    pub id: TaskId,
    /// Human-readable task name.
    pub name: &'static str,
    /// FreeRTOS priority (0 = lowest, configMAX_PRIORITIES-1 = highest).
    pub priority: u32,
    /// The RTOS-configured stack size (in words for FreeRTOS).
    pub requested_stack_words: u32,
    /// Actual host stack size provided to corosensei.
    pub host_stack_size: usize,
    /// Current task state.
    pub state: TaskState,
    /// The coroutine handle.
    coroutine: Option<Coroutine<ResumeReason, YieldReason, (), corosensei::stack::DefaultStack>>,
    /// The last yield reason (for debugging).
    pub last_yield_reason: Option<YieldReason>,
    /// Monotonic creation sequence number.
    pub creation_seq: u64,
    /// Pointer to this fiber's yielder, set by the coroutine body.
    #[allow(dead_code)]
    yielder_ptr: std::cell::Cell<Option<std::ptr::NonNull<SimYielder>>>,
}

impl fmt::Debug for Fiber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fiber")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("state", &self.state)
            .field("requested_stack_words", &self.requested_stack_words)
            .field("host_stack_size", &self.host_stack_size)
            .finish_non_exhaustive()
    }
}

/// Minimum host coroutine stack size in bytes.
///
/// Embedded RTOS tasks may request very small stacks (e.g., 128 words = 512
/// bytes), but host C libraries and debug logging can easily exceed such
/// small stacks.  This minimum guards against stack overflow in the host
/// environment.
pub const MIN_HOST_COROUTINE_STACK: usize = 64 * 1024; // 64 KiB

impl Fiber {
    /// Create a new fiber.
    ///
    /// The fiber is created in `Created` state.  The coroutine body `f`
    /// receives the `ResumeReason` as input and is expected to yield
    /// `YieldReason` values and eventually return `()`.
    pub fn new<F>(
        id: TaskId,
        name: &'static str,
        priority: u32,
        requested_stack_words: u32,
        host_stack_size: usize,
        creation_seq: u64,
        f: F,
    ) -> Self
    where
        F: FnOnce(ResumeReason) + 'static,
    {
        let host_stack_size = host_stack_size.max(MIN_HOST_COROUTINE_STACK);

        // The yielder is pushed into TLS on entry so that C hooks can use it.
        let coroutine = Coroutine::new(move |yielder, input: ResumeReason| {
            tls::set_active_yielder(yielder);
            f(input);
        });

        Self {
            id,
            name,
            priority,
            requested_stack_words,
            host_stack_size,
            state: TaskState::Created,
            coroutine: Some(coroutine),
            last_yield_reason: None,
            creation_seq,
            yielder_ptr: std::cell::Cell::new(None),
        }
    }

    /// Resume a fiber, passing a reason for the resume.
    ///
    /// Returns the yield reason if the fiber suspended, or `None` if it
    /// exited (normally or via fault).
    pub fn resume(&mut self, reason: ResumeReason) -> Option<YieldReason> {
        // Ensure we only resume in appropriate states
        match self.state {
            TaskState::Created
            | TaskState::Ready
            | TaskState::Suspended
            | TaskState::Blocked
            | TaskState::Sleeping { .. }
            | TaskState::IoWaiting => {
                self.state = TaskState::Running;
            }
            TaskState::Exited | TaskState::Faulted => {
                return None;
            }
            TaskState::Running => {
                // Already running - shouldn't happen in single-threaded model
                return None;
            }
        }

        let coroutine = self.coroutine.as_mut().expect("coroutine must exist");

        // Safety: we're single-threaded.  The coroutine may set TLS during
        // its execution and clear it before returning.
        match coroutine.resume(reason) {
            CoroutineResult::Yield(yield_reason) => {
                self.last_yield_reason = Some(yield_reason);
                match yield_reason {
                    YieldReason::SleepUntil(until) => {
                        self.state = TaskState::Sleeping { until };
                    }
                    YieldReason::Blocked => {
                        self.state = TaskState::Blocked;
                    }
                    YieldReason::IoWait => {
                        self.state = TaskState::IoWaiting;
                    }
                    YieldReason::TaskExit => {
                        self.state = TaskState::Exited;
                        // Don't drop the Coroutine yet — the TLS yielder
                        // still points to its stack.  The Coroutine is
                        // dropped when the Fiber is dropped (at simulation end).
                    }
                    _ => {
                        self.state = TaskState::Suspended;
                    }
                }
                Some(yield_reason)
            }
            CoroutineResult::Return(()) => {
                // Coroutine returned normally — task exited.
                self.state = TaskState::Exited;
                // Don't drop the Coroutine yet (see above).
                self.last_yield_reason = Some(YieldReason::TaskExit);
                Some(YieldReason::TaskExit)
            }
        }
    }

    /// Whether the fiber is runnable (i.e., could be resumed right now).
    pub fn is_runnable(&self) -> bool {
        matches!(
            self.state,
            TaskState::Created | TaskState::Ready | TaskState::Suspended
        )
    }

    /// Whether the fiber has terminated.
    pub fn is_terminated(&self) -> bool {
        matches!(self.state, TaskState::Exited | TaskState::Faulted)
    }

    /// Wake a sleeping task if its sleep deadline has passed.
    pub fn try_wake(&mut self, now: Tick) {
        if let TaskState::Sleeping { until } = self.state {
            if now >= until {
                self.state = TaskState::Ready;
            }
        }
    }

    /// Mark the task as ready to run.
    pub fn set_ready(&mut self) {
        if !self.is_terminated() {
            self.state = TaskState::Ready;
        }
    }

    /// Mark this fiber as deleted by the RTOS kernel.
    ///
    /// Sets the state to `Exited` and takes the coroutine without dropping it.
    /// This avoids `Coroutine::drop`'s force-unwind, which would try to resume
    /// the coroutine inside a C function that has no active yielder (the task
    /// was suspended inside `vTaskDelay` or similar when deleted).  The coroutine
    /// stack memory is leaked, which is safe because this only happens at
    /// simulation end — the OS reclaims all memory at process exit.
    pub fn mark_deleted(&mut self) {
        self.state = TaskState::Exited;
        // Take the coroutine and prevent its Drop from running.
        // ManuallyDrop wraps the Coroutine; when _leaked goes out of
        // scope, the wrapper is dropped but the inner Coroutine is not.
        if let Some(c) = self.coroutine.take() {
            let _leaked = std::mem::ManuallyDrop::new(c);
        }
    }
}

impl Drop for Fiber {
    fn drop(&mut self) {
        // Prevent Coroutine::drop from running — it calls force_unwind
        // which tries to resume the coroutine.  A coroutine suspended
        // inside a C function (vTaskDelay, etc.) has no valid yielder
        // and force_unwind will panic (non-unwinding abort).
        // Instead, leak the coroutine stack.  This is safe because
        // fiber drops only happen at simulation end; the OS reclaims
        // all memory at process exit.
        eprintln!("Fiber::drop id={} state={:?}", self.id, self.state);
        if let Some(c) = self.coroutine.take() {
            eprintln!("Fiber::drop leaking coroutine id={}", self.id);
            let _leaked = std::mem::ManuallyDrop::new(c);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_fiber_create_and_run() {
        let mut fiber = Fiber::new(1, "test", 1, 256, MIN_HOST_COROUTINE_STACK, 0, |_reason| {
            // Task does nothing, just exits.
        });

        assert_eq!(fiber.state, TaskState::Created);
        assert!(fiber.is_runnable());

        let result = fiber.resume(ResumeReason::Start);
        assert_eq!(result, Some(YieldReason::TaskExit));
        assert_eq!(fiber.state, TaskState::Exited);
        assert!(fiber.is_terminated());
    }

    #[test]
    fn test_fiber_yield_and_resume() {
        let mut fiber = Fiber::new(
            1,
            "yielder",
            1,
            256,
            MIN_HOST_COROUTINE_STACK,
            0,
            |_reason| {
                // Yield cooperatively, then exit on next resume.
                tls::suspend_active_fiber(YieldReason::Cooperative);
            },
        );

        // First resume: should yield
        let result = fiber.resume(ResumeReason::Start);
        assert_eq!(result, Some(YieldReason::Cooperative));
        assert_eq!(fiber.state, TaskState::Suspended);

        // Second resume: should exit (closure returns)
        let result = fiber.resume(ResumeReason::SchedulerSelected);
        assert_eq!(result, Some(YieldReason::TaskExit));
        assert_eq!(fiber.state, TaskState::Exited);
    }

    #[test]
    fn test_fiber_yield_many_times() {
        const COUNT: u32 = 100;

        let mut fiber = Fiber::new(
            1,
            "many_yields",
            1,
            256,
            MIN_HOST_COROUTINE_STACK,
            0,
            |_reason| {
                for i in 0..COUNT {
                    // We use suspend_active_fiber to push through TLS
                    // (simulates what sim_port_yield does from C)
                    tls::suspend_active_fiber(YieldReason::Cooperative);
                    // The value of i is lost across yields in this test,
                    // but the loop counter survives in the coroutine's stack.
                    let _ = i;
                }
            },
        );

        for _ in 0..COUNT {
            let result = fiber.resume(ResumeReason::SchedulerSelected);
            assert_eq!(result, Some(YieldReason::Cooperative));
            assert!(fiber.state == TaskState::Suspended);
        }

        // Final resume: task exits
        let result = fiber.resume(ResumeReason::SchedulerSelected);
        assert_eq!(result, Some(YieldReason::TaskExit));
        assert_eq!(fiber.state, TaskState::Exited);
    }

    #[test]
    fn test_fiber_sleep() {
        let mut fiber = Fiber::new(
            1,
            "sleeper",
            1,
            256,
            MIN_HOST_COROUTINE_STACK,
            0,
            |_reason| {
                tls::suspend_active_fiber(YieldReason::SleepUntil(1000));
            },
        );

        // First resume: should sleep
        let result = fiber.resume(ResumeReason::Start);
        assert_eq!(result, Some(YieldReason::SleepUntil(1000)));
        assert!(matches!(fiber.state, TaskState::Sleeping { until: 1000 }));

        // Not woken yet
        fiber.try_wake(500);
        assert!(matches!(fiber.state, TaskState::Sleeping { .. }));

        // Wake it
        fiber.try_wake(1000);
        assert_eq!(fiber.state, TaskState::Ready);

        // Resume again
        let result = fiber.resume(ResumeReason::TimeoutExpired);
        assert_eq!(result, Some(YieldReason::TaskExit));
    }

    #[test]
    fn test_min_stack_enforcement() {
        let fiber = Fiber::new(
            1,
            "tiny_stack",
            1,
            32,   // Very small RTOS stack request
            4096, // Small host stack below minimum
            0,
            |_| {},
        );

        assert_eq!(fiber.host_stack_size, MIN_HOST_COROUTINE_STACK);
        assert_eq!(fiber.requested_stack_words, 32);
    }

    #[test]
    fn test_task_exit_via_yield() {
        let mut fiber = Fiber::new(
            1,
            "exiter",
            1,
            256,
            MIN_HOST_COROUTINE_STACK,
            0,
            |_reason| {
                tls::suspend_active_fiber(YieldReason::TaskExit);
                // After TaskExit suspend, the coroutine still runs to
                // completion but shouldn't be resumable.
            },
        );

        let result = fiber.resume(ResumeReason::Start);
        assert_eq!(result, Some(YieldReason::TaskExit));
        assert_eq!(fiber.state, TaskState::Exited);

        // Resuming an exited fiber should be a no-op
        let result = fiber.resume(ResumeReason::SchedulerSelected);
        assert_eq!(result, None);
    }

    #[test]
    fn test_tls_cleared_after_resume() {
        // TLS yielder is now persistent after resume — it's overwritten
        // by the next fiber, not cleared.
        let mut fiber = Fiber::new(1, "tls_check", 1, 256, MIN_HOST_COROUTINE_STACK, 0, |_| {
            assert!(tls::has_active_fiber());
        });

        assert!(!tls::has_active_fiber());
        fiber.resume(ResumeReason::Start);
        // Yielder stays set (the coroutine body sets it and doesn't clear).
        assert!(tls::has_active_fiber());
    }

    #[test]
    fn test_fiber_panic_boundary() {
        let mut fiber = Fiber::new(1, "panicer", 1, 256, MIN_HOST_COROUTINE_STACK, 0, |_| {
            panic!("test panic inside fiber");
        });

        // Panics propagate through coroutines (no catch_unwind in MVP).
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fiber.resume(ResumeReason::Start);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn test_fiber_panic_caught_and_faulted() {
        // Test the production panic-boundary pattern: catch_unwind
        // around resume(), mark task as Faulted on panic.
        let mut fiber = Fiber::new(1, "panicer2", 1, 256, MIN_HOST_COROUTINE_STACK, 0, |_| {
            panic!("deliberate panic in fiber");
        });

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fiber.resume(ResumeReason::SchedulerSelected)
        }));

        match result {
            Ok(_yield_reason) => {
                // If it didn't panic, the task exited normally (unlikely with panic! above)
            }
            Err(_) => {
                // Mark as faulted — this is what the scheduler does
                fiber.state = TaskState::Faulted;
            }
        }

        assert_eq!(fiber.state, TaskState::Faulted);
        assert!(fiber.is_terminated());

        // Resuming a faulted fiber should be a no-op
        let result = fiber.resume(ResumeReason::SchedulerSelected);
        assert_eq!(result, None);
    }

    #[test]
    fn test_fiber_yield_1m_stress() {
        const COUNT: u32 = 1_000_000;

        let mut fiber = Fiber::new(
            1,
            "stress_1m",
            1,
            256,
            MIN_HOST_COROUTINE_STACK,
            0,
            |_reason| {
                for _ in 0..COUNT {
                    tls::suspend_active_fiber(YieldReason::Cooperative);
                }
            },
        );

        for _ in 0..COUNT {
            let result = fiber.resume(ResumeReason::SchedulerSelected);
            assert_eq!(result, Some(YieldReason::Cooperative));
            assert!(fiber.state == TaskState::Suspended);
        }

        // Final resume: task exits
        let result = fiber.resume(ResumeReason::SchedulerSelected);
        assert_eq!(result, Some(YieldReason::TaskExit));
        assert_eq!(fiber.state, TaskState::Exited);
    }
}
