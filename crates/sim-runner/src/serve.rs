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

mod run_loop;
mod transport;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sim_world::firmware::FirmwareFactory;
use sim_world::scenario::Scenario;
use sim_world::{drive_world, RunLimit, RunTermination, SessionState, World};

use run_loop::{drive_cooperative, ConnectionLiveness, RunControl, DEFAULT_TICK_BATCH};

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
    /// Rejected `session.destroy` while a cooperative worker holds the world.
    pub const SESSION_IN_USE: i64 = -32001;
    pub const NO_SCENARIO_LOADED: i64 = -32002;
    pub const SIM_ALREADY_RUNNING: i64 = -32003;
    pub const SIM_ERROR: i64 = -32004;
    #[allow(dead_code)]
    pub const INVALID_FORMAT: i64 = -32005;
    pub const SCENARIO_PARSE_ERROR: i64 = -32006;
    /// Server error codes (-32010 to -32019).
    pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32010;
}

/// The JSON-RPC protocol version. Incremented on breaking RPC changes.
pub const PROTOCOL_VERSION: u64 = 1;

/// Maximum retained trace records per session (ring buffer, matches gRPC).
pub const MAX_TRACE_RECORDS: usize = 100_000;

/// Registry mapping scenario firmware paths to factories.
#[derive(Default, Clone)]
pub struct FirmwareRegistry {
    factories: HashMap<String, FirmwareFactory>,
}

impl FirmwareRegistry {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub fn register(&mut self, path: &str, factory: FirmwareFactory) {
        self.factories.insert(path.to_string(), factory);
    }

    pub fn get(&self, path: &str) -> Option<&FirmwareFactory> {
        self.factories.get(path)
    }
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
    /// Control channel for an in-flight cooperative run (stop / disconnect).
    run_control: Option<Arc<RunControl>>,
}

impl Session {
    fn new(id: u64) -> Self {
        Self {
            id,
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
            run_control: None,
        }
    }

    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Whether this session is exempt from idle-TTL cleanup.
    fn ttl_exempt(&self) -> bool {
        matches!(self.state, SessionState::Running | SessionState::Paused)
    }

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
/// Minimum host interval between automatic cleanup passes (matches gRPC).
pub const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

/// The JSON-RPC server state shared across transport threads.
///
/// The map holds `Arc<Mutex<Session>>` values. The map lock is used to look up,
/// insert, or remove sessions, and is also held briefly during atomic run
/// checkout so destroy cannot remove a session between lookup and Running.
/// Per-session work then locks exactly one session without holding the map lock
/// during simulation.
///
/// All TCP connections share one [`Server`] so sibling clients can stop or
/// inspect sessions while another connection runs a cooperative batch loop.
pub struct Server {
    sessions: Mutex<BTreeMap<u64, Arc<Mutex<Session>>>>,
    next_id: AtomicU64,
    shutdown: Mutex<bool>,
    /// Session idle TTL — sessions with no activity for this long are auto-destroyed.
    session_ttl: Duration,
    /// Last time expired-session cleanup was performed.
    last_cleanup: Mutex<Instant>,
    /// Optional firmware factories applied when loading scenarios.
    firmware_registry: Mutex<Option<FirmwareRegistry>>,
    /// Test-only hook invoked during run checkout before the world is taken.
    #[cfg(test)]
    run_checkout_hook: Mutex<Option<RunCheckoutHook>>,
}

#[cfg(test)]
type RunCheckoutHook = Arc<dyn Fn(u64) + Send + Sync>;

impl Server {
    pub fn new(session_ttl: Duration) -> Self {
        Server {
            sessions: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            shutdown: Mutex::new(false),
            session_ttl,
            last_cleanup: Mutex::new(Instant::now()),
            firmware_registry: Mutex::new(None),
            #[cfg(test)]
            run_checkout_hook: Mutex::new(None),
        }
    }

    /// Attach a firmware registry used by subsequent scenario loads.
    #[allow(dead_code)]
    pub fn set_firmware_registry(&self, registry: FirmwareRegistry) {
        *self
            .firmware_registry
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(registry);
    }

    /// Check if shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        *self.shutdown.lock().unwrap()
    }

    /// Set the shutdown flag.
    fn request_shutdown(&self) {
        *self.shutdown.lock().unwrap() = true;
    }

    /// Look up a session Arc. Holds the map lock only for the lookup.
    fn get_arc(&self, session_id: u64, id: &Value) -> Result<Arc<Mutex<Session>>, Value> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(&session_id).cloned().ok_or_else(|| {
            rpc_error(
                id,
                error_codes::SESSION_NOT_FOUND,
                &format!("session {} not found", session_id),
                None,
            )
        })
    }

    /// Destroy sessions that have been idle longer than the TTL.
    ///
    /// Running and Paused sessions are never TTL-expired.
    ///
    /// Returns the number of sessions removed.
    pub fn cleanup_expired_sessions(&self) -> usize {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap();
        let ttl = self.session_ttl;
        let before = sessions.len();
        sessions.retain(|_id, arc| {
            let s = arc.lock().unwrap();
            s.ttl_exempt() || now.duration_since(s.last_activity) < ttl
        });
        before - sessions.len()
    }

    /// Run cleanup if enough time has passed since the last run.
    ///
    /// Rate-limited to at most once per [`CLEANUP_INTERVAL`] host seconds.
    pub fn maybe_cleanup_expired_sessions(&self) {
        self.cleanup_expired_sessions_inner(false);
    }

    /// Force a cleanup pass (used on create/list).
    pub fn force_cleanup_expired_sessions(&self) {
        self.cleanup_expired_sessions_inner(true);
    }

    fn cleanup_expired_sessions_inner(&self, force: bool) {
        {
            let mut last = self.last_cleanup.lock().unwrap();
            if !force && last.elapsed() < CLEANUP_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        let removed = self.cleanup_expired_sessions();
        if removed > 0 {
            eprintln!("ttl cleanup: removed {} expired session(s)", removed);
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
/// to send (or None for notifications / silent disconnect completion).
///
/// For methods that produce streaming output (e.g. `trace.stream`), the
/// handler writes NDJSON lines directly to `writer` before returning the
/// final response.
///
/// `liveness` is probed by long-running handlers between cooperative batches
/// so TCP disconnect can pause an active `sim.run`. Stdio passes an
/// always-connected stub (no mid-request EOF detection).
fn dispatch(
    server: &Server,
    request: &Value,
    writer: &mut dyn std::io::Write,
    liveness: &mut dyn ConnectionLiveness,
) -> Option<Value> {
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
        "session.create" => handle_session_create(server, &id, &params).map(Some),
        "session.destroy" => handle_session_destroy(server, &id, &params).map(Some),
        "session.clone" => handle_session_clone(server, &id, &params).map(Some),
        "session.list" => handle_session_list(server, &id, &params).map(Some),
        "scenario.load" => handle_scenario_load(server, &id, &params).map(Some),
        "scenario.load_inline" => handle_scenario_load_inline(server, &id, &params).map(Some),
        "sim.run" => handle_sim_run(server, &id, &params, liveness),
        "sim.run_until" => handle_sim_run_until(server, &id, &params).map(Some),
        "sim.step" => handle_sim_step(server, &id, &params).map(Some),
        "sim.reset" => handle_sim_reset(server, &id, &params).map(Some),
        "sim.status" => handle_sim_status(server, &id, &params).map(Some),
        "sim.stop" => handle_sim_stop(server, &id, &params).map(Some),
        "board.configure" => handle_board_configure(server, &id, &params).map(Some),
        "trace.get" => handle_trace_get(server, &id, &params).map(Some),
        "trace.stream" => handle_trace_stream(server, &id, &params, writer).map(Some),
        "server.shutdown" => handle_server_shutdown(server, &id, &params).map(Some),
        "server.version" => handle_server_version(server, &id, &params).map(Some),
        _ => Err(rpc_error(
            &id,
            error_codes::METHOD_NOT_FOUND,
            &format!("method not found: {}", method),
            None,
        )),
    };

    match result {
        Ok(Some(resp)) => {
            if id.is_null() {
                None // notification
            } else {
                Some(resp)
            }
        }
        Ok(None) => None, // disconnect — world already returned as Paused
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

/// Collect firmware factories for machines in `scenario` under the registry lock.
fn firmware_factories_for(server: &Server, scenario: &Scenario) -> Vec<(u64, FirmwareFactory)> {
    let guard = server
        .firmware_registry
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(ref registry) = *guard else {
        return Vec::new();
    };
    scenario
        .machine
        .iter()
        .filter_map(|m| {
            m.firmware
                .as_ref()
                .and_then(|path| registry.get(path).map(|factory| (m.id, factory.clone())))
        })
        .collect()
}

/// Apply registered firmware factories to machines that declare a `firmware` path.
///
/// Factory invocation is isolated with `catch_unwind`; a panicking factory returns
/// `Err` without mutating machines beyond any successfully loaded firmware.
fn apply_firmware_registry(
    server: &Server,
    scenario: &Scenario,
    world: &mut World,
) -> Result<(), String> {
    let factories = firmware_factories_for(server, scenario);
    for (machine_id, factory) in factories {
        let Some(machine) = world.machine_mut(machine_id) else {
            continue;
        };
        machine.set_firmware_factory(factory.clone());
        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory()));
        match loaded {
            Ok(firmware) => machine.load_firmware(firmware),
            Err(payload) => {
                return Err(format!(
                    "firmware factory panicked for machine {machine_id}: {}",
                    firmware_panic_to_string(payload)
                ));
            }
        }
    }
    Ok(())
}

fn firmware_panic_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "firmware factory panic".to_string()
    }
}

/// Test-only hook invoked while registry + session locks are held during run
/// checkout, before the world is taken and state becomes Running.
#[cfg(test)]
impl Server {
    fn run_checkout_hook_if_set(&self, session_id: u64) {
        let hook = {
            let guard = self.run_checkout_hook.lock().unwrap();
            guard.clone()
        };
        if let Some(hook) = hook {
            hook(session_id);
        }
    }

