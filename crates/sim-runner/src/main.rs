//! costar — Cooperative Scheduler Testing And Runtime (host executable).
//!
//! Loads a configuration, compiles the guest firmware, spawns the simulator
//! core, and runs the event loop.
//!
//! # Usage
//!
//! ```bash
//! costar                                    # Default deterministic run (FreeRTOS)
//! costar run [OPTIONS]                      # Run a simulation (default subcommand)
//! costar test [SCENARIOS...] [OPTIONS]      # Run scenario tests (headless CI runner)
//! costar test --all                         # Run all discoverable scenario tests
//! costar test --list                        # List discoverable scenario tests
//! costar shell [SCENARIO]                   # Interactive monitor
//! ```

mod config;
mod serve;
mod shell;
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
    // c_sim_interactive_main uses TCP loopback (cross-platform).
    fn c_sim_interactive_main() -> i32;
    fn c_sim_tight_loop_main() -> i32;
    fn c_sim_broader_api_main() -> i32;
    fn c_sim_i2c_spi_main() -> i32;
    fn c_sim_can_main() -> i32;
    fn c_sim_devices_main() -> i32;
    fn c_sim_entropy_main() -> i32;
    fn c_sim_task_delete_main() -> i32;
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
    /// Entropy: deterministic pseudo-random number generator demo.
    Entropy,
    /// TaskDelete: task deletion (vTaskDelete) and static allocation (xTaskCreateStatic) demo.
    TaskDelete,
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
    eprintln!("Usage:");
    eprintln!("  {} [SUBCOMMAND] [OPTIONS]", prog);
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  run [OPTIONS]               Run a simulation (default)");
    eprintln!("  test [SCENARIOS...] [OPTS]  Run scenario tests (headless CI runner)");
    eprintln!("  shell [SCENARIO]            Interactive monitor");
    eprintln!("  replay <trace.jsonl>        Replay a trace file with symbolication");
    eprintln!("  serve [--bind <addr>] [--stdio] [--json] [--session-ttl <secs>]");
    eprintln!("                              Start JSON-RPC 2.0 server");
    eprintln!();
    eprintln!("Run options:");
    eprintln!("  --rtos <freertos|zephyr>   RTOS backend (default: freertos)");
    eprintln!("  --golden                    Machine-readable trace output (no header/footer)");
    eprintln!(
        "  --mode <deterministic|interactive|tight-loop|broader-api|i2c-spi|can|devices|entropy|ztest>"
    );
    eprintln!("                              Simulation mode (default: deterministic)");
    eprintln!("  --trace-format <human|jsonl>  Trace output format (default: human)");
    eprintln!("  --scenario <path>           TOML scenario file (multi-machine simulation)");
    eprintln!("  --diff <path>               Compare trace output against expected file");
    eprintln!("  --watchdog <secs>           Wall-clock timeout in seconds (default: none)");
    eprintln!("  --config <path>             TOML configuration file");
    eprintln!("  --board <config.toml>       Board peripheral config (devicetree → devices)");
    eprintln!("  --verbose                   Enable verbose logging");
    eprintln!("  --symbolicate               Show task names resolved from TaskCreated events");
    eprintln!("  --list-modes                List available simulation modes and exit");
    eprintln!();
    eprintln!("Zephyr app compilation (set before 'cargo build'):");
    eprintln!("  --zephyr-app <path>         External Zephyr app .c file to compile");
    eprintln!("  --zephyr-config <dir>       External config headers directory");
    eprintln!("  --app-sources <glob>        Additional C source files (space-separated)");
    eprintln!("  --app-includes <dir>        Additional include directories (colon-separated)");
    eprintln!("  Note: these print the build-time configuration. Set ZEPHYR_APP_SOURCES,");
    eprintln!("        ZEPHYR_CONFIG_DIR, ZEPHYR_EXTRA_SOURCES, ZEPHYR_APP_INCLUDES env");
    eprintln!("        vars before 'cargo build' to compile an external Zephyr app.");
    eprintln!();
    eprintln!("Test options:");
    eprintln!("  --all                       Run all discoverable scenario tests");
    eprintln!("  --list                      List discoverable scenario tests");
    eprintln!();
    eprintln!("General:");
    eprintln!("  --help, -h                  Show this help message");
    eprintln!("  --version, -V               Show version information");
}

