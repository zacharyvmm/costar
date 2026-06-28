//! Simulation configuration loaded from TOML files.
//!
//! # Example config file (sim.toml)
//!
//! ```toml
//! [simulation]
//! mode = "deterministic"       # or "interactive"
//! watchdog_secs = 10           # optional wall-clock timeout
//! tick_rate_hz = 1000          # virtual ticks per second
//!
//! [trace]
//! golden = false               # machine-readable output format
//! ```

use serde::Deserialize;

use crate::cli::{SimMode, TraceFormat};

/// Top-level configuration loaded from a TOML file.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SimConfig {
    /// Simulation settings.
    #[serde(default)]
    pub simulation: SimulationSection,
    /// Trace output settings.
    #[serde(default)]
    pub trace: TraceSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationSection {
    /// Simulation mode.
    #[serde(default)]
    pub mode: SimMode,
    /// Optional wall-clock watchdog timeout in seconds.
    #[serde(default)]
    pub watchdog_secs: Option<u64>,
    /// Virtual tick rate in Hz (default: 1000).
    #[serde(default = "default_tick_rate")]
    pub tick_rate_hz: u32,
}

fn default_tick_rate() -> u32 {
    1000
}

impl Default for SimulationSection {
    fn default() -> Self {
        Self {
            mode: SimMode::default(),
            watchdog_secs: None,
            tick_rate_hz: default_tick_rate(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TraceSection {
    /// Machine-readable golden trace output (no header/footer).
    #[serde(default)]
    pub golden: bool,
    /// Trace output format.
    #[serde(default)]
    pub format: Option<TraceFormat>,
}

impl SimConfig {
    /// Load configuration from a TOML file path.
    pub fn from_file(path: &str) -> Result<Self, ConfigError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| ConfigError::ReadFailed(path.into(), e))?;
        toml::from_str(&content).map_err(|e| ConfigError::ParseFailed(path.into(), e))
    }
}

/// Errors that can occur when loading configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file '{0}': {1}")]
    ReadFailed(String, #[source] std::io::Error),
    #[error("failed to parse config file '{0}': {1}")]
    ParseFailed(String, #[source] toml::de::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SimConfig::default();
        assert_eq!(config.simulation.mode, SimMode::Deterministic);
        assert_eq!(config.simulation.tick_rate_hz, 1000);
        assert!(config.simulation.watchdog_secs.is_none());
        assert!(!config.trace.golden);
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml_str = r#"
[simulation]
mode = "interactive"
"#;
        let config: SimConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.simulation.mode, SimMode::Interactive);
        // defaults preserved
        assert_eq!(config.simulation.tick_rate_hz, 1000);
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
[simulation]
mode = "deterministic"
watchdog_secs = 30
tick_rate_hz = 2000

[trace]
golden = true
"#;
        let config: SimConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.simulation.watchdog_secs, Some(30));
        assert_eq!(config.simulation.tick_rate_hz, 2000);
        assert!(config.trace.golden);
    }

    #[test]
    fn test_unknown_field_rejected() {
        let toml_str = r#"
[simulation]
mode = "deterministic"
bogus_field = 42
"#;
        let result: Result<SimConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }
}