    fn set_run_checkout_hook(&self, hook: Option<RunCheckoutHook>) {
        *self.run_checkout_hook.lock().unwrap() = hook;
    }
}

#[cfg(test)]
pub struct RunCheckoutHookGuard<'a> {
    server: &'a Server,
}

#[cfg(test)]
impl<'a> RunCheckoutHookGuard<'a> {
    pub fn install(server: &'a Server, hook: RunCheckoutHook) -> RunCheckoutHookGuard<'a> {
        server.set_run_checkout_hook(Some(Arc::clone(&hook)));
        RunCheckoutHookGuard { server }
    }
}

#[cfg(test)]
impl Drop for RunCheckoutHookGuard<'_> {
    fn drop(&mut self) {
        self.server.set_run_checkout_hook(None);
    }
}

/// Checkout the world for synchronous execution. Caller must hold the session lock.
fn checkout_world_from_session(
    session: &mut Session,
    id: &Value,
    control: Option<Arc<RunControl>>,
) -> Result<World, Value> {
    session.touch();
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
    session.state = SessionState::Running;
    session.started_at.get_or_insert_with(Instant::now);
    session.error_message = None;
    session.run_control = control;
    Ok(world)
}

/// Atomically look up a session and check out its world for a run.
///
/// Holds the registry lock until checkout completes so `session.destroy` cannot
/// remove a Ready session between lookup and `SessionState::Running`.
fn checkout_registered_world_for_run(
    server: &Server,
    session_id: u64,
    id: &Value,
    control: Option<Arc<RunControl>>,
) -> Result<(Arc<Mutex<Session>>, World), Value> {
    let sessions = server.sessions.lock().unwrap();
    let arc = sessions.get(&session_id).cloned().ok_or_else(|| {
        rpc_error(
            id,
            error_codes::SESSION_NOT_FOUND,
            &format!("session {} not found", session_id),
            None,
        )
    })?;
    let world = {
        let mut session = arc.lock().unwrap();
        #[cfg(test)]
        server.run_checkout_hook_if_set(session_id);
        checkout_world_from_session(&mut session, id, control)?
    };
    Ok((arc, world))
}

/// Return a world after a bounded or cooperative run and set the terminal state.
fn return_world(
    arc: &Arc<Mutex<Session>>,
    world: World,
    state: SessionState,
    traces: Vec<String>,
    error: Option<String>,
) {
    let mut session = arc.lock().unwrap();
    session.world = Some(world);
    session.push_traces(traces);
    session.state = state;
    session.run_control = None;
    if let Some(msg) = error {
        session.error_message = Some(msg);
        session.exit_code = 1;
    } else if state == SessionState::Done {
        session.exit_code = 0;
    }
    session.touch();
}

// ── Method handlers ───────────────────────────────────────────────────────

fn handle_session_create(server: &Server, id: &Value, _params: &Value) -> Result<Value, Value> {
    server.force_cleanup_expired_sessions();
    let session_id = server.next_id.fetch_add(1, Ordering::SeqCst);
    let mut sessions = server.sessions.lock().unwrap();
    if sessions.len() >= MAX_SESSIONS {
        return Err(rpc_error(
            id,
            error_codes::INVALID_REQUEST,
            &format!("session limit reached (max {})", MAX_SESSIONS),
            None,
        ));
    }
    sessions.insert(session_id, Arc::new(Mutex::new(Session::new(session_id))));
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
    // Registry lock → session lock (never the reverse) so destroy and run
    // checkout cannot race: a Running worker's Arc must remain reachable.
    let mut sessions = server.sessions.lock().unwrap();
    let Some(arc) = sessions.get(&session_id).cloned() else {
        return Err(rpc_error(
            id,
            error_codes::SESSION_NOT_FOUND,
            &format!("session {} not found", session_id),
            None,
        ));
    };
    {
        let session = arc.lock().unwrap();
        if session.state == SessionState::Running || session.run_control.is_some() {
            return Err(rpc_error(
                id,
                error_codes::SESSION_IN_USE,
                &format!(
                    "session {} is in use (state={:?}); stop or wait for completion before destroy",
                    session_id, session.state
                ),
                None,
            ));
        }
    }
    sessions.remove(&session_id);
    Ok(rpc_response(
        id,
        json!({"destroyed": true, "session_id": session_id}),
    ))
}

