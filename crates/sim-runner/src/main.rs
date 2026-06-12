//! Universal RTOS Native Simulator — host executable.
//!
//! Loads a configuration, compiles the guest firmware, spawns the simulator
//! core, and runs the event loop.

use std::env;

// C entry point for the FreeRTOS application (compiled via `cc`).
#[link(name = "embedded_c_payload", kind = "static")]
extern "C" {
    fn c_sim_main() -> i32;
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let golden_mode = args.iter().any(|a| a == "--golden");

    env_logger::init();

    if !golden_mode {
        log::info!("Universal RTOS Native Simulator starting");
    }

    // Initialize the trace sink.
    let trace = Box::new(sim_core::trace::TraceSink::new());
    sim_ffi::init_global(trace);

    // Call the C firmware entry point.
    if !golden_mode {
        log::info!("Starting C firmware entry");
    }
    let _exit_code = unsafe { c_sim_main() };

    // Print the trace.
    sim_ffi::flush_trace();
    sim_ffi::with_global(|global| {
        if let Some(ref trace) = global.trace {
            if golden_mode {
                // Machine-readable golden trace format (no header/footer)
                for event in trace.events() {
                    println!("{}", event);
                }
            } else {
                println!("=== Simulation Trace ===");
                for event in trace.events() {
                    println!("{}", event);
                }
                println!("=== End Trace ({} events) ===", trace.len());
            }
        }
    });
}
