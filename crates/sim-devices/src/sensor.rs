//! Virtual sensor devices.
//!
//! This module provides:
//! * [`VirtualAdc`] — multi-channel ADC with configurable resolution,
//!   reference voltage, and per-channel injected readings.
//! * [`VirtualTempSensor`] — temperature sensor in millidegrees Celsius.
//!
//! These devices are purely data models — they do not schedule events
//! or raise interrupts directly. Interrupt generation is handled by
//! the caller or an adapter.

/// A virtual multi-channel ADC (analog-to-digital converter).
///
/// Supports configurable resolution (8, 10, 12, or 16 bits), a
/// reference voltage, and per-channel pre-injected readings.
/// Calling [`read`](VirtualAdc::read) returns the injected value
/// for the currently selected channel.
#[derive(Debug, Clone)]
pub struct VirtualAdc {
    /// ADC controller ID.
    pub id: u32,
    /// Resolution in bits. Valid values: 8, 10, 12 (default), 16.
    pub resolution_bits: u8,
    /// Reference voltage in millivolts (default 3300 mV = 3.3 V).
    pub reference_mv: u32,
    /// Number of channels (default 8).
    pub channel_count: usize,
    /// Currently selected channel (0-based).
    pub current_channel: usize,
    /// Per-channel readings. `readings[channel]` is returned by `read()`
    /// when `current_channel` matches that channel index.  Indexed by
    /// channel number; injected via [`inject_reading`](VirtualAdc::inject_reading).
    pub readings: Vec<u16>,
}

impl VirtualAdc {
    /// Create a new ADC with the given ID.
    ///
    /// Defaults: 12-bit resolution, 3300 mV reference, 8 channels,
    /// channel 0 selected, all readings zero.
    pub fn new(id: u32) -> Self {
        Self {
            id,
            resolution_bits: 12,
            reference_mv: 3300,
            channel_count: 8,
            current_channel: 0,
            readings: vec![0u16; 8],
        }
    }

    /// Read the ADC value for the currently selected channel.
    ///
    /// Returns the pre-injected reading for `current_channel`.
    /// If the channel index is out of bounds, returns 0.
    pub fn read(&self) -> u16 {
        if self.current_channel < self.readings.len() {
            self.readings[self.current_channel]
        } else {
            0
        }
    }

    /// Select the active channel.
    ///
    /// If `channel` is out of bounds (≥ `channel_count`), it is
    /// clamped to the last valid channel.
    pub fn set_channel(&mut self, channel: usize) {
        self.current_channel = channel.min(self.channel_count.saturating_sub(1));
    }

    /// Set the ADC resolution in bits.
    ///
    /// Only accepts 8, 10, 12, or 16.  Invalid values are silently
    /// ignored (the previous resolution is kept).
    pub fn set_resolution(&mut self, bits: u8) {
        if matches!(bits, 8 | 10 | 12 | 16) {
            self.resolution_bits = bits;
        }
    }

    /// Inject a reading for a specific channel.
    ///
    /// If `channel` is out of bounds (≥ `readings.len()`), this is a
    /// no-op.
    pub fn inject_reading(&mut self, channel: usize, value: u16) {
        if channel < self.readings.len() {
            self.readings[channel] = value;
        }
    }

    /// Inject readings for all channels at once from a slice.
    ///
    /// Copies up to `readings.len()` values; extra values in the slice
    /// are ignored, missing values leave channels unchanged.
    pub fn inject_readings(&mut self, values: &[u16]) {
        let n = values.len().min(self.readings.len());
        self.readings[..n].copy_from_slice(&values[..n]);
    }

    /// Reset all readings to zero and re-select channel 0.
    pub fn reset(&mut self) {
        self.readings.fill(0);
        self.current_channel = 0;
    }
}

