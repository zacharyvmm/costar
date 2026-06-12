//! Yield and resume reasons for the fiber runtime.
//!
//! Every fiber suspend/resume carries one of these reasons, making traces
//! and debugging significantly easier than bare `()`.

use sim_core::time::Tick;

/// Why a fiber suspended itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YieldReason {
    /// The task called `taskYIELD()` (cooperative yield).
    Cooperative,
    /// The RTOS port layer explicitly requested a yield.
    RtosPortYield,
    /// The task is blocked waiting for a resource.
    Blocked,
    /// The task called a sleep/delay primitive.
    SleepUntil(Tick),
    /// The task is waiting for I/O.
    IoWait,
    /// An interrupt handler returned and the scheduler should re-evaluate.
    InterruptExit,
    /// The task exited normally.
    TaskExit,
    /// The task exceeded its instruction/time budget.
    BudgetExceeded,
    /// The task encountered a fatal fault.
    Fault,
}

/// Why a fiber is being resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeReason {
    /// Initial start of the task.
    Start,
    /// The RTOS scheduler selected this task.
    SchedulerSelected,
    /// A timeout expired (e.g., `vTaskDelay` finished).
    TimeoutExpired,
    /// I/O became ready.
    IoReady,
    /// An interrupt returned and this task should resume.
    InterruptReturn,
    /// Manual resume (test harness).
    Manual,
}
