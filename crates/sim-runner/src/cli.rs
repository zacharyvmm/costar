//! CLI argument types, usage printing, and mode listing.

/// Which RTOS backend to use.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RtosBackend {
    /// FreeRTOS (default).
    #[default]
    FreeRtos,
    /// Zephyr (standalone test).
    Zephyr,
}

/// Simulation mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SimMode {
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
    /// Net: virtual Ethernet device loopback demo (Phase 38a).
    Net,
    /// Block: virtual block device demo (Phase 38b).
    Block,
    /// Bt: virtual HCI controller demo (Phase 38c).
    Bt,
    /// TcpEcho: FreeRTOS+TCP echo server/client demo (requires SIM_TCP=1).
    TcpEcho,
    /// Display: exercises virtual display and touch screen via sim_display_* / sim_touch_* ABI.
    Display,
}

/// Trace output format.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TraceFormat {
    /// Human-readable line-oriented format (default, backward-compatible).
    #[default]
    Human,
    /// JSONL — one JSON object per line, self-describing with `"event"` tag.
    Jsonl,
}

/// Default scenario directory relative to the project root.
pub const DEFAULT_SCENARIO_DIR: &str = "tests/scenarios";

pub fn print_usage(prog: &str) {
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
        "  --mode <deterministic|interactive|tight-loop|broader-api|i2c-spi|can|devices|entropy|task-delete|net|block|bt|tcp-echo|display>"
    );
    eprintln!("                              Simulation mode (default: deterministic)");
    eprintln!("  --trace-format <human|jsonl>  Trace output format (default: human)");
    eprintln!("  --scenario <path>           TOML scenario file (multi-machine simulation)");
    eprintln!("  --diff <path>               Compare trace output against expected file");
    eprintln!("  --watchdog <secs>           Wall-clock timeout in seconds (default: none)");
    eprintln!("  --config <path>             TOML configuration file");
    eprintln!("  --board <config.toml>       Board peripheral config (devicetree → devices)");
    eprintln!("  --tap <ifname>              Bridge Ethernet to host TAP interface (e.g., 'tap0')");
    eprintln!("  --verbose                   Enable verbose logging");
    eprintln!("  --symbolicate               Show task names resolved from TaskCreated events");
    eprintln!("  --machine-filter <name>     Filter trace output to only show events from a specific machine");
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
    eprintln!("  --scenario-dir <path>       Set scenario discovery directory");
    eprintln!("  --microcar                  Shorthand for --scenario-dir ../microcar/scenarios");
    eprintln!("  --list                      List discoverable scenario tests");
    eprintln!("  --verbose                   Show PASS/FAIL for each test");
    eprintln!();
    eprintln!("General:");
    eprintln!("  --help, -h                  Show this help message");
    eprintln!("  --version, -V               Show version information");
}

pub fn print_modes() {
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
    println!("  display         Virtual display and touch screen demo");
    println!();
    println!("Use --rtos zephyr for Zephyr backend (standalone hello-thread by default).");
    println!("Use --rtos zephyr --mode broader-api for Zephyr k_sem/k_mutex/k_msgq demo.");
    println!("Zephyr modes require ZEPHYR_BASE for real kernel builds.");
}

pub fn print_version() {
    println!(
        "costar {} (protocol {})",
        env!("CARGO_PKG_VERSION"),
        crate::serve::PROTOCOL_VERSION
    );
}

pub fn print_test_usage() {
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
    eprintln!("  --scenario-dir <path>       Set scenario discovery directory");
    eprintln!("  --microcar                  Shorthand for --scenario-dir ../microcar/scenarios");
    eprintln!("  --list                      List discoverable scenario tests and exit");
    eprintln!("  --no-golden                 Skip golden trace comparison (simulation run only)");
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
    eprintln!("  costar test --microcar --all");
    eprintln!("  costar test --scenario-dir ../microcar/scenarios --all");
    eprintln!("  costar test ping_pong three_chain");
    eprintln!("  costar test --list");
}
