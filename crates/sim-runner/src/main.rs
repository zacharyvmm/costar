//! Universal RTOS Native Simulator — host executable.
//!
//! Loads a configuration, compiles the guest firmware, spawns the simulator
//! core, and runs the event loop.

// C entry point for the FreeRTOS application (compiled via `cc`).
#[link(name = "embedded_c_payload", kind = "static")]
extern "C" {
    fn c_sim_main() -> i32;
}

fn main() {
    env_logger::init();
    log::info!("Universal RTOS Native Simulator starting");

    // Initialize the trace sink.
    let trace = Box::new(sim_core::trace::TraceSink::new());
    sim_ffi::init_global(trace);

    // Call the C firmware entry point.
    log::info!("Starting C firmware entry");
    let _exit_code = unsafe { c_sim_main() };

    // Print the trace.
    sim_ffi::flush_trace();
    sim_ffi::with_global(|global| {
        if let Some(ref trace) = global.trace {
            println!("=== Simulation Trace ===");
            for event in trace.events() {
                println!("{}", event);
            }
            println!("=== End Trace ({} events) ===", trace.len());
        }
    });
}
