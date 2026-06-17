//! JSON-RPC 2.0 server for `costar serve`.
//!
//! Provides a long-lived server that manages multiple concurrent simulation
//! sessions. Clients (like the mcu Go CLI) connect via stdin/stdout or TCP
//! and speak JSON-RPC 2.0 over newline-delimited JSON.
//!
//! # Transport modes
//!
//! - **stdio**: reads requests from stdin, writes responses to stdout
//!   (one JSON object per line). Primary mode for subprocess integration.
//! - **bind**: TCP listener on the given address (e.g. `127.0.0.1:9321`).

mod transport;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use sim_world::scenario::Scenario;
use sim_world::World;

/// JSON-RPC 2.0 standard error codes.
#[allow(dead_code)]
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    // Application errors (-32000 to -32099).
    pub const SESSION_NOT_FOUND: i64 = -32000;
    pub const SESSION_IN_USE: i64 = -32001;
    pub const NO_SCENARIO_LOADED: i64 = -32002;
    pub const SIM_ALREADY_RUNNING: i64 = -32003;
    pub const SIM_ERROR: i64 = -32004;
    pub const INVALID_FORMAT: i64 = -32005;
    pub const SCENARIO_PARSE_ERROR: i64 = -32006;
}

/// State of a simulation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// Session created, no scenario loaded yet.
    Idle,
    /// Scenario loaded and ready.
    Ready,
    /// Simulation is running.
    Running,
    /// Simulation completed successfully.
    Done,
    /// Simulation encountered an error.
    Error,
}

/// A managed simulation session.
struct Session {
    id: u64,
    state: SessionState,
    world: Option<World>,
    /// The parsed scenario, preserved for clone/reset.
    scenario: Option<Scenario>,
    /// Board config TOML string, preserved for clone.
    board_config_toml: Option<String>,
    /// Human-format trace collected after run.
    trace_human: Vec<String>,
    /// JSONL trace collected after run.
    trace_jsonl: Vec<String>,
    scenario_summary: Option<ScenarioSummary>,
    started_at: Option<Instant>,
    n_events: u64,
    exit_code: i32,
    error_message: Option<String>,
    /// Build-time Zephyr app compilation parameters (informational).
    app_sources: Option<String>,
    app_includes: Option<String>,
    zephyr_config_dir: Option<String>,
    /// Last time this session was accessed (for TTL expiry).
    last_activity: Instant,
}

#[derive(Debug, Clone)]
struct ScenarioSummary {
    n_machines: usize,
    n_links: usize,
    n_injections: usize,
}

/// The JSON-RPC server state shared across transport threads.
pub struct Server {
    sessions: Mutex<HashMap<u64, Session>>,
    next_id: AtomicU64,
    shutdown: Mutex<bool>,
    /// Session idle TTL — sessions with no activity for this long are auto-destroyed.
    session_ttl: Duration,
    /// Last time expired-session cleanup was performed.
    last_cleanup: Mutex<Instant>,
}

impl Server {
    pub fn new(session_ttl: Duration) -> Self {
        Server {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            shutdown: Mutex::new(false),
            session_ttl,
            last_cleanup: Mutex::new(Instant::now()),
        }
    }

    /// Check if shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        *self.shutdown.lock().unwrap()
    }

    /// Set the shutdown flag.
    fn request_shutdown(&self) {
        *self.shutdown.lock().unwrap() = true;
    }

    /// Destroy sessions that have been idle longer than the TTL.
    ///
    /// Returns the number of sessions removed.
    pub fn cleanup_expired_sessions(&self) -> usize {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap();
        let ttl = self.session_ttl;
        let before = sessions.len();
        sessions.retain(|_id, s| now.duration_since(s.last_activity) < ttl);
        before - sessions.len()
    }

    /// Run cleanup if enough time has passed since the last run.
    ///
    /// Rate-limited to at most once per second to avoid overhead on
    /// every request.
    pub fn maybe_cleanup_expired_sessions(&self) {
        let now = Instant::now();
        let mut last = self.last_cleanup.lock().unwrap();
        if now.duration_since(*last) >= Duration::from_secs(1) {
            *last = now;
            drop(last);
            let removed = self.cleanup_expired_sessions();
            if removed > 0 {
                eprintln!("ttl cleanup: removed {} expired session(s)", removed);
            }
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new(Duration::from_secs(300))
    }
}

/// Build a JSON-RPC 2.0 response object.
///
/// If `id` is null, this is a notification — no response is sent.
fn rpc_response(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

/// Build a JSON-RPC 2.0 error response object.
fn rpc_error(id: &Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    });
    if let Some(d) = data {
        err["error"]["data"] = d;
    }
    err
}

