//! Device inspection facade — collect snapshots of all virtual device state
//! for the GUI device inspector panel.
//!
#![allow(missing_docs)]
//! [`DeviceSnapshot::collect_all`] gathers a point-in-time snapshot of every
//! registered virtual device.  The resulting vec can be serialized and sent
//! to a GUI for display.

/// A snapshot of a single GPIO pin.
#[derive(Debug, Clone)]
pub struct GpioPinSnapshot {
    pub num: u32,
    pub mode: String,   // "input" | "output" | "alternate"
    pub state: bool,
    pub value: u32,
}

/// A snapshot of an ADC channel.
#[derive(Debug, Clone)]
pub struct AdcChannelSnapshot {
    pub channel: u32,
    pub value: u32,
    pub resolution: u32,
}

/// A dirty rectangle with base64-encoded pixel data.
#[derive(Debug, Clone)]
pub struct DirtyRectSnapshot {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub data_base64: String,
}

/// A complete snapshot of any virtual device's state.
#[derive(Debug, Clone)]
pub enum DeviceSnapshot {
    Uart {
        id: u32,
        tx_buffer_len: usize,
        rx_buffer_len: usize,
        enabled: bool,
    },
    Gpio {
        id: u32,
        pins: Vec<GpioPinSnapshot>,
    },
    I2c {
        id: u32,
        tx_len: usize,
        rx_len: usize,
        address: u16,
        nack: bool,
    },
    Spi {
        id: u32,
        tx_len: usize,
        rx_len: usize,
    },
    Can {
        id: u32,
        tx_queue_len: usize,
        rx_queue_len: usize,
        error_state: String,
        loopback: bool,
    },
    Timer {
        id: u32,
        armed: bool,
        remaining_ticks: u64,
        period: u64,
        irq: u32,
    },
    Adc {
        id: u32,
        channels: Vec<AdcChannelSnapshot>,
    },
    TempSensor {
        id: u32,
        temp_milli_c: i32,
    },
    Eeprom {
        id: u32,
        size_bytes: u64,
    },
    Flash {
        id: u32,
        size_bytes: u64,
        sector_size: u64,
    },
    Display {
        id: u32,
        width: u16,
        height: u16,
        color_mode: String,
        enabled: bool,
        backlight: u8,
        framebuffer_base64: String,
        dirty_rects: Vec<DirtyRectSnapshot>,
    },
    Touch {
        id: u32,
        display_id: u32,
        pending_events: usize,
    },
}