fn print_modes() {
    println!("Available simulation modes:");
    println!("  deterministic   Fully deterministic FreeRTOS demo (queue ping-pong)");
    println!("  interactive     Host I/O demo with TCP loopback (Unix only for poller)");
    println!("  tight-loop      Tier 3 edge-instrumentation demo (CPU-bound + watchdog)");
    println!("  broader-api     FreeRTOS broader API demo (sem/mutex/event-group/notify)");
    println!("  i2c-spi         Virtual I2C and SPI controller demo");
    println!("  can             Virtual CAN bus controller demo");
    println!("  devices         Combined sensor, storage, and fault injection demo");
    println!("  entropy         Virtual entropy source (deterministic RNG) demo");
    println!("  task-delete     Task deletion (vTaskDelete) + static allocation (xTaskCreateStatic) demo");
    println!("  ztest           Zephyr ztest framework demo (requires --rtos zephyr)");
    println!();
    println!("Use --rtos zephyr for Zephyr backend (standalone hello-thread by default).");
    println!("Use --rtos zephyr --mode broader-api for Zephyr k_sem/k_mutex/k_msgq demo.");
    println!("Zephyr modes require ZEPHYR_BASE for real kernel builds.");
}

fn print_version() {
    println!(
        "costar {} (protocol {})",
        env!("CARGO_PKG_VERSION"),
        serve::PROTOCOL_VERSION
    );
}

/// Default scenario directory relative to the project root.
const DEFAULT_SCENARIO_DIR: &str = "tests/scenarios";

fn main() {
    env_logger::try_init().ok();

    let args: Vec<String> = env::args().collect();
    let prog = &args[0];

    // ── Subcommand detection ────────────────────────────────────────
    //
    // The first non-flag positional argument is treated as a subcommand.
    // If none is provided, we default to `run` for backward compatibility.
    let (subcommand, arg_start) = if args.len() > 1 && !args[1].starts_with('-') {
        (args[1].as_str(), 2)
    } else {
        ("run", 1)
    };

    match subcommand {
        "run" => cmd_run(prog, &args, arg_start),
        "test" => cmd_test(&args, arg_start),
        "shell" => {
            // Parse scenario path argument.
            if arg_start >= args.len() {
                eprintln!("error: 'shell' requires a scenario file path");
                eprintln!("usage: {} shell <scenario.toml>", prog);
                process::exit(1);
            }
            let scenario_path = &args[arg_start];
            shell::run_shell(scenario_path);
            process::exit(0);
        }
        "replay" => cmd_replay(&args, arg_start),
        "serve" => cmd_serve(&args, arg_start),
        "help" | "-h" | "--help" => {
            print_usage(prog);
            process::exit(0);
        }
        "--version" | "-V" => {
            print_version();
            process::exit(0);
        }
        other => {
            eprintln!("error: unknown subcommand '{}'", other);
            eprintln!("       use '{} --help' for usage information", prog);
            process::exit(1);
        }
    }
}

// ── `run` subcommand ───────────────────────────────────────────────────────

