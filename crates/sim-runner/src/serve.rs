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

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sim_world::scenario::Scenario;
use sim_world::{drive_world, RunLimit, RunTermination, SessionState, World};

/// JSON-RPC 2.0 standard error codes.
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    /// Reserved for future use.
    #[allow(dead_code)]
    pub const INTERNAL_ERROR: i64 = -32603;

    // Application errors (-32000 to -32099).
    pub const SESSION_NOT_FOUND: i64 = -32000;
    /// Reserved for future use.
    #[allow(dead_code)]
    pub const SESSION_IN_USE: i64 = -32001;
    pub const NO_SCENARIO_LOADED: i64 = -32002;
    pub const SIM_ALREADY_RUNNING: i64 = -32003;
    pub const SIM_ERROR: i64 = -32004;
    pub const INVALID_FORMAT: i64 = -32005;
    pub const SCENARIO_PARSE_ERROR: i64 = -32006;
    /// Server error codes (-32010 to -32019).
    pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32010;
}

/// The JSON-RPC protocol version. Incremented on breaking RPC changes.
pub const PROTOCOL_VERSION: u64 = 1;

/// Maximum retained trace records per session (ring buffer, matches gRPC).
pub const MAX_TRACE_RECORDS: usize = 100_000;

/// A managed simulation session.
struct Session {
    id: u64,
    state: SessionState,
    world: Option<World>,
    /// The parsed scenario, preserved for clone/reset.
    scenario: Option<Scenario>,
    /// Board config TOML string, preserved for clone.
    board_config_toml: Option<String>,
    /// Retained trace records (ring buffer, capped at [`MAX_TRACE_RECORDS`]).
    traces: VecDeque<String>,
    /// Count of trace records evicted from the ring.
    dropped_trace_records: u64,
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

impl Session {
    /// Append trace records into the retained ring buffer, evicting the oldest
    /// records past [`MAX_TRACE_RECORDS`] and counting the drops.
    fn push_traces<I: IntoIterator<Item = String>>(&mut self, lines: I) {
        for line in lines {
            if self.traces.len() >= MAX_TRACE_RECORDS {
                self.traces.pop_front();
                self.dropped_trace_records += 1;
            }
            self.traces.push_back(line);
        }
    }

