//! costar — Cooperative Scheduler Testing And Runtime (host executable).
//!
//! Loads a configuration, compiles the guest firmware, spawns the simulator
//! core, and runs the event loop.
//!
//! # Usage
//!
//! ```bash
//! cargo run                                    # Default deterministic run (FreeRTOS)
//! cargo run -- --rtos zephyr                   # Zephyr backend (hello-thread demo)
//! cargo run -- --golden                        # Machine-readable trace output
//! cargo run -- --mode deterministic            # Deterministic mode (default)
//! cargo run -- --mode interactive              # Interactive mode (host I/O)
//! cargo run -- --watchdog 5                    # Wall-clock watchdog (5s timeout)
//! cargo run -- --config sim.toml               # TOML config file
//! cargo run -- --help                          # Show usage
//! ```

mod config;
#[cfg(any(zephyr_linked, zephyr_cc_kernel))]
mod zephyr_glue;

use std::env;
use std::process;
use std::time::{Duration, Instant};

use config::SimConfig;

// C entry point for the FreeRTOS application (compiled via `cc`).
#[link(name = "embedded_c_payload", kind = "static")]
extern "C" {
    fn c_sim_main() -> i32;
    // c_sim_interactive_main uses POSIX socketpair — only available on unix.
    #[cfg(not(windows))]
    fn c_sim_interactive_main() -> i32;
    fn c_sim_tight_loop_main() -> i32;
    fn c_sim_broader_api_main() -> i32;
    fn c_sim_i2c_spi_main() -> i32;
    fn c_sim_can_main() -> i32;
    fn c_sim_devices_main() -> i32;
}

// C entry point for the Zephyr application (compiled via `cc`).
// Only used when Zephyr is NOT linked/compiled from source.
#[cfg(not(any(zephyr_linked, zephyr_cc_kernel)))]
#[link(name = "embedded_zephyr_payload", kind = "static")]
extern "C" {
    fn c_zephyr_main() -> i32;
}

// Real Zephyr entry point (linked from west build or cc crate).
// `posix_boot_cpu()` initialises the native_sim CPU emulation and
// calls z_cstart() which never returns.
#[cfg(any(zephyr_linked, zephyr_cc_kernel))]
#[link(name = "embedded_zephyr_payload", kind = "static")]
extern "C" {
    fn posix_boot_cpu();
}

/// Which RTOS backend to use.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum RtosBackend {
    /// FreeRTOS (default).
    #[default]
    FreeRtos,
    /// Zephyr (standalone test).
    Zephyr,
}

/// Simulation mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SimMode {
    /// Fully deterministic: no host I/O, virtual-time-only events.
    #[default]
    Deterministic,
    /// Interactive: wall-clock time allowed, host sockets permitted.
    Interactive,
    /// Tight-loop: Tier 3 edge-instrumentation demo (CPU-bound task + watchdog).
    TightLoop,
    /// Broader-API: exercises semaphores, mutexes, event groups, task notifications.
    BroaderApi,
    /// Ztest: Zephyr ztest framework integration (requires --rtos zephyr + zephyr_real).
    Ztest,
    /// I2cSpi: exercises virtual I2C and SPI controllers.
    I2cSpi,
    /// Can: exercises virtual CAN bus controller.
    Can,
    /// Devices: combined sensor + storage + fault injection demo.
    Devices,
}

/// Trace output format.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum TraceFormat {
    /// Human-readable line-oriented format (default, backward-compatible).
    #[default]
    Human,
    /// JSONL — one JSON object per line, self-describing with `"event"` tag.
    Jsonl,
}

fn print_usage(prog: &str) {
    eprintln!("Usage: {} [OPTIONS]", prog);
    eprintln!("Options:");
    eprintln!("  --rtos <freertos|zephyr>   RTOS backend (default: freertos)");
    eprintln!("  --golden                    Machine-readable trace output (no header/footer)");
    eprintln!(
        "  --mode <deterministic|interactive|tight-loop|broader-api|i2c-spi|can|devices|ztest>"
    );
    eprintln!("                              Simulation mode (default: deterministic)");
    eprintln!("  --trace-format <human|jsonl>  Trace output format (default: human)");
    eprintln!("  --scenario <path>           TOML scenario file (multi-machine simulation)");
    eprintln!("  --diff <path>               Compare trace output against expected file");
    eprintln!("  --watchdog <secs>           Wall-clock timeout in seconds (default: none)");
    eprintln!("  --config <path>             TOML configuration file");
    eprintln!("  --verbose                   Enable verbose logging");
    eprintln!("  --list-modes                List available simulation modes and exit");
    eprintln!("  --help                      Show this help message");
}