fn cmd_run(_prog: &str, args: &[String], arg_start: usize) {
    let mut golden_mode = false;
    let mut sim_mode = SimMode::default();
    let mut rtos = RtosBackend::default();
    let mut trace_format = TraceFormat::default();
    let mut watchdog_secs: Option<u64> = None;
    let mut config_path: Option<String> = None;
    let mut scenario_path: Option<String> = None;
    let mut verbose = false;
    let mut symbolicate = false;
    let mut diff_path: Option<String> = None;
    let mut board_path: Option<String> = None;

    let mut i = arg_start;
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
                    "entropy" => SimMode::Entropy,
                    "task-delete" => SimMode::TaskDelete,
                    other => {
                        eprintln!(
                            "error: unknown mode '{}' (expected 'deterministic', 'interactive', 'tight-loop', 'broader-api', 'i2c-spi', 'can', 'devices', 'entropy', 'task-delete', or 'ztest')",
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
            "--board" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --board requires a path to a board config TOML file");
                    process::exit(1);
                }
                board_path = Some(args[i].clone());
            }
            "--verbose" => verbose = true,
            "--symbolicate" => symbolicate = true,
            "--list-modes" => {
                print_modes();
                process::exit(0);
            }
            // Zephyr app compilation flags (build-time, informational at runtime).
            "--zephyr-app" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --zephyr-app requires a path");
                    process::exit(1);
                }
                eprintln!(
                    "note: --zephyr-app is a build-time flag. Set ZEPHYR_APP_SOURCES={:?} before 'cargo build'",
                    args[i]
                );
            }
            "--zephyr-config" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --zephyr-config requires a path");
                    process::exit(1);
                }
                eprintln!(
                    "note: --zephyr-config is a build-time flag. Set ZEPHYR_CONFIG_DIR={:?} before 'cargo build'",
                    args[i]
                );
            }
            "--app-sources" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --app-sources requires a glob pattern");
                    process::exit(1);
                }
                eprintln!(
                    "note: --app-sources is a build-time flag. Set ZEPHYR_EXTRA_SOURCES={:?} before 'cargo build'",
                    args[i]
                );
            }
            "--app-includes" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --app-includes requires a directory");
                    process::exit(1);
                }
                eprintln!(
                    "note: --app-includes is a build-time flag. Set ZEPHYR_APP_INCLUDES={:?} before 'cargo build'",
                    args[i]
                );
            }
            "--help" | "-h" => {
                print_usage(_prog);
                process::exit(0);
            }
            "--version" | "-V" => {
                print_version();
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
            "entropy" => SimMode::Entropy,
            "task-delete" => SimMode::TaskDelete,
            other => {
                eprintln!(
                    "error: invalid mode '{}' in config (expected 'deterministic', 'interactive', 'tight-loop', 'broader-api', 'i2c-spi', 'can', 'devices', 'entropy', 'task-delete', or 'ztest')",
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
        // Print build-time Zephyr app compilation configuration.
        let embedded_app = option_env!("ZEPHYR_APP_SOURCES").unwrap_or("");
        let embedded_config = option_env!("ZEPHYR_CONFIG_DIR").unwrap_or("");
        if !embedded_app.is_empty() {
            log::info!("  zephyr_app: {}", embedded_app);
        }
        if !embedded_config.is_empty() {
            log::info!("  zephyr_config: {}", embedded_config);
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

    // Interactive mode requires host poller (Unix-only — uses std::os::fd).
    #[cfg(windows)]
    if sim_mode == SimMode::Interactive {
        eprintln!(
            "error: interactive mode is not supported on Windows (host poller uses Unix fd types)"
        );
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

    // ── Board peripheral mapping ─────────────────────────────
    // If a board config is provided via --board, initialise virtual
    // devices from the devicetree label → device ID mapping.
    if let Some(ref path) = board_path {
        if !golden_mode {
            log::info!("  board: {}", path);
        }
        match sim_world::BoardConfig::from_file(path) {
            Ok(board_cfg) => {
                let n = board_cfg.initialize_devices();
                if !golden_mode {
                    log::info!("  board: {} peripheral(s) initialised", n);
                }
            }
            Err(e) => {
                eprintln!("error loading board config '{}': {}", path, e);
                process::exit(1);
            }
        }
    }

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
    if sim_mode == SimMode::Entropy {
        sim_devices::entropy_insert(sim_devices::VirtualEntropy::new(0));
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
        (RtosBackend::FreeRtos, SimMode::Interactive) => unsafe { c_sim_interactive_main() },
        (RtosBackend::FreeRtos, SimMode::TightLoop) => unsafe { c_sim_tight_loop_main() },
        (RtosBackend::FreeRtos, SimMode::BroaderApi) => unsafe { c_sim_broader_api_main() },
        (RtosBackend::FreeRtos, SimMode::I2cSpi) => unsafe { c_sim_i2c_spi_main() },
        (RtosBackend::FreeRtos, SimMode::Can) => unsafe { c_sim_can_main() },
        (RtosBackend::FreeRtos, SimMode::Devices) => unsafe { c_sim_devices_main() },
        (RtosBackend::FreeRtos, SimMode::Entropy) => unsafe { c_sim_entropy_main() },
        (RtosBackend::FreeRtos, SimMode::TaskDelete) => unsafe { c_sim_task_delete_main() },
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
                        if symbolicate {
                            // Symbolicated: one line per event with resolved names.
                            // We print format_symbolicated as individual lines.
                            let sym = trace.format_symbolicated();
                            if !sym.is_empty() {
                                println!("{}", sym);
                            }
                        } else {
                            for event in trace.events() {
                                println!("{}", event);
                            }
                        }
                    } else {
                        println!("=== Simulation Trace ===");
                        if symbolicate {
                            let sym = trace.format_symbolicated();
                            if !sym.is_empty() {
                                println!("{}", sym);
                            }
                        } else {
                            for event in trace.events() {
                                println!("{}", event);
                            }
                        }
                        println!("=== End Trace ({} events) ===", trace.len());
                    }
                }
            }
        }
    });
}

