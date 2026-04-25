//! Waveform sample payload.
//!
//! Defines the `WaveformSample` pushed to the frontend at 10Hz via the
//! `waveform-sample` Tauri event. Charts subscribe directly; there is no
//! snapshot query path.

use serde::Serialize;

/// A single waveform sample capturing the state at a specific moment
#[derive(Debug, Clone, Serialize)]
pub struct WaveformSample {
    pub timestamp: u64,              // Unix timestamp in ms
    pub channel_a_intensity: u8,     // 0-200 (scaled)
    pub channel_b_intensity: u8,     // 0-200 (scaled)
    pub channel_a_frequency: f64,    // Hz
    pub channel_b_frequency: f64,    // Hz
    pub channel_a_freq_balance: u8,  // 0-255
    pub channel_b_freq_balance: u8,  // 0-255
    pub channel_a_int_balance: u8,   // 0-255
    pub channel_b_int_balance: u8,   // 0-255
    pub channel_a_waveform: [u8; 4], // 4 sub-values (0-100)
    pub channel_b_waveform: [u8; 4], // 4 sub-values (0-100)
}
