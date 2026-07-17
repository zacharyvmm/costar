//! Conversion from Rust `DeviceSnapshot` to protobuf `DeviceSnapshot`.

use crate::proto::*;

/// Convert a `sim_devices::inspect::DeviceSnapshot` to the protobuf
/// `DeviceSnapshot` message.
pub fn to_proto(snapshot: &sim_devices::inspect::DeviceSnapshot) -> DeviceSnapshot {
    match snapshot {
        sim_devices::inspect::DeviceSnapshot::Uart {
            id,
            tx_buffer_len,
            rx_buffer_len,
            enabled,
        } => DeviceSnapshot {
            r#type: "uart".into(),
            id: *id,
            tx_buffer_len: *tx_buffer_len as u32,
            rx_buffer_len: *rx_buffer_len as u32,
            uart_enabled: *enabled,
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::Gpio { id, pins } => DeviceSnapshot {
            r#type: "gpio".into(),
            id: *id,
            pins: pins
                .iter()
                .map(|p| GpioPin {
                    num: p.num,
                    mode: p.mode.clone(),
                    state: p.state,
                    value: p.value,
                })
                .collect(),
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::I2c {
            id,
            tx_len,
            rx_len,
            address,
            nack,
        } => DeviceSnapshot {
            r#type: "i2c".into(),
            id: *id,
            i2c_tx_len: *tx_len as u32,
            i2c_rx_len: *rx_len as u32,
            i2c_address: *address as u32,
            i2c_nack: *nack,
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::Spi { id, tx_len, rx_len } => DeviceSnapshot {
            r#type: "spi".into(),
            id: *id,
            spi_tx_len: *tx_len as u32,
            spi_rx_len: *rx_len as u32,
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::Can {
            id,
            tx_queue_len,
            rx_queue_len,
            error_state,
            loopback,
        } => DeviceSnapshot {
            r#type: "can".into(),
            id: *id,
            can_tx_queue_len: *tx_queue_len as u32,
            can_rx_queue_len: *rx_queue_len as u32,
            can_error_state: error_state.clone(),
            can_loopback: *loopback,
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::Timer {
            id,
            armed,
            remaining_ticks,
            period,
            irq,
        } => DeviceSnapshot {
            r#type: "timer".into(),
            id: *id,
            timer_armed: *armed,
            timer_remaining_ticks: *remaining_ticks,
            timer_period: *period,
            timer_irq: *irq,
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::Adc { id, channels } => DeviceSnapshot {
            r#type: "adc".into(),
            id: *id,
            adc_channels: channels
                .iter()
                .map(|c| AdcChannel {
                    channel: c.channel,
                    value: c.value,
                    resolution: c.resolution,
                })
                .collect(),
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::TempSensor { id, temp_milli_c } => DeviceSnapshot {
            r#type: "temp_sensor".into(),
            id: *id,
            temp_milli_c: *temp_milli_c,
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::Eeprom { id, size_bytes } => DeviceSnapshot {
            r#type: "eeprom".into(),
            id: *id,
            storage_size_bytes: *size_bytes,
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::Flash {
            id,
            size_bytes,
            sector_size,
        } => DeviceSnapshot {
            r#type: "flash".into(),
            id: *id,
            storage_size_bytes: *size_bytes,
            storage_sector_size: *sector_size,
            ..Default::default()
        },

        sim_devices::inspect::DeviceSnapshot::Display {
            id,
            width,
            height,
            color_mode,
            enabled,
            backlight,
            framebuffer_base64: _,
            dirty_rects,
        } => {
            let rects: Vec<DirtyRect> = dirty_rects
                .iter()
                .map(|r| DirtyRect {
                    x: r.x as u32,
                    y: r.y as u32,
                    w: r.w as u32,
                    h: r.h as u32,
                    data: Vec::new(),
                })
                .collect();
            let full =
                dirty_rects.len() == 1 && dirty_rects[0].w == *width && dirty_rects[0].h == *height;
            DeviceSnapshot {
                r#type: "display".into(),
                id: *id,
                display_width: *width as u32,
                display_height: *height as u32,
                display_color_mode: color_mode.clone(),
                display_enabled: *enabled,
                display_backlight: *backlight as u32,
                display_dirty_rects: rects,
                display_full_frame: full,
                ..Default::default()
            }
        }

        sim_devices::inspect::DeviceSnapshot::Touch {
            id,
            display_id,
            pending_events,
            last_inject_x,
            last_inject_y,
            has_last_inject,
        } => DeviceSnapshot {
            r#type: "touch".into(),
            id: *id,
            touch_display_id: *display_id,
            touch_pending_events: *pending_events as u32,
            touch_last_inject_x: *last_inject_x,
            touch_last_inject_y: *last_inject_y,
            touch_has_last_inject: *has_last_inject,
            ..Default::default()
        },
    }
}