    /// Drain all trace records into a Vec (for API responses that need Vec).
    fn traces_vec(&self) -> Vec<String> {
        self.traces.iter().cloned().collect()
    }
}

#[derive(Debug, Clone)]
struct ScenarioSummary {
    n_machines: usize,
    n_links: usize,
    n_injections: usize,
}

/// Maximum number of concurrent sessions (matches gRPC limit).
pub const MAX_SESSIONS: usize = 128;

/// The JSON-RPC server state shared across transport threads.
pub struct Server {
    sessions: Mutex<BTreeMap<u64, Session>>,
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
            sessions: Mutex::new(BTreeMap::new()),
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
        "protocol_version": PROTOCOL_VERSION,
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
        },
        "protocol_version": PROTOCOL_VERSION,
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
        "server.version" => handle_server_version(server, &id, &params),
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
fn get_session<'a>(
    sessions: &'a mut BTreeMap<u64, Session>,
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
    if sessions.len() >= MAX_SESSIONS {
        return Err(rpc_error(
            id,
            error_codes::INVALID_REQUEST,
            &format!("session limit reached (max {})", MAX_SESSIONS),
            None,
        ));
    }
    sessions.insert(
        session_id,
        Session {
            id: session_id,
            state: SessionState::Idle,
            world: None,
            scenario: None,
            board_config_toml: None,
            traces: VecDeque::new(),
            dropped_trace_records: 0,
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

    let mut world = scenario.build_world().map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to build world: {}", e),
            None,
        )
    })?;
    world.enable_owned_device_banks();

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

    let mut world = scenario.build_world().map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to build world: {}", e),
            None,
        )
    })?;
    world.enable_owned_device_banks();

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
    // Take the World out under a brief map lock so the global map lock is NOT
    // held during the simulation (take/return pattern, mirroring gRPC).
    let (mut world, started_at) = {
        let mut sessions = server.sessions.lock().unwrap();
        let session = get_session(&mut sessions, session_id, id)?;
        session.last_activity = Instant::now();
        if session.state == SessionState::Running {
            return Err(rpc_error(
                id,
                error_codes::SIM_ALREADY_RUNNING,
                "simulation is already running",
                None,
            ));
        }
        let world = match session.world.take() {
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
        let started = Instant::now();
        session.state = SessionState::Running;
        session.started_at = Some(started);
        (world, started)
    };

    // Run the simulation without holding the map lock.
    let outcome = drive_world(&mut world, RunLimit::ToCompletion);

    match outcome.termination {
        RunTermination::Error | RunTermination::Panic => {
            let msg = outcome
                .error
                .unwrap_or_else(|| "simulation error".to_string());
            let mut sessions = server.sessions.lock().unwrap();
            let session = get_session(&mut sessions, session_id, id)?;
            session.world = Some(world);
            session.state = SessionState::Error;
            session.exit_code = 1;
            session.error_message = Some(msg.clone());
            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 1,
                    "n_events": 0,
                    "trace_jsonl": [],
                    "error": msg,
                    "duration_ms": 0,
                }),
            ))
        }
        _ => {
            let duration_ms = started_at.elapsed().as_millis() as u64;
            let traces = world.drain_all_traces();
            let mut sessions = server.sessions.lock().unwrap();
            let session = get_session(&mut sessions, session_id, id)?;
            session.world = Some(world);
            session.push_traces(traces.clone());
            session.n_events = traces.len() as u64;
            session.state = SessionState::Done;
            session.exit_code = 0;
            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 0,
                    "n_events": traces.len(),
                    "trace_jsonl": session.traces_vec(),
                    "duration_ms": duration_ms,
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

    let mut world = {
        let mut sessions = server.sessions.lock().unwrap();
        let session = get_session(&mut sessions, session_id, id)?;
        let world = match session.world.take() {
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
        world
    };

    let outcome = drive_world(&mut world, RunLimit::Until(deadline));
    if matches!(
        outcome.termination,
        RunTermination::Error | RunTermination::Panic
    ) {
        let msg = outcome
            .error
            .unwrap_or_else(|| "simulation error".to_string());
        let mut sessions = server.sessions.lock().unwrap();
        let session = get_session(&mut sessions, session_id, id)?;
        session.world = Some(world);
        session.state = SessionState::Error;
        session.error_message = Some(msg.clone());
        return Err(rpc_error(
            id,
            error_codes::SIM_ERROR,
            &format!("simulation error: {}", msg),
            None,
        ));
    }
    let traces = world.drain_all_traces();
    let now_ticks = world.now;
    let all_idle = world.all_idle();

    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;
    session.world = Some(world);
    session.push_traces(traces.clone());
    if all_idle {
        session.state = SessionState::Done;
    }

    Ok(rpc_response(
        id,
        json!({
            "now_ticks": now_ticks,
            "all_idle": all_idle,
            "n_events": traces.len(),
            "trace_jsonl": session.traces_vec(),
        }),
    ))
}

fn handle_sim_step(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let n_ticks = params.get("n_ticks").and_then(|v| v.as_u64()).unwrap_or(1);

    let mut world = {
        let mut sessions = server.sessions.lock().unwrap();
        let session = get_session(&mut sessions, session_id, id)?;
        let world = match session.world.take() {
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
        if session.state != SessionState::Running {
            session.state = SessionState::Running;
        }
        world
    };

    let start_ticks = world.now;
    let deadline = start_ticks.saturating_add(n_ticks);

    let outcome = drive_world(&mut world, RunLimit::Until(deadline));
    if matches!(
        outcome.termination,
        RunTermination::Error | RunTermination::Panic
    ) {
        let msg = outcome
            .error
            .unwrap_or_else(|| "simulation error".to_string());
        let mut sessions = server.sessions.lock().unwrap();
        let session = get_session(&mut sessions, session_id, id)?;
        session.world = Some(world);
        session.state = SessionState::Error;
        session.error_message = Some(msg.clone());
        return Err(rpc_error(
            id,
            error_codes::SIM_ERROR,
            &format!("simulation error: {}", msg),
            None,
        ));
    }
    let traces = world.drain_all_traces();
    let now_ticks = world.now;
    let all_idle = world.all_idle();

    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;
    session.world = Some(world);
    let new_events: Vec<String> = traces.into_iter().collect();
    if all_idle {
        session.state = SessionState::Done;
    }

    Ok(rpc_response(
        id,
        json!({
            "state": session.state,
            "now_ticks": now_ticks,
            "new_events": new_events,
        }),
    ))
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
    // TODO(Stage A follow-up): sim.stop sets the session back to Ready
    // but the underlying world is now WorldRunState::Stopped.  A subsequent
    // sim.run on this session will see a stopped world and return a
    // successful (but empty) result.  Decide whether:
    //   (a) stop is terminal — sim.run after stop returns a clear
    //       "world stopped" result or error,
    //   (b) stop rebuilds from the stored scenario so the session is
    //       genuinely Ready, or
    //   (c) this is fine for Stage A and a separate sim.restart method
    //       will be added later.
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
    let machine_id: Option<u64> = params.get("machine_id").and_then(|v| v.as_u64());

    // Parse the board config.
    let board_cfg = sim_world::BoardConfig::from_str(config_toml).map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to parse board config: {}", e),
            None,
        )
    })?;

    // Resolve target machine and configure its board inside its device context.
    let mut sessions = server.sessions.lock().unwrap();
    let session = get_session(&mut sessions, session_id, id)?;
    let n_peripherals = match session.world.as_mut() {
        Some(world) => {
            let target = match machine_id {
                Some(mid) => mid,
                None => {
                    // Backwards compat: one-machine world → auto-select.
                    let ids: Vec<u64> = world.machine_ids().collect();
                    match ids.len() {
                        0 => {
                            return Err(rpc_error(
                                id,
                                error_codes::INVALID_PARAMS,
                                "no machines in world; specify machine_id",
                                None,
                            ));
                        }
                        1 => ids[0],
                        _ => {
                            return Err(rpc_error(
                                id,
                                error_codes::INVALID_PARAMS,
                                "multiple machines in world; specify machine_id",
                                None,
                            ));
                        }
                    }
                }
            };
            world
                .configure_machine_board(target, board_cfg)
                .map_err(|e| {
                    rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("board configure failed: {}", e),
                        None,
                    )
                })?
        }
        None => {
            return Err(rpc_error(
                id,
                error_codes::NO_SCENARIO_LOADED,
                "no scenario loaded in this session",
                None,
            ));
        }
    };

    session.board_config_toml = Some(config_toml.to_string());
    session.last_activity = Instant::now();

    Ok(rpc_response(
        id,
        json!({
            "n_peripherals": n_peripherals,
        }),
    ))
}

