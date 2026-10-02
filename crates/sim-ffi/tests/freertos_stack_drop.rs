//! A FreeRTOS task's suspended stack stays valid when the Simulator is
//! dropped: its C code may have called Rust code that lends the stack to
//! another thread (here, a scoped thread).

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
use std::ffi::{c_char, c_long, c_ulong, c_void};

extern "C" {
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
}

struct ScopeData {
    ready: std::sync::mpsc::Sender<()>,
    read: std::sync::mpsc::Receiver<()>,
    done: std::sync::mpsc::Sender<u64>,
}
unsafe extern "C" fn scoped_task(arg: *mut c_void) {
    let ScopeData { ready, read, done } = *Box::from_raw(arg as *mut ScopeData);
    let data = std::hint::black_box([42u64; 128]);
    std::thread::scope(|scope| {
        let data_ref = &data;
        scope.spawn(move || {
            ready.send(()).unwrap();
            read.recv().unwrap();
            let value = std::hint::black_box(data_ref)[64];
            done.send(value).unwrap();
        });
        sim_ffi::sim_port_yield();
    });
}
#[test]
fn freertos_callback_stack_with_scoped_borrow_must_remain_valid() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let p = Box::into_raw(Box::new(ScopeData {
        ready: ready_tx,
        read: read_rx,
        done: done_tx,
    }));
    let mut sim = Simulator::new(SimConfig::default());
    {
        let _a = sim.activate();
        unsafe {
            assert_eq!(
                xTaskCreate(
                    scoped_task,
                    c"scoped".as_ptr(),
                    128,
                    p.cast(),
                    5,
                    std::ptr::null_mut()
                ),
                1
            );
            sim_ffi::sim_scheduler_tick();
        }
    }
    ready_rx.recv().unwrap();
    drop(sim);
    read_tx.send(()).unwrap();
    assert_eq!(done_rx.recv().unwrap(), 42);
}
