//! Scenario files — TOML descriptions of multi-machine simulations.
//!
//! A scenario file describes a set of machines, the links/buses connecting them,
//! plant models, timed inputs, fault injections, and optional expected trace
//! output.
//!
//! # Core format
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
//! # Microcar extensions
//!
//! The format also supports bus-based topologies, plant models, driver inputs,
//! fault injection, and expected event assertions:
//!
//! ```toml
//! [[bus]]
//! name = "vcan0"
//! type = "can"
//! latency_us = 500
//!
//! [[bus.node]]
//! bus = "vcan0"
//! machine = "gateway"
//!
//! [plant]
//! type = "microcar"
//! tick_ms = 10
//!
//! [[input]]
//! at_ms = 500
//! type = "driver_input"
//! throttle_percent = 30
//! brake_pressed = false
//!
//! [[fault]]
//! at_ms = 1200
//! target = "plant.battery"
//! type = "force_temperature"
//! value_c = 82
//!
//! [[expect.event]]
//! before_ms = 1500
//! machine = "gateway"
//! event = "vehicle_mode"
//! value = "LIMP"
//! ```
//!
//! # Semantics
//!
//! - Machines are created with IDs and human-readable names.
//! - Links are deterministic FIFO channels with configurable latency.
//! - Buses are N*(N-1) point-to-point FIFO links between all attached nodes.
//! - Plant, input, fault, and expect.event are parsed but are currently
//!   informational — the simulation runner reports them but does not act on
//!   them (firmware integration is needed for full semantics).
//! - Injections are packet data sent through a link at a specific virtual time.
//! - The `[expect]` section optionally specifies golden trace comparison.

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;

use sim_core::SimError;

use crate::canbus::CanBus;
use crate::link::Link;
use crate::machine::Machine;
use crate::plant::EnvironmentModel;
use crate::world::World;

// ── TOML representation ───────────────────────────────────────────────────

/// Top-level scenario loaded from a TOML file.
///
/// Note: `#[serde(deny_unknown_fields)]` is not used here because TOML's
/// `[[bus.node]]` and `[[expect.event]]` array-of-tables syntax creates
/// intermediate table keys that serde processes as part of the bus/expect
/// deserialization rather than as separate top-level fields.
#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    /// Human-readable scenario name.
    #[serde(default)]
    pub name: String,

    /// Simulation duration in milliseconds (microcar extension).
    #[serde(default)]
    pub duration_ms: Option<u64>,

    /// Machines participating in the simulation.
    #[serde(default)]
    pub machine: Vec<MachineDef>,

    /// Deterministic links between machines.
    #[serde(default)]
    pub link: Vec<LinkDef>,

    /// Bus definitions (CAN-like broadcast).
    #[serde(default)]
    pub bus: Vec<BusDef>,

    /// Plant/environment model configuration.
    #[serde(default)]
    pub plant: Option<PlantDef>,

    /// Timed driver/sensor inputs.
    #[serde(default)]
    pub input: Vec<InputDef>,

    /// Timed fault injections.
    #[serde(default)]
    pub fault: Vec<FaultDef>,

    /// Packet injections at specific times.
    #[serde(default)]
    pub inject: Vec<InjectDef>,

    /// CAN bus frame injections at specific times (microcar extension).
    #[serde(default)]
    pub bus_inject: Vec<BusInjectDef>,

    /// Expected outcomes (golden trace comparison).
    #[serde(default)]
    pub expect: Option<ExpectDef>,

    /// Base directory of the scenario file (set by from_file, not from TOML).
    #[serde(skip)]
    pub base_dir: Option<std::path::PathBuf>,
}

/// A machine definition in a scenario file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineDef {
    /// Unique machine identifier within the scenario.
    pub id: u64,

    /// Human-readable machine name.
    pub name: String,

    /// Path to firmware binary (microcar extension).
    #[serde(default)]
    pub firmware: Option<String>,

    /// RTOS backend: "freertos" or "zephyr" (microcar extension).
    #[serde(default)]
    pub rtos: Option<String>,
}

/// A bus definition (CAN-like broadcast topology).
///
/// A bus is a named communication channel.  Nodes attached to a bus
/// (via [[bus.node]]) are connected with N*(N-1) point-to-point FIFO
/// links so every node receives messages from every other node.
///
/// In TOML, `[[bus]]` entries have `name`, `type`, and `latency_us`.
/// `[[bus.node]]` entries appear as additional entries in the `bus`
/// array with only a `node` sub-table — they are distinguished post-parse.
#[derive(Debug, Clone, Deserialize)]
pub struct BusDef {
    /// Human-readable bus name (set for [[bus]] entries, None for [[bus.node]]).
    #[serde(default)]
    pub name: Option<String>,