fn print_modes() {
    println!("Available simulation modes:");
    println!("  deterministic   Fully deterministic FreeRTOS demo (queue ping-pong)");
    println!("  interactive     Host I/O demo with socketpair (Unix only)");
    println!("  tight-loop      Tier 3 edge-instrumentation demo (CPU-bound + watchdog)");
    println!("  broader-api     FreeRTOS broader API demo (sem/mutex/event-group/notify)");
    println!("  i2c-spi         Virtual I2C and SPI controller demo");
    println!("  can             Virtual CAN bus controller demo");
    println!("  devices         Combined sensor, storage, and fault injection demo");
    println!("  ztest           Zephyr ztest framework demo (requires --rtos zephyr)");
    println!();
    println!("Use --rtos zephyr for Zephyr backend (standalone hello-thread by default).");
    println!("Use --rtos zephyr --mode broader-api for Zephyr k_sem/k_mutex/k_msgq demo.");
    println!("Zephyr modes require ZEPHYR_BASE for real kernel builds.");
}

fn main() {
    env_logger::try_init().ok();

    let args: Vec<String> = env::args().collect();
    let prog = &args[0];

    let mut golden_mode = false;
    let mut sim_mode = SimMode::default();
    let mut rtos = RtosBackend::default();
    let mut trace_format = TraceFormat::default();
    let mut watchdog_secs: Option<u64> = None;
    let mut config_path: Option<String> = None;
    let mut scenario_path: Option<String> = None;
    let mut verbose = false;
    let mut diff_path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--rtos" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --rtos requires a value (freertos or zephyr)");
                    process::exit(1);
                }
                rtos = match args[i].as_str() {
                    "freertos" => RtosBackend::FreeRtos,
                    "zephyr" => RtosBackend::Zephyr,
                    other => {
                        eprintln!(
                            "error: unknown rtos '{}' (expected 'freertos' or 'zephyr')",
                            other
                        );
                        process::exit(1);
                    }
                };
            }
            "--golden" => golden_mode = true,
            "--mode" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --mode requires a value (deterministic, interactive, tight-loop, broader-api, i2c-spi, can, or ztest)");
                    process::exit(1);
                }
                sim_mode = match args[i].as_str() {
                    "deterministic" => SimMode::Deterministic,
                    "interactive" => SimMode::Interactive,
                    "tight-loop" => SimMode::TightLoop,
                    "broader-api" => SimMode::BroaderApi,
                    "ztest" => SimMode::Ztest,
                    "i2c-spi" => SimMode::I2cSpi,
                    "can" => SimMode::Can,
                    "devices" => SimMode::Devices,
                    other => {
                        eprintln!(
                            "error: unknown mode '{}' (expected 'deterministic', 'interactive', 'tight-loop', 'broader-api', 'i2c-spi', 'can', 'devices', or 'ztest')",
                            other
                        );
                        process::exit(1);
                    }
                };
            }
            "--trace-format" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --trace-format requires a value (human or jsonl)");
                    process::exit(1);
                }
                trace_format = match args[i].as_str() {
                    "human" => TraceFormat::Human,
                    "jsonl" => TraceFormat::Jsonl,
                    other => {
                        eprintln!(
                            "error: unknown trace format '{}' (expected 'human' or 'jsonl')",
                            other
                        );
                        process::exit(1);
                    }
                };
            }
            "--watchdog" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --watchdog requires a value (seconds)");
                    process::exit(1);
                }
                match args[i].parse::<u64>() {
                    Ok(s) if s > 0 => watchdog_secs = Some(s),
                    _ => {
                        eprintln!("error: --watchdog requires a positive integer");
                        process::exit(1);
                    }
                }
            }
            "--config" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --config requires a path");
                    process::exit(1);
                }
                config_path = Some(args[i].clone());
            }
            "--scenario" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --scenario requires a path");
                    process::exit(1);
                }
                scenario_path = Some(args[i].clone());
            }
            "--diff" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --diff requires a path to an expected trace file");
                    process::exit(1);
                }
                diff_path = Some(args[i].clone());
            }
            "--verbose" => verbose = true,
            "--list-modes" => {
                print_modes();
                process::exit(0);
            }
            "--help" | "-h" => {
                print_usage(prog);
                process::exit(0);
            }
            other => {
                eprintln!("error: unknown option '{}'", other);
                eprintln!("       use --help for usage information");
                process::exit(1);
            }
        }
        i += 1;
    }

    // ── Load config file (overrides defaults, CLI args take precedence) ─
    let mut config = SimConfig::default();
    if let Some(ref path) = config_path {
        match SimConfig::from_file(path) {
            Ok(cfg) => config = cfg,
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
    }

    // CLI flags override config file values (when CLI flags are explicitly set).
    if !args.iter().any(|a| a == "--mode") {
        sim_mode = match config.simulation.mode.as_str() {
            "deterministic" => SimMode::Deterministic,
            "interactive" => SimMode::Interactive,
            "tight-loop" => SimMode::TightLoop,
            "broader-api" => SimMode::BroaderApi,
            "ztest" => SimMode::Ztest,
            "i2c-spi" => SimMode::I2cSpi,
            "can" => SimMode::Can,
            "devices" => SimMode::Devices,
            other => {
                eprintln!(
                    "error: invalid mode '{}' in config (expected 'deterministic', 'interactive', 'tight-loop', 'broader-api', 'i2c-spi', 'can', 'devices', or 'ztest')",
                    other
                );
                process::exit(1);
            }
        };
    }

    // Use config watchdog if not set on CLI
    if watchdog_secs.is_none() {
        watchdog_secs = config.simulation.watchdog_secs;
    }

    // Use config golden if not set on CLI
    if !args.iter().any(|a| a == "--golden") && config.trace.golden {
        golden_mode = true;
    }

    // Use config trace_format if not set on CLI
    if !args.iter().any(|a| a == "--trace-format") {
        trace_format = match config.trace.format.as_deref() {
            Some("jsonl") => TraceFormat::Jsonl,
            _ => TraceFormat::Human,
        };
    }

    // Enable verbose logging if requested
    if verbose {
        log::set_max_level(log::LevelFilter::Debug);
    }

    if !golden_mode {
        log::info!("costar starting");
        log::info!("  rtos: {:?}", rtos);
        log::info!("  mode: {:?}", sim_mode);
        log::info!("  tick_rate_hz: {}", config.simulation.tick_rate_hz);
        if let Some(ref path) = config_path {
            log::info!("  config: {}", path);
        }
        if let Some(ref s_path) = scenario_path {
            log::info!("  scenario: {}", s_path);
        }
        if let Some(secs) = watchdog_secs {
            log::info!("  watchdog timeout: {}s", secs);
        }
    }

    // ── Scenario mode: multi-machine simulation from TOML file ──
    if let Some(ref s_path) = scenario_path {
        match run_scenario(s_path, golden_mode) {
            Ok(()) => process::exit(0),
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
    }

    // ── Interactive mode is only supported for FreeRTOS ─────────
    if sim_mode == SimMode::Interactive && rtos == RtosBackend::Zephyr {
        eprintln!("error: interactive mode is not supported with --rtos zephyr");
        process::exit(1);
    }

    // Interactive mode requires POSIX socketpair — not available on Windows.
    #[cfg(windows)]
    if sim_mode == SimMode::Interactive {
        eprintln!("error: interactive mode is not supported on Windows");
        process::exit(1);
    }

    // ── Tight-loop mode is only supported for FreeRTOS ──────────
    if sim_mode == SimMode::TightLoop && rtos == RtosBackend::Zephyr {
        eprintln!("error: tight-loop mode is not supported with --rtos zephyr");
        process::exit(1);
    }

    // ── Broader-api mode is supported for both FreeRTOS and Zephyr ──

    // ── Ztest mode requires Zephyr with zephyr_real feature ───────
    if sim_mode == SimMode::Ztest {
        if rtos != RtosBackend::Zephyr {
            eprintln!("error: --mode ztest requires --rtos zephyr");
            process::exit(1);
        }
        #[cfg(not(any(zephyr_linked, zephyr_cc_kernel)))]
        {
            eprintln!("error: --mode ztest requires real Zephyr kernel (set ZEPHYR_BASE and enable zephyr_real feature)");
            process::exit(1);
        }
    }

    // ── Interactive mode setup ─────────────────────────────────
    // host_poller uses Unix-specific FD types — only available on unix.
    #[cfg(unix)]
    if sim_mode == SimMode::Interactive {
        if !golden_mode {
            log::info!("Initializing host poller for interactive mode");
        }
        sim_net::host_poller::init_host_poller()
            .expect("Failed to initialize host poller for interactive mode");
    }

    // Initialize the trace sink.
    let trace = Box::new(sim_core::trace::TraceSink::new());
    sim_ffi::init_global(trace);

    // ── Virtual device initialization ─────────────────────────
    // Register I2C and SPI controllers so C firmware can use them.
    // These are placed in thread-local storage before C code runs.
    if sim_mode == SimMode::I2cSpi {
        sim_devices::i2c_insert(sim_devices::VirtualI2c::new(0, 100_000));
        sim_devices::spi_insert(sim_devices::VirtualSpi::new(0, 1_000_000));
    }
    if sim_mode == SimMode::Can {
        sim_devices::can_insert(sim_devices::VirtualCan::new(0, 500_000));
    }
    if sim_mode == SimMode::Devices {
        sim_devices::adc_insert(sim_devices::VirtualAdc::new(0));
        sim_devices::temp_sensor_insert(sim_devices::VirtualTempSensor::new(0));
        sim_devices::eeprom_insert(sim_devices::VirtualEeprom::new(0));
        sim_devices::flash_insert(sim_devices::VirtualFlash::new(0));
    }

    // Call the C firmware entry point.
    if !golden_mode {
        log::info!("Starting C firmware entry ({:?}, {:?})", rtos, sim_mode);
    }

    let start = Instant::now();
    let exit_code = match (rtos, sim_mode) {
        #[cfg(any(zephyr_linked, zephyr_cc_kernel))]
        (RtosBackend::Zephyr, _) => run_zephyr_real(),
        #[cfg(not(any(zephyr_linked, zephyr_cc_kernel)))]
        (RtosBackend::Zephyr, _) => unsafe { c_zephyr_main() },
        #[cfg(not(windows))]
        (RtosBackend::FreeRtos, SimMode::Interactive) => unsafe { c_sim_interactive_main() },
        #[cfg(windows)]
        (RtosBackend::FreeRtos, SimMode::Interactive) => {
            eprintln!("error: interactive mode is not supported on Windows");
            process::exit(1);
        }
        (RtosBackend::FreeRtos, SimMode::TightLoop) => unsafe { c_sim_tight_loop_main() },
        (RtosBackend::FreeRtos, SimMode::BroaderApi) => unsafe { c_sim_broader_api_main() },
        (RtosBackend::FreeRtos, SimMode::I2cSpi) => unsafe { c_sim_i2c_spi_main() },
        (RtosBackend::FreeRtos, SimMode::Can) => unsafe { c_sim_can_main() },
        (RtosBackend::FreeRtos, SimMode::Devices) => unsafe { c_sim_devices_main() },
        (RtosBackend::FreeRtos, SimMode::Deterministic) => unsafe { c_sim_main() },
        // Ztest mode requires --rtos zephyr; the pre-check above already exits.
        (RtosBackend::FreeRtos, SimMode::Ztest) => {
            unreachable!("ztest mode requires --rtos zephyr")
        }
    };
    let elapsed = start.elapsed();

    if !golden_mode {
        log::info!(
            "Simulation completed in {:.3}s (exit code: {})",
            elapsed.as_secs_f64(),
            exit_code
        );
    }

    // Wall-clock watchdog check
    if let Some(secs) = watchdog_secs {
        if elapsed > Duration::from_secs(secs) {
            eprintln!(
                "WATCHDOG: simulation took {:.3}s, exceeding {}s limit",
                elapsed.as_secs_f64(),
                secs
            );
        }
    }

    // Print the trace.
    sim_ffi::flush_trace();

    // ── If --diff is set, collect trace output and compare ──────
    if let Some(ref expected_path) = diff_path {
        let diff_result = sim_ffi::with_global(|global| {
            if let Some(ref trace) = global.trace {
                let actual = match trace_format {
                    TraceFormat::Jsonl => trace.to_jsonl(),
                    TraceFormat::Human => {
                        if golden_mode {
                            trace
                                .events()
                                .iter()
                                .map(|e| e.to_string())
                                .collect::<Vec<_>>()
                                .join("\n")
                        } else {
                            trace.format()
                        }
                    }
                };
                let expected = std::fs::read_to_string(expected_path).unwrap_or_default();
                let expected = expected.trim_end();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "trace differs from expected file '{}'\n--- expected\n+++ actual\n",
                        expected_path
                    ))
                }
            } else {
                Err("no trace data to compare".to_string())
            }
        });

        match diff_result {
            Ok(()) => {
                if !golden_mode {
                    eprintln!(
                        "=== PASS: Trace matches expected output '{}' ===",
                        expected_path
                    );
                }
            }
            Err(msg) => {
                eprintln!("{}", msg);
                process::exit(1);
            }
        }
    }

    sim_ffi::with_global(|global| {
        if let Some(ref trace) = global.trace {
            match trace_format {
                TraceFormat::Jsonl => {
                    let jsonl = trace.to_jsonl();
                    if !jsonl.is_empty() {
                        println!("{}", jsonl);
                    }
                }
                TraceFormat::Human => {
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
            }
        }
    });
}

