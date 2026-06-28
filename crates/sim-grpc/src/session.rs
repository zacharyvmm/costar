use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use sim_world::scenario::Scenario;
use sim_world::World;

#[allow(dead_code)]
pub struct Session {
    pub id: u64,
    pub world: Option<World>,
    pub scenario: Option<Scenario>,
    pub scenario_toml: Option<String>,
    pub keyframes: Vec<(u64, Vec<u8>)>,
    pub next_keyframe_id: u64,
    pub state: SessionState,
    pub n_events: u64,
    pub error_message: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Ready,
    Running,
    Paused,
    Done,
    Error(String),
}

impl SessionState {
    pub fn as_str(&self) -> &str {
        match self {
            SessionState::Idle => "idle",
            SessionState::Ready => "ready",
            SessionState::Running => "running",
            SessionState::Paused => "paused",
            SessionState::Done => "done",
            SessionState::Error(_) => "error",
        }
    }
}

pub struct SessionMap {
    inner: Mutex<HashMap<u64, Session>>,
    next_id: AtomicU64,
}

impl SessionMap {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn create(&self) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut map = self.inner.lock().expect("session map lock poisoned");
        map.insert(
            id,
            Session {
                id,
                world: None,
                scenario: None,
                scenario_toml: None,
                keyframes: Vec::new(),
                next_keyframe_id: 1,
                state: SessionState::Idle,
                n_events: 0,
                error_message: None,
            },
        );
        id
    }

    pub fn destroy(&self, session_id: u64) -> bool {
        let mut map = self.inner.lock().expect("session map lock poisoned");
        map.remove(&session_id).is_some()
    }

    pub fn clone_session(&self, session_id: u64) -> Option<u64> {
        let mut map = self.inner.lock().expect("session map lock poisoned");
        let source = map.get(&session_id)?;
        // Clone data from source before mutable insert.
        let scenario_toml = source.scenario_toml.clone();
        let (world, scenario) = if let Some(ref sc) = source.scenario {
            match sc.build_world() {
                Ok(w) => (Some(w), Some(sc.clone())),
                Err(_) => (None, None),
            }
        } else {
            (None, None)
        };
        let has_world = world.is_some();
        let new_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        map.insert(
            new_id,
            Session {
                id: new_id,
                world,
                scenario,
                scenario_toml,
                keyframes: Vec::new(),
                next_keyframe_id: 1,
                state: if has_world {
                    SessionState::Ready
                } else {
                    SessionState::Idle
                },
                n_events: 0,
                error_message: None,
            },
        );
        Some(new_id)
    }

    pub fn list(&self) -> Vec<(u64, String, u64, u32)> {
        let map = self.inner.lock().expect("session map lock poisoned");
        map.iter()
            .map(|(id, s)| {
                (
                    *id,
                    s.state.as_str().to_string(),
                    s.world.as_ref().map_or(0, |w| w.now),
                    s.scenario
                        .as_ref()
                        .map_or(0, |sc| sc.machine.len() as u32),
                )
            })
            .collect()
    }

    pub fn load_scenario(
        &self,
        session_id: u64,
        toml_str: &str,
    ) -> Result<(u32, u32, u32), String> {
        let scenario =
            Scenario::from_str(toml_str).map_err(|e| format!("parse error: {}", e))?;
        let n_machines = scenario.machine.len() as u32;
        let n_links = scenario.link.len() as u32;
        let n_injections = scenario.inject.len() as u32;
        let world = scenario
            .build_world()
            .map_err(|e| format!("build error: {}", e))?;
        let mut map = self.inner.lock().expect("session map lock poisoned");
        let session = map
            .get_mut(&session_id)
            .ok_or_else(|| format!("session {} not found", session_id))?;
        session.world = Some(world);
        session.scenario = Some(scenario);
        session.scenario_toml = Some(toml_str.to_string());
        session.state = SessionState::Ready;
        session.n_events = 0;
        session.error_message = None;
        Ok((n_machines, n_links, n_injections))
    }

    pub fn status(&self, session_id: u64) -> Result<SessionStatus, String> {
        let map = self.inner.lock().expect("session map lock poisoned");
        let session = map
            .get(&session_id)
            .ok_or_else(|| format!("session {} not found", session_id))?;
        Ok(SessionStatus {
            state: session.state.clone(),
            now_ticks: session.world.as_ref().map_or(0, |w| w.now),
            n_machines: session
                .scenario
                .as_ref()
                .map_or(0, |s| s.machine.len() as u32),
            n_events: session.n_events,
            error: session.error_message.clone(),
        })
    }

    pub fn take_world(&self, session_id: u64) -> Result<World, String> {
        let mut map = self.inner.lock().expect("session map lock poisoned");
        let session = map
            .get_mut(&session_id)
            .ok_or_else(|| format!("session {} not found", session_id))?;
        session
            .world
            .take()
            .ok_or_else(|| format!("no world loaded in session {}", session_id))
    }

    #[allow(dead_code)]
    pub fn return_world(
        &self,
        session_id: u64,
        world: World,
        state: SessionState,
        n_events: u64,
        error: Option<String>,
    ) -> Result<(), String> {
        let mut map = self.inner.lock().expect("session map lock poisoned");
        let session = map
            .get_mut(&session_id)
            .ok_or_else(|| format!("session {} not found", session_id))?;
        session.world = Some(world);
        session.state = state;
        session.n_events = n_events;
        session.error_message = error;
        Ok(())
    }

    pub fn save_keyframe(&self, session_id: u64) -> Result<(u64, u64, u64), String> {
        let mut map = self.inner.lock().expect("session map lock poisoned");
        let session = map
            .get_mut(&session_id)
            .ok_or_else(|| format!("session {} not found", session_id))?;
        let world = session
            .world
            .as_mut()
            .ok_or_else(|| "no world loaded".to_string())?;
        let kf = world.save_keyframe();
        let data: Vec<u8> = Vec::new();
        let byte_size = data.len() as u64;
        let kf_id = session.next_keyframe_id;
        session.next_keyframe_id += 1;
        session.keyframes.push((kf_id, data));
        Ok((kf_id, kf.now, byte_size))
    }

    pub fn load_keyframe(&self, session_id: u64, kf_id: u64) -> Result<(bool, u64), String> {
        let mut map = self.inner.lock().expect("session map lock poisoned");
        let session = map
            .get_mut(&session_id)
            .ok_or_else(|| format!("session {} not found", session_id))?;
        let _kf_data = session
            .keyframes
            .iter()
            .find(|(id, _)| *id == kf_id)
            .map(|(_, data)| data.clone())
            .ok_or_else(|| format!("keyframe {} not found", kf_id))?;
        if let Some(ref world) = session.world {
            let now = world.now;
            Ok((true, now))
        } else {
            Err("no world loaded".to_string())
        }
    }

    pub fn list_keyframes(&self, session_id: u64) -> Result<Vec<(u64, u64, u64)>, String> {
        let map = self.inner.lock().expect("session map lock poisoned");
        let session = map
            .get(&session_id)
            .ok_or_else(|| format!("session {} not found", session_id))?;
        Ok(session
            .keyframes
            .iter()
            .map(|(id, data)| (*id, 0u64, data.len() as u64))
            .collect())
    }

    pub fn reset(&self, session_id: u64) -> Result<(), String> {
        let mut map = self.inner.lock().expect("session map lock poisoned");
        let session = map
            .get_mut(&session_id)
            .ok_or_else(|| format!("session {} not found", session_id))?;
        let scenario = session
            .scenario
            .clone()
            .ok_or_else(|| "no scenario loaded".to_string())?;
        let world = scenario
            .build_world()
            .map_err(|e| format!("rebuild error: {}", e))?;
        session.world = Some(world);
        session.state = SessionState::Ready;
        session.n_events = 0;
        session.error_message = None;
        session.keyframes.clear();
        session.next_keyframe_id = 1;
        Ok(())
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