/// Parse and dispatch a single JSON-RPC request, returning the response
/// to send (or None for notifications).
///
/// For methods that produce streaming output (e.g. `trace.stream`), the
/// handler writes NDJSON lines directly to `writer` before returning the
/// final response.
fn dispatch(server: &Server, request: &Value, writer: &mut dyn std::io::Write) -> Option<Value> {
    // Rate-limited TTL cleanup — destroy idle sessions.
    server.maybe_cleanup_expired_sessions();

    // Validate JSON-RPC 2.0 envelope.
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = match request.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            if id.is_null() {
                return None; // notification with no method — ignore
            }
            return Some(rpc_error(
                &id,
                error_codes::INVALID_REQUEST,
                "missing 'method' field",
                None,
            ));
        }
    };
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    let result = match method {
        "session.create" => handle_session_create(server, &id, &params),
        "session.destroy" => handle_session_destroy(server, &id, &params),
        "session.clone" => handle_session_clone(server, &id, &params),
        "session.list" => handle_session_list(server, &id, &params),
        "scenario.load" => handle_scenario_load(server, &id, &params),
        "scenario.load_inline" => handle_scenario_load_inline(server, &id, &params),
        "sim.run" => handle_sim_run(server, &id, &params),
        "sim.run_until" => handle_sim_run_until(server, &id, &params),
        "sim.step" => handle_sim_step(server, &id, &params),
        "sim.reset" => handle_sim_reset(server, &id, &params),
        "sim.status" => handle_sim_status(server, &id, &params),
        "sim.stop" => handle_sim_stop(server, &id, &params),
        "board.configure" => handle_board_configure(server, &id, &params),
        "trace.get" => handle_trace_get(server, &id, &params),
        "trace.stream" => handle_trace_stream(server, &id, &params, writer),
        "server.shutdown" => handle_server_shutdown(server, &id, &params),
        _ => Err(rpc_error(
            &id,
            error_codes::METHOD_NOT_FOUND,
            &format!("method not found: {}", method),
            None,
        )),
    };

    match result {
        Ok(resp) => {
            if id.is_null() {
                None // notification
            } else {
                Some(resp)
            }
        }
        Err(err_resp) => {
            if id.is_null() {
                None // notification
            } else {
                Some(err_resp)
            }
        }
    }
}

/// Extract `session_id` from params. Returns `(id, session_id)` or error.
fn get_session_id(params: &Value) -> Result<u64, Value> {
    params
        .get("session_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            rpc_error(
                &Value::Null,
                error_codes::INVALID_PARAMS,
                "missing or invalid 'session_id'",
                None,
            )
        })
}

/// Look up a session by ID. Returns error if not found.
fn get_session<'a>(
    sessions: &'a mut HashMap<u64, Session>,
    session_id: u64,
    id: &Value,
) -> Result<&'a mut Session, Value> {
    sessions.get_mut(&session_id).ok_or_else(|| {
        rpc_error(
            id,
            error_codes::SESSION_NOT_FOUND,
            &format!("session {} not found", session_id),
            None,
        )
    })
}

// ── Method handlers ───────────────────────────────────────────────────────

fn handle_session_create(server: &Server, id: &Value, _params: &Value) -> Result<Value, Value> {
    let session_id = server.next_id.fetch_add(1, Ordering::SeqCst);
    let now = Instant::now();
    let mut sessions = server.sessions.lock().unwrap();
    sessions.insert(
        session_id,
        Session {
            id: session_id,
            state: SessionState::Idle,
            world: None,
            scenario: None,
            board_config_toml: None,
            trace_human: Vec::new(),
            trace_jsonl: Vec::new(),
            scenario_summary: None,
            started_at: None,
            n_events: 0,
            exit_code: 0,
            error_message: None,
            app_sources: None,
            app_includes: None,
            zephyr_config_dir: None,
            last_activity: now,
        },
    );
    Ok(rpc_response(
        id,
        json!({
            "session_id": session_id,
            "state": SessionState::Idle,
        }),
    ))
}

fn handle_session_destroy(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let mut sessions = server.sessions.lock().unwrap();
    if sessions.remove(&session_id).is_some() {
        Ok(rpc_response(
            id,
            json!({"destroyed": true, "session_id": session_id}),
        ))
    } else {
        Err(rpc_error(
            id,
            error_codes::SESSION_NOT_FOUND,
            &format!("session {} not found", session_id),
            None,
        ))
    }
}

