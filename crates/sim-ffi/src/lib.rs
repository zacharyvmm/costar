//! # sim-ffi
//!
//! C ABI exports consumed by the FreeRTOS port layer.
//!
//! This crate provides `#[no_mangle]` functions that the C port layer
//! (port.c, sim_hooks.c) calls to interact with the Rust simulator.
//!
//! # Architecture
//!
//! All heavy state lives behind a single-threaded `RefCell`.  The
//! scheduler never holds a borrow across fiber resume (which would
//! cause `RefCell` panics on re-entrant calls from within a fiber).
//! Yields from C code go through the TLS yielder directly and do not
//! touch the global state.
//!
//! Functions that CAN be called from within a running fiber (and thus
//! must not panic when the global RefCell is borrowed) use separate
//! thread-local storage or atomic primitives:
//!   - `sim_port_yield` → TLS yielder (never touches global)
//!   - `sim_task_exit` → TLS yielder (never touches global)
//!   - `sim_now_ticks` → atomic Tick (lock-free read)
//!   - `sim_enter_critical` / `sim_exit_critical` → separate TLS counter
//!   - `sim_trace_u32` → append to a thread-local trace buffer

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use sim_core::time::Tick;
use sim_core::trace::TraceSink;
use sim_fiber::yield_reason::YieldReason;
use sim_fiber::{suspend_active_fiber, Fiber, TaskId};

// ---------------------------------------------------------------------------
// Thread-local state (re-entrant safe)
// ---------------------------------------------------------------------------

/// Current virtual time.  Atomic so it can be read from within a fiber
/// without touching the global RefCell.
static SIM_NOW: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static CRITICAL_NESTING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static TL_TRACE: RefCell<Vec<sim_core::trace::TraceEvent>> = RefCell::new(Vec::new());
}

// ---------------------------------------------------------------------------
// Global state (accessed only from the scheduler outside fiber context)
// ---------------------------------------------------------------------------

/// Interrupt state for virtual critical sections.
#[derive(Debug, Clone, Copy, Default)]
pub struct InterruptState {
    /// Whether virtual interrupts are locked.
    pub locked: bool,
}

/// Global simulator state.
///
/// This is accessed only from the scheduler main loop, never from
/// within a running fiber.  Functions called from fibers use separate
/// TLS or atomic state.
pub struct SimGlobal {
    /// All simulated tasks (fibers).
    pub tasks: Vec<Fiber>,
    /// Index of the currently running task, if any.
    pub current_task: Option<usize>,
    /// Interrupt state for critical sections.
    pub interrupt: InterruptState,
    /// Next task ID.
    pub next_task_id: TaskId,
    /// Trace sink (may be null before initialization).
    pub trace: Option<Box<TraceSink>>,
}

impl SimGlobal {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            current_task: None,
            interrupt: InterruptState::default(),
            next_task_id: 1,
            trace: None,
        }
    }
}

impl Default for SimGlobal {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static SIM_GLOBAL: RefCell<SimGlobal> = RefCell::new(SimGlobal::new());
}

// ---------------------------------------------------------------------------
// C ABI exports
// ---------------------------------------------------------------------------

/// Return the current virtual time in ticks.
///
/// Safe to call from within a running fiber (uses atomic read).
#[no_mangle]
pub unsafe extern "C" fn sim_now_ticks() -> u64 {
    SIM_NOW.load(Ordering::Relaxed)
}

/// Set the current virtual time (called from the scheduler only).
pub fn set_sim_now(now: Tick) {
    SIM_NOW.store(now, Ordering::Relaxed);
}

