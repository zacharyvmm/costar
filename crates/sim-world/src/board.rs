//! Board peripheral mapping — maps Zephyr devicetree labels to costar virtual
//! device IDs.
//!
//! A board config TOML file describes which virtual peripherals a simulated
//! board provides, e.g.:
//!
//! ```toml
//! [peripherals]
//! uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1" }
//! i2c0 = { device = "i2c", id = 0, sda = "gpio4", scl = "gpio5" }
//! spi0 = { device = "spi", id = 0, mosi = "gpio16", miso = "gpio17", sck = "gpio18" }
//! gpio0 = { device = "gpio", id = 0 }
//! ```
//!
//! # Validation rules
//!
//! - Duplicate (device_type, id) pairs are rejected.
//! - Required port mappings must be present (e.g. UART requires `tx` + `rx`).
//! - Unknown device types are rejected.
//! - Unknown fields in peripheral definitions are rejected.
//!
//! # Device type reference
//!
//! | Type        | Required ports       | Default speed    |
//! |-------------|---------------------|------------------|
//! | uart        | tx, rx              | 115200           |
//! | i2c         | sda, scl            | 100000           |
//! | spi         | mosi, miso, sck     | 1000000          |
//! | gpio        | (none)              | —                |
//! | timer       | (none)              | —                |
//! | can         | (none)              | 500000           |
//! | adc         | (none)              | —                |
//! | temp_sensor | (none)              | —                |
//! | entropy     | (none)              | —                |
//! | eeprom      | (none)              | —                |
//! | flash       | (none)              | —                |

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

// ── TOML representation ────────────────────────────────────────────────────

/// Top-level board configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardConfig {
    /// Devicetree label → peripheral definition mapping.
    #[serde(default)]
    pub peripherals: BTreeMap<String, PeripheralDef>,
}

/// A peripheral definition mapping a devicetree label to a virtual device.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeripheralDef {
    /// Device type: "uart", "i2c", "spi", "gpio", "timer", "can",
    /// "adc", "temp_sensor", "entropy", "eeprom", "flash".
    pub device: String,

    /// Device instance ID (unique per device type).
    pub id: u32,

    // ── Optional UART pins ──
    /// GPIO label for UART TX pin.
    #[serde(default)]
    pub tx: Option<String>,

    /// GPIO label for UART RX pin.
    #[serde(default)]
    pub rx: Option<String>,

    // ── Optional I2C pins ──
    /// GPIO label for I2C SDA pin.
    #[serde(default)]
    pub sda: Option<String>,

    /// GPIO label for I2C SCL pin.
    #[serde(default)]
    pub scl: Option<String>,

    // ── Optional SPI pins ──
    /// GPIO label for SPI MOSI pin.
    #[serde(default)]
    pub mosi: Option<String>,

    /// GPIO label for SPI MISO pin.
    #[serde(default)]
    pub miso: Option<String>,

    /// GPIO label for SPI SCK pin.
    #[serde(default)]
    pub sck: Option<String>,

    // ── Optional speed / timing fields ──
    /// Bus speed in Hz (defaults vary by device type).
    #[serde(default)]
    pub speed_hz: Option<u32>,

    /// IRQ line number for timer devices.
    #[serde(default)]
    pub irq: Option<u32>,
}

// ── Error type ─────────────────────────────────────────────────────────────

/// Errors that can occur when loading or validating a board config.
#[derive(Debug)]
pub enum BoardError {
    /// I/O error reading the config file.
    Io(std::io::Error),
    /// TOML parse error.
    Parse(toml::de::Error),
    /// Validation error.
    Invalid(String),
}

impl fmt::Display for BoardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BoardError::Io(e) => write!(f, "failed to read board config: {}", e),
            BoardError::Parse(e) => write!(f, "failed to parse board config: {}", e),
            BoardError::Invalid(msg) => write!(f, "invalid board config: {}", msg),
        }
    }
}