fn handle_session_list(server: &Server, id: &Value, _params: &Value) -> Result<Value, Value> {
    let sessions = server.sessions.lock().unwrap();
    let list: Vec<Value> = sessions
        .values()
        .map(|s| {
            json!({
                "session_id": s.id,
                "state": s.state,
                "n_machines": s.scenario_summary.as_ref().map_or(0, |sm| sm.n_machines),
                "uptime_ticks": s.world.as_ref().map_or(0, |w| w.now),
            })
        })
        .collect();
    Ok(rpc_response(id, json!(list)))
}

fn handle_scenario_load(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_error(id, error_codes::INVALID_PARAMS, "missing 'path'", None))?;

    // Optional Zephyr app compilation parameters.
    let app_sources = params
        .get("app_sources")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let app_includes = params
        .get("app_includes")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let zephyr_config_dir = params
        .get("zephyr_config_dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let scenario = Scenario::from_file(path).map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to load scenario: {}", e),
            None,
        )
    })?;

    let summary = ScenarioSummary {
        n_machines: scenario.machine.len(),
        n_links: scenario.link.len(),
        n_injections: scenario.inject.len(),
    };

    let world = scenario.build_world().map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to build world: {}", e),
            None,
        )
    })?;

    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;
    session.world = Some(world);
    session.scenario = Some(scenario);
    session.state = SessionState::Ready;
    session.scenario_summary = Some(summary.clone());
    session.app_sources = app_sources;
    session.app_includes = app_includes;
    session.zephyr_config_dir = zephyr_config_dir;
    session.last_activity = Instant::now();

    Ok(rpc_response(
        id,
        json!({
            "n_machines": summary.n_machines,
            "n_links": summary.n_links,
            "n_injections": summary.n_injections,
        }),
    ))
}

fn handle_scenario_load_inline(
    server: &Server,
    id: &Value,
    params: &Value,
) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let toml_str = params
        .get("toml")
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_error(id, error_codes::INVALID_PARAMS, "missing 'toml'", None))?;

    // Optional Zephyr app compilation parameters.
    let app_sources = params
        .get("app_sources")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let app_includes = params
        .get("app_includes")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let zephyr_config_dir = params
        .get("zephyr_config_dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let scenario = Scenario::from_str(toml_str).map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to parse inline scenario: {}", e),
            None,
        )
    })?;

    let summary = ScenarioSummary {
        n_machines: scenario.machine.len(),
        n_links: scenario.link.len(),
        n_injections: scenario.inject.len(),
    };

    let world = scenario.build_world().map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to build world: {}", e),
            None,
        )
    })?;

    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;
    session.world = Some(world);
    session.scenario = Some(scenario);
    session.state = SessionState::Ready;
    session.scenario_summary = Some(summary.clone());
    session.app_sources = app_sources;
    session.app_includes = app_includes;
    session.zephyr_config_dir = zephyr_config_dir;
    session.last_activity = Instant::now();

    Ok(rpc_response(
        id,
        json!({
            "n_machines": summary.n_machines,
            "n_links": summary.n_links,
            "n_injections": summary.n_injections,
        }),
    ))
}

fn handle_sim_run(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;
    session.last_activity = Instant::now();

    let world = match session.world.as_mut() {
        Some(w) => w,
        None => {
            return Err(rpc_error(
                id,
                error_codes::NO_SCENARIO_LOADED,
                "no scenario loaded in this session",
                None,
            ));
        }
    };

    if session.state == SessionState::Running {
        return Err(rpc_error(
            id,
            error_codes::SIM_ALREADY_RUNNING,
            "simulation is already running",
            None,
        ));
    }

    session.state = SessionState::Running;
    session.started_at = Some(Instant::now());

    // Run the simulation.
    match world.run() {
        Ok(()) => {
            let duration_ms = session
                .started_at
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            let traces = world.drain_all_traces();
            let jsonl_traces: Vec<String> = traces.to_vec();

            session.trace_human = traces.clone();
            session.trace_jsonl = jsonl_traces.clone();
            session.n_events = traces.len() as u64;
            session.state = SessionState::Done;
            session.exit_code = 0;

            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 0,
                    "n_events": traces.len(),
                    "trace_jsonl": traces,
                    "duration_ms": duration_ms,
                }),
            ))
        }
        Err(e) => {
            session.state = SessionState::Error;
            session.exit_code = 1;
            session.error_message = Some(e.to_string());

            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 1,
                    "n_events": 0,
                    "trace_jsonl": [],
                    "error": e.to_string(),
                    "duration_ms": 0,
                }),
            ))
        }
    }
}

