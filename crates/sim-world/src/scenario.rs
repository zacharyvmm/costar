//! Scenario files — TOML descriptions of multi-machine simulations.
//!
//! A scenario file describes a set of machines, the links connecting them,
//! packet injections at specific times, and optional expected trace output.
//!
//! # Format
//!
//! ```toml
//! name = "two-machine-ping-pong"
//!
//! [[machine]]
//! id = 0
//! name = "sender"
//!
//! [[machine]]
//! id = 1
//! name = "receiver"
//!
//! [[link]]
//! from = 0
//! to = 1
//! latency = 5
//!
//! [[inject]]
//! at = 10
//! link = { from = 0, to = 1 }
//! data = "ping"
//!
//! [expect]
//! trace = "tests/traces/expected_ping_pong.trace"
//! ```
//!
//! # Semantics
//!
//! - Machines are created with IDs and human-readable names.
//! - Links are deterministic FIFO channels with configurable latency.
//! - Injections are packet data sent through a link at a specific virtual time.
//! - The `[expect]` section optionally specifies golden trace comparison.

use std::fmt;

use serde::Deserialize;

use sim_core::SimError;

use crate::link::Link;
use crate::machine::Machine;
use crate::world::World;

// ── TOML representation ───────────────────────────────────────────────────

/// Top-level scenario loaded from a TOML file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Human-readable scenario name.
    #[serde(default)]
    pub name: String,

    /// Machines participating in the simulation.
    #[serde(default)]
    pub machine: Vec<MachineDef>,

    /// Deterministic links between machines.
    #[serde(default)]
    pub link: Vec<LinkDef>,

    /// Packet injections at specific times.
    #[serde(default)]
    pub inject: Vec<InjectDef>,

    /// Expected outcomes (golden trace comparison).
    #[serde(default)]
    pub expect: Option<ExpectDef>,
}

/// A machine definition in a scenario file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineDef {
    /// Unique machine identifier within the scenario.
    pub id: u64,

    /// Human-readable machine name.
    pub name: String,
}

/// A link definition in a scenario file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkDef {
    /// Source machine ID.
    pub from: u64,

    /// Target machine ID.
    pub to: u64,

    /// Delivery latency in ticks (must be ≥ 0).
    pub latency: u64,
}

/// A packet injection definition.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InjectDef {
    /// Virtual time (ticks) at which the packet is sent.
    pub at: u64,

    /// The link to send through, identified by (from, to) pair.
    pub link: LinkEndpointDef,

    /// Packet payload as a string (encoded as UTF-8 bytes).
    pub data: String,
}

/// Identifies a specific link by its endpoint machine IDs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkEndpointDef {
    pub from: u64,
    pub to: u64,
}

/// Expected trace output for golden testing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectDef {
    /// Path to the expected golden trace file.
    pub trace: Option<String>,
}

// ── Error type ────────────────────────────────────────────────────────────

/// Errors that can occur when loading or running a scenario.
#[derive(Debug)]
pub enum ScenarioError {
    /// I/O error reading the scenario file.
    Io(std::io::Error),
    /// TOML parse error.
    Parse(toml::de::Error),
    /// Invalid scenario definition.
    Invalid(String),
    /// Runtime simulation error.
    Sim(SimError),
    /// Golden trace mismatch.
    TraceMismatch {
        expected: String,
        actual: Vec<String>,
    },
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScenarioError::Io(e) => write!(f, "failed to read scenario file: {}", e),
            ScenarioError::Parse(e) => write!(f, "failed to parse scenario: {}", e),
            ScenarioError::Invalid(msg) => write!(f, "invalid scenario: {}", msg),
            ScenarioError::Sim(e) => write!(f, "simulation error: {}", e),
            ScenarioError::TraceMismatch { expected, actual } => {
                write!(
                    f,
                    "trace mismatch: expected {} lines, got {} lines; compare against {}",
                    expected.lines().count(),
                    actual.len(),
                    expected
                )
            }
        }
    }
}

impl From<std::io::Error> for ScenarioError {
    fn from(e: std::io::Error) -> Self {
        ScenarioError::Io(e)
    }
}