// ── `serve` subcommand ─────────────────────────────────────────────────────

fn cmd_serve(args: &[String], arg_start: usize) {
    let mut bind_addr: Option<String> = None;
    let mut stdio_mode = false;
    let mut json_startup = false;
    let mut session_ttl: u64 = 300; // default 5 minutes

    let mut i = arg_start;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --bind requires an address (e.g. 127.0.0.1:9321)");
                    std::process::exit(1);
                }
                bind_addr = Some(args[i].clone());
            }
            "--stdio" => stdio_mode = true,
            "--json" => json_startup = true,
            "--session-ttl" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --session-ttl requires a value in seconds");
                    std::process::exit(1);
                }
                session_ttl = match args[i].parse::<u64>() {
                    Ok(v) if v > 0 => v,
                    _ => {
                        eprintln!("error: --session-ttl must be a positive integer");
                        std::process::exit(1);
                    }
                };
            }
            "--help" | "-h" => {
                eprintln!("Usage: costar serve [OPTIONS]");
                eprintln!();
                eprintln!(
                    "Start a long-lived JSON-RPC 2.0 server for managing simulation sessions."
                );
                eprintln!();
                eprintln!("Options:");
                eprintln!("  --bind <addr>         Listen on TCP address (e.g. 127.0.0.1:9321)");
                eprintln!("  --stdio               Read JSON-RPC from stdin, write to stdout");
                eprintln!("  --json                Print server metadata as JSON on startup");
                eprintln!("  --session-ttl <secs>  Idle session timeout in seconds (default: 300)");
                eprintln!("  --help, -h            Show this help message");
                eprintln!("  --version, -V         Show version information");
                eprintln!();
                eprintln!("By default, listens on 127.0.0.1:9321.");
                std::process::exit(0);
            }
            "--version" | "-V" => {
                print_version();
                std::process::exit(0);
            }
            other => {
                eprintln!("error: unknown option '{}' for serve", other);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    if json_startup {
        let metadata = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "bind": bind_addr.as_deref().unwrap_or("127.0.0.1:9321"),
            "pid": std::process::id(),
            "mode": if stdio_mode { "stdio" } else { "tcp" },
            "session_ttl": session_ttl,
        });
        println!("{}", serde_json::to_string(&metadata).unwrap_or_default());
    }

    let ttl = std::time::Duration::from_secs(session_ttl);

    if stdio_mode {
        serve::run_stdio(ttl);
    } else {
        let addr = bind_addr.as_deref().unwrap_or("127.0.0.1:9321");
        serve::run_bind(addr, ttl);
    }
}

// ── `test` subcommand: headless CI test runner ─────────────────────────────

/// Discover scenario TOML files in the default scenario directory.
///
/// Returns a sorted vector of (stem, path) pairs where `stem` is the
/// filename without extension (e.g., "ping_pong") and `path` is the
/// full relative path.
fn discover_scenarios() -> Vec<(String, String)> {
    let dir = std::path::Path::new(DEFAULT_SCENARIO_DIR);
    if !dir.is_dir() {
        return vec![];
    }
    let mut scenarios: Vec<(String, String)> = vec![];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    scenarios.push((stem.to_string(), path.to_string_lossy().into_owned()));
                }
            }
        }
    }
    scenarios.sort_by(|a, b| a.0.cmp(&b.0));
    scenarios
}

