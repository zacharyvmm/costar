//! Virtual GPIO peripheral.
//!
//! Models a set of GPIO pins with configurable direction (input/output),
//! level state, and optional interrupt-on-change.

/// Maximum number of GPIO pins per port.
pub const MAX_PINS: usize = 32;

/// GPIO pin direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioMode {
    /// Pin configured as an input (high-impedance).
    Input,
    /// Pin configured as an output (driven).
    Output,
    /// Pin in alternate function mode (UART, timer, etc.).
    Alternate,
}

/// A single GPIO pin.
#[derive(Debug, Clone, Copy)]
pub struct GpioPin {
    /// Pin direction.
    pub mode: GpioMode,
    /// Logic level: `true` = high, `false` = low.
    pub state: bool,
    /// IRQ number to raise on state change, if any.
    pub irq_on_change: Option<u32>,
    /// Whether the interrupt is edge-triggered (`false` = level).
    pub edge_triggered: bool,
    /// Rising-edge trigger enabled.
    pub rising_trigger: bool,
    /// Falling-edge trigger enabled.
    pub falling_trigger: bool,
}

impl Default for GpioPin {
    fn default() -> Self {
        Self {
            mode: GpioMode::Input,
            state: false,
            irq_on_change: None,
            edge_triggered: false,
            rising_trigger: false,
            falling_trigger: false,
        }
    }
}

impl GpioPin {
    /// Create a new pin, defaulting to input with no IRQ.
    pub const fn new() -> Self {
        Self {
            mode: GpioMode::Input,
            state: false,
            irq_on_change: None,
            edge_triggered: false,
            rising_trigger: false,
            falling_trigger: false,
        }
    }
}

/// A virtual GPIO port with up to `MAX_PINS` pins.
#[derive(Debug, Clone)]
pub struct VirtualGpio {
    /// Port ID.
    pub id: u32,
    /// Pin array.
    pub pins: [GpioPin; MAX_PINS],
}

impl VirtualGpio {
    /// Create a new GPIO port.
    pub fn new(id: u32) -> Self {
        Self {
            id,
            pins: [GpioPin::new(); MAX_PINS],
        }
    }

    /// Set a pin's output state.  If the mode is not `Output`, this is a
    /// no-op.  Returns the IRQ number if a change-triggered interrupt
    /// should fire.
    pub fn set(&mut self, pin: usize, state: bool) -> Option<u32> {
        if pin >= MAX_PINS {
            return None;
        }

        let p = &mut self.pins[pin];

        // Cannot drive an input or alternate-function pin.
        if p.mode != GpioMode::Output {
            return None;
        }

        let old_state = p.state;

        if state == old_state {
            return None; // No change.
        }

        p.state = state;

        // Check interrupt trigger conditions.
        let trigger = match (old_state, state) {
            (false, true) => p.rising_trigger,
            (true, false) => p.falling_trigger,
            _ => false,
        };

        if trigger {
            p.irq_on_change
        } else {
            None
        }
    }

    /// Read a pin state.  Works for all modes.
    pub fn get(&self, pin: usize) -> Option<bool> {
        if pin >= MAX_PINS {
            return None;
        }
        Some(self.pins[pin].state)
    }

    /// Configure a pin's mode.
    pub fn configure(&mut self, pin: usize, mode: GpioMode) {
        if pin < MAX_PINS {
            self.pins[pin].mode = mode;
        }
    }

    /// Enable interrupt-on-change for a pin with the given IRQ number.
    pub fn enable_irq(&mut self, pin: usize, irq: u32, rising: bool, falling: bool) {
        if pin < MAX_PINS {
            self.pins[pin].irq_on_change = Some(irq);
            self.pins[pin].rising_trigger = rising;
            self.pins[pin].falling_trigger = falling;
            self.pins[pin].edge_triggered = true;
        }
    }

    /// Disable interrupt-on-change for a pin.
    pub fn disable_irq(&mut self, pin: usize) {
        if pin < MAX_PINS {
            self.pins[pin].irq_on_change = None;
            self.pins[pin].rising_trigger = false;
            self.pins[pin].falling_trigger = false;
            self.pins[pin].edge_triggered = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpio_output_set_get() {
        let mut gpio = VirtualGpio::new(0);
        gpio.configure(0, GpioMode::Output);

        gpio.set(0, true);
        assert_eq!(gpio.get(0), Some(true));

        gpio.set(0, false);
        assert_eq!(gpio.get(0), Some(false));
    }

    #[test]
    fn test_gpio_input_ignores_set() {
        let mut gpio = VirtualGpio::new(1);
        // Pin 0 defaults to Input
        assert_eq!(gpio.set(0, true), None); // ignored
        assert_eq!(gpio.get(0), Some(false)); // still false
    }

    #[test]
    fn test_gpio_irq_on_change() {
        let mut gpio = VirtualGpio::new(2);
        gpio.configure(0, GpioMode::Output);
        gpio.enable_irq(0, 10, true, false); // rising only

        // Rising edge triggers IRQ
        let irq = gpio.set(0, true);
        assert_eq!(irq, Some(10));

        // No change = no IRQ
        let irq = gpio.set(0, true);
        assert_eq!(irq, None);

        // Falling edge does NOT trigger (only rising enabled)
        let irq = gpio.set(0, false);
        assert_eq!(irq, None);
    }

    #[test]
    fn test_gpio_both_edges() {
        let mut gpio = VirtualGpio::new(3);
        gpio.configure(1, GpioMode::Output);
        gpio.enable_irq(1, 11, true, true);

        assert_eq!(gpio.set(1, true), Some(11));
        assert_eq!(gpio.set(1, false), Some(11));
    }

    #[test]
    fn test_gpio_out_of_range() {
        let mut gpio = VirtualGpio::new(4);
        assert_eq!(gpio.set(99, true), None);
        assert_eq!(gpio.get(99), None);
    }
}