/// Register a new simulated task.
///
/// Returns an opaque handle, or 0 on failure.
///
/// Must NOT be called from within a running fiber.
#[no_mangle]
pub unsafe extern "C" fn sim_create_task(
    name_ptr: *const std::ffi::c_char,
    entry: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    arg: *mut std::ffi::c_void,
    requested_stack_words: u32,
    priority: u32,
) -> usize {
    SIM_GLOBAL.with(|global| {
        let mut global = global.borrow_mut();

        let name = if name_ptr.is_null() {
            "unnamed"
        } else {
            let c_str = std::ffi::CStr::from_ptr(name_ptr);
            c_str.to_str().unwrap_or("unnamed")
        };
        let name_static: &'static str = Box::leak(name.to_string().into_boxed_str());

        let entry = entry.expect("sim_create_task: NULL entry point");

        let id = global.next_task_id;
        global.next_task_id += 1;

        let _pri = priority;


        let fiber = Fiber::new(
            id,
            name_static,
            requested_stack_words,
            sim_fiber::MIN_HOST_COROUTINE_STACK,
            id,
            move |_reason| {
                // Safety: we're in a fiber, TLS is set.
                entry(arg);
                // Signal task exit via TLS (doesn't touch global).
                suspend_active_fiber(YieldReason::TaskExit);
            },
        );
        global.tasks.push(fiber);
        id as usize
    })
}

/// Start the scheduler — simple round-robin drain loop.
///
/// This function does not return until all tasks have terminated.
#[no_mangle]
pub unsafe extern "C" fn sim_start_scheduler() {
    // Main scheduler loop.
    //
    // The pattern:
    // 1. Find next runnable task (briefly borrow global)
    // 2. Drop global borrow
    // 3. Resume the fiber (may call back into sim_*)
    // 4. Re-borrow global to update state
    // 5. Repeat

    loop {
        // Phase 1: Find a runnable task.
        let task_idx: Option<usize> = SIM_GLOBAL.with(|global| {
            let global = global.borrow();
            let task_count = global.tasks.len();

            if task_count == 0 {
                return None;
            }

            let runnable: Vec<usize> = (0..task_count)
                .filter(|&i| global.tasks[i].is_runnable())
                .collect();

            if runnable.is_empty() {
                return None;
            }

            // Simple round-robin: start after current
            let start = global
                .current_task
                .map(|i| (i + 1) % task_count)
                .unwrap_or(0);

            // Find the first runnable at or after start
            for offset in 0..task_count {
                let idx = (start + offset) % task_count;
                if global.tasks[idx].is_runnable() {
                    return Some(idx);
                }
            }

            None
        });

        let idx = match task_idx {
            Some(i) => i,
            None => break, // No runnable tasks — simulation complete
        };
        // Phase 2: Gather task metadata before resuming.
        let (task_id, _task_name): (TaskId, &'static str) = SIM_GLOBAL.with(|global| {
            let mut global = global.borrow_mut();
            global.current_task = Some(idx);
            let task = &global.tasks[idx];
            let tid = task.id;
            let tname = task.name;
            if let Some(ref mut trace) = global.trace {
                trace.record(sim_core::trace::TraceEvent::TaskResume {
                    at: SIM_NOW.load(Ordering::Relaxed),
                    task: tid,
                    reason: "scheduler",
                });
            }
            (tid, tname)
        });

        // Phase 3: Resume the fiber.
        let yield_reason: Option<YieldReason> = SIM_GLOBAL.with(|global| {
            let mut global = global.borrow_mut();
            let task = &mut global.tasks[idx];
            task.resume(sim_fiber::ResumeReason::SchedulerSelected)
        });

        // Phase 3: Update state after yield.
        SIM_GLOBAL.with(|global| {
            let mut global = global.borrow_mut();
            if let Some(reason) = yield_reason {
                if let Some(ref mut trace) = global.trace {
                    let reason_str: &'static str =
                        Box::leak(format!("{:?}", reason).into_boxed_str());
                    trace.record(sim_core::trace::TraceEvent::TaskYield {
                        at: SIM_NOW.load(Ordering::Relaxed),
                        task: task_id,
                        reason: reason_str,
                    });
                }
            }

            // Flush thread-local trace events into main trace.
            TL_TRACE.with(|tl| {
                let mut tl = tl.borrow_mut();
                if !tl.is_empty() {
                    if let Some(ref mut trace) = global.trace {
                        trace.events.append(&mut tl);
                    }
                    tl.clear();
                }
            });
        });
    }
}