fn cmd_test(args: &[String], arg_start: usize) {
    let mut test_all = false;
    let mut list_only = false;
    let mut verbose = false;
    let mut scenario_paths: Vec<String> = vec![];

    let mut i = arg_start;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => test_all = true,
            "--list" => list_only = true,
            "--verbose" => verbose = true,
            "--help" | "-h" => {
                print_test_usage();
                process::exit(0);
            }
            "--version" | "-V" => {
                print_version();
                process::exit(0);
            }
            other if !other.starts_with('-') => {
                scenario_paths.push(other.to_string());
            }
            other => {
                eprintln!("error: unknown option '{}'", other);
                eprintln!("       use 'costar test --help' for usage information");
                process::exit(1);
            }
        }
        i += 1;
    }

    if verbose {
        log::set_max_level(log::LevelFilter::Debug);
    }

    // ── Collect scenario paths ───────────────────────────────────
    let all_discovered = discover_scenarios();

    if list_only {
        if all_discovered.is_empty() {
            println!("No scenario tests found in {}", DEFAULT_SCENARIO_DIR);
        } else {
            println!("Discoverable scenario tests in {}:", DEFAULT_SCENARIO_DIR);
            for (stem, path) in &all_discovered {
                println!("  {}  ({})", stem, path);
            }
        }
        process::exit(0);
    }

    let test_list: Vec<(String, String)> = if test_all {
        if all_discovered.is_empty() {
            eprintln!("error: no scenario tests found in {}", DEFAULT_SCENARIO_DIR);
            process::exit(1);
        }
        all_discovered
    } else if !scenario_paths.is_empty() {
        // Resolve provided paths — filter out non-existent files.
        let mut resolved: Vec<(String, String)> = vec![];
        for p in &scenario_paths {
            let path = std::path::Path::new(p);
            if !path.exists() {
                // Try appending .toml
                let with_ext = format!("{}.toml", p);
                let alt_path = std::path::Path::new(&with_ext);
                if alt_path.exists() {
                    if let Some(stem) = alt_path.file_stem().and_then(|s| s.to_str()) {
                        resolved.push((stem.to_string(), alt_path.to_string_lossy().into_owned()));
                    }
                    continue;
                }
                eprintln!("error: scenario file not found: {}", p);
                process::exit(1);
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            resolved.push((stem, path.to_string_lossy().into_owned()));
        }
        resolved
    } else if all_discovered.len() == 1 {
        // If exactly one scenario is discoverable, run it by default
        // (convenient for `costar test` with no args).
        all_discovered
    } else {
        // No paths, not --all, and >1 discoverable → list them instead
        // of running all (to be explicit about what's being tested).
        eprintln!("Multiple scenario tests available. Use --all to run all, or specify paths:");
        for (stem, path) in &all_discovered {
            eprintln!("  {}  ({})", stem, path);
        }
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  costar test --all");
        eprintln!("  costar test {}", all_discovered[0].1);
        process::exit(1);
    };

    // ── Run tests ─────────────────────────────────────────────────
    let mut pass: usize = 0;
    let mut fail: usize = 0;

    for (stem, path) in &test_list {
        let label = stem.as_str();
        let result = run_scenario_test(path, label);

        match result {
            Ok(()) => {
                if verbose {
                    eprintln!("  PASS  {}", label);
                }
                pass += 1;
            }
            Err(msg) => {
                if verbose || fail == 0 {
                    eprintln!("  FAIL  {} — {}", label, msg);
                } else {
                    eprintln!("  FAIL  {}", label);
                }
                fail += 1;
            }
        }
    }

    // ── Summary ───────────────────────────────────────────────────
    eprintln!();
    if fail == 0 {
        eprintln!(
            "Test results: {} passed, {} failed — ALL PASSED",
            pass, fail
        );
        process::exit(0);
    } else {
        eprintln!(
            "Test results: {} passed, {} failed — {} FAILED",
            pass, fail, fail
        );
        process::exit(1);
    }
}

// ── `replay` subcommand ─────────────────────────────────────────────────────