fn handle_sim_run_until(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let deadline = params
        .get("deadline_ticks")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "missing or invalid 'deadline_ticks'",
                None,
            )
        })?;

    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;

    let world = match session.world.as_mut() {
        Some(w) => w,
        None => {
            return Err(rpc_error(
                id,
                error_codes::NO_SCENARIO_LOADED,
                "no scenario loaded in this session",
                None,
            ));
        }
    };

    session.state = SessionState::Running;

    match world.run_until(deadline) {
        Ok(()) => {
            let traces = world.drain_all_traces();
            session.trace_human = traces.clone();
            session.trace_jsonl = traces.clone();
            session.n_events = traces.len() as u64;

            let now_ticks = world.now;
            let all_idle = world.all_idle();
            if all_idle {
                session.state = SessionState::Done;
            }

            Ok(rpc_response(
                id,
                json!({
                    "now_ticks": now_ticks,
                    "all_idle": all_idle,
                    "n_events": traces.len(),
                    "trace_jsonl": traces,
                }),
            ))
        }
        Err(e) => {
            session.state = SessionState::Error;
            session.error_message = Some(e.to_string());
            Err(rpc_error(
                id,
                error_codes::SIM_ERROR,
                &format!("simulation error: {}", e),
                None,
            ))
        }
    }
}

fn handle_sim_step(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let n_ticks = params.get("n_ticks").and_then(|v| v.as_u64()).unwrap_or(1);

    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;

    let world = match session.world.as_mut() {
        Some(w) => w,
        None => {
            return Err(rpc_error(
                id,
                error_codes::NO_SCENARIO_LOADED,
                "no scenario loaded in this session",
                None,
            ));
        }
    };

    let start_ticks = world.now;
    let deadline = start_ticks.saturating_add(n_ticks);

    match world.run_until(deadline) {
        Ok(()) => {
            let traces = world.drain_all_traces();
            let new_events: Vec<String> = traces.into_iter().collect();

            if world.all_idle() {
                session.state = SessionState::Done;
            } else if session.state != SessionState::Running {
                session.state = SessionState::Running;
            }

            Ok(rpc_response(
                id,
                json!({
                    "state": session.state,
                    "now_ticks": world.now,
                    "new_events": new_events,
                }),
            ))
        }
        Err(e) => {
            session.state = SessionState::Error;
            session.error_message = Some(e.to_string());
            Err(rpc_error(
                id,
                error_codes::SIM_ERROR,
                &format!("simulation error: {}", e),
                None,
            ))
        }
    }
}

fn handle_sim_status(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let sessions = server.sessions.lock().unwrap();
    let session = sessions.get(&session_id).ok_or_else(|| {
        rpc_error(
            id,
            error_codes::SESSION_NOT_FOUND,
            &format!("session {} not found", session_id),
            None,
        )
    })?;

    Ok(rpc_response(
        id,
        json!({
            "state": session.state,
            "now_ticks": session.world.as_ref().map_or(0, |w| w.now),
            "n_machines": session.scenario_summary.as_ref().map_or(0, |sm| sm.n_machines),
        }),
    ))
}

fn handle_sim_stop(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;

    if let Some(ref mut world) = session.world {
        world.stop();
    }
    session.state = SessionState::Ready;

    Ok(rpc_response(
        id,
        json!({
            "stopped": true,
            "session_id": session_id,
        }),
    ))
}

fn handle_board_configure(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let config_toml = params
        .get("config_toml")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "missing 'config_toml' field",
                None,
            )
        })?;

    // Parse the board config and initialise virtual devices.
    let board_cfg = sim_world::BoardConfig::from_str(config_toml).map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to parse board config: {}", e),
            None,
        )
    })?;

    let n_peripherals = board_cfg.initialize_devices();

    // Store the board config TOML for session.clone.
    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;
    session.board_config_toml = Some(config_toml.to_string());
    session.last_activity = Instant::now();
    drop(sessions);

    Ok(rpc_response(
        id,
        json!({
            "n_peripherals": n_peripherals,
        }),
    ))
}

fn handle_trace_get(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("human");

    let sessions = server.sessions.lock().unwrap();
    let session = sessions.get(&session_id).ok_or_else(|| {
        rpc_error(
            id,
            error_codes::SESSION_NOT_FOUND,
            &format!("session {} not found", session_id),
            None,
        )
    })?;

    let trace = match format {
        "jsonl" => session.trace_jsonl.join("\n"),
        "human" => session.trace_human.join("\n"),
        _ => {
            return Err(rpc_error(
                id,
                error_codes::INVALID_FORMAT,
                &format!(
                    "unknown trace format: '{}' (use 'human' or 'jsonl')",
                    format
                ),
                None,
            ));
        }
    };

    Ok(rpc_response(id, json!({ "trace": trace })))
}

