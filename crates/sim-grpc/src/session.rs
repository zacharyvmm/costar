//! Session registry for the gRPC simulator server.
//!
//! The map holds `Arc<Mutex<Session>>` values. The map lock is used only to
//! look up / insert / remove the `Arc`; per-session work then locks exactly one
//! session, so the global map lock is never held during simulation (Stage A2).
//!
//! Fixed resource limits (Stage A5): 128 live sessions, 16 keyframes per
//! session, a 100 000-record trace ring with a dropped-records counter, a
//! 300-second idle TTL (Running/Paused sessions exempt), and a cleanup pass at
//! most once per 30 host seconds plus on every create/list.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sim_world::scenario::Scenario;
use sim_world::{SessionState, World};

/// Maximum number of live sessions.
pub const MAX_SESSIONS: usize = 128;
/// Maximum retained keyframes per session (the 17th evicts the oldest).
pub const MAX_KEYFRAMES: usize = 16;
/// Maximum retained trace records per session (ring buffer).
pub const MAX_TRACE_RECORDS: usize = 100_000;
/// Idle TTL for Idle/Ready/Done/Error sessions.
pub const IDLE_TTL: Duration = Duration::from_secs(300);
/// Minimum host interval between automatic cleanup passes.
pub const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

/// Error string returned when an operation needs the World but it is currently
/// checked out by a run worker. The server maps this to `FAILED_PRECONDITION`.
pub const RUNNING_ERR: &str = "session is running";

/// Error string returned when a terminal session must be reset before re-run.
pub const SESSION_DONE_ERR: &str =
    "session is done; reset or load a new scenario before running again";

/// Error string returned when an errored session must be reset before re-run.
pub const SESSION_ERROR_ERR: &str =
    "session is in error; reset or load a new scenario before running again";

pub struct Session {
    pub id: u64,
    pub world: Option<World>,
    pub scenario: Option<Scenario>,
    pub scenario_toml: Option<String>,
    pub keyframes: VecDeque<(u64, Vec<u8>)>,
    pub next_keyframe_id: u64,
    pub state: SessionState,
    pub n_events: u64,
    pub error_message: Option<String>,
    /// Retained trace records (ring buffer, capped at [`MAX_TRACE_RECORDS`]).
    pub traces: VecDeque<String>,
    /// Count of trace records evicted from the ring.
    pub dropped_trace_records: u64,
    /// Host time of the last activity, used for idle TTL cleanup.
    pub last_active: Instant,
}

impl Session {
    fn new(id: u64) -> Self {
        Self {
            id,
            world: None,
            scenario: None,
            scenario_toml: None,
            keyframes: VecDeque::new(),
            next_keyframe_id: 1,
            state: SessionState::Idle,
            n_events: 0,
            error_message: None,
            traces: VecDeque::new(),
            dropped_trace_records: 0,
            last_active: Instant::now(),
        }
    }

    /// Mark this session as touched (resets its idle TTL clock).
    pub fn touch(&mut self) {
        self.last_active = Instant::now();
    }

    /// Append trace records into the retained ring buffer, evicting the oldest
    /// records past [`MAX_TRACE_RECORDS`] and counting the drops.
    pub fn push_traces<I: IntoIterator<Item = String>>(&mut self, lines: I) {
        for line in lines {
            if self.traces.len() >= MAX_TRACE_RECORDS {
                self.traces.pop_front();
                self.dropped_trace_records += 1;
            }
            self.traces.push_back(line);
        }
    }

    /// Whether the session's world is checked out by a run worker.
    fn is_running(&self) -> bool {
        self.state == SessionState::Running
    }

    /// Whether this session is exempt from idle-TTL cleanup.
    fn ttl_exempt(&self) -> bool {
        matches!(self.state, SessionState::Running | SessionState::Paused)
    }
}

pub struct SessionMap {
    inner: Mutex<BTreeMap<u64, Arc<Mutex<Session>>>>,
    next_id: AtomicU64,
    last_cleanup: Mutex<Instant>,
    idle_ttl: Mutex<Duration>,
}