    /// Bus type: "can", "lin", "flexray", etc.
    #[serde(default, rename = "type")]
    pub bus_type: Option<String>,

    /// Per-message delivery latency in microseconds.
    #[serde(default)]
    pub latency_us: Option<u64>,

    /// Bus node entries (set for [[bus.node]] entries nested under [[bus]]).
    #[serde(default)]
    pub node: Vec<BusNodeDef>,
}

/// Associates a machine with a bus.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusNodeDef {
    /// Name of the bus to attach to.
    pub bus: String,

    /// Name of the machine to attach.
    pub machine: String,
}

/// Plant/environment model configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlantDef {
    /// Plant model type (e.g. "microcar").
    #[serde(rename = "type")]
    pub plant_type: String,

    /// Plant tick interval in milliseconds.
    #[serde(default)]
    pub tick_ms: Option<u64>,
}

/// A timed driver or sensor input.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputDef {
    /// Virtual time (milliseconds) at which the input is applied.
    pub at_ms: u64,

    /// Input type: "driver_input", "sensor_reading", etc.
    #[serde(rename = "type")]
    pub input_type: String,

    /// Throttle position in percent (0-100).
    #[serde(default)]
    pub throttle_percent: Option<u8>,

    /// Whether the brake pedal is pressed.
    #[serde(default)]
    pub brake_pressed: Option<bool>,
}

/// A timed fault injection.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultDef {
    /// Virtual time (milliseconds) at which the fault is triggered.
    pub at_ms: u64,

    /// Target of the fault (e.g. "plant.battery", "machine.gateway", "bus.vcan0").
    pub target: String,

    /// Fault type: "force_temperature", "stop_heartbeat", "reboot",
    /// "drop_frame", "delay_frame".
    #[serde(rename = "type")]
    pub fault_type: String,

    /// Temperature value in Celsius (for force_temperature faults).
    #[serde(default)]
    pub value_c: Option<u32>,

    /// Delay in milliseconds (for delay_frame faults).
    #[serde(default)]
    pub delay_ms: Option<u64>,

    /// CAN frame ID (for drop_frame / delay_frame faults).
    #[serde(default)]
    pub id: Option<u32>,
}

/// An expected event assertion — the simulation should produce this event.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectEventDef {
    /// The event must occur before this virtual time (milliseconds).
    pub before_ms: u64,

    /// Name of the machine that should produce the event.
    pub machine: String,

    /// Event type string (e.g. "node_online", "vehicle_mode", "motor_command").
    pub event: String,

    /// Expected event value (optional).
    #[serde(default)]
    pub value: Option<String>,

    /// Expected maximum percentage (optional, for torque_limited events).
    #[serde(default)]
    pub max_percent: Option<u8>,

    /// Expected node name (optional, for node_online / node_lost events).
    #[serde(default)]
    pub node: Option<String>,

    /// Expected torque value (optional, for motor_command events).
    #[serde(default)]
    pub torque: Option<i32>,
}

/// A scenario-level assertion.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertDef {
    /// Human-readable assertion name.
    #[serde(default)]
    pub name: String,

    /// Condition that must always hold.
    #[serde(default)]
    pub always_when: Option<String>,

    /// Condition that must be true.
    #[serde(default)]
    pub condition: Option<String>,

    /// Event that triggers the assertion check.
    #[serde(default)]
    pub after_event: Option<String>,

    /// Time window for the assertion (milliseconds).
    #[serde(default)]
    pub within_ms: Option<u64>,

    /// Event name for the assertion.
    #[serde(default)]
    pub event: Option<String>,
}

/// A link definition in a scenario file.
///
/// Supports two link types:
///
/// - `type = "fifo"` (default): generic packet FIFO with fixed latency.
/// - `type = "uart"`: per-byte UART serial link at a given baud rate.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkDef {
    /// Link type: `"fifo"` (default) or `"uart"`.
    #[serde(default = "default_link_type", rename = "type")]
    pub link_type: String,

    /// Source machine ID.
    pub from: u64,

    /// Target machine ID.
    pub to: u64,

    // ── Fifo-specific fields ──────────────────────────────────────
    /// Delivery latency in ticks (must be ≥ 0).  Required for fifo links.
    #[serde(default)]
    pub latency: Option<u64>,

    // ── UART-specific fields ──────────────────────────────────────
    /// Baud rate (e.g. 115200).  Required for uart links.
    #[serde(default)]
    pub baud: Option<u32>,

    /// Data bits per frame (typically 8).  Default: 8.
    #[serde(default)]
    pub data_bits: Option<u8>,

    /// Parity: 'N' (none, default), 'E' (even), 'O' (odd).
    #[serde(default)]
    pub parity: Option<char>,

    /// Stop bits (typically 1).  Default: 1.
    #[serde(default)]
    pub stop_bits: Option<u8>,

    /// Simulation tick rate in Hz (e.g. 1_000_000 for 1 µs ticks).
    /// Default: 1_000_000.
    #[serde(default)]
    pub tick_rate_hz: Option<u64>,
}