fn handle_server_shutdown(server: &Server, id: &Value, _params: &Value) -> Result<Value, Value> {
    server.request_shutdown();
    Ok(rpc_response(id, json!({"shutdown": true})))
}

// ── Phase 32f: session.clone, sim.reset, trace.stream ─────────────────────

fn handle_session_clone(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let mut sessions = server.sessions.lock().unwrap();
    let source = get_session(&mut sessions, session_id, id)?;

    // Clone the scenario (if any) and build a fresh World.
    let (new_world, new_scenario, summary) = match source.scenario.as_ref() {
        Some(scenario) => {
            let cloned = scenario.clone();
            let summary = ScenarioSummary {
                n_machines: cloned.machine.len(),
                n_links: cloned.link.len(),
                n_injections: cloned.inject.len(),
            };
            let world = cloned.build_world().map_err(|e| {
                rpc_error(
                    id,
                    error_codes::SCENARIO_PARSE_ERROR,
                    &format!("failed to build world for clone: {}", e),
                    None,
                )
            })?;
            (Some(world), Some(cloned), Some(summary))
        }
        None => (None, None, None),
    };

    // If there was no scenario, clone as an idle session.
    let board_config_toml = source.board_config_toml.clone();
    let app_sources = source.app_sources.clone();
    let app_includes = source.app_includes.clone();
    let zephyr_config_dir = source.zephyr_config_dir.clone();

    // Mark source as recently active.
    source.last_activity = Instant::now();

    let new_id = server.next_id.fetch_add(1, Ordering::SeqCst);
    let new_state = if new_world.is_some() {
        SessionState::Ready
    } else {
        SessionState::Idle
    };

    sessions.insert(
        new_id,
        Session {
            id: new_id,
            state: new_state,
            world: new_world,
            scenario: new_scenario,
            board_config_toml,
            trace_human: Vec::new(),
            trace_jsonl: Vec::new(),
            scenario_summary: summary,
            started_at: None,
            n_events: 0,
            exit_code: 0,
            error_message: None,
            app_sources,
            app_includes,
            zephyr_config_dir,
            last_activity: Instant::now(),
        },
    );

    Ok(rpc_response(
        id,
        json!({
            "session_id": new_id,
            "state": new_state,
        }),
    ))
}

fn handle_sim_reset(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;

    // Rebuild the world from the stored scenario.
    let scenario = session.scenario.as_ref().ok_or_else(|| {
        rpc_error(
            id,
            error_codes::NO_SCENARIO_LOADED,
            "no scenario loaded in this session — cannot reset",
            None,
        )
    })?;

    let world = scenario.build_world().map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to rebuild world: {}", e),
            None,
        )
    })?;

    // Clear all transient state.
    session.world = Some(world);
    session.state = SessionState::Ready;
    session.trace_human.clear();
    session.trace_jsonl.clear();
    session.n_events = 0;
    session.exit_code = 0;
    session.error_message = None;
    session.started_at = None;
    session.last_activity = Instant::now();

    Ok(rpc_response(
        id,
        json!({
            "session_id": session_id,
            "state": SessionState::Ready,
            "now_ticks": 0,
        }),
    ))
}

