//! Virtual touch screen peripheral.
//!
//! A `VirtualTouchScreen` models a touch input device associated with a
//! display.  The GUI injects touch events; the C firmware reads them via
//! a FIFO queue.

/// A touch event from the simulated touch screen.
#[derive(Debug, Clone, Copy)]
pub struct TouchEvent {
    /// Unique point identifier (for multi-touch).
    pub point_id: u32,
    /// X coordinate.
    pub x: u16,
    /// Y coordinate.
    pub y: u16,
    /// Pressure 0-255.
    pub pressure: u8,
    /// Event type.
    pub event_type: TouchEventType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchEventType {
    Press,
    Release,
    Move,
}

/// Virtual touch screen device.
pub struct VirtualTouchScreen {
    /// Device instance ID.
    pub id: u32,
    /// Associated display device ID.
    pub display_id: u32,
    /// Queue of pending touch events (FIFO).
    events: std::collections::VecDeque<TouchEvent>,
    /// Maximum queue size.
    max_events: usize,
}

impl VirtualTouchScreen {
    /// Create a new touch screen.
    pub fn new(id: u32, display_id: u32) -> Self {
        Self {
            id,
            display_id,
            events: std::collections::VecDeque::new(),
            max_events: 64,
        }
    }

    /// Firmware reads the next touch event. Returns false if queue is empty.
    pub fn get_event(&mut self, out: &mut TouchEvent) -> bool {
        match self.events.pop_front() {
            Some(ev) => {
                *out = ev;
                true
            }
            None => false,
        }
    }

    /// GUI injects a touch event.
    pub fn inject_event(&mut self, event: TouchEvent) {
        if self.events.len() >= self.max_events {
            // Drop oldest to make room.
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Number of pending touch events.
    pub fn pending_count(&self) -> usize {
        self.events.len()
    }
}