/// Yield the currently executing task from C code.
///
/// Uses the TLS yielder directly — never touches the global RefCell.
/// This is safe to call from within a running fiber.
#[no_mangle]
pub unsafe extern "C" fn sim_port_yield() {
    let ok = suspend_active_fiber(YieldReason::RtosPortYield);
    if !ok {
        // Record fatal error via thread-local trace
        TL_TRACE.with(|tl| {
            tl.borrow_mut().push(sim_core::trace::TraceEvent::Fatal {
                at: SIM_NOW.load(Ordering::Relaxed),
                code: sim_core::error::SimErrorCode::YieldWithoutActiveFiber,
            });
        });
    }
}

/// Mark the current task as exited.
#[no_mangle]
pub unsafe extern "C" fn sim_task_exit() {
    suspend_active_fiber(YieldReason::TaskExit);
}

/// Enter a virtual critical section.
///
/// Uses thread-local counter — safe to call from within a fiber.
#[no_mangle]
pub unsafe extern "C" fn sim_enter_critical() {
    CRITICAL_NESTING.with(|c| {
        c.set(c.get().saturating_add(1));
    });
}

/// Exit a virtual critical section.
///
/// Uses thread-local counter — safe to call from within a fiber.
#[no_mangle]
pub unsafe extern "C" fn sim_exit_critical() {
    CRITICAL_NESTING.with(|c| {
        c.set(c.get().saturating_sub(1));
    });
}

/// Whether virtual interrupts are currently locked.
pub fn is_critical_locked() -> bool {
    CRITICAL_NESTING.with(|c| c.get() > 0)
}

/// Record a u32 value in the trace.
///
/// Uses thread-local trace buffer — safe to call from within a fiber.
#[no_mangle]
pub unsafe extern "C" fn sim_trace_u32(
    label_ptr: *const std::ffi::c_char,
    value: u32,
) {
    let label = if label_ptr.is_null() {
        "?"
    } else {
        let c_str = std::ffi::CStr::from_ptr(label_ptr);
        c_str.to_str().unwrap_or("?")
    };
    let label_static: &'static str = Box::leak(label.to_string().into_boxed_str());

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: SIM_NOW.load(Ordering::Relaxed),
            label: label_static,
            value,
        });
    });
}

// ---------------------------------------------------------------------------
// Public Rust API (for initialization)
// ---------------------------------------------------------------------------

/// Initialize the simulator global state with a trace sink.
pub fn init_global(trace: Box<TraceSink>) {
    SIM_GLOBAL.with(|global| {
        let mut global = global.borrow_mut();
        global.trace = Some(trace);
    });
    set_sim_now(0);
}

/// Access the global state for a read-only operation.
pub fn with_global<F, R>(f: F) -> R
where
    F: FnOnce(&SimGlobal) -> R,
{
    SIM_GLOBAL.with(|global| {
        let global = global.borrow();
        f(&global)
    })
}

/// Access the global state for a mutable operation.
pub fn with_global_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut SimGlobal) -> R,
{
    SIM_GLOBAL.with(|global| {
        let mut global = global.borrow_mut();
        f(&mut global)
    })
}

/// Flush the thread-local trace into the main trace sink.
pub fn flush_trace() {
    SIM_GLOBAL.with(|global| {
        let mut global = global.borrow_mut();
        TL_TRACE.with(|tl| {
            let mut tl = tl.borrow_mut();
            if !tl.is_empty() && global.trace.is_some() {
                global
                    .trace
                    .as_mut()
                    .unwrap()
                    .events
                    .append(&mut tl);
            }
            tl.clear();
        });
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_task_returns_handle() {
        unsafe {
            let name = std::ffi::CString::new("test_task").unwrap();
            let handle = sim_create_task(
                name.as_ptr(),
                Some(test_entry),
                std::ptr::null_mut(),
                256,
                1,
            );
            assert!(handle > 0);
        }
    }

    unsafe extern "C" fn test_entry(_arg: *mut std::ffi::c_void) {
        // Minimal entry that just exits
    }

    #[test]
    fn test_critical_section_nesting() {
        unsafe {
            assert!(!is_critical_locked());
            sim_enter_critical();
            assert!(is_critical_locked());
            sim_enter_critical();
            sim_exit_critical();
            assert!(is_critical_locked());
            sim_exit_critical();
            assert!(!is_critical_locked());
        }
    }
}
