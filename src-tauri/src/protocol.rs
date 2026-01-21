//! DG-LAB Coyote Protocol Implementation
//! Generates B0 commands for the Coyote device

/// Generate a B0 command for real-time waveform control
///
/// Format:
/// 0xB0 (1 byte) + serial/interpretation (1 byte) + intensityA (1 byte) + intensityB (1 byte)
/// + waveformAfrequency (4 bytes) + waveformAintensity (4 bytes)
/// + waveformBfrequency (4 bytes) + waveformBintensity (4 bytes)
pub fn generate_b0_command(
    interpretation_a: u8,
    interpretation_b: u8,
    intensity_a: u8,
    intensity_b: u8,
    waveform_a_frequency: [u8; 4],
    waveform_a_intensity: [u8; 4],
    waveform_b_frequency: [u8; 4],
    waveform_b_intensity: [u8; 4],
) -> Vec<u8> {
    let mut command = Vec::with_capacity(20);

    // Header
    command.push(0xB0);

    // Serial number (4 bits) + interpretation methods (2 bits each)
    // Serial is always 0, interpretations are typically 3 (0b11)
    let interpretation_byte = ((interpretation_a & 0x03) << 2) | (interpretation_b & 0x03);
    command.push(interpretation_byte);

    // Channel intensities (0-200)
    command.push(intensity_a.min(200));
    command.push(intensity_b.min(200));

    // Waveform frequencies (4 values per channel)
    command.extend_from_slice(&waveform_a_frequency);
    command.extend_from_slice(&waveform_a_intensity);
    command.extend_from_slice(&waveform_b_frequency);
    command.extend_from_slice(&waveform_b_intensity);

    command
}

/// Convert frequency (Hz) to period value for the device
/// Uses the same algorithm as the original Python implementation
pub fn convert_period(period: u16) -> u8 {
    if period <= 100 {
        period as u8
    } else if period <= 600 {
        ((period - 100) / 5 + 100) as u8
    } else if period <= 1000 {
        ((period - 600) / 10 + 200) as u8
    } else {
        240 // Max value
    }
}

/// Convert frequency in Hz to period in ms
pub fn frequency_to_period(frequency: f64) -> u16 {
    if frequency <= 0.0 {
        1000
    } else {
        (1000.0 / frequency).round() as u16
    }
}

// ================= V2 protocol support code ================

/// Generate V2 intensity command (PWM_AB2)
/// 3 bytes: [Reserved (2 bits)][A channel strength (11 bits)][B channel strength (11 bits)]
pub fn generate_v2_intensity(intensity_a: u16, intensity_b: u16) -> Vec<u8> {
    // Limit Range 0-2047
    let a = intensity_a.min(2047) as u32;
    let b = intensity_b.min(2047) as u32;

    // Construct 24bit data: Bits 23-22: 0, Bits 21-11: A, Bits 10-0: B
    let combined = (a << 11) | b;

    vec![
        (combined & 0xFF) as u8,         // Byte 0 (Low)
        ((combined >> 8) & 0xFF) as u8,  // Byte 1 (Mid)
        ((combined >> 16) & 0xFF) as u8, // Byte 2 (High)
    ]
}

/// Generate V2 waveform command (PWM_A34/B34)
/// 3 bytes: [Reserved (4 bits)][Z(5 bits)][Y(10 bits)][X(5 bits)]
/// X: Pulse Count, Y: Interval, Z: Pulse Width
pub fn generate_v2_waveform(x: u8, y: u16, z: u8) -> Vec<u8> {
    let x_val = (x.min(31)) as u32;
    let y_val = (y.min(1023)) as u32;
    let z_val = (z.min(31)) as u32;

    // Bits 23-20: 0, Bits 19-15: Z, Bits 14-5: Y, Bits 4-0: X
    let combined = (z_val << 15) | (y_val << 5) | x_val;

    vec![
        (combined & 0xFF) as u8,         // Byte 0 (Low)
        ((combined >> 8) & 0xFF) as u8,  // Byte 1 (Mid)
        ((combined >> 16) & 0xFF) as u8, // Byte 2 (High)
    ]
}

/// Convert frequency (Hz) to X and Y parameters of V2
/// Based on the "optimal X, Y ratio formula" in the document
pub fn freq_to_v2_xy(frequency_hz: f64) -> (u8, u16) {
    if frequency_hz <= 0.0 {
        return (1, 100);
    }

    // "Frequency" of V2 is actually period ms (X + Y), range 10-1000
    let period_ms = (1000.0 / frequency_hz).clamp(10.0, 1000.0);

    // Formula: X = ((Period / 1000) ^ 0.5) * 15
    let x = ((period_ms / 1000.0).sqrt() * 15.0).round() as u8;
    let x = x.max(1).min(31); // Make sure X is at least 1

    let y = (period_ms as u16).saturating_sub(x as u16);

    (x, y)
}

/// Convert intensity balance (0-255) to Z (pulse width 0-31) of V2
pub fn balance_to_v2_z(balance: u8) -> u8 {
    // Map 0-255 to 0-31
    ((balance as f32 / 255.0) * 31.0).round() as u8
}