/// Replay a JSONL trace file with symbolication.
///
/// Reads a trace file (JSONL or human-readable), resolves task names
/// from `TaskCreated` events, and prints the trace with names.
///
/// Usage: `costar replay <trace.jsonl> [--step]`
fn cmd_replay(args: &[String], arg_start: usize) {
    let mut step_mode = false;

    // Parse flags.
    let mut path: Option<&str> = None;
    let mut i = arg_start;
    while i < args.len() {
        match args[i].as_str() {
            "--step" | "-s" => step_mode = true,
            "--help" | "-h" => {
                eprintln!("Usage: costar replay <trace.jsonl> [--step]");
                eprintln!();
                eprintln!("  Reads a trace file and prints it with task names resolved");
                eprintln!("  from TaskCreated events.");
                eprintln!();
                eprintln!("  --step, -s    Step through events one at a time (press Enter)");
                process::exit(0);
            }
            other if !other.starts_with('-') => {
                path = Some(other);
            }
            other => {
                eprintln!("error: unknown option '{}'", other);
                process::exit(1);
            }
        }
        i += 1;
    }

    let path = match path {
        Some(p) => p,
        None => {
            eprintln!("error: 'replay' requires a trace file path");
            eprintln!("usage: costar replay <trace.jsonl>");
            process::exit(1);
        }
    };

    // Read and parse the trace file.
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read '{}': {}", path, e);
            process::exit(1);
        }
    };

    // Detect format: JSONL starts with '{', human format is plain text.
    if content.trim_start().starts_with('{') {
        // JSONL format — parse each line as a JSON value and format directly.
        // We can't deserialize into TraceEvent because TraceEvent uses
        // &'static str which requires lifetime guarantees.  Instead, we
        // parse into serde_json::Value and format each line.
        let mut task_names: std::collections::BTreeMap<u64, String> =
            std::collections::BTreeMap::new();

        if step_mode {
            let mut stdin = std::io::stdin().lock();
            use std::io::BufRead;
            let line_count = content.lines().filter(|l| !l.trim().is_empty()).count();
            println!(
                "Trace replay — {} events (press Enter to step, 'q' to quit)",
                line_count
            );
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let formatted = format_jsonl_line(line, &mut task_names);
                println!("{}", formatted);
                let mut buf = String::new();
                let _ = stdin.read_line(&mut buf);
                if buf.trim() == "q" {
                    println!("(stopped)");
                    break;
                }
            }
        } else {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let formatted = format_jsonl_line(line, &mut task_names);
                println!("{}", formatted);
            }
        }
    } else {
        // Human-readable format — we can't parse it back to events.
        // Just print it as-is.
        eprintln!("Replaying human-format trace (as-is, no parsing)");
        if step_mode {
            let mut stdin = std::io::stdin().lock();
            use std::io::BufRead;
            for line in content.lines() {
                println!("{}", line);
                let mut buf = String::new();
                let _ = stdin.read_line(&mut buf);
                if buf.trim() == "q" {
                    break;
                }
            }
        } else {
            println!("{}", content);
        }
        process::exit(0);
    }
}