fn handle_session_list(server: &Server, id: &Value, _params: &Value) -> Result<Value, Value> {
    server.force_cleanup_expired_sessions();
    let sessions = server.sessions.lock().unwrap();
    let list: Vec<Value> = sessions
        .values()
        .map(|arc| {
            let s = arc.lock().unwrap();
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
    apply_firmware_registry(server, &scenario, &mut world).map_err(|e| {
        rpc_error(
            id,
            error_codes::SIM_ERROR,
            &format!("firmware load failed: {}", e),
            None,
        )
    })?;

    let arc = server.get_arc(session_id, id)?;
    let mut session = arc.lock().unwrap_or_else(|e| e.into_inner());
    if session.state == SessionState::Running {
        return Err(rpc_error(
            id,
            error_codes::SIM_ALREADY_RUNNING,
            "session is running",
            None,
        ));
    }
    session.world = Some(world);
    session.scenario = Some(scenario);
    session.state = SessionState::Ready;
    session.scenario_summary = Some(summary.clone());
    session.app_sources = app_sources;
    session.app_includes = app_includes;
    session.zephyr_config_dir = zephyr_config_dir;
    session.touch();

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
    apply_firmware_registry(server, &scenario, &mut world).map_err(|e| {
        rpc_error(
            id,
            error_codes::SIM_ERROR,
            &format!("firmware load failed: {}", e),
            None,
        )
    })?;

    let arc = server.get_arc(session_id, id)?;
    let mut session = arc.lock().unwrap_or_else(|e| e.into_inner());
    if session.state == SessionState::Running {
        return Err(rpc_error(
            id,
            error_codes::SIM_ALREADY_RUNNING,
            "session is running",
            None,
        ));
    }
    session.world = Some(world);
    session.scenario = Some(scenario);
    session.state = SessionState::Ready;
    session.scenario_summary = Some(summary.clone());
    session.app_sources = app_sources;
    session.app_includes = app_includes;
    session.zephyr_config_dir = zephyr_config_dir;
    session.touch();

    Ok(rpc_response(
        id,
        json!({
            "n_machines": summary.n_machines,
            "n_links": summary.n_links,
            "n_injections": summary.n_injections,
        }),
    ))
}

fn handle_sim_run(
    server: &Server,
    id: &Value,
    params: &Value,
    liveness: &mut dyn ConnectionLiveness,
) -> Result<Option<Value>, Value> {
    let session_id = get_session_id(params)?;
    let tick_batch = params
        .get("tick_batch_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TICK_BATCH);
    let control = Arc::new(RunControl::new());
    let started_at = Instant::now();
    let (arc, mut world) =
        checkout_registered_world_for_run(server, session_id, id, Some(Arc::clone(&control)))?;

    let mut disconnected = false;
    let coop = drive_cooperative(&mut world, &control, tick_batch, |_| {
        let ok = liveness.is_connected();
        if !ok {
            disconnected = true;
        }
        ok
    });
    let traces = world.drain_all_traces();
    let duration_ms = started_at.elapsed().as_millis() as u64;

    // Client gone — return the world as Paused and do not write a final
    // response onto the dead connection.
    if disconnected {
        return_world(&arc, world, SessionState::Paused, traces, None);
        return Ok(None);
    }

    match coop.state {
        SessionState::Error => {
            let msg = coop.error.unwrap_or_else(|| "simulation error".to_string());
            return_world(&arc, world, SessionState::Error, traces, Some(msg.clone()));
            Ok(Some(rpc_response(
                id,
                json!({
                    "exit_code": 1,
                    "n_events": 0,
                    "trace_jsonl": [],
                    "error": msg,
                    "duration_ms": duration_ms,
                    "state": "error",
                }),
            )))
        }
        state => {
            let n_events = traces.len();
            {
                let mut session = arc.lock().unwrap();
                session.n_events = n_events as u64;
            }
            return_world(&arc, world, state, traces.clone(), None);
            let traces_vec = {
                let session = arc.lock().unwrap();
                session.traces_vec()
            };
            Ok(Some(rpc_response(
                id,
                json!({
                    "exit_code": if state == SessionState::Error { 1 } else { 0 },
                    "n_events": n_events,
                    "trace_jsonl": traces_vec,
                    "duration_ms": duration_ms,
                    "state": state,
                }),
            )))
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

    let (arc, mut world) = checkout_registered_world_for_run(server, session_id, id, None)?;

    let outcome = drive_world(&mut world, RunLimit::Until(deadline));
    if matches!(
        outcome.termination,
        RunTermination::Error | RunTermination::Panic
    ) {
        let msg = outcome
            .error
            .unwrap_or_else(|| "simulation error".to_string());
        return_world(
            &arc,
            world,
            SessionState::Error,
            Vec::new(),
            Some(msg.clone()),
        );
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
    let state = if all_idle {
        SessionState::Done
    } else {
        SessionState::Paused
    };
    return_world(&arc, world, state, traces.clone(), None);
    let traces_vec = {
        let session = arc.lock().unwrap();
        session.traces_vec()
    };

    Ok(rpc_response(
        id,
        json!({
            "now_ticks": now_ticks,
            "all_idle": all_idle,
            "n_events": traces.len(),
            "trace_jsonl": traces_vec,
            "state": state,
        }),
    ))
}

fn handle_sim_step(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let n_ticks = params.get("n_ticks").and_then(|v| v.as_u64()).unwrap_or(1);

    let (arc, mut world) = checkout_registered_world_for_run(server, session_id, id, None)?;

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
        return_world(
            &arc,
            world,
            SessionState::Error,
            Vec::new(),
            Some(msg.clone()),
        );
        return Err(rpc_error(
            id,
            error_codes::SIM_ERROR,
            &format!("simulation error: {}", msg),
            None,
        ));
    }
    let new_events: Vec<String> = world.drain_all_traces();
    let now_ticks = world.now;
    let all_idle = world.all_idle();
    let state = if all_idle {
        SessionState::Done
    } else {
        SessionState::Paused
    };
    return_world(&arc, world, state, new_events.clone(), None);

    Ok(rpc_response(
        id,
        json!({
            "state": state,
            "now_ticks": now_ticks,
            "new_events": new_events,
        }),
    ))
}

fn handle_sim_status(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let arc = server.get_arc(session_id, id)?;
    let session = arc.lock().unwrap();

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
    let arc = server.get_arc(session_id, id)?;
    let mut session = arc.lock().unwrap();

    // Signal any cooperative worker that owns the world.
    if let Some(ref ctrl) = session.run_control {
        ctrl.request_stop();
    }

    if let Some(ref mut world) = session.world {
        world.stop();
        // Explicit Stop is a terminal Done state (matches gRPC).
        session.state = SessionState::Done;
        session.run_control = None;
    }
    // If the world is checked out, the cooperative loop observes the stop
    // flag between batches, applies world.stop(), and returns Done.
    session.touch();

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

    let board_cfg = sim_world::BoardConfig::from_str(config_toml).map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to parse board config: {}", e),
            None,
        )
    })?;

    let arc = server.get_arc(session_id, id)?;
    let mut session = arc.lock().unwrap();
    if session.state == SessionState::Running {
        return Err(rpc_error(
            id,
            error_codes::SIM_ALREADY_RUNNING,
            "session is running",
            None,
        ));
    }
    let n_peripherals = match session.world.as_mut() {
        Some(world) => {
            let target = match machine_id {
                Some(mid) => mid,
                None => {
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
    session.touch();

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

    let arc = server.get_arc(session_id, id)?;
    let session = arc.lock().unwrap();
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

fn handle_session_clone(server: &Server, id: &Value, params: &Value) -> Result<Value, Value> {
    let session_id = get_session_id(params)?;
    let source_arc = server.get_arc(session_id, id)?;

    let (
        new_world,
        new_scenario,
        summary,
        board_config_toml,
        app_sources,
        app_includes,
        zephyr_config_dir,
    ) = {
        let mut source = source_arc.lock().unwrap();
        if source.state == SessionState::Running {
            return Err(rpc_error(
                id,
                error_codes::SIM_ALREADY_RUNNING,
                "session is running",
                None,
            ));
        }
        let built = match source.scenario.as_ref() {
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
                apply_firmware_registry(server, &cloned, &mut world).map_err(|e| {
                    rpc_error(
                        id,
                        error_codes::SIM_ERROR,
                        &format!("firmware load failed: {}", e),
                        None,
                    )
                })?;
                (Some(world), Some(cloned), Some(summary))
            }
            None => (None, None, None),
        };
        source.touch();
        (
            built.0,
            built.1,
            built.2,
            source.board_config_toml.clone(),
            source.app_sources.clone(),
            source.app_includes.clone(),
            source.zephyr_config_dir.clone(),
        )
    };

    let new_id = server.next_id.fetch_add(1, Ordering::SeqCst);
    let new_state = if new_world.is_some() {
        SessionState::Ready
    } else {
        SessionState::Idle
    };

    let mut new_session = Session::new(new_id);
    new_session.state = new_state;
    new_session.world = new_world;
    new_session.scenario = new_scenario;
    new_session.board_config_toml = board_config_toml;
    new_session.scenario_summary = summary;
    new_session.app_sources = app_sources;
    new_session.app_includes = app_includes;
    new_session.zephyr_config_dir = zephyr_config_dir;

    {
        let mut sessions = server.sessions.lock().unwrap();
        if sessions.len() >= MAX_SESSIONS {
            return Err(rpc_error(
                id,
                error_codes::INVALID_REQUEST,
                &format!("session limit reached (max {})", MAX_SESSIONS),
                None,
            ));
        }
        sessions.insert(new_id, Arc::new(Mutex::new(new_session)));
    }

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
    // Per-session lock is the transaction boundary: hold it across World
    // reconstruction so no sibling request can check out or replace the World
    // mid-reset. Factory panics are caught inside `apply_firmware_registry`, so
    // they cannot unwind through this guard and poison the mutex.
    let arc = server.get_arc(session_id, id)?;
    let mut session = arc.lock().unwrap_or_else(|e| e.into_inner());
    if session.state == SessionState::Running {
        return Err(rpc_error(
            id,
            error_codes::SIM_ALREADY_RUNNING,
            "session is running",
            None,
        ));
    }
    let scenario = session.scenario.clone().ok_or_else(|| {
        rpc_error(
            id,
            error_codes::NO_SCENARIO_LOADED,
            "no scenario loaded in this session — cannot reset",
            None,
        )
    })?;

    // Prepare a full replacement before mutating any session fields so a
    // failed rebuild leaves the previous World and metadata untouched.
    let mut replacement = scenario.build_world().map_err(|e| {
        rpc_error(
            id,
            error_codes::SCENARIO_PARSE_ERROR,
            &format!("failed to rebuild world: {}", e),
            None,
        )
    })?;
    replacement.enable_owned_device_banks();
    apply_firmware_registry(server, &scenario, &mut replacement).map_err(|e| {
        rpc_error(
            id,
            error_codes::SIM_ERROR,
            &format!("firmware load failed: {}", e),
            None,
        )
    })?;

    session.world = Some(replacement);
    session.state = SessionState::Ready;
    session.traces.clear();
    session.dropped_trace_records = 0;
    session.n_events = 0;
    session.exit_code = 0;
    session.error_message = None;
    session.started_at = None;
    session.run_control = None;
    session.touch();

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
    let tick_batch = params
        .get("tick_batch_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TICK_BATCH);
    let control = Arc::new(RunControl::new());
    let started_at = Instant::now();
    let (arc, mut world) =
        checkout_registered_world_for_run(server, session_id, id, Some(Arc::clone(&control)))?;

    let mut retained: Vec<String> = Vec::new();
    let coop = drive_cooperative(&mut world, &control, tick_batch, |world| {
        let batch_traces = world.drain_all_traces();
        for line in &batch_traces {
            let stream_event = json!({
                "event": "trace",
                "data": line,
            });
            if writeln!(
                writer,
                "{}",
                serde_json::to_string(&stream_event).unwrap_or_default()
            )
            .is_err()
                || writer.flush().is_err()
            {
                retained.extend(batch_traces.clone());
                return false;
            }
        }
        retained.extend(batch_traces);
        // Heartbeat so a quiet disconnect is still observed promptly.
        let tick_event = json!({
            "event": "trace.stream.tick",
            "now_ticks": world.now,
        });
        if writeln!(
            writer,
            "{}",
            serde_json::to_string(&tick_event).unwrap_or_default()
        )
        .is_err()
            || writer.flush().is_err()
        {
            return false;
        }
        true
    });

    let duration_ms = started_at.elapsed().as_millis() as u64;
    retained.extend(world.drain_all_traces());
    let n_streamed = retained.len() as u64;

    match coop.state {
        SessionState::Error => {
            let msg = coop.error.unwrap_or_else(|| "simulation error".to_string());
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
            return_world(
                &arc,
                world,
                SessionState::Error,
                retained,
                Some(msg.clone()),
            );
            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 1,
                    "n_events": n_streamed,
                    "error": msg,
                    "duration_ms": duration_ms,
                    "state": "error",
                }),
            ))
        }
        SessionState::Paused => {
            // Client disconnected or cancelled — retain world for resume.
            return_world(&arc, world, SessionState::Paused, retained, None);
            // Do not write a final RPC response if the writer is already dead.
            let _ = writeln!(
                writer,
                "{}",
                serde_json::to_string(&json!({
                    "event": "trace.stream.paused",
                    "n_events": n_streamed,
                    "duration_ms": duration_ms,
                }))
                .unwrap_or_default()
            );
            let _ = writer.flush();
            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 0,
                    "n_events": n_streamed,
                    "duration_ms": duration_ms,
                    "state": "paused",
                }),
            ))
        }
        state => {
            let done_event = json!({
                "event": "trace.stream.done",
                "n_events": n_streamed,
                "duration_ms": duration_ms,
            });
            let _ = writeln!(
                writer,
                "{}",
                serde_json::to_string(&done_event).unwrap_or_default()
            );
            let _ = writer.flush();
            {
                let mut session = arc.lock().unwrap();
                session.n_events = n_streamed;
            }
            return_world(&arc, world, state, retained, None);
            Ok(rpc_response(
                id,
                json!({
                    "exit_code": 0,
                    "n_events": n_streamed,
                    "duration_ms": duration_ms,
                    "state": state,
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

    // One shared Server across all TCP connections so sibling clients can
    // stop / inspect sessions while another connection runs cooperatively.
    let server = Arc::new(Server::new(session_ttl));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
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
    let server = Arc::new(Server::new(session_ttl));
    transport::handle_stdio(&server);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::AtomicBool;

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
    ///
    /// All accepted connections share one [`Server`] so multi-client lifecycle
    /// tests can stop / inspect sibling sessions.
    fn start_server_on_random_port() -> u16 {
        start_server_on_random_port_with_registry(None)
    }

    fn start_server_on_random_port_with_registry(registry: Option<FirmwareRegistry>) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        std::thread::spawn(move || {
            let server = Arc::new(Server::new(Duration::from_secs(300)));
            if let Some(reg) = registry {
                server.set_firmware_registry(reg);
            }
            for stream in listener.incoming().flatten() {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    transport::handle_tcp(server, stream);
                });
            }
        });

        // Give the server thread a moment to start.
        std::thread::sleep(std::time::Duration::from_millis(50));

        port
    }

    fn status_state(stream: &mut TcpStream, session_id: u64, id: u64) -> String {
        let resp = rpc_call(
            stream,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "sim.status",
                "params": {"session_id": session_id},
            }),
        );
        resp["result"]["state"].as_str().unwrap().to_string()
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
                Arc::new(Mutex::new(Session {
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
                    run_control: None,
                    last_activity: now, // recently active
                })),
            );
            // Backdate session 2 by 10 seconds (> 5s TTL).
            sessions.insert(
                session_id_2,
                Arc::new(Mutex::new(Session {
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
                    run_control: None,
                    last_activity: now - Duration::from_secs(10),
                })),
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
                Arc::new(Mutex::new(Session {
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
                    run_control: None,
                    last_activity: Instant::now(),
                })),
            );
        }

        // Push more than MAX_TRACE_RECORDS items.
        let total = MAX_TRACE_RECORDS + 10;
        {
            let sessions = server.sessions.lock().unwrap();
            let mut session = sessions.get(&sid).unwrap().lock().unwrap();
            let traces: Vec<String> = (0..total).map(|i| format!("line_{}", i)).collect();
            session.push_traces(traces);
        }

        // Verify the ring behavior.
        let sessions = server.sessions.lock().unwrap();
        let session = sessions.get(&sid).unwrap().lock().unwrap();
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

    #[test]
    fn jsonrpc_session_limit_rejects_129th() {
        let server = Server::new(Duration::from_secs(300));
        for _ in 0..MAX_SESSIONS {
            let id = Value::Null;
            let resp = handle_session_create(&server, &id, &json!({}));
            assert!(resp.is_ok(), "create within limit should succeed");
        }
        let err = handle_session_create(&server, &Value::Null, &json!({})).unwrap_err();
        assert_eq!(err["error"]["code"], json!(error_codes::INVALID_REQUEST));
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap()
                .contains("session limit"),
            "expected session-limit message, got {}",
            err["error"]["message"]
        );
    }

    #[test]
    fn jsonrpc_ttl_exempts_running_and_paused_not_done() {
        let server = Server::new(Duration::from_secs(5));
        let idle_id = server.next_id.fetch_add(1, Ordering::SeqCst);
        let running_id = server.next_id.fetch_add(1, Ordering::SeqCst);
        let paused_id = server.next_id.fetch_add(1, Ordering::SeqCst);
        let done_id = server.next_id.fetch_add(1, Ordering::SeqCst);
        let now = Instant::now();
        {
            let mut sessions = server.sessions.lock().unwrap();
            for (id, state) in [
                (idle_id, SessionState::Idle),
                (running_id, SessionState::Running),
                (paused_id, SessionState::Paused),
                (done_id, SessionState::Done),
            ] {
                let mut s = Session::new(id);
                s.last_activity = now - Duration::from_secs(10);
                s.state = state;
                sessions.insert(id, Arc::new(Mutex::new(s)));
            }
        }
        let removed = server.cleanup_expired_sessions();
        assert_eq!(removed, 2, "idle + done expire; running/paused exempt");
        let sessions = server.sessions.lock().unwrap();
        assert!(!sessions.contains_key(&idle_id));
        assert!(!sessions.contains_key(&done_id));
        assert!(sessions.contains_key(&running_id));
        assert!(sessions.contains_key(&paused_id));
    }

    /// Pending-work scenario: injection arrives after the first bounded deadline.
    const PENDING_WORK_SCENARIO: &str = r#"
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

    #[test]
    fn jsonrpc_run_until_returns_paused_and_resumes_to_done() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{"session_id":sid,"toml":PENDING_WORK_SCENARIO},
            }),
        );

        let resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":3,"method":"sim.run_until",
                "params":{"session_id":sid,"deadline_ticks":50},
            }),
        );
        assert!(!resp["result"]["all_idle"].as_bool().unwrap());
        assert_eq!(status_state(&mut stream, sid, 4), "paused");

        // Resume with another run_until past the injection.
        let resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":5,"method":"sim.run_until",
                "params":{"session_id":sid,"deadline_ticks":200},
            }),
        );
        assert!(resp["result"]["now_ticks"].as_u64().unwrap() >= 100);
        // After the injection is delivered the world becomes idle → Done.
        assert_eq!(status_state(&mut stream, sid, 6), "done");
    }

    #[test]
    fn jsonrpc_step_returns_paused_and_can_step_again() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{"session_id":sid,"toml":PENDING_WORK_SCENARIO},
            }),
        );

        let resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":3,"method":"sim.step",
                "params":{"session_id":sid,"n_ticks":1},
            }),
        );
        assert_eq!(resp["result"]["state"], "paused");
        assert_eq!(status_state(&mut stream, sid, 4), "paused");

        let resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":5,"method":"sim.step",
                "params":{"session_id":sid,"n_ticks":200},
            }),
        );
        assert!(resp["result"]["now_ticks"].as_u64().unwrap() >= 100);
        assert_eq!(status_state(&mut stream, sid, 6), "done");
    }

    #[test]
    fn jsonrpc_paused_session_ttl_exempt_and_board_ops() {
        let port = start_server_on_random_port();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{"session_id":sid,"toml":PENDING_WORK_SCENARIO},
            }),
        );
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":3,"method":"sim.run_until",
                "params":{"session_id":sid,"deadline_ticks":50},
            }),
        );
        assert_eq!(status_state(&mut stream, sid, 4), "paused");

        // Board configure / reset / clone are allowed from Paused.
        let cfg = r#"