#[cfg(any(zephyr_linked, zephyr_cc_kernel))]
fn run_zephyr_real() -> i32 {
    use sim_fiber::{Fiber, ResumeReason, YieldReason};

    // ── C ABI: Zephyr timeout hook globals ──────────────────────
    extern "C" {
        static mut nsi_simu_time: u64;
        /// Delta ticks until next Zephyr timeout (set by sys_clock_set_timeout hook).
        static mut g_rtos_ticks_until_wake: i64;
        /// Calls the kernel's sys_clock_announce() to process expired timeouts.
        fn sim_clock_announce(ticks: i32);
        /// Returns the thread index of the highest-priority ready thread, or -1.
        fn sim_get_ready_thread_id() -> i32;
    }
    const CYCLES_PER_TICK: u64 = 10_000;

    // ── Peripheral event queue ──────────────────────────────────
    //
    // RTOS-agnostic: owned by the costar engine (via sim_ffi), not by
    // any RTOS.  Virtual devices (UART, timer, GPIO) schedule events
    // via the sim_schedule_event() C ABI.  The drain loop checks
    // next_event_deadline() alongside the RTOS timeout queue when
    // deciding how far to advance virtual time.

    let mut boot_fiber = Fiber::new(
        1,
        "zephyr_boot",
        0,
        4096,
        64 * 1024,
        1,
        move |_reason| unsafe {
            posix_boot_cpu();
        },
    );

    let mut sim_time: u64 = 0;
    unsafe {
        nsi_simu_time = sim_time;
        g_rtos_ticks_until_wake = i64::MAX;
    }
    sim_ffi::set_sim_now(sim_time);

    // ── Phase 1: Run the boot fiber ────────────────────────────
    let yielded = {
        update_sim_time(&mut sim_time);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            boot_fiber.resume(ResumeReason::Start)
        }));
        sim_time = unsafe { nsi_simu_time };
        match result {
            Ok(r) => r,
            Err(_) => {
                eprintln!("FATAL: boot fiber panicked");
                return 1;
            }
        }
    };

    if yielded == Some(YieldReason::TaskExit) || yielded.is_none() {
        return 0;
    }

    // ── Phase 2: Multi-fiber drain loop with time advancement ──
    //
    // After each fiber yield, we read g_rtos_ticks_until_wake (set by
    // our sys_clock_set_timeout hook) to find the next Zephyr timeout
    // deadline.  We advance virtual time to that deadline, call
    // sys_clock_announce() to process expired timeouts, which may wake
    // sleeping threads.  Then we resume whichever thread Zephyr's
    // scheduler selected (signaled via nct_swap_threads).
    //
    // When a peripheral event is scheduled sooner than the next Zephyr
    // timeout, we advance to the event deadline instead and dispatch
    // the callback (which may raise IRQs, etc.).

    loop {
        let next_id = crate::zephyr_glue::nct_take_next_to_resume();

        // ── Advance time to next deadline ─────────────────────
        //
        // Check both the RTOS timeout queue and the peripheral event
        // queue.  Advance to whichever deadline is sooner.  This
        // ensures peripherals keep pace with the CPU — the MCU
        // cannot "run ahead" past a pending peripheral event.
        loop {
            let ticks = unsafe { g_rtos_ticks_until_wake };
            let rtos_deadline = if ticks > 0 && ticks != i64::MAX {
                Some(sim_time.saturating_add((ticks as u64) * CYCLES_PER_TICK))
            } else {
                None
            };

            let event_deadline = sim_ffi::next_event_deadline();

            match (rtos_deadline, event_deadline) {
                (None, None) => break,

                (Some(rt), None) => {
                    sim_time = rt;
                    update_sim_time(&mut sim_time);
                    unsafe {
                        sim_clock_announce(ticks as i32);
                        // After processing timeouts, check if a
                        // higher-priority thread became ready and
                        // manually signal the drain loop.
                        let ready_id = sim_get_ready_thread_id();
                        if ready_id >= 0 {
                            crate::zephyr_glue::nct_signal_next(ready_id);
                        }
                    }
                }

                (None, Some(ev)) => {
                    // Only peripheral event pending: advance and dispatch.
                    sim_time = ev;
                    update_sim_time(&mut sim_time);
                    sim_ffi::dispatch_events(sim_time);
                }

                (Some(rt), Some(ev)) if ev <= rt => {
                    // Peripheral event sooner: advance and dispatch.
                    sim_time = ev;
                    update_sim_time(&mut sim_time);
                    sim_ffi::dispatch_events(sim_time);
                    // Don't announce RTOS timeout — it hasn't expired yet.
                    // g_rtos_ticks_until_wake still holds the delta;
                    // next loop iteration will recalculate the deadline.
                }

                (Some(rt), Some(_ev)) => {
                    // RTOS timeout sooner: advance, announce, signal.
                    sim_time = rt;
                    update_sim_time(&mut sim_time);
                    unsafe {
                        sim_clock_announce(ticks as i32);
                        let ready_id = sim_get_ready_thread_id();
                        if ready_id >= 0 {
                            crate::zephyr_glue::nct_signal_next(ready_id);
                        }
                    }
                }
            }
        }

        // ── Handle thread yield / drain ───────────────────────

        if next_id < 0 {
            if !crate::zephyr_glue::nct_has_live_threads() {
                break;
            }
            // No thread signaled and timeouts exhausted — idle spin.
            // Advance by 1 tick to make progress.
            sim_time = sim_time.saturating_add(CYCLES_PER_TICK);
            update_sim_time(&mut sim_time);
            continue;
        }

        let taken = crate::zephyr_glue::nct_take_fiber(next_id);
        let (mut fiber, fiber_idx) = match taken {
            Some(t) => t,
            None => {
                sim_time = sim_time.saturating_add(CYCLES_PER_TICK);
                update_sim_time(&mut sim_time);
                continue;
            }
        };

        update_sim_time(&mut sim_time);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fiber.resume(ResumeReason::SchedulerSelected)
        }));
        sim_time = unsafe { nsi_simu_time };

        match result {
            Ok(Some(YieldReason::RtosPortYield))
            | Ok(Some(YieldReason::Cooperative))
            | Ok(Some(YieldReason::TaskExit)) => {
                crate::zephyr_glue::nct_return_fiber(fiber_idx, fiber);
            }
            Ok(None) => {
                crate::zephyr_glue::nct_return_fiber(fiber_idx, fiber);
            }
            Err(_) => {
                fiber.state = sim_fiber::TaskState::Faulted;
                crate::zephyr_glue::nct_return_fiber(fiber_idx, fiber);
                eprintln!("WARNING: Zephyr thread {} panicked", next_id);
            }
            _ => {
                crate::zephyr_glue::nct_return_fiber(fiber_idx, fiber);
            }
        }
    }
    0
}