fn handle_trace_stream(
    server: &Server,
    id: &Value,
    params: &Value,
    writer: &mut dyn std::io::Write,
) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;
    session.last_activity = Instant::now();

    let world = match session.world.as_mut() {
        Some(w) => w,
        None => {
            return Err(rpc_error(
                id,
                error_codes::NO_SCENARIO_LOADED,
                "no scenario loaded in this session",
                None,
            ));
        }
    };

    if session.state == SessionState::Running {
        return Err(rpc_error(
            id,
            error_codes::SIM_ALREADY_RUNNING,
            "simulation is already running",
            None,
        ));
    }

    session.state = SessionState::Running;
    session.started_at = Some(Instant::now());

    // Run the simulation, writing each trace event as NDJSON as it is
    // collected.  Because the simulation is synchronous, we drain traces
    // after the run completes (deterministic sims run in milliseconds).
    match world.run() {
        Ok(()) => {
            let duration_ms = session
                .started_at
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            let traces = world.drain_all_traces();

            // Write each trace event as a newline-delimited JSON object.
            for line in &traces {
                let stream_event = json!({
                    "event": "trace",
                    "data": line,
                });
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&stream_event).unwrap_or_default()
                );
            }

            // Write a final "done" event.
            let done_event = json!({
                "event": "trace.stream.done",
                "n_events": traces.len(),
                "duration_ms": duration_ms,
            });
            let _ = writeln!(
                writer,
                "{}",
                serde_json::to_string(&done_event).unwrap_or_default()
            );
            let _ = writer.flush();

            session.trace_human = traces.clone();
            session.trace_jsonl = traces.clone();
            session.n_events = traces.len() as u64;
            session.state = SessionState::Done;
            session.exit_code = 0;

            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 0,
                    "n_events": traces.len(),
                    "duration_ms": duration_ms,
                }),
            ))
        }
        Err(e) => {
            session.state = SessionState::Error;
            session.exit_code = 1;
            session.error_message = Some(e.to_string());

            let error_event = json!({
                "event": "trace.stream.error",
                "error": e.to_string(),
            });
            let _ = writeln!(
                writer,
                "{}",
                serde_json::to_string(&error_event).unwrap_or_default()
            );
            let _ = writer.flush();

            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 1,
                    "n_events": 0,
                    "error": e.to_string(),
                    "duration_ms": 0,
                }),
            ))
        }
    }
}

// ── Entry points ───────────────────────────────────────────────────────────

/// Run the JSON-RPC server on a TCP listener.
pub fn run_bind(addr: &str, session_ttl: Duration) {
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to bind to {}: {}", addr, e);
            std::process::exit(1);
        }
    };

    let local_addr = listener.local_addr().unwrap();
    eprintln!("costar JSON-RPC server listening on {}", local_addr);

    // Accept connections in a loop, spawning one thread per connection.
    // Each connection gets its own Server (sessions are not shared across
    // connections because World is not Send — EventCallback holds non-Send
    // closures).
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    let server = Server::new(session_ttl);
                    transport::handle_tcp(server, stream);
                });
            }
            Err(e) => {
                eprintln!("error: accept failed: {}", e);
            }
        }
    }
}

/// Run the JSON-RPC server on stdio (stdin/stdout).
///
/// Reads newline-delimited JSON-RPC requests from stdin, writes
/// responses to stdout (one JSON object per line).
pub fn run_stdio(session_ttl: Duration) {
    let server = Server::new(session_ttl);
    transport::handle_stdio(&server);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;

    /// Send a JSON-RPC request over TCP and read the response.
    fn rpc_call(stream: &mut TcpStream, request: &Value) -> Value {
        let req_str = serde_json::to_string(request).unwrap() + "\n";
        stream.write_all(req_str.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// Start a TCP server on a random port, return the port.
    fn start_server_on_random_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let server = Server::new(Duration::from_secs(300));
                transport::handle_tcp(server, stream);
            }
        });

        // Give the server thread a moment to start.
        std::thread::sleep(std::time::Duration::from_millis(50));

        port
    }

    fn _next_id() -> Value {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        json!(COUNTER.fetch_add(1, Ordering::SeqCst))
    }

    #[test]
    fn test_session_create_destroy() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Create a session.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session.create",
            "params": {},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["id"], json!(1));
        let session_id = resp["result"]["session_id"].as_u64().unwrap();
        assert!(session_id > 0);
        assert_eq!(resp["result"]["state"], "idle");

        // Destroy the session.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session.destroy",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["id"], json!(2));
        assert_eq!(resp["result"]["destroyed"], json!(true));
    }

    #[test]
    fn test_session_list() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Create two sessions.
        let req1 = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let _resp1 = rpc_call(&mut stream, &req1);

        let req2 = json!({"jsonrpc": "2.0", "id": 2, "method": "session.create", "params": {}});
        let _resp2 = rpc_call(&mut stream, &req2);

        // List sessions.
        let req = json!({"jsonrpc": "2.0", "id": 3, "method": "session.list", "params": {}});
        let resp = rpc_call(&mut stream, &req);
        let list = resp["result"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        for s in list {
            assert!(s["session_id"].as_u64().is_some());
            assert_eq!(s["state"], "idle");
        }
    }

    #[test]
    fn test_scenario_load_inline_and_run() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Create a session.
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let resp = rpc_call(&mut stream, &req);
        let session_id = resp["result"]["session_id"].as_u64().unwrap();

        // Load a minimal scenario.
        let scenario_toml = r#"