/// Format a single JSONL trace line, resolving task names if known.
///
/// Task names are discovered from `TaskCreated` events and stored in
/// `task_names`.  TaskResume/TaskYield events use the resolved name.
fn format_jsonl_line(
    line: &str,
    task_names: &mut std::collections::BTreeMap<u64, String>,
) -> String {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(val) => {
            let event_type = val["event"].as_str().unwrap_or("?");
            let at = val["at"].as_u64().unwrap_or(0);

            // Track TaskCreated events for symbol resolution.
            if event_type == "TaskCreated" {
                if let (Some(id), Some(name)) = (val["task"].as_u64(), val["name"].as_str()) {
                    task_names.insert(id, name.to_string());
                }
            }

            // Format TaskResume with resolved name.
            if event_type == "TaskResume" {
                if let Some(task) = val["task"].as_u64() {
                    let reason = val["reason"].as_str().unwrap_or("?");
                    if let Some(name) = task_names.get(&task) {
                        return format!(
                            "{at:>12} task-resume id={task} name=\"{name}\" reason={reason}"
                        );
                    } else {
                        return format!("{at:>12} task-resume id={task} reason={reason}");
                    }
                }
            }

            // Format TaskYield with resolved name.
            if event_type == "TaskYield" {
                if let Some(task) = val["task"].as_u64() {
                    let reason = val["reason"].as_str().unwrap_or("?");
                    if let Some(name) = task_names.get(&task) {
                        return format!(
                            "{at:>12} task-yield id={task} name=\"{name}\" reason={reason}"
                        );
                    } else {
                        return format!("{at:>12} task-yield id={task} reason={reason}");
                    }
                }
            }

            // For other event types, reconstruct a human-readable line.
            match event_type {
                "EventScheduled" => {
                    let id = val["id"].as_u64().unwrap_or(0);
                    let pri = val["priority"].as_u64().unwrap_or(0);
                    let label = val["label"].as_str().unwrap_or("?");
                    let target = val["target_at"].as_u64().unwrap_or(0);
                    format!("{at:>12} schedule id={id} pri={pri} \"{label}\" target={target}")
                }
                "EventDispatched" => {
                    let id = val["id"].as_u64().unwrap_or(0);
                    let label = val["label"].as_str().unwrap_or("?");
                    format!("{at:>12} dispatch id={id} \"{label}\"")
                }
                "EventCancelled" => {
                    let id = val["id"].as_u64().unwrap_or(0);
                    format!("{at:>12} cancel id={id}")
                }
                "InterruptRaised" => {
                    let irq = val["irq"].as_u64().unwrap_or(0);
                    format!("{at:>12} irq-raised irq={irq}")
                }
                "InterruptDelivered" => {
                    let irq = val["irq"].as_u64().unwrap_or(0);
                    format!("{at:>12} irq-delivered irq={irq}")
                }
                "PacketRx" => {
                    let len = val["len"].as_u64().unwrap_or(0);
                    format!("{at:>12} pkt-rx len={len}")
                }
                "PacketTx" => {
                    let len = val["len"].as_u64().unwrap_or(0);
                    format!("{at:>12} pkt-tx len={len}")
                }
                "Fatal" => {
                    let code = val["code"].as_str().unwrap_or("?");
                    format!("{at:>12} FATAL code={code}")
                }
                "UserU32" => {
                    let label = val["label"].as_str().unwrap_or("?");
                    let value = val["value"].as_u64().unwrap_or(0);
                    format!("{at:>12} user-u32 \"{label}\" = {value}")
                }
                "TaskCreated" => {
                    let task = val["task"].as_u64().unwrap_or(0);
                    let name = val["name"].as_str().unwrap_or("?");
                    format!("{at:>12} task-created id={task} name=\"{name}\"")
                }
                _ => line.to_string(),
            }
        }
        Err(_) => format!("(parse error) {}", line),
    }
}

/// Run a single scenario test: load, run, and compare against expected trace.
fn run_scenario_test(path: &str, label: &str) -> Result<(), String> {
    use sim_world::Scenario;

    let scenario = Scenario::from_file(path).map_err(|e| e.to_string())?;
    let result = scenario.run().map_err(|e| e.to_string())?;

    if !result.trace_match {
        return Err(format!(
            "trace does not match expected golden output ({} events)",
            result.trace.len()
        ));
    }

    // Success — trace matched.
    let _ = label; // used by caller for reporting
    Ok(())
}

fn print_test_usage() {
    eprintln!("Usage: costar test [SCENARIOS...] [OPTIONS]");
    eprintln!();
    eprintln!("Run scenario tests with automatic golden trace comparison.");
    eprintln!();
    eprintln!("Arguments:");
    eprintln!("  [SCENARIOS...]             One or more scenario TOML files to test");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --all                       Run all discoverable scenario tests");
    eprintln!(
        "                                (scans {})",
        DEFAULT_SCENARIO_DIR
    );
    eprintln!("  --list                      List discoverable scenario tests and exit");
    eprintln!("  --verbose                   Show PASS/FAIL for each test");
    eprintln!("  --help, -h                  Show this help message");
    eprintln!();
    eprintln!("Exit codes:");
    eprintln!("  0   All tests passed");
    eprintln!("  1   One or more tests failed (or scenario file not found)");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  costar test tests/scenarios/ping_pong.toml");
    eprintln!("  costar test --all");
    eprintln!("  costar test ping_pong three_chain");
    eprintln!("  costar test --list");
}

// ── Shared helpers ─────────────────────────────────────────────────────────

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

// ── Zephyr real kernel run loop ────────────────────────────────────────────

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