impl DeviceSnapshot {
    /// Return the device type string (e.g. "uart", "display").
    pub fn type_str(&self) -> &'static str {
        match self {
            DeviceSnapshot::Uart { .. } => "uart",
            DeviceSnapshot::Gpio { .. } => "gpio",
            DeviceSnapshot::I2c { .. } => "i2c",
            DeviceSnapshot::Spi { .. } => "spi",
            DeviceSnapshot::Can { .. } => "can",
            DeviceSnapshot::Timer { .. } => "timer",
            DeviceSnapshot::Adc { .. } => "adc",
            DeviceSnapshot::TempSensor { .. } => "temp_sensor",
            DeviceSnapshot::Eeprom { .. } => "eeprom",
            DeviceSnapshot::Flash { .. } => "flash",
            DeviceSnapshot::Display { .. } => "display",
            DeviceSnapshot::Touch { .. } => "touch",
        }
    }

    /// Return the device ID.
    pub fn device_id(&self) -> u32 {
        match self {
            DeviceSnapshot::Uart { id, .. }
            | DeviceSnapshot::Gpio { id, .. }
            | DeviceSnapshot::I2c { id, .. }
            | DeviceSnapshot::Spi { id, .. }
            | DeviceSnapshot::Can { id, .. }
            | DeviceSnapshot::Timer { id, .. }
            | DeviceSnapshot::Adc { id, .. }
            | DeviceSnapshot::TempSensor { id, .. }
            | DeviceSnapshot::Eeprom { id, .. }
            | DeviceSnapshot::Flash { id, .. }
            | DeviceSnapshot::Display { id, .. }
            | DeviceSnapshot::Touch { id, .. } => *id,
        }
    }

    /// Collect snapshots of all registered virtual devices across all types.
    pub fn collect_all() -> Vec<DeviceSnapshot> {
        let mut snapshots = Vec::new();

        // UARTs
        for id in super::uart_ids() {
            if let Some(s) = super::with_uart(id, |u| DeviceSnapshot::Uart {
                id: u.id,
                tx_buffer_len: u.tx_buf.len(),
                rx_buffer_len: u.rx_buf.len(),
                enabled: u.enabled,
            }) {
                snapshots.push(s);
            }
        }

        // GPIOs
        for id in super::gpio_ids() {
            if let Some(s) = super::with_gpio_mut(id, |g| {
                let pins: Vec<GpioPinSnapshot> = g.pins
                    .iter()
                    .enumerate()
                    .map(|(i, p)| GpioPinSnapshot {
                        num: i as u32,
                        mode: format!("{:?}", p.mode).to_lowercase(),
                        state: p.state,
                        value: p.state as u32,
                    })
                    .collect();
                DeviceSnapshot::Gpio { id: g.id, pins }
            }) {
                snapshots.push(s);
            }
        }

        // I2Cs
        for id in super::i2c_ids() {
            if let Some(s) = super::with_i2c(id, |i| DeviceSnapshot::I2c {
                id: i.id,
                tx_len: i.tx_buf.len(),
                rx_len: i.rx_buf.len(),
                address: i.address.unwrap_or(0),
                nack: i.nack,
            }) {
                snapshots.push(s);
            }
        }

        // SPIs
        for id in super::spi_ids() {
            if let Some(s) = super::with_spi(id, |s| DeviceSnapshot::Spi {
                id: s.id,
                tx_len: s.tx_buf.len(),
                rx_len: s.rx_buf.len(),
            }) {
                snapshots.push(s);
            }
        }

        // CANs
        for id in super::can_ids() {
            if let Some(s) = super::with_can(id, |c| DeviceSnapshot::Can {
                id: c.id,
                tx_queue_len: c.tx_queue.len(),
                rx_queue_len: c.rx_queue.len(),
                error_state: format!("{:?}", c.error_state()).to_lowercase(),
                loopback: c.loopback,
            }) {
                snapshots.push(s);
            }
        }

        // Timers
        for id in super::timer_ids() {
            if let Some(s) = super::with_timer(id, |t| DeviceSnapshot::Timer {
                id: t.id,
                armed: t.armed,
                remaining_ticks: t.next_expiry.unwrap_or(0),
                period: t.period.unwrap_or(0),
                irq: t.irq,
            }) {
                snapshots.push(s);
            }
        }

        // ADCs
        for id in super::adc_ids() {
            if let Some(s) = super::with_adc(id, |a| {
                let channels: Vec<AdcChannelSnapshot> = a.readings
                    .iter()
                    .enumerate()
                    .map(|(i, &value)| AdcChannelSnapshot {
                        channel: i as u32,
                        value: value as u32,
                        resolution: a.resolution_bits as u32,
                    })
                    .collect();
                DeviceSnapshot::Adc { id: a.id, channels }
            }) {
                snapshots.push(s);
            }
        }

        // TempSensors
        for id in super::temp_sensor_ids() {
            if let Some(s) = super::with_temp_sensor(id, |t| DeviceSnapshot::TempSensor {
                id: t.id,
                temp_milli_c: t.milli_celsius,
            }) {
                snapshots.push(s);
            }
        }

        // EEPROMs
        for id in super::eeprom_ids() {
            if let Some(s) = super::with_eeprom(id, |e| DeviceSnapshot::Eeprom {
                id: e.id,
                size_bytes: e.size as u64,
            }) {
                snapshots.push(s);
            }
        }

        // Flashes
        for id in super::flash_ids() {
            if let Some(s) = super::with_flash(id, |f| DeviceSnapshot::Flash {
                id: f.id,
                size_bytes: (f.page_size * f.page_count) as u64,
                sector_size: f.page_size as u64,
            }) {
                snapshots.push(s);
            }
        }

        // Displays — placeholder snapshots until VirtualDisplay is fully implemented.
        // Sub-agent A delivers VirtualDisplay; callers should update these closures
        // to read framebuffer, dirty rects, colour mode, etc. from the real device.
        for id in super::display_ids() {
            snapshots.push(DeviceSnapshot::Display {
                id,
                width: 0,
                height: 0,
                color_mode: String::new(),
                enabled: false,
                backlight: 0,
                framebuffer_base64: String::new(),
                dirty_rects: Vec::new(),
            });
        }

        // Touch screens — placeholder snapshots until VirtualTouchScreen is fully implemented.
        for id in super::touch_ids() {
            snapshots.push(DeviceSnapshot::Touch {
                id,
                display_id: 0,
                pending_events: 0,
            });
        }

        snapshots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_collect_all() {
        let snapshots = DeviceSnapshot::collect_all();
        // Without any registered devices, collect_all returns an empty vec.
        assert!(snapshots.is_empty());
    }

    #[test]
    fn test_uart_snapshot() {
        let uart = crate::VirtualUart::new(0, 115200);
        crate::uart_insert(uart);

        let snapshots = DeviceSnapshot::collect_all();
        assert_eq!(snapshots.len(), 1);

        match &snapshots[0] {
            DeviceSnapshot::Uart { id, tx_buffer_len, rx_buffer_len, enabled } => {
                assert_eq!(*id, 0);
                assert_eq!(*tx_buffer_len, 0);
                assert_eq!(*rx_buffer_len, 0);
                assert!(*enabled); // UART is enabled by default
            }
            _ => panic!("expected Uart snapshot"),
        }
    }

    #[test]
    fn test_type_str_and_device_id() {
        let snap = DeviceSnapshot::Uart {
            id: 42,
            tx_buffer_len: 0,
            rx_buffer_len: 0,
            enabled: false,
        };
        assert_eq!(snap.type_str(), "uart");
        assert_eq!(snap.device_id(), 42);
    }

    #[test]
    fn test_gpio_snapshot() {
        let gpio = crate::VirtualGpio::new(1);
        crate::gpio_insert(gpio);

        let snapshots = DeviceSnapshot::collect_all();
        assert_eq!(snapshots.len(), 1);

        match &snapshots[0] {
            DeviceSnapshot::Gpio { id, pins } => {
                assert_eq!(*id, 1);
                assert_eq!(pins.len(), 32); // MAX_PINS
                assert_eq!(pins[0].num, 0);
                assert_eq!(pins[0].mode, "input");
                assert!(!pins[0].state);
                assert_eq!(pins[0].value, 0);
            }
            _ => panic!("expected Gpio snapshot"),
        }
    }

    #[test]
    fn test_timer_snapshot() {
        let timer = crate::VirtualTimer::new_oneshot(2, 16);
        crate::timer_insert(timer);

        let snapshots = DeviceSnapshot::collect_all();
        assert_eq!(snapshots.len(), 1);

        match &snapshots[0] {
            DeviceSnapshot::Timer { id, armed, irq, .. } => {
                assert_eq!(*id, 2);
                assert!(!armed);
                assert_eq!(*irq, 16);
            }
            _ => panic!("expected Timer snapshot"),
        }
    }

    #[test]
    fn test_adc_snapshot() {
        let adc = crate::VirtualAdc::new(3);
        crate::adc_insert(adc);

        let snapshots = DeviceSnapshot::collect_all();
        assert_eq!(snapshots.len(), 1);

        match &snapshots[0] {
            DeviceSnapshot::Adc { id, channels } => {
                assert_eq!(*id, 3);
                assert_eq!(channels.len(), 8); // default channel count
                assert_eq!(channels[0].channel, 0);
                assert_eq!(channels[0].resolution, 12); // default resolution_bits
            }
            _ => panic!("expected Adc snapshot"),
        }
    }

    #[test]
    fn test_temp_sensor_snapshot() {
        let sensor = crate::VirtualTempSensor::new(4);
        crate::temp_sensor_insert(sensor);

        let snapshots = DeviceSnapshot::collect_all();
        assert_eq!(snapshots.len(), 1);

        match &snapshots[0] {
            DeviceSnapshot::TempSensor { id, temp_milli_c } => {
                assert_eq!(*id, 4);
                assert_eq!(*temp_milli_c, 25000); // default 25°C
            }
            _ => panic!("expected TempSensor snapshot"),
        }
    }

    #[test]
    fn test_eeprom_snapshot() {
        let eeprom = crate::VirtualEeprom::new(5);
        crate::eeprom_insert(eeprom);

        let snapshots = DeviceSnapshot::collect_all();
        assert_eq!(snapshots.len(), 1);

        match &snapshots[0] {
            DeviceSnapshot::Eeprom { id, size_bytes } => {
                assert_eq!(*id, 5);
                assert_eq!(*size_bytes, 4096); // EEPROM_DEFAULT_SIZE
            }
            _ => panic!("expected Eeprom snapshot"),
        }
    }

    #[test]
    fn test_flash_snapshot() {
        let flash = crate::VirtualFlash::new(6);
        crate::flash_insert(flash);

        let snapshots = DeviceSnapshot::collect_all();
        assert_eq!(snapshots.len(), 1);

        match &snapshots[0] {
            DeviceSnapshot::Flash { id, size_bytes, sector_size } => {
                assert_eq!(*id, 6);
                // 64 pages × 256 bytes = 16384
                assert_eq!(*size_bytes, 16384);
                assert_eq!(*sector_size, 256);
            }
            _ => panic!("expected Flash snapshot"),
        }
    }

    #[test]
    fn test_multiple_devices() {
        crate::uart_insert(crate::VirtualUart::new(0, 115200));
        crate::gpio_insert(crate::VirtualGpio::new(1));
        crate::timer_insert(crate::VirtualTimer::new_oneshot(2, 16));

        let snapshots = DeviceSnapshot::collect_all();
        assert_eq!(snapshots.len(), 3);
    }
}
