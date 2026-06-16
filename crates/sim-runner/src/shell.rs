//! Interactive monitor shell for costar multi-machine simulations.
//!
//! Provides a REPL for loading scenario files, stepping through
//! simulations, inspecting machine/device/link state, and viewing
//! trace output — similar to Renode's monitor.
//!
//! # Usage
//!
//! ```bash
//! costar shell tests/scenarios/ping_pong.toml
//! ```
//!
//! Once inside the shell:
//! ```text
//! costar> run
//! costar> step 5
//! costar> info
//! costar> trace
//! costar> quit
//! ```

use std::fmt::Write as FmtWrite;
use std::io::{self, Write};

use sim_core::Tick;
use sim_world::{Scenario, World};

/// Run the interactive shell for a scenario file.
///
/// Loads the scenario, builds the world, and enters the REPL loop.
/// Returns `Ok(())` on clean exit, or prints an error and exits the
/// process on failure.
pub fn run_shell(path: &str) {
    // Load the scenario and build the world.
    let scenario = match Scenario::from_file(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("costar shell: failed to load scenario: {}", e);
            std::process::exit(1);
        }
    };

    let mut world = match scenario.build_world() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("costar shell: failed to build world: {}", e);
            std::process::exit(1);
        }
    };

    println!("costar shell — interactive monitor");
    println!("  scenario: {}", scenario.name);
    println!(
        "  machines: {}  links: {}",
        world.machine_count(),
        world.link_count()
    );
    println!("  type 'help' for available commands");
    println!();

    let stdin = io::stdin();
    let mut line = String::new();

    loop {
        print!("costar> ");
        io::stdout().flush().ok();

        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) => {
                // EOF — exit cleanly.
                println!();
                break;
            }
            Ok(_) => {
                // Got input — process below.
            }
            Err(e) => {
                eprintln!("error reading input: {}", e);
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let args: Vec<&str> = parts.collect();

        match cmd {
            "run" | "r" => {
                cmd_run(&mut world);
            }
            "step" | "s" => {
                let n: Tick = args.first().and_then(|a| a.parse().ok()).unwrap_or(1);
                cmd_step(&mut world, n);
            }
            "info" | "i" => {
                cmd_info(&world);
            }
            "machines" | "m" => {
                cmd_machines(&world);
            }
            "links" | "l" => {
                cmd_links(&world);
            }
            "trace" | "t" => {
                cmd_trace(&world);
            }
            "time" => {
                cmd_time(&world);
            }
            "help" | "?" | "h" => {
                cmd_help();
            }
            "quit" | "exit" | "q" => {
                println!("bye.");
                break;
            }
            other => {
                eprintln!(
                    "unknown command: '{}'  (type 'help' for available commands)",
                    other
                );
            }
        }
    }
}

// ── Command implementations ────────────────────────────────────────────

fn cmd_run(world: &mut World) {
    if world.all_idle() {
        println!("simulation is already complete (all machines idle).");
        return;
    }

    print!("running simulation... ");
    io::stdout().flush().ok();

    match world.run() {
        Ok(()) => {
            println!("done at t={}", world.now);
            // Show newly generated trace events.
            let new_traces = world.drain_all_traces();
            if new_traces.is_empty() {
                println!("(no trace events generated)");
            } else {
                println!("{} trace event(s):", new_traces.len());
                for line in &new_traces {
                    println!("  {}", line);
                }
            }
        }
        Err(e) => {
            eprintln!("error: {}", e);
        }
    }
}

fn cmd_step(world: &mut World, n: Tick) {
    if world.all_idle() {
        println!("simulation is already complete (all machines idle).");
        return;
    }

    let before = world.now;
    let target = before.saturating_add(n);
    match world.run_until(target) {
        Ok(()) => {
            if world.now > before {
                println!("advanced to t={}", world.now);
                // Show events that fired.
                let new_traces = world.drain_all_traces();
                if !new_traces.is_empty() {
                    for line in &new_traces {
                        println!("  {}", line);
                    }
                }
            } else {
                // Time didn't advance — no events in the window.
                let next = world
                    .machines()
                    .filter_map(|m| m.next_event_time())
                    .chain(world.links().iter().filter_map(|l| l.next_arrival_time()))
                    .min();
                match next {
                    Some(t) => println!(
                        "no events in [t={}, t={}] — next event at t={}",
                        before, target, t
                    ),
                    None => println!("no events in [t={}, t={}] — done", before, target),
                }
            }
            if world.all_idle() {
                println!("(all machines now idle — simulation complete)");
            }
        }
        Err(e) => {
            eprintln!("error: {}", e);
        }
    }
}

fn cmd_info(world: &World) {
    println!("World state:");
    println!("  virtual time: {}", world.now);
    println!("  machines:     {}", world.machine_count());
    println!("  links:        {}", world.link_count());

    // Show machine details.
    for machine in world.machines() {
        let status = if machine.is_idle() { "idle" } else { "active" };
        let next = machine
            .next_event_time()
            .map_or_else(|| "none".to_string(), |t| format!("t={}", t));
        println!(
            "  [machine.{}] {}  next_event={}  status={}",
            machine.id, machine.name, next, status
        );
    }

    // Show link details.
    for link in world.links() {
        let pending = link.pending_count();
        let next = link
            .next_arrival_time()
            .map_or_else(|| "none".to_string(), |t| format!("t={}", t));
        println!(
            "  [link {}→{}]  latency={}  pending={}  next_arrival={}",
            link.source, link.target, link.latency, pending, next
        );
    }
}