fn handle_trace_get(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let _format = params
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

    // Both "human" and "jsonl" formats return the same trace lines (now a single
    // ring-buffered store). Format distinction preserved for API compatibility.
    let trace = session.traces_vec().join("\n");

    Ok(rpc_response(id, json!({ "trace": trace })))
}

fn handle_server_shutdown(server: &Server, id: &Value, _params: &Value) -> Result<Value, Value> {
    server.request_shutdown();
    Ok(rpc_response(id, json!({"shutdown": true})))
}

fn handle_server_version(_server: &Server, id: &Value, _params: &Value) -> Result<Value, Value> {
    Ok(rpc_response(
        id,
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "protocol_version": PROTOCOL_VERSION,
        }),
    ))
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
            let mut world = cloned.build_world().map_err(|e| {
                rpc_error(
                    id,
                    error_codes::SCENARIO_PARSE_ERROR,
                    &format!("failed to build world for clone: {}", e),
                    None,
                )
            })?;
            world.enable_owned_device_banks();
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
            traces: VecDeque::new(),
            dropped_trace_records: 0,
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
    let mut world = scenario.build_world().map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to rebuild world: {}", e),
            None,
        )
    })?;
    world.enable_owned_device_banks();

    // Clear all transient state.
    session.world = Some(world);
    session.state = SessionState::Ready;
    session.traces.clear();
    session.dropped_trace_records = 0;
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

    // Take the world out under a brief map lock.
    let (mut world, started_at) = {
        let mut sessions = server.sessions.lock().unwrap();
        let session = get_session(&mut sessions, session_id, id)?;
        session.last_activity = Instant::now();

        if session.state == SessionState::Running {
            return Err(rpc_error(
                id,
                error_codes::SIM_ALREADY_RUNNING,
                "simulation is already running",
                None,
            ));
        }

        let world = match session.world.take() {
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

        let started = Instant::now();
        session.state = SessionState::Running;
        session.started_at = Some(started);
        (world, started)
    };

    // Run without holding the map lock.
    let outcome = drive_world(&mut world, RunLimit::ToCompletion);

    match outcome.termination {
        RunTermination::Error | RunTermination::Panic => {
            let msg = outcome
                .error
                .unwrap_or_else(|| "simulation error".to_string());
            let error_event = json!({
                "event": "trace.stream.error",
                "error": msg,
            });
            let _ = writeln!(
                writer,
                "{}",
                serde_json::to_string(&error_event).unwrap_or_default()
            );
            let _ = writer.flush();

            let mut sessions = server.sessions.lock().unwrap();
            let session = get_session(&mut sessions, session_id, id)?;
            session.world = Some(world);
            session.state = SessionState::Error;
            session.exit_code = 1;
            session.error_message = Some(msg.clone());

            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 1,
                    "n_events": 0,
                    "error": msg,
                    "duration_ms": 0,
                }),
            ))
        }
        _ => {
            let duration_ms = started_at.elapsed().as_millis() as u64;
            let traces = world.drain_all_traces();

            // Write each trace event as NDJSON without holding the map lock.
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

            // Re-lock and return world + store results.
            let mut sessions = server.sessions.lock().unwrap();
            let session = get_session(&mut sessions, session_id, id)?;
            session.world = Some(world);
            session.push_traces(traces.clone());
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
    use std::io::{BufRead, BufReader, Read, Write};
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
                    traces: VecDeque::new(),
                    dropped_trace_records: 0,
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
                    traces: VecDeque::new(),
                    dropped_trace_records: 0,
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

    // ── JSON-RPC owned banks isolation test ──────────────────────────

    #[test]
    fn jsonrpc_two_sessions_run_independently() {
        // Prove that two sessions in the same server run independently:
        // loading the same scenario into both and running them to completion
        // does not cross-contaminate state between sessions.
        //
        // This validates session-level isolation in the JSON-RPC server
        // (owned device banks protect cross-machine access *within* a world;
        //  separate sessions / worlds protect cross-session access).
        //
        // TODO: add a stronger jsonrpc test that configures and uses device
        // ID 0 (e.g. CAN controller 0) in two sessions and asserts no
        // cross-contamination at the device level, as a complement to the
        // existing sim-world `two_worlds_owned_can_interleave_100x` test.
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Create two sessions.
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let sid1 = rpc_call(&mut stream, &req)["result"]["session_id"]
            .as_u64()
            .unwrap();

        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "session.create", "params": {}});
        let sid2 = rpc_call(&mut stream, &req)["result"]["session_id"]
            .as_u64()
            .unwrap();

        // Load identical scenarios into both sessions.
        let scenario_toml = r#"