name = "minimal"
[[machine]]
id = 0
name = "m0"
"#;
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "scenario.load_inline",
            "params": {
                "session_id": session_id,
                "toml": scenario_toml,
            },
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["n_machines"], json!(1));
        assert_eq!(resp["result"]["n_links"], json!(0));
        assert_eq!(resp["result"]["n_injections"], json!(0));

        // Run the simulation.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "sim.run",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["exit_code"], json!(0));

        // Get the trace.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "trace.get",
            "params": {
                "session_id": session_id,
                "format": "human",
            },
        });
        let resp = rpc_call(&mut stream, &req);
        assert!(resp["result"]["trace"].as_str().is_some());

        // Destroy the session.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "session.destroy",
            "params": {"session_id": session_id},
        });
        rpc_call(&mut stream, &req);
    }

    #[test]
    fn test_sim_status() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let resp = rpc_call(&mut stream, &req);
        let session_id = resp["result"]["session_id"].as_u64().unwrap();

        // Check status before loading.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "sim.status",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["state"], "idle");
        assert_eq!(resp["result"]["now_ticks"], json!(0));
        assert_eq!(resp["result"]["n_machines"], json!(0));
    }

    #[test]
    fn test_sim_step() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let resp = rpc_call(&mut stream, &req);
        let session_id = resp["result"]["session_id"].as_u64().unwrap();

        // Load a scenario with an event at time 100.
        let scenario_toml = r#"
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
at = 100
link = { from = 0, to = 1 }
data = "hello"
"#;
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "scenario.load_inline",
            "params": {"session_id": session_id, "toml": scenario_toml},
        });
        rpc_call(&mut stream, &req);

        // Step 1 tick — no events yet, so time stays at 0.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "sim.step",
            "params": {"session_id": session_id, "n_ticks": 1},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["now_ticks"], json!(0));

        // Step 200 ticks — injection at 100 + 5-tick latency = arrival at 105.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "sim.step",
            "params": {"session_id": session_id, "n_ticks": 200},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["now_ticks"], json!(105));
    }

    #[test]
    fn test_server_shutdown() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server.shutdown",
            "params": {},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["shutdown"], json!(true));

        // After shutdown, the connection handler stops accepting requests.
        // Send another request — the connection will be closed by the server.
        // We just verify the shutdown response was correct.
    }

    #[test]
    fn test_method_not_found() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "nonexistent.method",
            "params": {},
        });
        let resp = rpc_call(&mut stream, &req);
        assert!(resp.get("error").is_some());
        assert_eq!(resp["error"]["code"], json!(error_codes::METHOD_NOT_FOUND));
    }

    #[test]
    fn test_invalid_params() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Missing session_id.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session.destroy",
            "params": {},
        });
        let resp = rpc_call(&mut stream, &req);
        assert!(resp.get("error").is_some());
        assert_eq!(resp["error"]["code"], json!(error_codes::INVALID_PARAMS));
    }

    #[test]
    fn test_session_not_found() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sim.run",
            "params": {"session_id": 99999},
        });
        let resp = rpc_call(&mut stream, &req);
        assert!(resp.get("error").is_some());
        assert_eq!(resp["error"]["code"], json!(error_codes::SESSION_NOT_FOUND));
    }

    #[test]
    fn test_run_until() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let resp = rpc_call(&mut stream, &req);
        let session_id = resp["result"]["session_id"].as_u64().unwrap();

        // Load a scenario with an injection at time 100.
        let scenario_toml = r#"