impl SessionMap {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            last_cleanup: Mutex::new(Instant::now()),
            idle_ttl: Mutex::new(IDLE_TTL),
        }
    }

    /// Override the idle TTL (used by tests for deterministic cleanup).
    pub fn set_idle_ttl(&self, ttl: Duration) {
        *self.idle_ttl.lock().expect("idle_ttl poisoned") = ttl;
    }

    fn get_arc(&self, session_id: u64) -> Result<Arc<Mutex<Session>>, String> {
        let map = self.inner.lock().expect("session map lock poisoned");
        map.get(&session_id)
            .cloned()
            .ok_or_else(|| format!("session {} not found", session_id))
    }

    /// Remove expired sessions. When `force` is false the pass only runs if at
    /// least [`CLEANUP_INTERVAL`] has elapsed since the last pass.
    pub fn cleanup(&self, force: bool) {
        {
            let mut last = self.last_cleanup.lock().expect("last_cleanup poisoned");
            if !force && last.elapsed() < CLEANUP_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        let ttl = *self.idle_ttl.lock().expect("idle_ttl poisoned");
        let mut map = self.inner.lock().expect("session map lock poisoned");
        map.retain(|_, arc| {
            let s = arc.lock().expect("session poisoned");
            s.ttl_exempt() || s.last_active.elapsed() < ttl
        });
    }

    pub fn create(&self) -> Result<u64, String> {
        self.cleanup(false);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut map = self.inner.lock().expect("session map lock poisoned");
        if map.len() >= MAX_SESSIONS {
            return Err(format!(
                "session limit reached ({} live sessions)",
                MAX_SESSIONS
            ));
        }
        map.insert(id, Arc::new(Mutex::new(Session::new(id))));
        Ok(id)
    }

    pub fn destroy(&self, session_id: u64) -> bool {
        let mut map = self.inner.lock().expect("session map lock poisoned");
        map.remove(&session_id).is_some()
    }

    pub fn clone_session(&self, session_id: u64) -> Result<u64, String> {
        self.clone_session_with(session_id, |_, _| Ok(()))
    }

    /// Clone a session's scenario into a fresh World after optional preparation.
    ///
    /// `prepare` runs against the locally built World *before* a new session is
    /// inserted. A failed prepare leaves the session map unchanged.
    pub fn clone_session_with(
        &self,
        session_id: u64,
        prepare: impl FnOnce(&Scenario, &mut World) -> Result<(), String>,
    ) -> Result<u64, String> {
        let (scenario_toml, scenario) = {
            let arc = self.get_arc(session_id)?;
            let source = arc.lock().expect("session poisoned");
            if source.is_running() {
                return Err(RUNNING_ERR.to_string());
            }
            (source.scenario_toml.clone(), source.scenario.clone())
        };

        let (world, scenario) = match scenario {
            Some(sc) => {
                let mut world = sc
                    .build_world()
                    .map_err(|e| format!("build error: {}", e))?;
                world.enable_owned_device_banks();
                prepare(&sc, &mut world)?;
                (Some(world), Some(sc))
            }
            None => (None, None),
        };

        let has_world = world.is_some();
        let new_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut map = self.inner.lock().expect("session map lock poisoned");
        if map.len() >= MAX_SESSIONS {
            return Err(format!(
                "session limit reached ({} live sessions)",
                MAX_SESSIONS
            ));
        }
        let mut session = Session::new(new_id);
        session.world = world;
        session.scenario = scenario;
        session.scenario_toml = scenario_toml;
        session.state = if has_world {
            SessionState::Ready
        } else {
            SessionState::Idle
        };
        map.insert(new_id, Arc::new(Mutex::new(session)));
        Ok(new_id)
    }

    pub fn list(&self) -> Vec<(u64, String, u64, u32)> {
        self.cleanup(false);
        let map = self.inner.lock().expect("session map lock poisoned");
        // BTreeMap iteration is ascending by id — deterministic order.
        map.iter()
            .map(|(id, arc)| {
                let s = arc.lock().expect("session poisoned");
                (
                    *id,
                    s.state.as_str().to_string(),
                    s.world.as_ref().map_or(0, |w| w.now),
                    s.scenario.as_ref().map_or(0, |sc| sc.machine.len() as u32),
                )
            })
            .collect()
    }

    pub fn load_scenario(
        &self,
        session_id: u64,
        toml_str: &str,
    ) -> Result<(u32, u32, u32), String> {
        self.load_scenario_with(session_id, toml_str, |_, _| Ok(()))
    }

    /// Parse, build, prepare, then atomically publish a scenario World.
    ///
    /// `prepare` runs against the local World *before* the session becomes
    /// `Ready`. The session is not mutated until preparation succeeds, so a
    /// failed prepare leaves any previous World untouched and a concurrent
    /// `take_world` cannot observe a half-attached session.
    pub fn load_scenario_with(
        &self,
        session_id: u64,
        toml_str: &str,
        prepare: impl FnOnce(&Scenario, &mut World) -> Result<(), String>,
    ) -> Result<(u32, u32, u32), String> {
        let scenario = Scenario::from_str(toml_str).map_err(|e| format!("parse error: {}", e))?;
        let n_machines = scenario.machine.len() as u32;
        let n_links = scenario.link.len() as u32;
        let n_injections = scenario.inject.len() as u32;
        let mut world = scenario
            .build_world()
            .map_err(|e| format!("build error: {}", e))?;
        // Enable per-machine owned banks before any board config / firmware.
        world.enable_owned_device_banks();
        prepare(&scenario, &mut world)?;

        let arc = self.get_arc(session_id)?;
        let mut session = arc.lock().expect("session poisoned");
        if session.is_running() {
            return Err(RUNNING_ERR.to_string());
        }
        session.world = Some(world);
        session.scenario = Some(scenario);
        session.scenario_toml = Some(toml_str.to_string());
        session.state = SessionState::Ready;
        session.n_events = 0;
        session.error_message = None;
        session.traces.clear();
        session.dropped_trace_records = 0;
        session.touch();
        Ok((n_machines, n_links, n_injections))
    }

    pub fn status(&self, session_id: u64) -> Result<SessionStatus, String> {
        let arc = self.get_arc(session_id)?;
        let session = arc.lock().expect("session poisoned");
        Ok(SessionStatus {
            state: session.state,
            now_ticks: session.world.as_ref().map_or(0, |w| w.now),
            n_machines: session
                .scenario
                .as_ref()
                .map_or(0, |s| s.machine.len() as u32),
            n_events: session.n_events,
            error: session.error_message.clone(),
        })
    }

    /// Run `f` against the session's World with a mutable borrow.
    ///
    /// Returns [`RUNNING_ERR`] if the World is checked out by a run worker.
    pub fn with_world_mut<R>(
        &self,
        session_id: u64,
        f: impl FnOnce(&mut World) -> Result<R, String>,
    ) -> Result<R, String> {
        let arc = self.get_arc(session_id)?;
        let mut session = arc.lock().expect("session poisoned");
        if session.is_running() {
            return Err(RUNNING_ERR.to_string());
        }
        session.touch();
        let world = session
            .world
            .as_mut()
            .ok_or_else(|| "no world loaded".to_string())?;
        f(world)
    }

    /// Run `f` against the session's World with a shared borrow.
    pub fn with_world<R>(
        &self,
        session_id: u64,
        f: impl FnOnce(&World) -> Result<R, String>,
    ) -> Result<R, String> {
        let arc = self.get_arc(session_id)?;
        let mut session = arc.lock().expect("session poisoned");
        if session.is_running() {
            return Err(RUNNING_ERR.to_string());
        }
        session.touch();
        let world = session
            .world
            .as_ref()
            .ok_or_else(|| "no world loaded".to_string())?;
        f(world)
    }

    pub fn take_world(&self, session_id: u64) -> Result<World, String> {
        let arc = self.get_arc(session_id)?;
        let mut session = arc.lock().expect("session poisoned");
        match session.state {
            SessionState::Ready | SessionState::Paused => {}
            SessionState::Running => return Err(RUNNING_ERR.to_string()),
            SessionState::Done => return Err(SESSION_DONE_ERR.to_string()),
            SessionState::Error => return Err(SESSION_ERROR_ERR.to_string()),
            SessionState::Idle => {}
        }
        let world = session
            .world
            .take()
            .ok_or_else(|| format!("no world loaded in session {}", session_id))?;
        session.state = SessionState::Running;
        session.touch();
        Ok(world)
    }

    pub fn return_world(
        &self,
        session_id: u64,
        world: World,
        state: SessionState,
        n_events: u64,
        error: Option<String>,
    ) -> Result<(), String> {
        let arc = self.get_arc(session_id)?;
        let mut session = arc.lock().expect("session poisoned");
        session.world = Some(world);
        session.state = state;
        session.n_events = n_events;
        session.error_message = error;
        session.touch();
        Ok(())
    }

    pub fn save_keyframe(&self, session_id: u64) -> Result<(u64, u64, u64), String> {
        let arc = self.get_arc(session_id)?;
        let mut session = arc.lock().expect("session poisoned");
        if session.is_running() {
            return Err(RUNNING_ERR.to_string());
        }
        let scenario_toml = session.scenario_toml.clone().unwrap_or_default();
        let world = session
            .world
            .as_mut()
            .ok_or_else(|| "no world loaded".to_string())?;
        let kf = world.save_keyframe(scenario_toml);
        let data = sim_world::World::serialize_keyframe(&kf).unwrap_or_default();
        let byte_size = data.len() as u64;
        let kf_id = session.next_keyframe_id;
        session.next_keyframe_id += 1;
        session.keyframes.push_back((kf_id, data));
        // Evict the oldest keyframe past the cap.
        while session.keyframes.len() > MAX_KEYFRAMES {
            session.keyframes.pop_front();
        }
        session.touch();
        Ok((kf_id, kf.now, byte_size))
    }

    pub fn load_keyframe(&self, session_id: u64, kf_id: u64) -> Result<(bool, u64), String> {
        self.load_keyframe_with(session_id, kf_id, |_, _| Ok(()))
    }

    /// Rebuild a World from a saved keyframe after optional preparation.
    ///
    /// `prepare` runs against the locally rebuilt World *before* keyframe state
    /// is applied and published. A failed prepare leaves the session untouched.
    pub fn load_keyframe_with(
        &self,
        session_id: u64,
        kf_id: u64,
        prepare: impl FnOnce(&Scenario, &mut World) -> Result<(), String>,
    ) -> Result<(bool, u64), String> {
        let kf_data = {
            let arc = self.get_arc(session_id)?;
            let session = arc.lock().expect("session poisoned");
            if session.is_running() {
                return Err(RUNNING_ERR.to_string());
            }
            session
                .keyframes
                .iter()
                .find(|(id, _)| *id == kf_id)
                .map(|(_, data)| data.clone())
                .ok_or_else(|| format!("keyframe {} not found", kf_id))?
        };

        let kf = sim_world::World::deserialize_keyframe(&kf_data)
            .map_err(|e| format!("keyframe deserialize error: {}", e))?;

        let scenario = Scenario::from_str(&kf.scenario_toml)
            .map_err(|e| format!("keyframe scenario parse error: {}", e))?;
        let mut world = scenario
            .build_world()
            .map_err(|e| format!("keyframe rebuild error: {}", e))?;
        world.enable_owned_device_banks();
        prepare(&scenario, &mut world)?;

        if let Err(e) = world.run_until(kf.now) {
            log::warn!(
                "keyframe restore: run_until({}) failed: {}; setting now directly",
                kf.now,
                e
            );
            world.now = kf.now;
        }
        world.load_keyframe(&kf);

        let arc = self.get_arc(session_id)?;
        let mut session = arc.lock().expect("session poisoned");
        if session.is_running() {
            return Err(RUNNING_ERR.to_string());
        }
        session.world = Some(world);
        session.state = if kf.now > 0 {
            SessionState::Paused
        } else {
            SessionState::Ready
        };
        session.touch();
        Ok((true, kf.now))
    }

    pub fn list_keyframes(&self, session_id: u64) -> Result<Vec<(u64, u64, u64)>, String> {
        let arc = self.get_arc(session_id)?;
        let session = arc.lock().expect("session poisoned");
        Ok(session
            .keyframes
            .iter()
            .map(|(id, data)| (*id, 0u64, data.len() as u64))
            .collect())
    }

    pub fn reset(&self, session_id: u64) -> Result<(), String> {
        self.reset_with(session_id, |_, _| Ok(()))
    }

    /// Rebuild the session World from its stored scenario after optional preparation.
    ///
    /// `prepare` runs against the locally rebuilt World *before* the session is
    /// published. A failed prepare leaves the previous World and session state
    /// untouched.
    pub fn reset_with(
        &self,
        session_id: u64,
        prepare: impl FnOnce(&Scenario, &mut World) -> Result<(), String>,
    ) -> Result<(), String> {
        let scenario = {
            let arc = self.get_arc(session_id)?;
            let session = arc.lock().expect("session poisoned");
            if session.is_running() {
                return Err(RUNNING_ERR.to_string());
            }
            session
                .scenario
                .clone()
                .ok_or_else(|| "no scenario loaded".to_string())?
        };

        let mut world = scenario
            .build_world()
            .map_err(|e| format!("rebuild error: {}", e))?;
        world.enable_owned_device_banks();
        prepare(&scenario, &mut world)?;

        let arc = self.get_arc(session_id)?;
        let mut session = arc.lock().expect("session poisoned");
        if session.is_running() {
            return Err(RUNNING_ERR.to_string());
        }
        session.world = Some(world);
        session.state = SessionState::Ready;
        session.n_events = 0;
        session.error_message = None;
        session.keyframes.clear();
        session.next_keyframe_id = 1;
        session.traces.clear();
        session.dropped_trace_records = 0;
        session.touch();
        Ok(())
    }

    /// Number of live sessions (test/introspection helper).
    pub fn len(&self) -> usize {
        self.inner.lock().expect("session map lock poisoned").len()
    }

    /// Whether there are no live sessions.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SessionMap {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct SessionStatus {
    pub state: SessionState,
    pub now_ticks: u64,
    pub n_machines: u32,
    pub n_events: u64,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const MINIMAL: &str = "name = \"minimal\"\n[[machine]]\nid = 0\nname = \"m0\"\n";

    #[test]
    fn deterministic_listing_ascending() {
        let map = SessionMap::new();
        let a = map.create().unwrap();
        let b = map.create().unwrap();
        let c = map.create().unwrap();
        let ids: Vec<u64> = map.list().into_iter().map(|(id, ..)| id).collect();
        assert_eq!(ids, vec![a, b, c]);
        assert!(a < b && b < c, "ids monotonically increasing");
    }

    #[test]
    fn session_limit_rejected() {
        let map = SessionMap::new();
        for _ in 0..MAX_SESSIONS {
            map.create().expect("under limit");
        }
        assert_eq!(map.len(), MAX_SESSIONS);
        let err = map.create().expect_err("creation beyond limit must fail");
        assert!(err.contains("session limit reached"), "got: {err}");
    }

    #[test]
    fn keyframe_eviction_caps_at_16() {
        let map = SessionMap::new();
        let id = map.create().unwrap();
        map.load_scenario(id, MINIMAL).unwrap();
        for _ in 0..(MAX_KEYFRAMES + 1) {
            map.save_keyframe(id).unwrap();
        }
        let kfs = map.list_keyframes(id).unwrap();
        assert_eq!(kfs.len(), MAX_KEYFRAMES, "17th save evicts the oldest");
        // The oldest (id 1) was evicted; the retained window starts at id 2.
        assert_eq!(kfs.first().unwrap().0, 2, "oldest keyframe evicted");
    }

    #[test]
    fn trace_ring_evicts_and_counts() {
        let mut s = Session::new(1);
        let overflow = 5usize;
        s.push_traces((0..(MAX_TRACE_RECORDS + overflow)).map(|i| format!("line {i}")));
        assert_eq!(s.traces.len(), MAX_TRACE_RECORDS, "ring capped");
        assert_eq!(
            s.dropped_trace_records, overflow as u64,
            "dropped counter tracks evictions"
        );
        // The oldest lines were dropped; the front is line `overflow`.
        assert_eq!(s.traces.front().unwrap(), &format!("line {overflow}"));
    }

    #[test]
    fn ttl_cleanup_respects_state() {
        let map = SessionMap::new();
        map.set_idle_ttl(Duration::ZERO);
        let idle = map.create().unwrap();
        let running = map.create().unwrap();
        // Check out the running session's (empty) world stand-in by marking it
        // Running via take_world after loading a scenario.
        map.load_scenario(running, MINIMAL).unwrap();
        let _world = map.take_world(running).unwrap(); // state -> Running
        map.cleanup(true);
        assert!(
            map.status(idle).is_err(),
            "idle session past TTL must be cleaned up"
        );
        assert!(map.status(running).is_ok(), "running session is TTL-exempt");
    }

    #[test]
    fn operations_rejected_while_running() {
        let map = SessionMap::new();
        let id = map.create().unwrap();
        map.load_scenario(id, MINIMAL).unwrap();
        let _world = map.take_world(id).unwrap(); // state -> Running
        assert_eq!(map.load_scenario(id, MINIMAL).unwrap_err(), RUNNING_ERR);
        assert_eq!(map.save_keyframe(id).unwrap_err(), RUNNING_ERR);
        assert_eq!(map.reset(id).unwrap_err(), RUNNING_ERR);
        assert_eq!(map.clone_session(id).unwrap_err(), RUNNING_ERR);
    }

    #[test]
    fn take_world_rejects_terminal_states() {
        let map = SessionMap::new();
        let id = map.create().unwrap();
        map.load_scenario(id, MINIMAL).unwrap();

        {
            let arc = map.get_arc(id).unwrap();
            let mut session = arc.lock().unwrap();
            session.state = SessionState::Done;
        }
        assert_eq!(
            map.take_world(id)
                .err()
                .expect("done session must not checkout"),
            SESSION_DONE_ERR
        );

        {
            let arc = map.get_arc(id).unwrap();
            let mut session = arc.lock().unwrap();
            session.state = SessionState::Error;
        }
        assert_eq!(
            map.take_world(id)
                .err()
                .expect("error session must not checkout"),
            SESSION_ERROR_ERR
        );
    }

    #[test]
    fn run_cannot_checkout_world_before_factories_are_attached() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        use std::thread;

        let map = Arc::new(SessionMap::new());
        let id = map.create().unwrap();

        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let factory_attached = Arc::new(AtomicBool::new(false));
        let factory_attached_prep = Arc::clone(&factory_attached);

        let map_load = Arc::clone(&map);
        let load_handle = thread::spawn(move || {
            map_load.load_scenario_with(id, MINIMAL, |_scenario, _world| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                factory_attached_prep.store(true, Ordering::SeqCst);
                Ok(())
            })
        });

        entered_rx.recv().unwrap();

        // Before commit, the new World must not be visible / check-out-able.
        assert!(
            map.take_world(id).is_err(),
            "Run must not check out a World published before factory attachment"
        );
        assert!(!factory_attached.load(Ordering::SeqCst));
        assert_eq!(map.status(id).unwrap().state, SessionState::Idle);

        let map_race = Arc::clone(&map);
        let attached_race = Arc::clone(&factory_attached);
        let (saw_world_tx, saw_world_rx) = mpsc::channel::<bool>();
        let race_handle = thread::spawn(move || {
            // Park until load commits, then take the world.
            for _ in 0..1_000_000 {
                match map_race.take_world(id) {
                    Ok(world) => {
                        let _ = saw_world_tx.send(attached_race.load(Ordering::SeqCst));
                        return Some(world);
                    }
                    Err(_) => thread::yield_now(),
                }
            }
            None
        });

        // While prepare is blocked, the racer must not observe a World.
        assert!(
            saw_world_rx.try_recv().is_err(),
            "World must not be visible before prepare commits"
        );

        release_tx.send(()).unwrap();
        load_handle.join().unwrap().unwrap();
        assert!(factory_attached.load(Ordering::SeqCst));

        let world = race_handle
            .join()
            .unwrap()
            .expect("Run must obtain the World only after atomic commit");
        assert!(
            saw_world_rx.recv().unwrap(),
            "checkout must happen only after factory attachment"
        );
        map.return_world(id, world, SessionState::Done, 0, None)
            .unwrap();
    }

    #[test]
    fn preparation_failure_leaves_previous_world_untouched() {
        let map = SessionMap::new();
        let id = map.create().unwrap();
        map.load_scenario(id, MINIMAL).unwrap();
        {
            let arc = map.get_arc(id).unwrap();
            let mut session = arc.lock().unwrap();
            session.traces.push_back("keep".into());
            session.n_events = 3;
        }

        let err = map
            .load_scenario_with(id, MINIMAL, |_, _| Err("prepare boom".into()))
            .unwrap_err();
        assert!(err.contains("prepare boom"), "{err}");

        let status = map.status(id).unwrap();
        assert_eq!(status.state, SessionState::Ready);
        assert_eq!(status.n_events, 3);
        let arc = map.get_arc(id).unwrap();
        let session = arc.lock().unwrap();
        assert!(session.world.is_some());
        assert_eq!(session.traces.front().map(String::as_str), Some("keep"));
    }

    #[test]
    fn reset_preparation_failure_leaves_previous_world_untouched() {
        let map = SessionMap::new();
        let id = map.create().unwrap();
        map.load_scenario(id, MINIMAL).unwrap();
        {
            let arc = map.get_arc(id).unwrap();
            let mut session = arc.lock().unwrap();
            session.traces.push_back("keep".into());
            session.n_events = 3;
        }

        let err = map
            .reset_with(id, |_, _| Err("prepare boom".into()))
            .unwrap_err();
        assert!(err.contains("prepare boom"), "{err}");

        let status = map.status(id).unwrap();
        assert_eq!(status.state, SessionState::Ready);
        assert_eq!(status.n_events, 3);
        let arc = map.get_arc(id).unwrap();
        let session = arc.lock().unwrap();
        assert!(session.world.is_some());
        assert_eq!(session.traces.front().map(String::as_str), Some("keep"));
    }

    #[test]
    fn clone_preparation_failure_leaves_session_map_untouched() {
        let map = SessionMap::new();
        let id = map.create().unwrap();
        map.load_scenario(id, MINIMAL).unwrap();
        let before = map.len();

        let err = map
            .clone_session_with(id, |_, _| Err("prepare boom".into()))
            .unwrap_err();
        assert!(err.contains("prepare boom"), "{err}");
        assert_eq!(map.len(), before, "no new session on prepare failure");
    }

    #[test]
    fn keyframe_preparation_failure_leaves_previous_world_untouched() {
        let map = SessionMap::new();
        let id = map.create().unwrap();
        map.load_scenario(id, MINIMAL).unwrap();
        let (kf_id, _, _) = map.save_keyframe(id).unwrap();
        {
            let arc = map.get_arc(id).unwrap();
            let mut session = arc.lock().unwrap();
            session.traces.push_back("keep".into());
            session.n_events = 3;
        }

        let err = map
            .load_keyframe_with(id, kf_id, |_, _| Err("prepare boom".into()))
            .unwrap_err();
        assert!(err.contains("prepare boom"), "{err}");

        let status = map.status(id).unwrap();
        assert_eq!(status.state, SessionState::Ready);
        assert_eq!(status.n_events, 3);
        let arc = map.get_arc(id).unwrap();
        let session = arc.lock().unwrap();
        assert!(session.world.is_some());
        assert_eq!(session.traces.front().map(String::as_str), Some("keep"));
    }
}