fn default_link_type() -> String {
    "fifo".to_string()
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

/// A CAN bus frame injection definition (microcar extension).
///
/// Injects a raw CAN frame onto a named bus at a specific virtual time.
/// The frame is delivered to all attached nodes except the sender after
/// the bus's configured latency.
///
/// # TOML format
///
/// ```toml
/// [[bus_inject]]
/// at_ms = 50
/// bus = "vcan0"
/// sender = "gateway"
/// id = 0x001
/// data = [1, 50, 0, 0, 0]
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusInjectDef {
    /// Virtual time (milliseconds) at which the frame is sent.
    pub at_ms: u64,

    /// Name of the bus to send the frame on.
    pub bus: String,

    /// Name of the sending machine.
    pub sender: String,

    /// CAN frame identifier.
    pub id: u32,

    /// Frame payload as a list of byte values.
    pub data: Vec<u8>,
}

/// Expected trace output for golden testing.
///
/// In TOML, `[expect]` is a table with optional `trace` and `[[expect.event]]`
/// entries collected as the `event` array.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectDef {
    /// Path to the expected golden trace file.
    pub trace: Option<String>,

    /// Expected events (from [[expect.event]] entries).
    #[serde(default)]
    pub event: Vec<ExpectEventDef>,
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
        let mut scenario = Self::parse_scenario(&content)?;
        // Resolve expect.trace relative to the scenario file's directory.
        let base_dir = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new("."));
        scenario.base_dir = Some(base_dir.to_path_buf());
        scenario.validate_with_base(base_dir)?;
        Ok(scenario)
    }

    /// Load a scenario from a TOML string (for tests).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(toml_str: &str) -> Result<Self, ScenarioError> {
        let scenario = Self::parse_scenario(toml_str)?;
        scenario.validate_with_base(std::path::Path::new("."))?;
        Ok(scenario)
    }

    /// Parse a scenario TOML string directly via serde.
    fn parse_scenario(content: &str) -> Result<Self, ScenarioError> {
        let scenario: Scenario = toml::from_str(content)?;
        Ok(scenario)
    }

    /// Validate the scenario definition — check for duplicate IDs, missing
    /// link endpoints, injection targets that don't exist, bus topology, etc.
    ///
    /// `base_dir` is the directory containing the scenario file, used to
    /// resolve relative paths in `[expect].trace`.
    fn validate_with_base(&self, base_dir: &std::path::Path) -> Result<(), ScenarioError> {
        // Check for duplicate machine IDs.
        let mut seen_ids = BTreeSet::new();
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

        // Build name→id lookup for bus node validation.
        let mut name_to_id: std::collections::BTreeMap<&str, u64> =
            std::collections::BTreeMap::new();
        for m in &self.machine {
            name_to_id.insert(&m.name, m.id);
        }

        // Separate bus definitions from bus node entries.
        // [[bus]] entries have name set; [[bus.node]] entries have node set.
        let mut bus_defs: Vec<(&str, &str, u64)> = Vec::new(); // (name, type, latency_us)
        let mut bus_node_entries: Vec<&BusNodeDef> = Vec::new();
        let mut seen_bus_names = BTreeSet::new();

        for entry in &self.bus {
            // Collect [[bus.node]] entries from this bus definition.
            for bn in &entry.node {
                bus_node_entries.push(bn);
            }
            if let (Some(ref name), Some(ref bt), Some(lat)) =
                (&entry.name, &entry.bus_type, entry.latency_us)
            {
                // This is a [[bus]] definition.
                if !seen_bus_names.insert(name.as_str()) {
                    return Err(ScenarioError::Invalid(format!(
                        "duplicate bus name '{}'",
                        name
                    )));
                }
                if lat == 0 {
                    return Err(ScenarioError::Invalid(format!(
                        "bus '{}' has zero latency_us",
                        name
                    )));
                }
                bus_defs.push((name, bt, lat));
            } else {
                return Err(ScenarioError::Invalid(
                    "bus entry must be a [[bus]] definition (with name, type, latency_us) \
                     or a [[bus.node]] entry (with bus, machine)"
                        .into(),
                ));
            }
        }

        // Validate bus.node entries: bus must exist, machine must exist.
        for bn in &bus_node_entries {
            if !seen_bus_names.contains(bn.bus.as_str()) {
                return Err(ScenarioError::Invalid(format!(
                    "bus.node references unknown bus '{}'",
                    bn.bus
                )));
            }
            if !name_to_id.contains_key(bn.machine.as_str()) {
                return Err(ScenarioError::Invalid(format!(
                    "bus.node references unknown machine '{}'",
                    bn.machine
                )));
            }
        }

        // Validate expect.event machines exist.
        if let Some(ref expect) = self.expect {
            for ee in &expect.event {
                if !name_to_id.contains_key(ee.machine.as_str()) {
                    return Err(ScenarioError::Invalid(format!(
                        "expect.event references unknown machine '{}'",
                        ee.machine
                    )));
                }
            }
        }

        // Validate fault targets: "plant.xxx", "machine.xxx", "bus.xxx"
        for f in &self.fault {
            let parts: Vec<&str> = f.target.splitn(2, '.').collect();
            if parts.len() != 2 {
                return Err(ScenarioError::Invalid(format!(
                    "fault target '{}' must be in 'domain.name' format",
                    f.target
                )));
            }
            match parts[0] {
                "machine" => {
                    if !name_to_id.contains_key(parts[1]) {
                        return Err(ScenarioError::Invalid(format!(
                            "fault references unknown machine '{}'",
                            parts[1]
                        )));
                    }
                }
                "bus" => {
                    if !seen_bus_names.contains(parts[1]) {
                        return Err(ScenarioError::Invalid(format!(
                            "fault references unknown bus '{}'",
                            parts[1]
                        )));
                    }
                }
                "plant" => {
                    // plant targets are validated at runtime — the plant model
                    // knows its own subcomponents.
                }
                _ => {
                    return Err(ScenarioError::Invalid(format!(
                        "unknown fault target domain '{}' (expected 'machine', 'bus', or 'plant')",
                        parts[0]
                    )));
                }
            }
        }

        // Check for duplicate link definitions.
        let mut seen_links = BTreeSet::new();
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
            // Validate link-type-specific fields.
            match l.link_type.as_str() {
                "fifo" => {
                    if l.latency.is_none() {
                        return Err(ScenarioError::Invalid(
                            "fifo link requires 'latency' field".into(),
                        ));
                    }
                    if l.baud.is_some()
                        || l.data_bits.is_some()
                        || l.parity.is_some()
                        || l.stop_bits.is_some()
                    {
                        return Err(ScenarioError::Invalid(
                            "fifo link must not have UART fields (baud, data_bits, parity, stop_bits)"
                                .into(),
                        ));
                    }
                }
                "uart" => {
                    if l.baud.is_none() {
                        return Err(ScenarioError::Invalid(
                            "uart link requires 'baud' field".into(),
                        ));
                    }
                    if l.latency.is_some() {
                        return Err(ScenarioError::Invalid(
                            "uart link must not have 'latency' field (use tick_rate_hz instead)"
                                .into(),
                        ));
                    }
                    if let Some(p) = l.parity {
                        if p != 'N' && p != 'E' && p != 'O' {
                            return Err(ScenarioError::Invalid(format!(
                                "invalid parity '{}': must be N, E, or O",
                                p
                            )));
                        }
                    }
                }
                other => {
                    return Err(ScenarioError::Invalid(format!(
                        "unknown link type '{}': must be 'fifo' or 'uart'",
                        other
                    )));
                }
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
                let resolved = base_dir.join(trace_path);
                if !resolved.exists() {
                    return Err(ScenarioError::Invalid(format!(
                        "expected trace file not found: {} (resolved from base {})",
                        trace_path,
                        base_dir.display()
                    )));
                }
            }
        }

        // Validate bus_inject: sender machine must exist, bus must exist.
        for bi in &self.bus_inject {
            if !name_to_id.contains_key(bi.sender.as_str()) {
                return Err(ScenarioError::Invalid(format!(
                    "bus_inject references unknown sender machine '{}'",
                    bi.sender
                )));
            }
            if !seen_bus_names.contains(bi.bus.as_str()) {
                return Err(ScenarioError::Invalid(format!(
                    "bus_inject references unknown bus '{}'",
                    bi.bus
                )));
            }
        }

        Ok(())
    }

    /// Build a World from this scenario's machines, links, buses, and injections
    /// without running the simulation.
    ///
    /// The returned World has all machines and links created.  Bus topology
    /// creates [`CanBus`] instances instead of N*(N-1) point-to-point links,
    /// providing true broadcast semantics with sender exclusion.
    /// All pre-loaded injections and bus_inject entries are queued.
    /// The caller can then run the simulation step-by-step or in full
    /// via [`World::run`] or [`World::run_until`].
    pub fn build_world(&self) -> Result<World, ScenarioError> {
        let mut world = World::new();

        for m in &self.machine {
            let rtos = m.rtos.as_deref().unwrap_or("freertos");
            world.add_machine(Machine::with_rtos(m.id, &m.name, rtos));
        }

        // Build name→id lookup for bus topology.
        let name_to_id: std::collections::BTreeMap<&str, u64> = self
            .machine
            .iter()
            .map(|m| (m.name.as_str(), m.id))
            .collect();

        // Build bus name→(latency, nodes) from unified bus array.
        let mut bus_latency: std::collections::BTreeMap<&str, u64> =
            std::collections::BTreeMap::new();
        let mut bus_nodes: std::collections::BTreeMap<&str, Vec<u64>> =
            std::collections::BTreeMap::new();

        for entry in &self.bus {
            // Collect [[bus.node]] entries from this bus definition.
            for bn in &entry.node {
                if let Some(&machine_id) = name_to_id.get(bn.machine.as_str()) {
                    bus_nodes
                        .entry(bn.bus.as_str())
                        .or_default()
                        .push(machine_id);
                }
            }
            if let (Some(ref name), Some(lat)) = (&entry.name, entry.latency_us) {
                // [[bus]] definition
                bus_latency.insert(name, lat);
            }
        }

        // Create CanBus instances for each bus and attach nodes.
        for (bus_name, node_ids) in &bus_nodes {
            let latency = bus_latency.get(bus_name).copied().unwrap_or(500);
            let mut can_bus = CanBus::new(bus_name, latency);
            for &machine_id in node_ids {
                can_bus.attach(machine_id);
            }
            world.add_bus(can_bus);
        }

        // Add explicit point-to-point links.
        for l in &self.link {
            let link = match l.link_type.as_str() {
                "fifo" => {
                    let latency = l.latency.unwrap_or(0);
                    Link::new_fifo(l.from, l.to, latency)
                }
                "uart" => {
                    let baud = l.baud.unwrap_or(115200);
                    let data_bits = l.data_bits.unwrap_or(8);
                    let parity = l.parity.unwrap_or('N');
                    let stop_bits = l.stop_bits.unwrap_or(1);
                    let tick_rate_hz = l.tick_rate_hz.unwrap_or(1_000_000);
                    Link::new_uart(
                        l.from,
                        l.to,
                        baud,
                        data_bits,
                        parity,
                        stop_bits,
                        tick_rate_hz,
                    )
                }
                _ => {
                    return Err(ScenarioError::Invalid(format!(
                        "unknown link type '{}'",
                        l.link_type
                    )));
                }
            };
            world.add_link(link);
        }

        // Pre-load link packet injections.
        for inj in &self.inject {
            world.inject_packet(inj.link.from, inj.link.to, inj.data.as_bytes(), inj.at);
        }

        // Pre-load CAN bus frame injections.
        // at_ms is in milliseconds; costar ticks are 1 µs each,
        // so we multiply by 1000 to convert to virtual-time ticks.
        for bi in &self.bus_inject {
            let sender_id = name_to_id.get(bi.sender.as_str()).copied().unwrap_or(0);
            let at_ticks = bi.at_ms * 1000;
            world.inject_can_frame(&bi.bus, sender_id, bi.id, &bi.data, at_ticks);
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
        self.check_trace(trace)
    }

    /// Attach a plant model to a World built from this scenario.
    ///
    /// Queues all `[[input]]` entries as timed driver inputs on the plant.
    /// The plant will receive them during its step calls.
    pub fn attach_plant_to(
        &self,
        world: &mut World,
        mut plant: Box<dyn EnvironmentModel>,
    ) -> Result<(), ScenarioError> {
        // Queue all driver inputs at their scheduled times.
        // at_ms is in milliseconds — convert to ticks using the same
        // µs convention as bus injections (ms × 1000).
        for input_def in &self.input {
            if input_def.input_type == "driver_input" {
                let at_ticks = input_def.at_ms * 1000;
                plant.queue_driver_input(
                    at_ticks,
                    input_def.throttle_percent.unwrap_or(0),
                    input_def.brake_pressed.unwrap_or(false),
                );
            }
        }

        // Attach plant with the configured tick interval.
        let tick_ms = self.plant.as_ref().and_then(|p| p.tick_ms).unwrap_or(10);
        world.set_plant(plant, tick_ms);

        Ok(())
    }

    /// Schedule all `[[fault]]` entries from this scenario onto the given World.
    ///
    /// Each fault is scheduled at its `at_ms` time (converted to virtual-time
    /// ticks via `at_ms × 1000`).  The World's run loop will apply them
    /// at the right virtual time.
    pub fn schedule_faults_to(&self, world: &mut World) {
        let name_to_id: std::collections::BTreeMap<&str, u64> = self
            .machine
            .iter()
            .map(|m| (m.name.as_str(), m.id))
            .collect();

        for fault in &self.fault {
            let at_ticks = fault.at_ms * 1000;
            let parts: Vec<&str> = fault.target.splitn(2, '.').collect();
            if parts.len() != 2 {
                continue;
            }
            let (domain, name) = (parts[0], parts[1]);

            match (domain, fault.fault_type.as_str()) {
                ("plant", "force_temperature") => {
                    if let Some(value_c) = fault.value_c {
                        world.schedule_fault(
                            at_ticks,
                            crate::world::FaultAction::ForceTemperature {
                                target: name.to_string(),
                                value_c,
                            },
                        );
                    }
                }
                ("machine", "stop_heartbeat") => {
                    if let Some(&mid) = name_to_id.get(name) {
                        world.schedule_fault(
                            at_ticks,
                            crate::world::FaultAction::StopHeartbeat { machine_id: mid },
                        );
                    }
                }
                ("machine", "reboot") => {
                    if let Some(&mid) = name_to_id.get(name) {
                        world.schedule_fault(
                            at_ticks,
                            crate::world::FaultAction::Reboot { machine_id: mid },
                        );
                    }
                }
                ("bus", "drop_frame") => {
                    if let Some(id) = fault.id {
                        world.schedule_fault(
                            at_ticks,
                            crate::world::FaultAction::DropFrame {
                                bus_name: name.to_string(),
                                frame_id: id,
                            },
                        );
                    }
                }
                ("bus", "delay_frame") => {
                    if let Some(id) = fault.id {
                        let delay_ticks = fault.delay_ms.unwrap_or(0) * 1000;
                        world.schedule_fault(
                            at_ticks,
                            crate::world::FaultAction::DelayFrame {
                                bus_name: name.to_string(),
                                frame_id: id,
                                delay_ticks,
                            },
                        );
                    }
                }
                _ => {
                    // Unknown fault — skip.
                }
            }
        }
    }

    /// Common trace comparison logic.
    pub fn check_trace(&self, trace: Vec<String>) -> Result<ScenarioResult, ScenarioError> {
        let trace_match = if let Some(ref expect) = self.expect {
            if let Some(ref trace_path) = expect.trace {
                let resolved = if let Some(ref base) = self.base_dir {
                    base.join(trace_path)
                } else {
                    std::path::PathBuf::from(trace_path)
                };
                let expected_content =
                    std::fs::read_to_string(&resolved).map_err(ScenarioError::Io)?;
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
        assert!(scenario.bus.is_empty());
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
    fn test_parse_microcar_scenario() {
        let toml_str = r#"
name = "boot_and_heartbeat"
duration_ms = 2000

[[machine]]
id = 1
name = "gateway"
firmware = "firmware/gateway_ecu"
rtos = "freertos"

[[machine]]
id = 2
name = "powertrain"

[[bus]]
name = "vcan0"
type = "can"
latency_us = 500

[[bus.node]]
bus = "vcan0"
machine = "gateway"

[[bus.node]]
bus = "vcan0"
machine = "powertrain"

[plant]
type = "microcar"
tick_ms = 10

[[input]]
at_ms = 500
type = "driver_input"
throttle_percent = 30
brake_pressed = false

[[fault]]
at_ms = 1000
target = "plant.battery"
type = "force_temperature"
value_c = 82

[[expect.event]]
before_ms = 1000
machine = "gateway"
event = "node_online"
node = "powertrain"

[[expect.event]]
before_ms = 1500
machine = "gateway"
event = "vehicle_mode"
value = "READY"
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        assert_eq!(scenario.name, "boot_and_heartbeat");
        assert_eq!(scenario.duration_ms, Some(2000));
        assert_eq!(scenario.machine.len(), 2);
        assert_eq!(
            scenario.machine[0].firmware.as_deref(),
            Some("firmware/gateway_ecu")
        );
        assert_eq!(scenario.machine[0].rtos.as_deref(), Some("freertos"));
        // bus array: 1 [[bus]] entry with 2 nested [[bus.node]] entries
        assert_eq!(scenario.bus.len(), 1);
        let bus0 = &scenario.bus[0];
        assert_eq!(bus0.name.as_deref(), Some("vcan0"));
        assert_eq!(bus0.bus_type.as_deref(), Some("can"));
        assert_eq!(bus0.latency_us, Some(500));
        assert_eq!(bus0.node.len(), 2);
        assert!(scenario.plant.is_some());
        assert_eq!(scenario.plant.as_ref().unwrap().plant_type, "microcar");
        assert_eq!(scenario.input.len(), 1);
        assert_eq!(scenario.input[0].throttle_percent, Some(30));
        assert_eq!(scenario.fault.len(), 1);
        assert_eq!(scenario.fault[0].fault_type, "force_temperature");
        // expect.event entries are collected in expect.event
        let expect = scenario.expect.as_ref().unwrap();
        assert_eq!(expect.event.len(), 2);
        assert_eq!(expect.event[0].node.as_deref(), Some("powertrain"));
        assert_eq!(expect.event[1].value.as_deref(), Some("READY"));
    }

    #[test]
    fn test_bus_topo_creates_canbus() {
        let toml_str = r#"
name = "bus-test"

[[machine]]
id = 1
name = "node_a"

[[machine]]
id = 2
name = "node_b"

[[machine]]
id = 3
name = "node_c"

[[bus]]
name = "vcan0"
type = "can"
latency_us = 100

[[bus.node]]
bus = "vcan0"
machine = "node_a"

[[bus.node]]
bus = "vcan0"
machine = "node_b"

[[bus.node]]
bus = "vcan0"
machine = "node_c"
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        let world = scenario.build_world().unwrap();
        // 3 nodes → 1 CanBus (not 6 point-to-point links anymore).
        assert_eq!(world.link_count(), 0);
        assert_eq!(world.bus_count(), 1);
        let bus = &world.buses()[0];
        assert_eq!(bus.name, "vcan0");
        assert_eq!(bus.node_count(), 3);
    }

    #[test]
    fn test_bus_node_unknown_bus_rejected() {
        let toml_str = r#"
[[machine]]
id = 1
name = "m1"

[[bus]]
name = "vcan0"
type = "can"
latency_us = 100

[[bus.node]]
bus = "nonexistent"
machine = "m1"
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown bus"));
    }

    #[test]
    fn test_bus_node_unknown_machine_rejected() {
        let toml_str = r#"
[[machine]]
id = 1
name = "m1"

[[bus]]
name = "vcan0"
type = "can"
latency_us = 100

[[bus.node]]
bus = "vcan0"
machine = "nonexistent"
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown machine"));
    }

    #[test]
    fn test_fault_unknown_machine_rejected() {
        let toml_str = r#"
[[machine]]
id = 1
name = "m1"

[[fault]]
at_ms = 100
target = "machine.nonexistent"
type = "reboot"
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown machine"));
    }

    #[test]
    fn test_fault_plant_target_accepted() {
        let toml_str = r#"
[[machine]]
id = 1
name = "m1"

[[fault]]
at_ms = 100
target = "plant.battery"
type = "force_temperature"
value_c = 82
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        assert_eq!(scenario.fault.len(), 1);
        assert_eq!(scenario.fault[0].target, "plant.battery");
    }

    #[test]
    fn test_expect_event_unknown_machine_rejected() {
        let toml_str = r#"
[[machine]]
id = 1
name = "m1"

[[expect.event]]
before_ms = 100
machine = "nonexistent"
event = "test"
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown machine"));
    }

    #[test]
    fn test_duplicate_bus_name_rejected() {
        let toml_str = r#"
[[machine]]
id = 1
name = "m1"

[[bus]]
name = "vcan0"
type = "can"
latency_us = 100

[[bus]]
name = "vcan0"
type = "can"
latency_us = 200
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("duplicate bus name"));
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
        assert_eq!(result.trace.len(), 1);
        assert!(result.trace[0].contains("pkt-rx"));
    }

    // ── UART link tests ──────────────────────────────────────────────

    #[test]
    fn test_parse_uart_link() {
        let toml_str = r#"
name = "uart-test"

[[machine]]
id = 0
name = "board_a"

[[machine]]
id = 1
name = "board_b"

[[link]]
type = "uart"
from = 0
to = 1
baud = 115200
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        assert_eq!(scenario.name, "uart-test");
        assert_eq!(scenario.link.len(), 1);
        assert_eq!(scenario.link[0].link_type, "uart");
        assert_eq!(scenario.link[0].baud, Some(115200));
        assert_eq!(scenario.link[0].data_bits, None); // default
        assert_eq!(scenario.link[0].parity, None); // default
        assert_eq!(scenario.link[0].stop_bits, None); // default
    }

    #[test]
    fn test_uart_link_missing_baud_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"

[[machine]]
id = 1
name = "m1"

[[link]]
type = "uart"
from = 0
to = 1
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires 'baud'"));
    }

    #[test]
    fn test_uart_link_with_latency_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"

[[machine]]
id = 1
name = "m1"

[[link]]
type = "uart"
from = 0
to = 1
baud = 115200
latency = 5
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("latency"));
    }

    #[test]
    fn test_fifo_link_missing_latency_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"