name = "minimal"
[[machine]]
id = 0
name = "m0"
"#;
        let req = json!({
            "jsonrpc": "2.0", "id": 3, "method": "scenario.load_inline",
            "params": {"session_id": sid1, "toml": scenario_toml},
        });
        rpc_call(&mut stream, &req);

        let req = json!({
            "jsonrpc": "2.0", "id": 4, "method": "scenario.load_inline",
            "params": {"session_id": sid2, "toml": scenario_toml},
        });
        rpc_call(&mut stream, &req);

        // Run both sessions to completion.
        let req =
            json!({"jsonrpc": "2.0", "id": 5, "method": "sim.run", "params": {"session_id": sid1}});
        let r1 = rpc_call(&mut stream, &req);
        assert_eq!(r1["result"]["exit_code"], json!(0));

        let req =
            json!({"jsonrpc": "2.0", "id": 6, "method": "sim.run", "params": {"session_id": sid2}});
        let r2 = rpc_call(&mut stream, &req);
        assert_eq!(r2["result"]["exit_code"], json!(0));

        // Both sessions should be in "done" state independently.
        let req = json!({"jsonrpc": "2.0", "id": 7, "method": "sim.status", "params": {"session_id": sid1}});
        assert_eq!(rpc_call(&mut stream, &req)["result"]["state"], "done");

        let req = json!({"jsonrpc": "2.0", "id": 8, "method": "sim.status", "params": {"session_id": sid2}});
        assert_eq!(rpc_call(&mut stream, &req)["result"]["state"], "done");
    }

    #[test]
    fn jsonrpc_session_list_is_deterministic() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Create 5 sessions in order.
        let mut ids = Vec::new();
        for i in 0..5 {
            let req = json!({"jsonrpc": "2.0", "id": i, "method": "session.create", "params": {}});
            let resp = rpc_call(&mut stream, &req);
            ids.push(resp["result"]["session_id"].as_u64().unwrap());
        }

        // List must be in ascending id order (BTreeMap guarantees this).
        let req = json!({"jsonrpc": "2.0", "id": 10, "method": "session.list", "params": {}});
        let resp = rpc_call(&mut stream, &req);
        let list = resp["result"].as_array().unwrap();
        assert_eq!(list.len(), 5);

        let listed_ids: Vec<u64> = list
            .iter()
            .map(|s| s["session_id"].as_u64().unwrap())
            .collect();
        assert!(
            listed_ids.windows(2).all(|w| w[0] < w[1]),
            "session list must be in ascending id order, got {:?}",
            listed_ids
        );
    }

    #[test]
    fn jsonrpc_trace_ring_eviction_reports_dropped_count() {
        // Test that the trace ring buffer evicts old records and counts drops.
        let server = Server::new(Duration::from_secs(300));
        let sid = server.next_id.fetch_add(1, Ordering::SeqCst);

        // Create a session and stuff it with more traces than MAX_TRACE_RECORDS.
        {
            let mut sessions = server.sessions.lock().unwrap();
            sessions.insert(
                sid,
                Session {
                    id: sid,
                    state: SessionState::Idle,
                    world: None,
                    scenario: None,
                    board_config_toml: None,
                    traces: VecDeque::new(),
                    dropped_trace_records: 0,
                    scenario_summary: None,
                    started_at: None,
                    n_events: 0,
                    exit_code: 0,
                    error_message: None,
                    app_sources: None,
                    app_includes: None,
                    zephyr_config_dir: None,
                    last_activity: Instant::now(),
                },
            );
        }

        // Push more than MAX_TRACE_RECORDS items.
        let total = MAX_TRACE_RECORDS + 10;
        {
            let mut sessions = server.sessions.lock().unwrap();
            let session = sessions.get_mut(&sid).unwrap();
            let traces: Vec<String> = (0..total).map(|i| format!("line_{}", i)).collect();
            session.push_traces(traces);
        }

        // Verify the ring behavior.
        let sessions = server.sessions.lock().unwrap();
        let session = sessions.get(&sid).unwrap();
        assert_eq!(
            session.traces.len(),
            MAX_TRACE_RECORDS,
            "ring buffer must cap at MAX_TRACE_RECORDS"
        );
        assert_eq!(
            session.dropped_trace_records, 10,
            "must count 10 dropped records"
        );
        // The earliest surviving line should be line_10 (first 10 lines evicted).
        assert_eq!(session.traces.front().unwrap(), "line_10");
        // The last line should be the last one pushed.
        assert_eq!(
            session.traces.back().unwrap(),
            &format!("line_{}", total - 1)
        );
    }

    #[test]
    fn jsonrpc_trace_stream_does_not_hold_global_lock() {
        // Regression: trace.stream must not hold the sessions map lock while
        // the simulation runs. We prove this by calling trace.stream on one
        // session and then querying another session's status — both succeed.
        //
        // trace.stream uses the take/run/return pattern via drive_world:
        // the world is taken out, the map lock is released, the simulation
        // runs, and the world is returned.
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        // Create two sessions.
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
        let sid1 = rpc_call(&mut stream, &req)["result"]["session_id"]
            .as_u64()
            .unwrap();

        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "session.create", "params": {}});
        let sid2 = rpc_call(&mut stream, &req)["result"]["session_id"]
            .as_u64()
            .unwrap();

        // Load a scenario into session 1 — includes a link injection that
        // produces trace events.
        let scenario_toml = r#"
