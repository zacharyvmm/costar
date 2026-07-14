use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A typed scalar value used in semantic event fields and predicate matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalarValue {
    /// Boolean value.
    Bool(bool),
    /// Unsigned 64-bit integer.
    U64(u64),
    /// Signed 64-bit integer.
    I64(i64),
    /// UTF-8 string.
    String(String),
}

/// Kinds of simulated devices that can appear in a machine configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceType {
    /// Display device (screen, LED matrix).
    Display,
    /// Touch input device.
    Touch,
    /// CAN bus controller.
    Can,
    /// Hardware timer.
    Timer,
    /// ADC (analog-to-digital converter).
    Adc,
    /// UART serial interface.
    Uart,
    /// GPIO pin.
    Gpio,
    /// I²C bus interface.
    I2c,
    /// SPI bus interface.
    Spi,
    /// IRQ line.
    Irq,
    /// Hardware entropy / RNG source.
    Entropy,
}
/// A condition that can be checked against a device's current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceCondition {
    /// Whether the display is enabled.
    DisplayEnabled(bool),
    /// Display backlight level (0--255).
    DisplayBacklight(u8),
    /// Number of pending touch events.
    TouchPending(u32),
    /// Number of frames waiting in the CAN receive queue.
    CanRxQueueLen(u32),
    /// Number of frames waiting in the CAN transmit queue.
    CanTxQueueLen(u32),
}

/// A high-level semantic event emitted by a machine during simulation.
#[derive(Debug, Clone)]
pub struct SemanticEvent {
    /// Machine that emitted the event, or `None` for global events.
    pub machine_id: Option<u64>,
    /// Event type identifier (e.g. `"bms.overtemp"`).
    pub event_type: String,
    /// Named fields with typed scalar values.
    pub fields: BTreeMap<String, ScalarValue>,
}

/// A predicate that decides whether the simulation should continue running.
///
/// Predicates are checked after each world step; when a predicate no longer
/// holds, the run terminates.
#[derive(Debug, Clone)]
pub enum ContinuePredicate {
    /// Match a semantic event by machine, event type, and field values.
    Semantic {
        /// Machine that must emit the event, or `None` for any machine.
        machine_id: Option<u64>,
        /// Required event type.
        event_type: String,
        /// Required fields and their expected values.
        fields: BTreeMap<String, ScalarValue>,
    },
    /// Check a device condition on a specific machine.
    Device {
        /// The machine owning the device.
        machine_id: u64,
        /// Kind of device to check.
        device_type: DeviceType,
        /// Device instance index.
        device_id: u32,
        /// The condition to evaluate.
        condition: DeviceCondition,
    },
    /// Check whether a CAN frame was dropped on a bus.
    DroppedFrame {
        /// CAN bus name.
        bus: String,
        /// The expected dropped message id.
        message_id: u32,
    },
    /// Check whether a named assertion failure was recorded.
    AssertionFailure {
        /// Assertion name that must have fired.
        name: String,
    },
}

impl ContinuePredicate {
    /// Check whether this predicate holds for the given [`World`] state.
    pub fn holds(&self, world: &crate::world::World) -> bool {
        match self {
            ContinuePredicate::Semantic {
                machine_id,
                event_type,
                fields,
            } => world.semantic_events().iter().any(|ev| {
                machine_id.is_none_or(|id| ev.machine_id == Some(id))
                    && ev.event_type == *event_type
                    && fields.iter().all(|(k, v)| ev.fields.get(k) == Some(v))
            }),
            ContinuePredicate::Device {
                machine_id,
                device_type: _,
                device_id: _,
                condition: _,
            } => {
                // Device condition checking requires machine device state access.
                // Stub: return false until device introspection is plumbed.
                let _ = machine_id;
                false
            }
            ContinuePredicate::DroppedFrame { bus, message_id } => world
                .buses()
                .iter()
                .any(|b| b.name == *bus && b.is_dropped(*message_id)),
            ContinuePredicate::AssertionFailure { name } => {
                world.assertion_failures().iter().any(|n| n == name)
            }
        }
    }
}