[[machine]]
id = 1
name = "m1"

[[link]]
type = "fifo"
from = 0
to = 1
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("requires 'latency'"));
    }

    #[test]
    fn test_uart_link_with_full_params() {
        let toml_str = r#"
name = "uart-full"

[[machine]]
id = 0
name = "m0"

[[machine]]
id = 1
name = "m1"

[[link]]
type = "uart"
from = 0
to = 1
baud = 9600
data_bits = 8
parity = "E"
stop_bits = 2
tick_rate_hz = 500000
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        assert_eq!(scenario.link[0].baud, Some(9600));
        assert_eq!(scenario.link[0].data_bits, Some(8));
        assert_eq!(scenario.link[0].parity, Some('E'));
        assert_eq!(scenario.link[0].stop_bits, Some(2));
        assert_eq!(scenario.link[0].tick_rate_hz, Some(500000));
    }

    #[test]
    fn test_uart_link_invalid_parity_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"

[[machine]]
id = 1
name = "m1"

[[link]]
type = "uart"
from = 0
to = 1
baud = 115200
parity = "X"
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid parity"));
    }

    #[test]
    fn test_unknown_link_type_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"

[[machine]]
id = 1
name = "m1"

[[link]]
type = "spi"
from = 0
to = 1
baud = 1000000
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unknown link type"));
    }

    #[test]
    fn test_scenario_run_uart_cross() {
        // Two machines with crossed UART links exchanging data at 115200 baud.
        // 8N1, 1 MHz tick rate → 86 ticks per byte (10 * 1_000_000 / 115200 = 86).
        let toml_str = r#"
name = "uart-cross"

[[machine]]
id = 0
name = "board_a"

[[machine]]
id = 1
name = "board_b"

[[link]]
type = "uart"
from = 0
to = 1
baud = 115200

[[link]]
type = "uart"
from = 1
to = 0
baud = 115200

[[inject]]
at = 0
link = { from = 0, to = 1 }
data = "Hi"

[[inject]]
at = 1000
link = { from = 1, to = 0 }
data = "Yo"
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        let result = scenario.run().unwrap();
        assert_eq!(result.name, "uart-cross");

        // "Yo": 'Y' at 1086, 'o' at 1172 → 2 events on machine 0 (machine 0 comes first in BTreeMap order)
        // "Hi": 'H' at 86, 'i' at 172 → 2 events on machine 1
        assert_eq!(result.trace.len(), 4);

        // Traces are grouped by machine ID (BTreeMap order: 0 then 1).
        assert!(result.trace[0].contains("[machine.0]"));
        assert!(result.trace[0].contains("1086") && result.trace[0].contains("pkt-rx"));

        assert!(result.trace[1].contains("[machine.0]"));
        assert!(result.trace[1].contains("1172"));

        assert!(result.trace[2].contains("[machine.1]"));
        assert!(result.trace[2].contains("86"));

        assert!(result.trace[3].contains("[machine.1]"));
        assert!(result.trace[3].contains("172"));
    }

    #[test]
    fn test_fifo_link_with_uart_fields_rejected() {
        let toml_str = r#"
[[machine]]
id = 0
name = "m0"

[[machine]]
id = 1
name = "m1"

[[link]]
type = "fifo"
from = 0
to = 1
latency = 5
baud = 115200
"#;
        let result = Scenario::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("UART fields"));
    }

    #[test]
    fn test_duration_ms_field() {
        let toml_str = r#"
name = "timed"
duration_ms = 5000

[[machine]]
id = 0
name = "m0"
"#;
        let scenario = Scenario::from_str(toml_str).unwrap();
        assert_eq!(scenario.duration_ms, Some(5000));
    }
}