[[machine]]
id = 0
name = "m0"
[[machine]]
id = 1
name = "m1"
[[link]]
from = 0
to = 1
latency = 10
[[inject]]
at = 100
link = { from = 0, to = 1 }
data = "hello"
"#;
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "scenario.load_inline",
            "params": {"session_id": session_id, "toml": scenario_toml},
        });
        rpc_call(&mut stream, &req);

        // Run until time 50 — nothing should happen.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "sim.run_until",
            "params": {"session_id": session_id, "deadline_ticks": 50},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["now_ticks"], json!(0));
        assert!(!resp["result"]["all_idle"].as_bool().unwrap());

        // Run until time 200 — the injection fires.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "sim.run_until",
            "params": {"session_id": session_id, "deadline_ticks": 200},
        });
        let resp = rpc_call(&mut stream, &req);
        assert!(resp["result"]["now_ticks"].as_u64().unwrap() >= 100);
    }

    // ── Phase 32f tests ──────────────────────────────────────────────

    #[test]
    fn test_session_clone_produces_independent_simulation() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Create a session and load a scenario.
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let resp = rpc_call(&mut stream, &req);
        let session_id = resp["result"]["session_id"].as_u64().unwrap();

        let scenario_toml = r#"
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
at = 100
link = { from = 0, to = 1 }
data = "hello"
"#;
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "scenario.load_inline",
            "params": {"session_id": session_id, "toml": scenario_toml},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["n_machines"], json!(2));

        // Clone the session.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session.clone",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        let clone_id = resp["result"]["session_id"].as_u64().unwrap();
        assert_ne!(clone_id, session_id);
        assert_eq!(resp["result"]["state"], "ready");

        // Run the original session — it should complete.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "sim.run",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["exit_code"], json!(0));
        let orig_events = resp["result"]["n_events"].as_u64().unwrap();

        // Run the cloned session independently — it should also complete
        // with the same number of events (same scenario).
        let req = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "sim.run",
            "params": {"session_id": clone_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["exit_code"], json!(0));
        // The clone produces the same trace (same deterministic simulation).
        assert_eq!(resp["result"]["n_events"].as_u64().unwrap(), orig_events);

        // Verify the clone's status shows "done" while the original is also "done".
        let req = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "sim.status",
            "params": {"session_id": clone_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["state"], "done");
    }

    #[test]
    fn test_sim_reset_clears_state_preserves_scenario() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Create a session and load a scenario.
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let resp = rpc_call(&mut stream, &req);
        let session_id = resp["result"]["session_id"].as_u64().unwrap();

        let scenario_toml = r#"
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
data = "ping"
"#;
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "scenario.load_inline",
            "params": {"session_id": session_id, "toml": scenario_toml},
        });
        rpc_call(&mut stream, &req);

        // Run the simulation — it advances time.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "sim.run",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["exit_code"], json!(0));
        let first_n_events = resp["result"]["n_events"].as_u64().unwrap();
        assert!(first_n_events > 0, "should have trace events");

        // Check status — time is non-zero, state is "done".
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "sim.status",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["state"], "done");
        assert!(resp["result"]["now_ticks"].as_u64().unwrap() > 0);

        // Reset the session.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "sim.reset",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["state"], "ready");
        assert_eq!(resp["result"]["now_ticks"], json!(0));

        // Run again — should produce the SAME trace as before.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "sim.run",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["exit_code"], json!(0));
        // Same number of events — deterministic replay.
        assert_eq!(resp["result"]["n_events"].as_u64().unwrap(), first_n_events);

        // Status should be back to "done".
        let req = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "sim.status",
            "params": {"session_id": session_id},
        });
        let resp = rpc_call(&mut stream, &req);
        assert_eq!(resp["result"]["state"], "done");
    }

    #[test]
    fn test_session_ttl_expiry_destroys_idle_sessions() {
        // Test the cleanup mechanism directly — backdate a session's
        // last_activity to simulate TTL expiry.
        let server = Server::new(Duration::from_secs(5));

        // Create two sessions.
        let session_id_1 = server.next_id.fetch_add(1, Ordering::SeqCst);
        let session_id_2 = server.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut sessions = server.sessions.lock().unwrap();
            let now = Instant::now();
            sessions.insert(
                session_id_1,
                Session {
                    id: session_id_1,
                    state: SessionState::Idle,
                    world: None,
                    scenario: None,
                    board_config_toml: None,
                    trace_human: vec![],
                    trace_jsonl: vec![],
                    scenario_summary: None,
                    started_at: None,
                    n_events: 0,
                    exit_code: 0,
                    error_message: None,
                    app_sources: None,
                    app_includes: None,
                    zephyr_config_dir: None,
                    last_activity: now, // recently active
                },
            );
            // Backdate session 2 by 10 seconds (> 5s TTL).
            sessions.insert(
                session_id_2,
                Session {
                    id: session_id_2,
                    state: SessionState::Idle,
                    world: None,
                    scenario: None,
                    board_config_toml: None,
                    trace_human: vec![],
                    trace_jsonl: vec![],
                    scenario_summary: None,
                    started_at: None,
                    n_events: 0,
                    exit_code: 0,
                    error_message: None,
                    app_sources: None,
                    app_includes: None,
                    zephyr_config_dir: None,
                    last_activity: now - Duration::from_secs(10),
                },
            );
        }

        // Run cleanup — session 2 should be removed.
        let removed = server.cleanup_expired_sessions();
        assert_eq!(removed, 1, "one expired session should be removed");

        let sessions = server.sessions.lock().unwrap();
        assert!(
            sessions.contains_key(&session_id_1),
            "active session should survive"
        );
        assert!(
            !sessions.contains_key(&session_id_2),
            "expired session should be removed"
        );
    }
}
