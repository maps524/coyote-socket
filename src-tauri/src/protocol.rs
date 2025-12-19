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