impl From<toml::de::Error> for ScenarioError {
    fn from(e: toml::de::Error) -> Self {
        ScenarioError::Parse(e)
    }
}

impl From<SimError> for ScenarioError {
    fn from(e: SimError) -> Self {
        ScenarioError::Sim(e)
    }
}

// ── Scenario execution ────────────────────────────────────────────────────

/// Result of running a scenario.  The trace is interleaved from all machines,
/// sorted by virtual time, with machine-ID prefixes.
#[derive(Debug)]
pub struct ScenarioResult {
    /// Scenario name.
    pub name: String,
    /// Trace events from all machines, prefixed with machine ID.
    pub trace: Vec<String>,
    /// Whether the trace matched the expected golden file.
    pub trace_match: bool,
}

impl Scenario {
    /// Load a scenario from a TOML file path.
    pub fn from_file(path: &str) -> Result<Self, ScenarioError> {
        let content = std::fs::read_to_string(path)?;
        let scenario: Scenario = toml::from_str(&content)?;
        scenario.validate()?;
        Ok(scenario)
    }

    /// Load a scenario from a TOML string (for tests).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(toml_str: &str) -> Result<Self, ScenarioError> {
        let scenario: Scenario = toml::from_str(toml_str)?;
        scenario.validate()?;
        Ok(scenario)
    }

    /// Validate the scenario definition — check for duplicate IDs, missing
    /// link endpoints, injection targets that don't exist, etc.
    fn validate(&self) -> Result<(), ScenarioError> {
        // Check for duplicate machine IDs.
        let mut seen_ids = std::collections::BTreeSet::new();
        for m in &self.machine {
            if !seen_ids.insert(m.id) {
                return Err(ScenarioError::Invalid(format!(
                    "duplicate machine ID {}",
                    m.id
                )));
            }
        }
        if self.machine.is_empty() {
            return Err(ScenarioError::Invalid(
                "scenario must have at least one machine".into(),
            ));
        }

        // Check for duplicate link definitions.
        let mut seen_links = std::collections::BTreeSet::new();
        for l in &self.link {
            let key = (l.from, l.to);
            if !seen_links.insert(key) {
                return Err(ScenarioError::Invalid(format!(
                    "duplicate link from {} to {}",
                    l.from, l.to
                )));
            }
            // Validate endpoint machines exist.
            if !seen_ids.contains(&l.from) {
                return Err(ScenarioError::Invalid(format!(
                    "link from {} references unknown machine",
                    l.from
                )));
            }
            if !seen_ids.contains(&l.to) {
                return Err(ScenarioError::Invalid(format!(
                    "link to {} references unknown machine",
                    l.to
                )));
            }
        }

        // Validate injections reference existing links.
        for inj in &self.inject {
            if !seen_links.contains(&(inj.link.from, inj.link.to)) {
                return Err(ScenarioError::Invalid(format!(
                    "injection references unknown link ({} → {})",
                    inj.link.from, inj.link.to
                )));
            }
        }

        // Validate expected trace file exists if specified.
        if let Some(ref expect) = self.expect {
            if let Some(ref trace_path) = expect.trace {
                if !std::path::Path::new(trace_path).exists() {
                    return Err(ScenarioError::Invalid(format!(
                        "expected trace file not found: {}",
                        trace_path
                    )));
                }
            }
        }

        Ok(())
    }

    /// Build a World from this scenario's machines, links, and injections
    /// without running the simulation.
    ///
    /// The returned World has all machines and links created, and all
    /// pre-loaded injections queued.  The caller can then run the simulation
    /// step-by-step or in full via [`World::run`] or [`World::run_until`].
    pub fn build_world(&self) -> Result<World, ScenarioError> {
        let mut world = World::new();

        for m in &self.machine {
            world.add_machine(Machine::with_defaults(m.id, &m.name));
        }

        for l in &self.link {
            let link = Link::new(l.from, l.to, l.latency);
            world.add_link(link);
        }

        for inj in &self.inject {
            world.inject_packet(inj.link.from, inj.link.to, inj.data.as_bytes(), inj.at);
        }

        Ok(world)
    }

    /// Run the scenario: build the world, pre-load injections, execute,
    /// and optionally compare against expected trace.
    pub fn run(&self) -> Result<ScenarioResult, ScenarioError> {
        // ── Build the World ──────────────────────────────────────
        let mut world = self.build_world()?;

        // ── Run the simulation ───────────────────────────────────
        world.run()?;

        // ── Drain traces ─────────────────────────────────────────
        let trace = world.drain_all_traces();

        // ── Compare against expected trace ───────────────────────
        let trace_match = if let Some(ref expect) = self.expect {
            if let Some(ref trace_path) = expect.trace {
                let expected_content =
                    std::fs::read_to_string(trace_path).map_err(ScenarioError::Io)?;
                let expected_lines: Vec<&str> = expected_content
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect();

                if trace.len() != expected_lines.len() {
                    // Mismatch but not fatal — report result.
                    false
                } else {
                    // Compare line by line.
                    trace.iter().zip(expected_lines.iter()).all(|(a, b)| a == b)
                }
            } else {
                true // No expected trace — always "match".
            }
        } else {
            true // No expect section — always "match".
        };

        Ok(ScenarioResult {
            name: self.name.clone(),
            trace,
            trace_match,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_scenario() {
        let toml_str = r#"
name = "minimal"
[[machine]]
id = 0
name = "m0"
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        assert_eq!(scenario.name, "minimal");
        assert_eq!(scenario.machine.len(), 1);
        assert_eq!(scenario.machine[0].id, 0);
        assert!(scenario.link.is_empty());
        assert!(scenario.inject.is_empty());
    }

    #[test]
    fn test_parse_full_scenario() {
        let toml_str = r#"
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

[[link]]
from = 1
to = 0
latency = 5

[[inject]]
at = 10
link = { from = 0, to = 1 }
data = "ping"

[[inject]]
at = 30
link = { from = 1, to = 0 }
data = "pong"
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        assert_eq!(scenario.name, "ping-pong");
        assert_eq!(scenario.machine.len(), 2);
        assert_eq!(scenario.link.len(), 2);
        assert_eq!(scenario.inject.len(), 2);
        assert!(scenario.expect.is_none());
    }

    #[test]
    fn test_duplicate_machine_id_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"

[[machine]]
id = 0
name = "m0_duplicate"
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("duplicate machine ID"));
    }

    #[test]
    fn test_link_to_unknown_machine_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"

[[link]]
from = 0
to = 99
latency = 5
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown machine"));
    }

    #[test]
    fn test_injection_refs_unknown_link_rejected() {
        let toml_str = r#"
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
link = { from = 1, to = 0 }
data = "test"
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown link"));
    }

    #[test]
    fn test_duplicate_link_rejected() {
        let toml_str = r#"
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

[[link]]
from = 0
to = 1
latency = 10
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate link"));
    }

    #[test]
    fn test_empty_machine_rejected() {
        let toml_str = r#"
name = "empty"
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("at least one machine"));
    }

    #[test]
    fn test_unknown_field_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"
bogus = 42
"#;
        let result: Result<Scenario, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_scenario_run_empty() {
        let toml_str = r#"
name = "empty-run"

[[machine]]
id = 0
name = "m0"
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        let result = scenario.run().unwrap();
        assert_eq!(result.name, "empty-run");
        assert!(result.trace.is_empty());
        assert!(result.trace_match);
    }

    #[test]
    fn test_scenario_run_simple() {
        let toml_str = r#"
name = "simple-run"

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
data = "hello"
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        let result = scenario.run().unwrap();
        assert_eq!(result.name, "simple-run");
        // Link arrival at time 10 + 5 = 15. Machine 1 records a PacketRx.
        // Plus the injection callback at time 10 (priority 30) on m0.
        assert_eq!(result.trace.len(), 1); // Currently the injection is pre-loaded, so we get the PacketRx only
        assert!(result.trace[0].contains("pkt-rx"));
    }
}
