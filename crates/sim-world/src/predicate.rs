use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalarValue {
    Bool(bool),
    U64(u64),
    I64(i64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceType {
    Display,
    Touch,
    Can,
    Timer,
    Adc,
    Uart,
    Gpio,
    I2c,
    Spi,
    Irq,
    Entropy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceCondition {
    DisplayEnabled(bool),
    DisplayBacklight(u8),
    TouchPending(u32),
    CanRxQueueLen(u32),
    CanTxQueueLen(u32),
}

#[derive(Debug, Clone)]
pub struct SemanticEvent {
    pub machine_id: Option<u64>,
    pub event_type: String,
    pub fields: BTreeMap<String, ScalarValue>,
}

#[derive(Debug, Clone)]
pub enum ContinuePredicate {
    Semantic {
        machine_id: Option<u64>,
        event_type: String,
        fields: BTreeMap<String, ScalarValue>,
    },
    Device {
        machine_id: u64,
        device_type: DeviceType,
        device_id: u32,
        condition: DeviceCondition,
    },
    DroppedFrame {
        bus: String,
        message_id: u32,
    },
    AssertionFailure {
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
                machine_id.map_or(true, |id| ev.machine_id == Some(id))
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