#[cfg(any(zephyr_linked, zephyr_cc_kernel))]
fn update_sim_time(sim_time: &mut u64) {
    extern "C" {
        static mut nsi_simu_time: u64;
    }
    unsafe {
        nsi_simu_time = *sim_time;
    }
    sim_ffi::set_sim_now(*sim_time);
}

/// Run a multi-machine simulation from a TOML scenario file.
fn run_scenario(path: &str, golden_mode: bool) -> Result<(), String> {
    use sim_world::Scenario;

    let scenario = Scenario::from_file(path).map_err(|e| e.to_string())?;

    if !golden_mode {
        log::info!(
            "Running scenario '{}' with {} machine(s), {} link(s), {} injection(s)",
            if scenario.name.is_empty() {
                "(unnamed)"
            } else {
                &scenario.name
            },
            scenario.machine.len(),
            scenario.link.len(),
            scenario.inject.len(),
        );
    }

    let result = scenario.run().map_err(|e| e.to_string())?;

    if !golden_mode {
        log::info!(
            "Scenario completed: {} trace event(s), trace_match={}",
            result.trace.len(),
            result.trace_match,
        );
    }

    // Print trace.
    if golden_mode {
        for event in &result.trace {
            println!("{}", event);
        }
    } else {
        println!("=== Scenario: {} ===", scenario.name);
        for event in &result.trace {
            println!("{}", event);
        }
        println!(
            "=== End Scenario Trace ({} events, match={}) ===",
            result.trace.len(),
            result.trace_match
        );
    }

    if !result.trace_match {
        return Err("trace does not match expected golden output".into());
    }

    Ok(())
}