name = "minimal"
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
            "jsonrpc": "2.0", "id": 3, "method": "scenario.load_inline",
            "params": {"session_id": sid1, "toml": scenario_toml},
        });
        rpc_call(&mut stream, &req);

        // Call trace.stream on session 1 — the handler writes NDJSON
        // trace events and a "trace.stream.done" event to the stream
        // before returning the final JSON-RPC response.
        let req_str = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "trace.stream",
            "params": {"session_id": sid1},
        }))
        .unwrap()
            + "\n";
        stream.write_all(req_str.as_bytes()).unwrap();
        stream.flush().unwrap();

        // Read NDJSON lines until the JSON-RPC response.
        let (stream_lines, final_response) = {
            let mut reader = BufReader::new(&mut stream);
            let mut lines_buf: Vec<String> = Vec::new();
            let mut resp: Option<Value> = None;
            for line in reader.by_ref().lines() {
                let line = line.unwrap();
                if line.trim().is_empty() {
                    continue;
                }
                if line.contains("\"jsonrpc\":\"2.0\"") {
                    resp = Some(serde_json::from_str(&line).unwrap());
                    break;
                }
                lines_buf.push(line);
            }
            (lines_buf, resp)
        };

        let r1 = final_response.expect("trace.stream must return a JSON-RPC response");
        assert_eq!(
            r1["result"]["exit_code"],
            json!(0),
            "trace.stream must complete with exit_code 0"
        );

        // The stream output must include trace.stream.done.
        let has_done = stream_lines.iter().any(|l| l.contains("trace.stream.done"));
        assert!(
            has_done,
            "stream output must include trace.stream.done event"
        );

        // Session 2 can still be queried — proving the map lock was
        // released during the trace.stream execution.
        let req = json!({"jsonrpc": "2.0", "id": 5, "method": "sim.status", "params": {"session_id": sid2}});
        let r2 = rpc_call(&mut stream, &req);
        assert_eq!(r2["result"]["state"], "idle");

        // Session 1 should be in "done" state after a successful stream.
        let req = json!({"jsonrpc": "2.0", "id": 6, "method": "sim.status", "params": {"session_id": sid1}});
        let r3 = rpc_call(&mut stream, &req);
        assert_eq!(
            r3["result"]["state"], "done",
            "session 1 must be 'done' after trace.stream completes"
        );
    }
}
