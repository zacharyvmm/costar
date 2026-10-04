//! A suspended native Rust fiber's stack stays valid after the fiber is
//! dropped: values on it may still be borrowed (here by a scoped thread).

use sim_fiber::yield_reason::YieldReason;
use sim_fiber::{suspend_active_fiber, Fiber, ResumeReason};
use std::sync::mpsc;

#[test]
fn suspended_rust_stack_with_scoped_borrow_must_remain_valid() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (read_tx, read_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let mut fiber = Fiber::new(1, "scoped", 1, 128, 65536, 1, move |_| {
        let data = std::hint::black_box([42u64; 128]);
        std::thread::scope(|scope| {
            let data_ref = &data;
            scope.spawn(move || {
                ready_tx.send(()).unwrap();
                read_rx.recv().unwrap();
                let result = std::hint::black_box(data_ref)[64];
                done_tx.send(result).unwrap();
            });
            suspend_active_fiber(YieldReason::Cooperative);
        });
    });
    assert_eq!(
        fiber.resume(ResumeReason::SchedulerSelected),
        Some(YieldReason::Cooperative)
    );
    ready_rx.recv().unwrap();
    // Dropping the suspended fiber must not free the stack `data` lives on.
    drop(fiber);
    read_tx.send(()).unwrap();
    assert_eq!(done_rx.recv().unwrap(), 42);
}
