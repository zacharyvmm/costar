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

mod cli;
mod config;
mod run;
mod serve;
mod shell;
#[cfg(any(zephyr_linked, zephyr_cc_kernel))]
mod zephyr_glue;

use std::env;
use std::process;

use cli::{print_test_usage, print_usage, print_version, DEFAULT_SCENARIO_DIR};

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
        "run" => run::cmd_run(prog, &args, arg_start),
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

/// Discover scenario TOML files in a directory.
///
/// Returns a sorted vector of (stem, path) pairs where `stem` is the
/// filename without extension (e.g., "ping_pong") and `path` is the
/// full relative path.
fn discover_scenarios_in(dir: &str) -> Vec<(String, String)> {
    let dir_path = std::path::Path::new(dir);
    if !dir_path.is_dir() {
        return vec![];
    }
    let mut scenarios: Vec<(String, String)> = vec![];
    if let Ok(entries) = std::fs::read_dir(dir_path) {
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
    let mut no_golden = false;
    let mut scenario_dir: Option<String> = None;
    let mut scenario_paths: Vec<String> = vec![];

    let mut i = arg_start;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => test_all = true,
            "--list" => list_only = true,
            "--verbose" => verbose = true,
            "--no-golden" => no_golden = true,
            "--microcar" => {
                // Shorthand: discover microcar scenarios in ../microcar/scenarios/.
                // Golden comparison runs by default (scenarios with [expect] sections
                // must have matching expected/traces/*.trace files).
                scenario_dir = Some("../microcar/scenarios".to_string());
            }
            "--scenario-dir" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --scenario-dir requires a path");
                    process::exit(1);
                }
                scenario_dir = Some(args[i].clone());
                // Golden comparison enabled by default; use --no-golden to skip.
            }
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
    let default_scenario_dir = scenario_dir.as_deref().unwrap_or(DEFAULT_SCENARIO_DIR);
    let all_discovered = discover_scenarios_in(default_scenario_dir);

    if list_only {
        if all_discovered.is_empty() {
            println!("No scenario tests found in {}", default_scenario_dir);
        } else {
            println!("Discoverable scenario tests in {}:", default_scenario_dir);
            for (stem, path) in &all_discovered {
                println!("  {}  ({})", stem, path);
            }
        }
        process::exit(0);
    }

    let test_list: Vec<(String, String)> = if test_all {
        if all_discovered.is_empty() {
            eprintln!("error: no scenario tests found in {}", default_scenario_dir);
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
        let result = run_scenario_test(path, label, no_golden);

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
                "CanTx" => {
                    let sender = val["sender"].as_u64().unwrap_or(0);
                    let id = val["id"].as_u64().unwrap_or(0);
                    let len = val["len"].as_u64().unwrap_or(0);
                    format!("{at:>12} can-tx sender={sender} id={id:#06x} len={len}")
                }
                "CanRx" => {
                    let receiver = val["receiver"].as_u64().unwrap_or(0);
                    let id = val["id"].as_u64().unwrap_or(0);
                    let len = val["len"].as_u64().unwrap_or(0);
                    format!("{at:>12} can-rx receiver={receiver} id={id:#06x} len={len}")
                }
                "CanDrop" => {
                    let id = val["id"].as_u64().unwrap_or(0);
                    format!("{at:>12} can-drop id={id:#06x}")
                }
                "CanDelay" => {
                    let id = val["id"].as_u64().unwrap_or(0);
                    let xt = val["extra_ticks"].as_u64().unwrap_or(0);
                    format!("{at:>12} can-delay id={id:#06x} extra_ticks={xt}")
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
fn run_scenario_test(path: &str, _label: &str, no_golden: bool) -> Result<(), String> {
    use sim_world::Scenario;

    let scenario = Scenario::from_file(path).map_err(|e| e.to_string())?;

    // ── Build the World ──────────────────────────────────────
    let mut world = scenario.build_world().map_err(|e| e.to_string())?;

    // ── Attach plant model if configured ─────────────────────
    if let Some(ref plant_def) = scenario.plant {
        match plant_def.plant_type.as_str() {
            #[cfg(feature = "microcar")]
            "microcar" => {
                let tick_ms = plant_def.tick_ms.unwrap_or(10) as u32;
                let plant = microcar_plant::MicrocarPlant::new(tick_ms)
                    .with_machine_id(99)
                    .with_bus("vcan0");
                scenario
                    .attach_plant_to(&mut world, Box::new(plant))
                    .map_err(|e| e.to_string())?;
            }
            #[cfg(not(feature = "microcar"))]
            "microcar" => {
                return Err(
                    "microcar plant support not compiled (enable the 'microcar' feature)"
                        .to_string(),
                );
            }
            _ => { /* unknown plant type — skip */ }
        }
    }

    // ── Schedule faults ─────────────────────────────────
    scenario.schedule_faults_to(&mut world);

    // ── Run the simulation ───────────────────────────────────
    if let Some(duration_ms) = scenario.duration_ms {
        let deadline = duration_ms * 1000;
        world.run_until(deadline).map_err(|e| e.to_string())?;
    } else {
        world.run().map_err(|e| e.to_string())?;
    }

    // ── Drain traces ─────────────────────────────────────────
    let trace = world.drain_all_traces();

    if no_golden {
        // Skip golden trace comparison — just verify the simulation ran.
        if !trace.is_empty() {
            return Ok(()); // Simulation produced trace output — success.
        } else {
            return Err("simulation produced no trace output".to_string());
        }
    }

    let result = scenario.check_trace(trace).map_err(|e| e.to_string())?;

    if !result.trace_match {
        return Err(format!(
            "trace does not match expected golden output ({} events)",
            result.trace.len()
        ));
    }

    // Success — trace matched.
    Ok(())
}

// ── Shared helpers ─────────────────────────────────────────────────────────

/// Run a multi-machine simulation from a TOML scenario file.
fn run_scenario(path: &str, golden_mode: bool, machine_filter: Option<&str>) -> Result<(), String> {
    use sim_world::Scenario;

    let scenario = Scenario::from_file(path).map_err(|e| e.to_string())?;

    // ── Build the World ──────────────────────────────────────
    let mut world = scenario.build_world().map_err(|e| e.to_string())?;

    // ── Attach plant model if configured ─────────────────────
    if let Some(ref plant_def) = scenario.plant {
        match plant_def.plant_type.as_str() {
            #[cfg(feature = "microcar")]
            "microcar" => {
                let tick_ms = plant_def.tick_ms.unwrap_or(10) as u32;
                let plant = microcar_plant::MicrocarPlant::new(tick_ms)
                    .with_machine_id(99) // anonymous plant publisher
                    .with_bus("vcan0");
                scenario
                    .attach_plant_to(&mut world, Box::new(plant))
                    .map_err(|e| e.to_string())?;
            }
            #[cfg(not(feature = "microcar"))]
            "microcar" => {
                return Err(
                    "microcar plant support not compiled (enable the 'microcar' feature)"
                        .to_string(),
                );
            }
            other => {
                log::warn!("unknown plant type '{}' — running without plant", other);
            }
        }
    } else if !scenario.input.is_empty() {
        log::warn!("scenario has [[input]] entries but no [plant] section — inputs ignored");
    }

    // ── Schedule faults ─────────────────────────────────
    scenario.schedule_faults_to(&mut world);

    if !golden_mode {
        log::info!(
            "Running scenario '{}' with {} machine(s), {} link(s), {} injection(s){}{}",
            if scenario.name.is_empty() {
                "(unnamed)"
            } else {
                &scenario.name
            },
            scenario.machine.len(),
            scenario.link.len(),
            scenario.inject.len(),
            if let Some(ref plant) = scenario.plant {
                format!(", plant={}", plant.plant_type)
            } else {
                String::new()
            },
            if !scenario.input.is_empty() {
                format!(", {} input(s)", scenario.input.len())
            } else {
                String::new()
            },
        );
    }

    // ── Run the simulation ───────────────────────────────────
    if let Some(duration_ms) = scenario.duration_ms {
        let deadline = duration_ms * 1000; // ms → µs ticks
        world.run_until(deadline).map_err(|e| e.to_string())?;
    } else {
        world.run().map_err(|e| e.to_string())?;
    }

    // ── Drain traces ─────────────────────────────────────────
    let trace = world.drain_all_traces();

    // ── Compare against expected trace ───────────────────────
    let mut result = scenario.check_trace(trace).map_err(|e| e.to_string())?;

    // ── Filter by machine if requested ─────────────────────────
    if let Some(filter_name) = machine_filter {
        let machine_id = scenario
            .machine
            .iter()
            .find(|m| m.name == filter_name)
            .map(|m| m.id);
        if let Some(id) = machine_id {
            let prefix = format!("[machine.{}]", id);
            let total = result.trace.len();
            result.trace.retain(|line| line.starts_with(&prefix));
            if !golden_mode {
                log::info!(
                    "  machine-filter '{}' (id {}): {} of {} trace events",
                    filter_name,
                    id,
                    result.trace.len(),
                    total,
                );
            }
        } else {
            eprintln!(
                "warning: no machine named '{}' in scenario — showing all traces",
                filter_name
            );
        }
    }

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