/// A virtual temperature sensor.
///
/// Stores temperature in millidegrees Celsius (1 m°C = 0.001 °C).
/// Default is 25000 m°C = 25.0 °C (typical room temperature).
#[derive(Debug, Clone)]
pub struct VirtualTempSensor {
    /// Temperature sensor ID.
    pub id: u32,
    /// Current temperature in millidegrees Celsius (m°C).
    /// 25000 m°C = 25.000 °C.
    pub milli_celsius: i32,
}

impl VirtualTempSensor {
    /// Create a new temperature sensor with the given ID.
    ///
    /// Defaults to 25000 m°C (25.0 °C).
    pub fn new(id: u32) -> Self {
        Self {
            id,
            milli_celsius: 25000,
        }
    }

    /// Read the current temperature in millidegrees Celsius.
    ///
    /// Returns the value last set via [`set_value`](Self::set_value)
    /// or the default (25000 = 25.0 °C).
    pub fn read_milli_c(&self) -> i32 {
        self.milli_celsius
    }

    /// Set the temperature in millidegrees Celsius.
    ///
    /// Example: `set_value(25500)` sets the temperature to 25.5 °C.
    /// Example: `set_value(-10000)` sets the temperature to -10.0 °C.
    pub fn set_value(&mut self, milli_c: i32) {
        self.milli_celsius = milli_c;
    }