fn cmd_machines(world: &World) {
    if world.machine_count() == 0 {
        println!("(no machines)");
        return;
    }
    for machine in world.machines() {
        let status = if machine.is_idle() { "idle" } else { "active" };
        let next = machine
            .next_event_time()
            .map_or_else(|| "none".to_string(), |t| format!("t={}", t));
        println!(
            "  machine.{}  \"{}\"  status={}  next_event={}",
            machine.id, machine.name, status, next
        );
    }
}

fn cmd_links(world: &World) {
    if world.link_count() == 0 {
        println!("(no links)");
        return;
    }
    for link in world.links() {
        let pending = link.pending_count();
        let next = link
            .next_arrival_time()
            .map_or_else(|| "none".to_string(), |t| format!("t={}", t));
        println!(
            "  link {}→{}  latency={}  pending={}  next_arrival={}",
            link.source, link.target, link.latency, pending, next
        );
    }
}

fn cmd_trace(world: &World) {
    let traces = world.drain_all_traces();
    if traces.is_empty() {
        println!("(no trace events yet — run or step the simulation first)");
        return;
    }
    println!("{} trace event(s):", traces.len());
    for line in &traces {
        println!("  {}", line);
    }
}

fn cmd_time(world: &World) {
    println!("virtual time: {}", world.now);
}

fn cmd_help() {
    let mut help = String::new();
    let _ = writeln!(help, "costar shell commands:");
    let _ = writeln!(help, "  run, r            Run simulation to completion");
    let _ = writeln!(
        help,
        "  step [n], s [n]   Advance virtual time by n ticks (default 1)"
    );
    let _ = writeln!(
        help,
        "  info, i           Show full world state (machines + links)"
    );
    let _ = writeln!(help, "  machines, m       List machines");
    let _ = writeln!(help, "  links, l          List links");
    let _ = writeln!(help, "  trace, t          Show trace events");
    let _ = writeln!(help, "  time              Show current virtual time");
    let _ = writeln!(help, "  help, ?, h        Show this help");
    let _ = writeln!(help, "  quit, exit, q     Exit the shell");
    println!("{}", help);
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create an in-memory scenario and build the world.
    fn build_test_world(toml_str: &str) -> World {
        let scenario = Scenario::from_str(toml_str).unwrap();
        scenario.build_world().unwrap()
    }

    #[test]
    fn test_build_empty_world() {
        let toml = r#"
name = "empty"
[[machine]]
id = 0
name = "m0"
"#;
        let world = build_test_world(toml);
        assert_eq!(world.machine_count(), 1);
        assert_eq!(world.link_count(), 0);
        assert_eq!(world.now, 0);
    }

    #[test]
    fn test_build_world_with_links_and_injections() {
        let toml = r#"
name = "ping-pong"
[[machine]]
id = 0
name = "sender"
[[machine]]
id = 1
name = "receiver"
[[link]]
from = 0
to = 1
latency = 5
[[inject]]
at = 10
link = { from = 0, to = 1 }
data = "ping"
"#;
        let world = build_test_world(toml);
        assert_eq!(world.machine_count(), 2);
        assert_eq!(world.link_count(), 1);

        // Link should have one pending packet arriving at time 15 (10 + 5).
        let links = world.links();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].pending_count(), 1);
        assert_eq!(links[0].next_arrival_time(), Some(15));
    }

    #[test]
    fn test_world_step_by_step() {
        let toml = r#"
name = "stepping"
[[machine]]
id = 0
name = "m0"
[[machine]]
id = 1
name = "m1"
[[link]]
from = 0
to = 1
latency = 5
[[inject]]
at = 10
link = { from = 0, to = 1 }
data = "step-test"
"#;
        let mut world = build_test_world(toml);

        // Initially nothing has happened.
        assert_eq!(world.now, 0);
        assert_eq!(world.drain_all_traces().len(), 0);

        // Step to t=10 — no delivery yet (delivery at t=15).
        world.run_until(10).unwrap();
        // World time stays at 0 because no events fire in [0, 10].
        assert_eq!(world.now, 0);
        let traces = world.drain_all_traces();
        assert_eq!(traces.len(), 0, "no events at t=10");

        // Step to t=20 — link delivery at t=15 fires.
        world.run_until(20).unwrap();
        assert_eq!(world.now, 15);
        let traces = world.drain_all_traces();
        // Link delivery trace: PacketRx at t=15.
        assert_eq!(traces.len(), 1, "expected 1 event (PacketRx at t=15)");
        assert!(traces[0].contains("pkt-rx"));
        assert!(traces[0].contains("9")); // len of "step-test" = 9

        // World is now idle.
        assert!(world.all_idle());
    }

    #[test]
    fn test_world_run_to_completion() {
        let toml = r#"
name = "complete"
[[machine]]
id = 0
name = "m0"
[[machine]]
id = 1
name = "m1"
[[link]]
from = 0
to = 1
latency = 3
[[inject]]
at = 5
link = { from = 0, to = 1 }
data = "done"
"#;
        let mut world = build_test_world(toml);

        world.run().unwrap();
        assert_eq!(world.now, 8); // 5 + 3 = 8
        assert!(world.all_idle());

        let traces = world.drain_all_traces();
        assert_eq!(traces.len(), 1);
        assert!(traces[0].contains("pkt-rx"));
        assert!(traces[0].contains("4")); // len of "done" = 4
    }
}