[peripherals.can0]
device = "can"
id = 0
"#;
        let resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":5,"method":"board.configure",
                "params":{"session_id":sid,"machine_id":0,"config_toml":cfg},
            }),
        );
        assert!(
            resp.get("error").is_none(),
            "board.configure from paused: {resp}"
        );

        let resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":6,"method":"session.clone",
                "params":{"session_id":sid},
            }),
        );
        assert!(resp.get("error").is_none(), "clone from paused: {resp}");

        let resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":7,"method":"sim.reset",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(resp["result"]["state"], "ready");
        assert_eq!(status_state(&mut stream, sid, 8), "ready");
    }

    /// Firmware that reschedules itself for a long cooperative run.
    struct SlowFirmware {
        remaining: u64,
    }
    impl sim_world::firmware::Firmware for SlowFirmware {
        fn init(&mut self, machine: &mut sim_world::machine::Machine) {
            machine.schedule_at(0, 0, "slow_kick", Box::new(|_| {}));
        }
        fn step(&mut self, now: sim_core::Tick, machine: &mut sim_world::machine::Machine) {
            if self.remaining > 0 {
                self.remaining -= 1;
                machine.schedule_at(now + 1, 0, "slow_tick", Box::new(|_| {}));
            }
        }
    }

    struct PanickingFirmware;
    impl sim_world::firmware::Firmware for PanickingFirmware {
        fn init(&mut self, machine: &mut sim_world::machine::Machine) {
            machine.schedule_at(0, 0, "panic_kick", Box::new(|_| {}));
        }
        fn step(&mut self, _now: sim_core::Tick, _machine: &mut sim_world::machine::Machine) {
            panic!("deliberate test firmware panic");
        }
    }

    fn registry_with_slow_and_panic() -> FirmwareRegistry {
        let mut reg = FirmwareRegistry::new();
        reg.register(
            "slow_fw",
            // Long enough for a sibling client to observe Running and stop,
            // short enough that a missed-stop path still finishes in CI.
            Arc::new(|| Box::new(SlowFirmware { remaining: 50_000 }) as _),
        );
        reg.register("panic_fw", Arc::new(|| Box::new(PanickingFirmware) as _));
        reg
    }

    #[test]
    fn jsonrpc_stop_and_sibling_access_during_active_run() {
        let port = start_server_on_random_port_with_registry(Some(registry_with_slow_and_panic()));
        let mut runner = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        let mut control = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut runner,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        let sib = rpc_call(
            &mut control,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();

        let slow = r#"
name = "slow"
[[machine]]
id = 0
name = "m0"
firmware = "slow_fw"
"#;
        let minimal = r#"
name = "sib"
[[machine]]
id = 0
name = "m0"
"#;
        rpc_call(
            &mut runner,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{"session_id":sid,"toml":slow},
            }),
        );
        rpc_call(
            &mut control,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{"session_id":sib,"toml":minimal},
            }),
        );

        // Start a long run on a background thread (same logical Server).
        let run_handle = std::thread::spawn(move || {
            rpc_call(
                &mut runner,
                &json!({
                    "jsonrpc":"2.0","id":3,"method":"sim.run",
                    "params":{"session_id":sid,"tick_batch_size":100},
                }),
            )
        });

        // Wait until the session is Running, then query sibling + stop.
        let mut saw_running = false;
        for _ in 0..200 {
            let state = status_state(&mut control, sid, 10);
            if state == "running" {
                saw_running = true;
                break;
            }
            if state == "done" || state == "error" || state == "paused" {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_running, "active run must be observable as Running");

        // Sibling remains accessible while the first session runs.
        assert_eq!(status_state(&mut control, sib, 11), "ready");

        let stop = rpc_call(
            &mut control,
            &json!({
                "jsonrpc":"2.0","id":12,"method":"sim.stop",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(stop["result"]["stopped"], true);

        let mut done = false;
        for _ in 0..200 {
            if status_state(&mut control, sid, 13) == "done" {
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done, "stopped session must become Done");

        let run_resp = run_handle.join().unwrap();
        assert_eq!(run_resp["result"]["state"], "done");

        // World remains inspectable after stop.
        let status = rpc_call(
            &mut control,
            &json!({
                "jsonrpc":"2.0","id":14,"method":"sim.status",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(status["result"]["state"], "done");
        assert!(status["result"]["now_ticks"].as_u64().is_some());

        // Sibling still runs to completion on the same Server.
        let sib_run = rpc_call(
            &mut control,
            &json!({
                "jsonrpc":"2.0","id":15,"method":"sim.run",
                "params":{"session_id":sib},
            }),
        );
        assert_eq!(sib_run["result"]["state"], "done");
    }

    #[test]
    fn jsonrpc_trace_stream_disconnect_pauses_and_resumes() {
        let port = start_server_on_random_port_with_registry(Some(registry_with_slow_and_panic()));
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        let mut peer = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{
                    "session_id":sid,
                    "toml":"name=\"slow\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
                },
            }),
        );

        // Issue trace.stream on `stream`, read a few tick heartbeats, then close.
        // Tiny batches force many on_batch heartbeats before completion.
        let req = serde_json::to_string(&json!({
            "jsonrpc":"2.0","id":3,"method":"trace.stream",
            "params":{"session_id":sid,"tick_batch_size":1},
        }))
        .unwrap()
            + "\n";
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();

        {
            let mut reader = BufReader::new(&mut stream);
            let mut saw_progress = false;
            let mut lines = Vec::new();
            for _ in 0..200 {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let is_done = line.contains("trace.stream.done") || line.contains("\"jsonrpc\"");
                lines.push(line.clone());
                if line.contains("trace.stream.tick") || line.contains("\"event\":\"trace\"") {
                    saw_progress = true;
                    break;
                }
                if is_done {
                    break;
                }
            }
            assert!(
                saw_progress,
                "expected streaming progress before disconnect; lines={lines:?}"
            );
        }
        drop(stream); // disconnect mid-run

        let mut paused = false;
        let mut last = String::new();
        for _ in 0..200 {
            last = status_state(&mut peer, sid, 20);
            if last == "paused" {
                paused = true;
                break;
            }
            // Already finished before disconnect could land — still prove inspectability.
            if last == "done" || last == "error" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            paused || last == "paused",
            "disconnect during trace.stream must yield Paused, got {last}"
        );

        // Resume from paused — stop finishes the session (world remains inspectable).
        // Also prove a fresh sim.run after reset can complete (resume path).
        let now_before = rpc_call(
            &mut peer,
            &json!({
                "jsonrpc":"2.0","id":21,"method":"sim.status",
                "params":{"session_id":sid},
            }),
        )["result"]["now_ticks"]
            .as_u64()
            .unwrap();
        assert!(now_before > 0, "paused session must retain progressed time");

        // Resume by running a bounded step — proves world was returned.
        let step = rpc_call(
            &mut peer,
            &json!({
                "jsonrpc":"2.0","id":22,"method":"sim.step",
                "params":{"session_id":sid,"n_ticks":10},
            }),
        );
        assert!(
            step.get("error").is_none(),
            "paused session must be resumable via sim.step: {step}"
        );
        let state = status_state(&mut peer, sid, 23);
        assert!(
            state == "paused" || state == "done",
            "after resume step expected paused/done, got {state}"
        );
    }

    #[test]
    fn jsonrpc_failed_session_returns_world_and_sibling_runs() {
        let port = start_server_on_random_port_with_registry(Some(registry_with_slow_and_panic()));
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let fail = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        let sib = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":2,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();

        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":3,"method":"scenario.load_inline",
                "params":{
                    "session_id":fail,
                    "toml":"name=\"panic\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"panic_fw\"\n",
                },
            }),
        );
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":4,"method":"scenario.load_inline",
                "params":{
                    "session_id":sib,
                    "toml":"name=\"sib\"\n[[machine]]\nid=0\nname=\"m0\"\n",
                },
            }),
        );

        let fail_resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":5,"method":"sim.run",
                "params":{"session_id":fail,"tick_batch_size":10},
            }),
        );
        assert_eq!(fail_resp["result"]["state"], "error");
        assert!(
            fail_resp["result"]["error"]
                .as_str()
                .unwrap_or("")
                .contains("panic"),
            "expected panic message, got {fail_resp}"
        );
        assert_eq!(status_state(&mut stream, fail, 6), "error");

        // Failed session remains inspectable (status works).
        let status = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":7,"method":"sim.status",
                "params":{"session_id":fail},
            }),
        );
        assert_eq!(status["result"]["state"], "error");

        let sib_resp = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":8,"method":"sim.run",
                "params":{"session_id":sib},
            }),
        );
        assert_eq!(sib_resp["result"]["state"], "done");
        assert_eq!(status_state(&mut stream, sib, 9), "done");
    }

    #[test]
    fn jsonrpc_concurrent_sibling_status_during_run() {
        let port = start_server_on_random_port_with_registry(Some(registry_with_slow_and_panic()));
        let mut runner = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        let mut watcher = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut runner,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        let sib = rpc_call(
            &mut watcher,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut runner,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{
                    "session_id":sid,
                    "toml":"name=\"slow\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
                },
            }),
        );

        let handle = std::thread::spawn(move || {
            rpc_call(
                &mut runner,
                &json!({
                    "jsonrpc":"2.0","id":3,"method":"sim.run",
                    "params":{"session_id":sid,"tick_batch_size":50},
                }),
            )
        });

        let mut overlapped = false;
        for i in 0..500u64 {
            let resp = rpc_call(
                &mut watcher,
                &json!({
                    "jsonrpc":"2.0","id":100 + i,"method":"sim.status",
                    "params":{"session_id":sid},
                }),
            );
            assert!(
                resp.get("error").is_none(),
                "sibling status during run must succeed: {resp}"
            );
            let state = resp["result"]["state"].as_str().unwrap_or("");
            let sib_state = status_state(&mut watcher, sib, 300 + i);
            assert_eq!(sib_state, "idle");
            if state == "running" {
                overlapped = true;
                let _ = rpc_call(
                    &mut watcher,
                    &json!({
                        "jsonrpc":"2.0","id":999,"method":"sim.stop",
                        "params":{"session_id":sid},
                    }),
                );
                break;
            }
            if state == "done" || state == "error" || state == "paused" {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            overlapped,
            "must observe Running while sibling status succeeds"
        );
        let run_resp = handle.join().unwrap();
        assert!(
            run_resp.get("error").is_none(),
            "run should complete after stop: {run_resp}"
        );
    }

    #[test]
    fn jsonrpc_sim_run_disconnect_pauses_and_resumes() {
        let port = start_server_on_random_port_with_registry(Some(registry_with_slow_and_panic()));
        let mut runner = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        let mut peer = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut runner,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut runner,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{
                    "session_id":sid,
                    "toml":"name=\"slow\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
                },
            }),
        );

        // Fire sim.run without waiting for the response — the server blocks in
        // the cooperative loop until disconnect, stop, or natural completion.
        let req = serde_json::to_string(&json!({
            "jsonrpc":"2.0","id":3,"method":"sim.run",
            "params":{"session_id":sid,"tick_batch_size":1},
        }))
        .unwrap()
            + "\n";
        runner.write_all(req.as_bytes()).unwrap();
        runner.flush().unwrap();

        let mut saw_running = false;
        for _ in 0..400 {
            let state = status_state(&mut peer, sid, 10);
            if state == "running" {
                saw_running = true;
                break;
            }
            if state == "done" || state == "error" || state == "paused" {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_running, "active sim.run must be observable as Running");
        // World is checked out while Running, so status.now_ticks is 0 until
        // the cooperative worker returns it. Give the run a few batches so
        // virtual time advances before we drop the client.
        std::thread::sleep(Duration::from_millis(50));

        drop(runner); // disconnect mid-run without sim.stop

        let mut paused = false;
        let mut last = String::new();
        for _ in 0..400 {
            last = status_state(&mut peer, sid, 20);
            if last == "paused" {
                paused = true;
                break;
            }
            // Must not race to Done merely because disconnect was ignored.
            assert_ne!(
                last, "done",
                "disconnect must not let the worker finish as Done"
            );
            if last == "error" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(paused, "sim.run disconnect must yield Paused, got {last}");

        let status = rpc_call(
            &mut peer,
            &json!({
                "jsonrpc":"2.0","id":21,"method":"sim.status",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(status["result"]["state"], "paused");
        let now = status["result"]["now_ticks"].as_u64().unwrap();
        assert!(
            now > 0,
            "paused session must retain progressed time, got {now}"
        );

        let step = rpc_call(
            &mut peer,
            &json!({
                "jsonrpc":"2.0","id":22,"method":"sim.step",
                "params":{"session_id":sid,"n_ticks":10},
            }),
        );
        assert!(
            step.get("error").is_none(),
            "paused session must be resumable via sim.step: {step}"
        );

        let stop = rpc_call(
            &mut peer,
            &json!({
                "jsonrpc":"2.0","id":23,"method":"sim.stop",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(stop["result"]["stopped"], true);

        // After stop from Paused with world present, state is Done.
        let mut done = false;
        for _ in 0..50 {
            if status_state(&mut peer, sid, 24) == "done" {
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done, "stopped resumed session must become Done");

        let destroy = rpc_call(
            &mut peer,
            &json!({
                "jsonrpc":"2.0","id":25,"method":"session.destroy",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(destroy["result"]["destroyed"], true);
        let missing = rpc_call(
            &mut peer,
            &json!({
                "jsonrpc":"2.0","id":26,"method":"sim.status",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(
            missing["error"]["code"],
            error_codes::SESSION_NOT_FOUND,
            "destroyed session must be gone: {missing}"
        );
    }

    #[test]
    fn jsonrpc_destroy_rejects_running_then_succeeds_after_stop() {
        let port = start_server_on_random_port_with_registry(Some(registry_with_slow_and_panic()));
        let mut runner = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        let mut control = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut runner,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut runner,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{
                    "session_id":sid,
                    "toml":"name=\"slow\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
                },
            }),
        );

        let run_handle = std::thread::spawn(move || {
            rpc_call(
                &mut runner,
                &json!({
                    "jsonrpc":"2.0","id":3,"method":"sim.run",
                    "params":{"session_id":sid,"tick_batch_size":100},
                }),
            )
        });

        let mut saw_running = false;
        for _ in 0..200 {
            if status_state(&mut control, sid, 10) == "running" {
                saw_running = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_running, "active run must be observable as Running");

        let destroy_while_running = rpc_call(
            &mut control,
            &json!({
                "jsonrpc":"2.0","id":11,"method":"session.destroy",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(
            destroy_while_running["error"]["code"],
            error_codes::SESSION_IN_USE,
            "destroy while Running must be SESSION_IN_USE: {destroy_while_running}"
        );

        // Session remains listed and the worker is still controllable.
        let list = rpc_call(
            &mut control,
            &json!({"jsonrpc":"2.0","id":12,"method":"session.list","params":{}}),
        );
        let listed = list["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["session_id"] == sid);
        assert!(listed, "rejected destroy must leave the session listed");
        assert_eq!(status_state(&mut control, sid, 13), "running");

        let stop = rpc_call(
            &mut control,
            &json!({
                "jsonrpc":"2.0","id":14,"method":"sim.stop",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(stop["result"]["stopped"], true);

        let run_resp = run_handle.join().unwrap();
        assert_eq!(run_resp["result"]["state"], "done");
        assert_eq!(status_state(&mut control, sid, 15), "done");

        let destroy = rpc_call(
            &mut control,
            &json!({
                "jsonrpc":"2.0","id":16,"method":"session.destroy",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(destroy["result"]["destroyed"], true);

        let missing = rpc_call(
            &mut control,
            &json!({
                "jsonrpc":"2.0","id":17,"method":"sim.status",
                "params":{"session_id":sid},
            }),
        );
        assert_eq!(missing["error"]["code"], error_codes::SESSION_NOT_FOUND);
    }

    #[test]
    fn destroy_cannot_race_run_checkout() {
        use std::sync::mpsc;

        let server = Arc::new(Server::new(Duration::from_secs(300)));
        server.set_firmware_registry(registry_with_slow_and_panic());
        let create = handle_session_create(&server, &json!(1), &json!({})).unwrap();
        let sid = create["result"]["session_id"].as_u64().unwrap();
        handle_scenario_load_inline(
            &server,
            &json!(2),
            &json!({
                "session_id": sid,
                "toml": "name=\"slow\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
            }),
        )
        .unwrap();

        let (hook_entered_tx, hook_entered_rx) = mpsc::channel::<()>();
        let destroy_attempted = Arc::new(AtomicBool::new(false));
        let destroy_attempted_hook = Arc::clone(&destroy_attempted);

        let _hook_guard = RunCheckoutHookGuard::install(
            &server,
            Arc::new(move |target_id| {
                assert_eq!(target_id, sid, "hook must target the run session");
                hook_entered_tx.send(()).unwrap();
                while !destroy_attempted_hook.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                std::thread::sleep(Duration::from_millis(20));
            }),
        );

        let server_run = Arc::clone(&server);
        let run_handle = std::thread::spawn(move || {
            let mut live = run_loop::AlwaysConnected;
            handle_sim_run(
                &server_run,
                &json!(3),
                &json!({ "session_id": sid, "tick_batch_size": 100 }),
                &mut live,
            )
        });

        hook_entered_rx
            .recv()
            .expect("run checkout hook must fire with registry lock held");

        let server_destroy = Arc::clone(&server);
        let destroy_attempted_t = Arc::clone(&destroy_attempted);
        let destroy_handle = std::thread::spawn(move || {
            destroy_attempted_t.store(true, Ordering::SeqCst);
            handle_session_destroy(&server_destroy, &json!(4), &json!({ "session_id": sid }))
        });

        let destroy_result = destroy_handle.join().unwrap();
        assert_eq!(
            destroy_result.as_ref().unwrap_err()["error"]["code"],
            error_codes::SESSION_IN_USE,
            "destroy must lose the checkout race: {destroy_result:?}"
        );

        assert!(
            server.get_arc(sid, &json!(0)).is_ok(),
            "session must remain registered while run holds the world"
        );
        {
            let arc = server.get_arc(sid, &json!(0)).unwrap();
            let session = arc.lock().unwrap();
            assert_eq!(session.state, SessionState::Running);
        }

        handle_sim_stop(&server, &json!(5), &json!({ "session_id": sid })).unwrap();
        let run_resp = run_handle.join().unwrap().unwrap().unwrap();
        assert_eq!(run_resp["result"]["state"], "done");

        let destroy =
            handle_session_destroy(&server, &json!(6), &json!({ "session_id": sid })).unwrap();
        assert_eq!(destroy["result"]["destroyed"], true);
    }

    #[test]
    fn checkout_hook_is_server_scoped() {
        use std::sync::mpsc;

        let server_a = Arc::new(Server::new(Duration::from_secs(300)));
        let server_b = Arc::new(Server::new(Duration::from_secs(300)));
        server_a.set_firmware_registry(registry_with_slow_and_panic());
        server_b.set_firmware_registry(registry_with_slow_and_panic());

        let sid_a = {
            let create = handle_session_create(&server_a, &json!(1), &json!({})).unwrap();
            let sid = create["result"]["session_id"].as_u64().unwrap();
            handle_scenario_load_inline(
                &server_a,
                &json!(2),
                &json!({
                    "session_id": sid,
                    "toml": "name=\"a\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
                }),
            )
            .unwrap();
            sid
        };
        let sid_b = {
            let create = handle_session_create(&server_b, &json!(1), &json!({})).unwrap();
            let sid = create["result"]["session_id"].as_u64().unwrap();
            handle_scenario_load_inline(
                &server_b,
                &json!(2),
                &json!({
                    "session_id": sid,
                    "toml": "name=\"b\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
                }),
            )
            .unwrap();
            sid
        };

        let (hook_tx, hook_rx) = mpsc::channel::<u64>();
        let _guard = RunCheckoutHookGuard::install(
            &server_a,
            Arc::new(move |session_id| {
                hook_tx.send(session_id).unwrap();
            }),
        );

        let server_a_run = Arc::clone(&server_a);
        let run_a = std::thread::spawn(move || {
            let mut live = run_loop::AlwaysConnected;
            handle_sim_run(
                &server_a_run,
                &json!(3),
                &json!({ "session_id": sid_a, "tick_batch_size": 100 }),
                &mut live,
            )
        });
        assert_eq!(hook_rx.recv().unwrap(), sid_a);
        assert!(
            hook_rx.try_recv().is_err(),
            "server B must not invoke server A hook"
        );

        let server_b_run = Arc::clone(&server_b);
        let run_b = std::thread::spawn(move || {
            let mut live = run_loop::AlwaysConnected;
            handle_sim_run(
                &server_b_run,
                &json!(4),
                &json!({ "session_id": sid_b, "tick_batch_size": 100 }),
                &mut live,
            )
        });

        handle_sim_stop(&server_a, &json!(5), &json!({ "session_id": sid_a })).unwrap();
        let _ = run_a.join().unwrap();
        handle_sim_stop(&server_b, &json!(6), &json!({ "session_id": sid_b })).unwrap();
        let _ = run_b.join().unwrap();
        assert!(
            hook_rx.try_recv().is_err(),
            "server B run must not trigger server A hook"
        );
    }

    #[test]
    fn checkout_hook_guard_clears_on_panic() {
        let server = Arc::new(Server::new(Duration::from_secs(300)));
        server.set_firmware_registry(registry_with_slow_and_panic());
        let create = handle_session_create(&server, &json!(1), &json!({})).unwrap();
        let sid = create["result"]["session_id"].as_u64().unwrap();
        handle_scenario_load_inline(
            &server,
            &json!(2),
            &json!({
                "session_id": sid,
                "toml": "name=\"slow\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
            }),
        )
        .unwrap();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = RunCheckoutHookGuard::install(
                &server,
                Arc::new(|_| {
                    panic!("hook must be cleared before this runs");
                }),
            );
            panic!("test panic");
        }));
        assert!(result.is_err());

        let mut live = run_loop::AlwaysConnected;
        let run_resp = handle_sim_run(
            &server,
            &json!(3),
            &json!({ "session_id": sid, "tick_batch_size": 100 }),
            &mut live,
        )
        .unwrap()
        .unwrap();
        assert_eq!(run_resp["result"]["state"], "done");
    }

    #[test]
    fn destroy_cannot_race_run_checkout_stress() {
        let server = Arc::new(Server::new(Duration::from_secs(300)));
        server.set_firmware_registry(registry_with_slow_and_panic());

        for _iter in 0..50 {
            let create = handle_session_create(&server, &json!(1), &json!({})).unwrap();
            let sid = create["result"]["session_id"].as_u64().unwrap();
            handle_scenario_load_inline(
                &server,
                &json!(2),
                &json!({
                    "session_id": sid,
                    "toml": "name=\"slow\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
                }),
            )
            .unwrap();

            let server_run = Arc::clone(&server);
            let server_destroy = Arc::clone(&server);
            let run_handle = std::thread::spawn(move || {
                let mut live = run_loop::AlwaysConnected;
                handle_sim_run(
                    &server_run,
                    &json!(3),
                    &json!({ "session_id": sid, "tick_batch_size": 50 }),
                    &mut live,
                )
            });
            let destroy_handle = std::thread::spawn(move || {
                handle_session_destroy(&server_destroy, &json!(4), &json!({ "session_id": sid }))
            });

            let run_result = run_handle.join().unwrap();
            let destroy_result = destroy_handle.join().unwrap();

            // If destroy lost the race, stop the worker and tear down.
            if destroy_result
                .as_ref()
                .err()
                .and_then(|e| e["error"]["code"].as_i64())
                == Some(error_codes::SESSION_IN_USE)
            {
                let _ = handle_sim_stop(&server, &json!(5), &json!({ "session_id": sid }));
                let _ = run_result;
            }

            if server.get_arc(sid, &json!(0)).is_ok() {
                let _ = handle_session_destroy(&server, &json!(6), &json!({ "session_id": sid }));
            }
        }
    }

    #[test]
    fn jsonrpc_destroy_allowed_for_terminal_and_idle_states() {
        let port = start_server_on_random_port_with_registry(Some(registry_with_slow_and_panic()));
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        // Idle
        let idle = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        assert_eq!(
            rpc_call(
                &mut stream,
                &json!({
                    "jsonrpc":"2.0","id":2,"method":"session.destroy",
                    "params":{"session_id":idle},
                }),
            )["result"]["destroyed"],
            true
        );

        // Ready
        let ready = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":3,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":4,"method":"scenario.load_inline",
                "params":{
                    "session_id":ready,
                    "toml":"name=\"r\"\n[[machine]]\nid=0\nname=\"m0\"\n",
                },
            }),
        );
        assert_eq!(status_state(&mut stream, ready, 5), "ready");
        assert_eq!(
            rpc_call(
                &mut stream,
                &json!({
                    "jsonrpc":"2.0","id":6,"method":"session.destroy",
                    "params":{"session_id":ready},
                }),
            )["result"]["destroyed"],
            true
        );

        // Done
        let done = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":7,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":8,"method":"scenario.load_inline",
                "params":{
                    "session_id":done,
                    "toml":"name=\"d\"\n[[machine]]\nid=0\nname=\"m0\"\n",
                },
            }),
        );
        assert_eq!(
            rpc_call(
                &mut stream,
                &json!({
                    "jsonrpc":"2.0","id":9,"method":"sim.run",
                    "params":{"session_id":done},
                }),
            )["result"]["state"],
            "done"
        );
        assert_eq!(
            rpc_call(
                &mut stream,
                &json!({
                    "jsonrpc":"2.0","id":10,"method":"session.destroy",
                    "params":{"session_id":done},
                }),
            )["result"]["destroyed"],
            true
        );

        // Error
        let err_sid = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":11,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":12,"method":"scenario.load_inline",
                "params":{
                    "session_id":err_sid,
                    "toml":"name=\"panic\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"panic_fw\"\n",
                },
            }),
        );
        assert_eq!(
            rpc_call(
                &mut stream,
                &json!({
                    "jsonrpc":"2.0","id":13,"method":"sim.run",
                    "params":{"session_id":err_sid,"tick_batch_size":10},
                }),
            )["result"]["state"],
            "error"
        );
        assert_eq!(
            rpc_call(
                &mut stream,
                &json!({
                    "jsonrpc":"2.0","id":14,"method":"session.destroy",
                    "params":{"session_id":err_sid},
                }),
            )["result"]["destroyed"],
            true
        );

        // Paused (bounded run_until with pending work)
        let paused = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":15,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":16,"method":"scenario.load_inline",
                "params":{
                    "session_id":paused,
                    "toml":"name=\"slow\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"slow_fw\"\n",
                },
            }),
        );
        let until = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":17,"method":"sim.run_until",
                "params":{"session_id":paused,"deadline_ticks":50},
            }),
        );
        assert_eq!(until["result"]["state"], "paused");
        assert_eq!(
            rpc_call(
                &mut stream,
                &json!({
                    "jsonrpc":"2.0","id":18,"method":"session.destroy",
                    "params":{"session_id":paused},
                }),
            )["result"]["destroyed"],
            true
        );
    }

    /// Firmware whose first event sits far beyond the cooperative batch size.
    struct SparseEventFirmware {
        fired: Arc<AtomicU64>,
    }
    impl sim_world::firmware::Firmware for SparseEventFirmware {
        fn init(&mut self, machine: &mut sim_world::machine::Machine) {
            // Sparse schedule: nothing until t=10_000.
            machine.schedule_at(10_000, 0, "sparse", Box::new(|_| {}));
        }
        fn step(&mut self, now: sim_core::Tick, machine: &mut sim_world::machine::Machine) {
            if now >= 10_000 && self.fired.load(Ordering::SeqCst) == 0 {
                self.fired.fetch_add(1, Ordering::SeqCst);
                machine.record_trace(sim_core::TraceEvent::UserU32 {
                    at: now,
                    label: "sparse_event",
                    value: 1,
                });
            }
        }
    }

    #[test]
    fn jsonrpc_sim_run_sparse_first_event_terminates() {
        let fired = Arc::new(AtomicU64::new(0));
        let fired_reg = Arc::clone(&fired);
        let mut reg = FirmwareRegistry::new();
        reg.register(
            "sparse_fw",
            Arc::new(move || {
                Box::new(SparseEventFirmware {
                    fired: Arc::clone(&fired_reg),
                }) as _
            }),
        );
        let port = start_server_on_random_port_with_registry(Some(reg));
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{
                    "session_id":sid,
                    "toml":"name=\"sparse\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"sparse_fw\"\n",
                },
            }),
        );

        let run = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":3,"method":"sim.run",
                "params":{"session_id":sid,"tick_batch_size":1000},
            }),
        );
        assert!(run.get("error").is_none(), "sim.run must return: {run}");
        assert_eq!(run["result"]["state"], "done");
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        let status = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":4,"method":"sim.status",
                "params":{"session_id":sid},
            }),
        );
        assert!(
            status["result"]["now_ticks"].as_u64().unwrap() >= 10_000,
            "clock must reach sparse event: {status}"
        );
        let traces = rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":5,"method":"trace.get",
                "params":{"session_id":sid},
            }),
        );
        let trace = traces["result"]["trace"].as_str().unwrap_or("");
        let sparse_hits = trace.matches("sparse_event").count();
        assert_eq!(
            sparse_hits, 1,
            "sparse event must appear exactly once: {traces}"
        );
    }

    #[test]
    fn jsonrpc_trace_stream_sparse_event_monotonic() {
        let fired = Arc::new(AtomicU64::new(0));
        let fired_reg = Arc::clone(&fired);
        let mut reg = FirmwareRegistry::new();
        reg.register(
            "sparse_fw",
            Arc::new(move || {
                Box::new(SparseEventFirmware {
                    fired: Arc::clone(&fired_reg),
                }) as _
            }),
        );
        let port = start_server_on_random_port_with_registry(Some(reg));
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();

        let sid = rpc_call(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{}}),
        )["result"]["session_id"]
            .as_u64()
            .unwrap();
        rpc_call(
            &mut stream,
            &json!({
                "jsonrpc":"2.0","id":2,"method":"scenario.load_inline",
                "params":{
                    "session_id":sid,
                    "toml":"name=\"sparse\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"sparse_fw\"\n",
                },
            }),
        );

        let req = json!({
            "jsonrpc":"2.0","id":3,"method":"trace.stream",
            "params":{"session_id":sid,"tick_batch_size":1000},
        });
        let req_str = serde_json::to_string(&req).unwrap() + "\n";
        stream.write_all(req_str.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut tick_ts = Vec::new();
        let mut saw_sparse = false;
        let mut final_resp = None;
        for _ in 0..10_000 {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            if v.get("id") == Some(&json!(3)) {
                final_resp = Some(v);
                break;
            }
            if v.get("event") == Some(&json!("trace")) {
                let data = v["data"].as_str().unwrap_or("");
                if data.contains("sparse_event") {
                    saw_sparse = true;
                }
            }
            if v.get("event") == Some(&json!("trace.stream.tick")) {
                if let Some(ts) = v.get("now_ticks").and_then(|t| t.as_u64()) {
                    tick_ts.push(ts);
                }
            }
        }
        let final_resp = final_resp.expect("trace.stream must finish");
        assert!(final_resp.get("error").is_none(), "{final_resp}");
        assert!(saw_sparse, "sparse event must be streamed");
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        for w in tick_ts.windows(2) {
            assert!(w[1] >= w[0], "timestamps must be monotonic: {tick_ts:?}");
        }
        let stagnant_zero = tick_ts.iter().filter(|&&t| t == 0).count();
        assert!(
            stagnant_zero <= 1,
            "must not stream unlimited ticks at t=0: {tick_ts:?}"
        );
        assert_eq!(status_state(&mut stream, sid, 4), "done");
    }

    #[test]
    fn reset_cannot_publish_ready_while_old_world_is_running() {
        use std::sync::mpsc;

        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let entered_tx = Mutex::new(Some(entered_tx));
        let release_rx = Mutex::new(release_rx);
        let constructs = Arc::new(AtomicU64::new(0));
        let constructs_f = Arc::clone(&constructs);
        let world_token = Arc::new(AtomicU64::new(0));
        let world_token_f = Arc::clone(&world_token);

        let mut reg = FirmwareRegistry::new();
        reg.register(
            "blocking_fw",
            Arc::new(move || {
                let n = constructs_f.fetch_add(1, Ordering::SeqCst);
                // First call is scenario load; second is reset reconstruction.
                if n >= 1 {
                    if let Some(tx) = entered_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                    let _ = release_rx.lock().unwrap().recv();
                }
                let token = world_token_f.fetch_add(1, Ordering::SeqCst) + 1;
                Box::new(TokenFirmware { token }) as _
            }),
        );

        let server = Arc::new(Server::new(Duration::from_secs(300)));
        server.set_firmware_registry(reg);
        let create = handle_session_create(&server, &json!(1), &json!({})).unwrap();
        let sid = create["result"]["session_id"].as_u64().unwrap();
        handle_scenario_load_inline(
            &server,
            &json!(2),
            &json!({
                "session_id": sid,
                "toml": "name=\"blk\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"blocking_fw\"\n",
            }),
        )
        .unwrap();

        let server_reset = Arc::clone(&server);
        let reset_handle = std::thread::spawn(move || {
            handle_sim_reset(&server_reset, &json!(3), &json!({ "session_id": sid }))
        });

        entered_rx
            .recv()
            .expect("reset must reach firmware reconstruction");

        // While reset holds the per-session lock, no other op can check out.
        let arc = server.get_arc(sid, &json!(0)).unwrap();
        assert!(
            arc.try_lock().is_err(),
            "reset must hold the session lock during World reconstruction"
        );

        let server_run = Arc::clone(&server);
        let run_finished = Arc::new(AtomicBool::new(false));
        let run_finished_t = Arc::clone(&run_finished);
        let run_handle = std::thread::spawn(move || {
            let mut live = run_loop::AlwaysConnected;
            let result = handle_sim_run(
                &server_run,
                &json!(4),
                &json!({ "session_id": sid, "tick_batch_size": 100 }),
                &mut live,
            );
            run_finished_t.store(true, Ordering::SeqCst);
            result
        });

        // Run cannot finish (or check out) until reset releases the lock.
        assert!(
            !run_finished.load(Ordering::SeqCst),
            "run must not complete while reset holds the session lock"
        );

        release_tx.send(()).unwrap();
        let reset_resp = reset_handle.join().unwrap().unwrap();
        assert_eq!(reset_resp["result"]["state"], "ready");

        let run_resp = run_handle.join().unwrap().unwrap().unwrap();
        assert_eq!(run_resp["result"]["state"], "done");
        assert!(run_finished.load(Ordering::SeqCst));

        // Load + reset constructed two firmwares; only the reset World ran.
        assert_eq!(constructs.load(Ordering::SeqCst), 2);
        let session = arc.lock().unwrap();
        assert!(session.world.is_some());
        assert_ne!(session.state, SessionState::Running);
        assert!(session.world.as_ref().unwrap().machine(0).is_some());
    }

    struct TokenFirmware {
        token: u64,
    }
    impl sim_world::firmware::Firmware for TokenFirmware {
        fn init(&mut self, machine: &mut sim_world::machine::Machine) {
            let token = self.token;
            machine.schedule_at(
                0,
                0,
                "token",
                Box::new(move |_| {
                    let _ = token;
                }),
            );
        }
    }

    #[test]
    fn reset_versus_load_keeps_scenario_world_consistent() {
        let server = Arc::new(Server::new(Duration::from_secs(300)));
        let create = handle_session_create(&server, &json!(1), &json!({})).unwrap();
        let sid = create["result"]["session_id"].as_u64().unwrap();
        handle_scenario_load_inline(
            &server,
            &json!(2),
            &json!({
                "session_id": sid,
                "toml": "name=\"a\"\n[[machine]]\nid=0\nname=\"m0\"\n",
            }),
        )
        .unwrap();

        let server_reset = Arc::clone(&server);
        let server_load = Arc::clone(&server);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let b_reset = Arc::clone(&barrier);
        let b_load = Arc::clone(&barrier);

        let reset_handle = std::thread::spawn(move || {
            b_reset.wait();
            handle_sim_reset(&server_reset, &json!(3), &json!({ "session_id": sid }))
        });
        let load_handle = std::thread::spawn(move || {
            b_load.wait();
            handle_scenario_load_inline(
                &server_load,
                &json!(4),
                &json!({
                    "session_id": sid,
                    "toml": "name=\"b\"\n[[machine]]\nid=0\nname=\"m0\"\n[[machine]]\nid=1\nname=\"m1\"\n",
                }),
            )
        });

        let reset_res = reset_handle.join().unwrap();
        let load_res = load_handle.join().unwrap();
        // At least one operation must succeed; the loser may see Running only if
        // a run were active — here both mutate Ready sessions.
        assert!(reset_res.is_ok() || load_res.is_ok());

        let arc = server.get_arc(sid, &json!(0)).unwrap();
        let session = arc.lock().unwrap();
        assert!(session.world.is_some());
        let scenario = session.scenario.as_ref().unwrap();
        let world = session.world.as_ref().unwrap();
        assert_eq!(
            scenario.machine.len(),
            world.machine_ids().count(),
            "scenario and world must describe the same machine set"
        );
        assert_eq!(session.state, SessionState::Ready);
    }

    #[test]
    fn reset_failure_leaves_previous_session_untouched() {
        let server = Server::new(Duration::from_secs(300));
        let create = handle_session_create(&server, &json!(1), &json!({})).unwrap();
        let sid = create["result"]["session_id"].as_u64().unwrap();
        handle_scenario_load_inline(
            &server,
            &json!(2),
            &json!({
                "session_id": sid,
                "toml": "name=\"keep\"\n[[machine]]\nid=0\nname=\"m0\"\n",
            }),
        )
        .unwrap();

        {
            let arc = server.get_arc(sid, &json!(0)).unwrap();
            let mut session = arc.lock().unwrap();
            session.traces.push_back("pre-reset-trace".into());
            session.n_events = 7;
            // Corrupt the stored scenario so rebuild fails; keep the live World.
            if let Some(ref mut scenario) = session.scenario {
                scenario.link.push(sim_world::scenario::LinkDef {
                    link_type: "not-a-real-link-type".into(),
                    from: 0,
                    to: 0,
                    latency: Some(0),
                    baud: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    tick_rate_hz: None,
                });
            }
        }

        let err = handle_sim_reset(&server, &json!(3), &json!({ "session_id": sid })).unwrap_err();
        assert_eq!(err["error"]["code"], error_codes::SCENARIO_PARSE_ERROR);

        let arc = server.get_arc(sid, &json!(0)).unwrap();
        let session = arc.lock().unwrap();
        assert!(
            session.world.is_some(),
            "previous World must remain installed"
        );
        assert_eq!(session.state, SessionState::Ready);
        assert_eq!(session.n_events, 7);
        assert_eq!(
            session.traces.front().map(String::as_str),
            Some("pre-reset-trace")
        );
        assert_eq!(session.scenario.as_ref().unwrap().name, "keep");
    }

    fn registry_with_factory_panic() -> FirmwareRegistry {
        let mut reg = FirmwareRegistry::new();
        reg.register(
            "factory_panic_fw",
            Arc::new(|| panic!("deliberate factory panic")),
        );
        reg
    }

    #[test]
    fn reset_factory_panic_leaves_world_intact_and_session_usable() {
        let server = Server::new(Duration::from_secs(300));
        let mut reg = FirmwareRegistry::new();
        reg.register(
            "good_fw",
            Arc::new(|| Box::new(TokenFirmware { token: 1 }) as _),
        );
        server.set_firmware_registry(reg);

        let create = handle_session_create(&server, &json!(1), &json!({})).unwrap();
        let sid = create["result"]["session_id"].as_u64().unwrap();
        handle_scenario_load_inline(
            &server,
            &json!(2),
            &json!({
                "session_id": sid,
                "toml": "name=\"good\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"good_fw\"\n",
            }),
        )
        .unwrap();

        {
            let arc = server.get_arc(sid, &json!(0)).unwrap();
            let mut session = arc.lock().unwrap();
            session.traces.push_back("keep-me".into());
            session.n_events = 11;
            if let Some(ref mut world) = session.world {
                world.now = 42;
            }
        }

        server.set_firmware_registry({
            let mut reg = FirmwareRegistry::new();
            reg.register("good_fw", Arc::new(|| panic!("deliberate factory panic")));
            reg
        });

        let reset_result = handle_sim_reset(&server, &json!(3), &json!({ "session_id": sid }));
        let err = reset_result.unwrap_err();
        assert_eq!(err["error"]["code"], error_codes::SIM_ERROR);
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("deliberate factory panic"),
            "expected factory panic message, got {err}"
        );

        {
            let arc = server.get_arc(sid, &json!(0)).unwrap();
            let session = arc.lock().unwrap();
            assert_eq!(session.state, SessionState::Ready);
            assert_eq!(session.n_events, 11);
            assert_eq!(session.traces.front().map(String::as_str), Some("keep-me"));
            assert_eq!(session.world.as_ref().unwrap().now, 42);
        }

        let mut reg = FirmwareRegistry::new();
        reg.register(
            "good_fw",
            Arc::new(|| Box::new(TokenFirmware { token: 2 }) as _),
        );
        server.set_firmware_registry(reg);

        let reset = handle_sim_reset(&server, &json!(4), &json!({ "session_id": sid })).unwrap();
        assert_eq!(reset["result"]["state"], "ready");
        assert_eq!(reset["result"]["now_ticks"], 0);
    }

    #[test]
    fn scenario_load_factory_panic_leaves_previous_world_intact() {
        let server = Server::new(Duration::from_secs(300));
        let mut reg = FirmwareRegistry::new();
        reg.register(
            "good_fw",
            Arc::new(|| Box::new(TokenFirmware { token: 1 }) as _),
        );
        server.set_firmware_registry(reg);

        let create = handle_session_create(&server, &json!(1), &json!({})).unwrap();
        let sid = create["result"]["session_id"].as_u64().unwrap();
        handle_scenario_load_inline(
            &server,
            &json!(2),
            &json!({
                "session_id": sid,
                "toml": "name=\"good\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"good_fw\"\n",
            }),
        )
        .unwrap();

        {
            let arc = server.get_arc(sid, &json!(0)).unwrap();
            let mut session = arc.lock().unwrap();
            session.traces.push_back("pre-load".into());
            session.n_events = 5;
        }

        server.set_firmware_registry(registry_with_factory_panic());

        let err = handle_scenario_load_inline(
            &server,
            &json!(3),
            &json!({
                "session_id": sid,
                "toml": "name=\"panic\"\n[[machine]]\nid=0\nname=\"m0\"\nfirmware=\"factory_panic_fw\"\n",
            }),
        )
        .unwrap_err();
        assert_eq!(err["error"]["code"], error_codes::SIM_ERROR);
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("deliberate factory panic"),
            "expected factory panic message, got {err}"
        );

        let arc = server.get_arc(sid, &json!(0)).unwrap();
        let session = arc.lock().unwrap();
        assert_eq!(session.state, SessionState::Ready);
        assert_eq!(session.n_events, 5);
        assert_eq!(session.traces.front().map(String::as_str), Some("pre-load"));
        assert_eq!(session.scenario.as_ref().unwrap().name, "good");
        assert!(session.world.is_some());
    }
}