impl From<std::io::Error> for BoardError {
    fn from(e: std::io::Error) -> Self {
        BoardError::Io(e)
    }
}

impl From<toml::de::Error> for BoardError {
    fn from(e: toml::de::Error) -> Self {
        BoardError::Parse(e)
    }
}

// ── Well-known device types ────────────────────────────────────────────────

/// The set of recognised device type strings.
const KNOWN_DEVICE_TYPES: &[&str] = &[
    "uart",
    "i2c",
    "spi",
    "gpio",
    "timer",
    "can",
    "adc",
    "temp_sensor",
    "entropy",
    "eeprom",
    "flash",
    "display",
    "touch",
];

/// Device types that require port mappings (pin assignments).
const PORTED_DEVICE_TYPES: &[(&str, &[&str])] = &[
    ("uart", &["tx", "rx"]),
    ("i2c", &["sda", "scl"]),
    ("spi", &["mosi", "miso", "sck"]),
];

// ── BoardConfig methods ────────────────────────────────────────────────────

impl BoardConfig {
    /// Load a board config from a TOML file path.
    pub fn from_file(path: &str) -> Result<Self, BoardError> {
        let content = std::fs::read_to_string(path)?;
        Self::from_str(&content)
    }

    /// Parse a board config from a TOML string.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(toml_str: &str) -> Result<Self, BoardError> {
        let config: BoardConfig = toml::from_str(toml_str)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate the board config.
    ///
    /// Checks:
    /// - No duplicate (device_type, id) pairs.
    /// - All device types are known.
    /// - Required port mappings are present.
    /// - No empty peripherals section (warn but not error — some boards
    ///   might only have CPU-level devices).
    pub fn validate(&self) -> Result<(), BoardError> {
        let mut seen_types_and_ids: BTreeSet<(String, u32)> = BTreeSet::new();

        for (label, def) in &self.peripherals {
            let device_type = def.device.as_str();

            // ── Unknown device type? ──────────────────────────
            if !KNOWN_DEVICE_TYPES.contains(&device_type) {
                return Err(BoardError::Invalid(format!(
                    "unknown device type '{}' for label '{}' (known types: {})",
                    device_type,
                    label,
                    KNOWN_DEVICE_TYPES.join(", ")
                )));
            }

            // ── Duplicate (type, id)? ─────────────────────────
            let type_id_key = (device_type.to_string(), def.id);
            if !seen_types_and_ids.insert(type_id_key) {
                return Err(BoardError::Invalid(format!(
                    "duplicate device '{}' with id {} (label '{}')",
                    device_type, def.id, label
                )));
            }

            // ── Required port mappings present? ──────────────
            for &(dtype, required_ports) in PORTED_DEVICE_TYPES {
                if dtype == device_type {
                    for port in required_ports {
                        let value = match *port {
                            "tx" => &def.tx,
                            "rx" => &def.rx,
                            "sda" => &def.sda,
                            "scl" => &def.scl,
                            "mosi" => &def.mosi,
                            "miso" => &def.miso,
                            "sck" => &def.sck,
                            _ => continue,
                        };
                        if value.is_none() {
                            return Err(BoardError::Invalid(format!(
                                "device '{}' (label '{}') requires port '{}'",
                                device_type, label, port
                            )));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Initialise virtual devices from the board config.
    ///
    /// Creates the appropriate virtual device for each peripheral entry
    /// and inserts it into the thread-local device storage (via the
    /// sim-devices crate).
    ///
    /// Returns the number of peripherals initialised.
    pub fn initialize_devices(&self) -> usize {
        let mut count = 0;

        for def in self.peripherals.values() {
            match def.device.as_str() {
                "uart" => {
                    let baud = def.speed_hz.unwrap_or(115200);
                    sim_devices::uart_insert(sim_devices::VirtualUart::new(def.id, baud));
                    count += 1;
                }
                "gpio" => {
                    sim_devices::gpio_insert(sim_devices::VirtualGpio::new(def.id));
                    count += 1;
                }
                "i2c" => {
                    let speed = def.speed_hz.unwrap_or(100_000);
                    sim_devices::i2c_insert(sim_devices::VirtualI2c::new(def.id, speed));
                    count += 1;
                }
                "spi" => {
                    let speed = def.speed_hz.unwrap_or(1_000_000);
                    sim_devices::spi_insert(sim_devices::VirtualSpi::new(def.id, speed));
                    count += 1;
                }
                "can" => {
                    let bitrate = def.speed_hz.unwrap_or(500_000);
                    sim_devices::can_insert(sim_devices::VirtualCan::new(def.id, bitrate));
                    count += 1;
                }
                "adc" => {
                    sim_devices::adc_insert(sim_devices::VirtualAdc::new(def.id));
                    count += 1;
                }
                "temp_sensor" => {
                    sim_devices::temp_sensor_insert(sim_devices::VirtualTempSensor::new(def.id));
                    count += 1;
                }
                "entropy" => {
                    sim_devices::entropy_insert(sim_devices::VirtualEntropy::new(def.id));
                    count += 1;
                }
                "eeprom" => {
                    sim_devices::eeprom_insert(sim_devices::VirtualEeprom::new(def.id));
                    count += 1;
                }
                "flash" => {
                    sim_devices::flash_insert(sim_devices::VirtualFlash::new(def.id));
                    count += 1;
                }
                "timer" => {
                    let irq = def.irq.unwrap_or(0);
                    sim_devices::timer_insert(sim_devices::VirtualTimer::new_oneshot(def.id, irq));
                    count += 1;
                }
                "display" => {
                    let width = def.speed_hz.unwrap_or(320) as u16;
                    let height = def.irq.unwrap_or(240) as u16;
                    sim_devices::display_insert(sim_devices::VirtualDisplay::new(
                        def.id,
                        width,
                        height,
                        sim_devices::DisplayColorMode::Rgb565,
                    ));
                    count += 1;
                }
                "touch" => {
                    sim_devices::touch_insert(sim_devices::VirtualTouchScreen::new(def.id, 0));
                    count += 1;
                }
                _ => {
                    // Ignore unknown types — validate() already catches these.
                }
            }
        }

        count
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_board_config() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1" }
gpio0 = { device = "gpio", id = 0 }
"#;
        let config = BoardConfig::from_str(toml_str).unwrap();
        assert_eq!(config.peripherals.len(), 2);
        assert_eq!(config.peripherals["uart0"].device, "uart");
        assert_eq!(config.peripherals["uart0"].tx.as_deref(), Some("gpio0"));
        assert_eq!(config.peripherals["gpio0"].device, "gpio");
    }

    #[test]
    fn test_valid_board_config_with_i2c_spi() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1" }
i2c0 = { device = "i2c", id = 0, sda = "gpio4", scl = "gpio5" }
spi0 = { device = "spi", id = 0, mosi = "gpio16", miso = "gpio17", sck = "gpio18" }
"#;
        let config = BoardConfig::from_str(toml_str).unwrap();
        assert_eq!(config.peripherals.len(), 3);
        assert_eq!(config.peripherals["i2c0"].sda.as_deref(), Some("gpio4"));
        assert_eq!(config.peripherals["spi0"].mosi.as_deref(), Some("gpio16"));
    }

    #[test]
    fn test_empty_peripherals_is_ok() {
        let toml_str = "";
        let config = BoardConfig::from_str(toml_str).unwrap();
        assert!(config.peripherals.is_empty());
    }

    #[test]
    fn test_duplicate_device_type_id_rejected() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1" }
uart1 = { device = "uart", id = 0, tx = "gpio2", rx = "gpio3" }
"#;
        let result = BoardConfig::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn test_different_types_same_id_ok() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1" }
gpio0 = { device = "gpio", id = 0 }
i2c0 = { device = "i2c", id = 0, sda = "gpio4", scl = "gpio5" }
"#;
        // Different device types can share the same numeric ID.
        let config = BoardConfig::from_str(toml_str).unwrap();
        assert_eq!(config.peripherals.len(), 3);
    }

    #[test]
    fn test_unknown_device_type_rejected() {
        let toml_str = r#"
[peripherals]
mystery0 = { device = "pixie_dust", id = 0 }
"#;
        let result = BoardConfig::from_str(toml_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unknown device type"));
    }

    #[test]
    fn test_missing_uart_tx_rejected() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, rx = "gpio1" }
"#;
        let result = BoardConfig::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires port"));
    }

    #[test]
    fn test_missing_uart_rx_rejected() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0" }
"#;
        let result = BoardConfig::from_str(toml_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires port"));
    }

    #[test]
    fn test_missing_i2c_sda_rejected() {
        let toml_str = r#"
[peripherals]
i2c0 = { device = "i2c", id = 0, scl = "gpio5" }
"#;
        let result = BoardConfig::from_str(toml_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("requires port 'sda'"));
    }

    #[test]
    fn test_missing_spi_mosi_rejected() {
        let toml_str = r#"
[peripherals]
spi0 = { device = "spi", id = 0, miso = "gpio17", sck = "gpio18" }
"#;
        let result = BoardConfig::from_str(toml_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("requires port 'mosi'"));
    }

    #[test]
    fn test_gpio_no_ports_required() {
        let toml_str = r#"
[peripherals]
gpio0 = { device = "gpio", id = 0 }
gpio1 = { device = "gpio", id = 1 }
"#;
        let config = BoardConfig::from_str(toml_str).unwrap();
        assert_eq!(config.peripherals.len(), 2);
    }

    #[test]
    fn test_optional_speed_hz() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1", speed_hz = 9600 }
spi0 = { device = "spi", id = 0, mosi = "gpio16", miso = "gpio17", sck = "gpio18", speed_hz = 500000 }
"#;
        let config = BoardConfig::from_str(toml_str).unwrap();
        assert_eq!(config.peripherals["uart0"].speed_hz, Some(9600));
        assert_eq!(config.peripherals["spi0"].speed_hz, Some(500_000));
    }

    #[test]
    fn test_unknown_field_rejected() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1", bogus = "oops" }
"#;
        let result = BoardConfig::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_initialize_devices() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1" }
gpio0 = { device = "gpio", id = 0 }
"#;
        let config = BoardConfig::from_str(toml_str).unwrap();
        let n = config.initialize_devices();
        assert_eq!(n, 2);

        // Verify UART was inserted.
        assert!(sim_devices::with_uart(0, |u| u.id == 0).unwrap_or(false));
        // Verify GPIO was inserted.
        assert!(sim_devices::with_gpio_mut(0, |_| {}).is_some());
    }

    #[test]
    fn test_initialize_devices_all_types() {
        let toml_str = r#"
[peripherals]
uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1" }
i2c0 = { device = "i2c", id = 0, sda = "gpio4", scl = "gpio5" }
spi0 = { device = "spi", id = 0, mosi = "gpio16", miso = "gpio17", sck = "gpio18" }
gpio0 = { device = "gpio", id = 0 }
can0 = { device = "can", id = 0 }
adc0 = { device = "adc", id = 0 }
temp0 = { device = "temp_sensor", id = 0 }
entropy0 = { device = "entropy", id = 0 }
eeprom0 = { device = "eeprom", id = 0 }
flash0 = { device = "flash", id = 0 }
timer0 = { device = "timer", id = 0, irq = 5 }
"#;
        let config = BoardConfig::from_str(toml_str).unwrap();
        let n = config.initialize_devices();
        assert_eq!(n, 11);

        // Spot-check a few.
        assert!(sim_devices::with_i2c(0, |d| d.speed_hz == 100_000).unwrap_or(false));
        assert!(sim_devices::with_can(0, |d| d.bitrate == 500_000).unwrap_or(false));
        assert!(sim_devices::with_entropy(0, |d| d.id == 0).unwrap_or(false));
    }
}