    /// Reset the temperature to the default (25000 m°C = 25.0 °C).
    pub fn reset(&mut self) {
        self.milli_celsius = 25000;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── VirtualAdc tests ─────────────────────────────────────────────

    #[test]
    fn test_adc_creation_defaults() {
        let adc = VirtualAdc::new(0);
        assert_eq!(adc.id, 0);
        assert_eq!(adc.resolution_bits, 12);
        assert_eq!(adc.reference_mv, 3300);
        assert_eq!(adc.channel_count, 8);
        assert_eq!(adc.current_channel, 0);
        assert_eq!(adc.readings.len(), 8);
        // All readings default to 0
        for v in &adc.readings {
            assert_eq!(*v, 0);
        }
    }

    #[test]
    fn test_adc_read_injected_value() {
        let mut adc = VirtualAdc::new(1);
        adc.inject_reading(0, 2048);
        adc.inject_reading(3, 4095);
        // Current channel is 0
        assert_eq!(adc.read(), 2048);
        // Switch to channel 3
        adc.set_channel(3);
        assert_eq!(adc.read(), 4095);
    }

    #[test]
    fn test_adc_read_empty_returns_zero() {
        let adc = VirtualAdc::new(2);
        // No injections — all channels should return 0
        assert_eq!(adc.read(), 0);
        // Also test out-of-bounds channel (should be clamped)
        // read() on a non-existent channel index returns 0
        // (this is tested via the clamping behavior in set_channel)
    }

    #[test]
    fn test_adc_set_channel() {
        let mut adc = VirtualAdc::new(3);
        adc.inject_reading(5, 12345);
        adc.set_channel(5);
        assert_eq!(adc.current_channel, 5);
        assert_eq!(adc.read(), 12345);

        // Clamping: channel >= channel_count should clamp
        adc.set_channel(100);
        assert_eq!(adc.current_channel, 7); // channel_count - 1 = 7

        // Clamping at 0: channel_count is 8, so 8-1=7, and min(0,7)=0
        adc.set_channel(0);
        assert_eq!(adc.current_channel, 0);

        // saturating_sub handles channel_count == 0 edge case gracefully
        let mut adc_zero = VirtualAdc::new(99);
        adc_zero.channel_count = 0;
        adc_zero.set_channel(5);
        assert_eq!(adc_zero.current_channel, 0); // 0.min(0) = 0
    }

    #[test]
    fn test_adc_resolution_limits() {
        let mut adc = VirtualAdc::new(4);
        assert_eq!(adc.resolution_bits, 12); // default

        // Set to valid values
        adc.set_resolution(8);
        assert_eq!(adc.resolution_bits, 8);
        adc.set_resolution(10);
        assert_eq!(adc.resolution_bits, 10);
        adc.set_resolution(12);
        assert_eq!(adc.resolution_bits, 12);
        adc.set_resolution(16);
        assert_eq!(adc.resolution_bits, 16);

        // Invalid values are ignored
        adc.set_resolution(9);
        assert_eq!(adc.resolution_bits, 16);
        adc.set_resolution(11);
        assert_eq!(adc.resolution_bits, 16);
        adc.set_resolution(0);
        assert_eq!(adc.resolution_bits, 16);
        adc.set_resolution(255);
        assert_eq!(adc.resolution_bits, 16);
    }

    #[test]
    fn test_adc_inject_reading_out_of_bounds() {
        let mut adc = VirtualAdc::new(5);
        // Inject for channel 10 (out of bounds for 8-channel ADC)
        adc.inject_reading(10, 9999);
        // Should be a no-op; readings unchanged
        for (i, v) in adc.readings.iter().enumerate() {
            assert_eq!(*v, 0, "channel {} should be 0", i);
        }
    }

    #[test]
    fn test_adc_inject_readings_bulk() {
        let mut adc = VirtualAdc::new(6);
        adc.inject_readings(&[10, 20, 30, 40, 50, 60, 70, 80]);
        assert_eq!(adc.readings, vec![10, 20, 30, 40, 50, 60, 70, 80]);

        // Partial slice: only first 3 channels updated
        adc.inject_readings(&[100, 200, 300]);
        assert_eq!(adc.readings, vec![100, 200, 300, 40, 50, 60, 70, 80]);

        // Oversized slice: extra values ignored
        adc.inject_readings(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(adc.readings, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn test_adc_reset() {
        let mut adc = VirtualAdc::new(7);
        adc.inject_readings(&[100; 8]);
        adc.set_channel(5);
        adc.set_resolution(16);

        adc.reset();
        assert_eq!(adc.current_channel, 0);
        for v in &adc.readings {
            assert_eq!(*v, 0);
        }
        // Resolution is NOT reset
        assert_eq!(adc.resolution_bits, 16);
    }

    // ── VirtualTempSensor tests ──────────────────────────────────────

    #[test]
    fn test_temp_sensor_creation_defaults() {
        let ts = VirtualTempSensor::new(0);
        assert_eq!(ts.id, 0);
        assert_eq!(ts.milli_celsius, 25000);
        assert_eq!(ts.read_milli_c(), 25000);
    }

    #[test]
    fn test_temp_sensor_read() {
        let mut ts = VirtualTempSensor::new(1);

        // Default value
        assert_eq!(ts.read_milli_c(), 25000);

        // Set to room temperature (25.5 °C)
        ts.set_value(25500);
        assert_eq!(ts.read_milli_c(), 25500);

        // Set to freezing point
        ts.set_value(0);
        assert_eq!(ts.read_milli_c(), 0);

        // Set to below zero
        ts.set_value(-10000);
        assert_eq!(ts.read_milli_c(), -10000);
    }

    #[test]
    fn test_temp_sensor_fractional() {
        let mut ts = VirtualTempSensor::new(2);

        // 25.000 °C
        assert_eq!(ts.read_milli_c(), 25000);

        // 25.001 °C
        ts.set_value(25001);
        assert_eq!(ts.read_milli_c(), 25001);

        // -0.500 °C
        ts.set_value(-500);
        assert_eq!(ts.read_milli_c(), -500);

        // 100.000 °C (boiling point at sea level)
        ts.set_value(100000);
        assert_eq!(ts.read_milli_c(), 100000);
    }

    #[test]
    fn test_temp_sensor_reset() {
        let mut ts = VirtualTempSensor::new(3);
        ts.set_value(12345);
        assert_eq!(ts.read_milli_c(), 12345);

        ts.reset();
        assert_eq!(ts.read_milli_c(), 25000);
    }
}
