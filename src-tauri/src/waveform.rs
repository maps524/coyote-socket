//! Waveform Data Collection Module
//!
//! Tracks waveform history for visualization purposes.
//! Maintains a circular buffer of recent samples (~10 seconds).

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

// ============================================================================
// Data Structures
// ============================================================================

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

/// Full state for a single channel
#[derive(Debug, Clone, Serialize)]
pub struct ChannelState {
    pub intensity: f64,        // 0.0-1.0 normalized
    pub frequency: f64,        // Hz
    pub frequency_balance: u8, // 0-255
    pub intensity_balance: u8, // 0-255
    pub input_value: f64,      // Raw input (0.0-1.0)
    pub input_source: String,  // "L0", "R2", "static", etc.
}

/// Response struct for channel state queries
#[derive(Debug, Clone, Serialize)]
pub struct ChannelStateResponse {
    pub channel_a: ChannelState,
    pub channel_b: ChannelState,
}

/// Waveform history storage with circular buffer
pub struct WaveformHistory {
    samples: VecDeque<WaveformSample>,
    max_samples: usize,
}

impl WaveformHistory {
    /// Create new history with capacity for ~10 seconds at 10Hz
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(100), // 10 seconds * 10Hz
            max_samples: 100,
        }
    }

    /// Add a new sample to the history
    pub fn add_sample(&mut self, sample: WaveformSample) {
        self.samples.push_back(sample);

        // Maintain circular buffer size
        while self.samples.len() > self.max_samples {
            self.samples.pop_front();
        }
    }

    /// Get samples since a given timestamp
    pub fn get_since(&self, since_timestamp: u64) -> Vec<WaveformSample> {
        self.samples
            .iter()
            .filter(|s| s.timestamp >= since_timestamp)
            .cloned()
            .collect()
    }

    /// Get all samples
    pub fn get_all(&self) -> Vec<WaveformSample> {
        self.samples.iter().cloned().collect()
    }
}

// ============================================================================
// Global State
// ============================================================================

static WAVEFORM_HISTORY: tokio::sync::OnceCell<Arc<RwLock<WaveformHistory>>> =
    tokio::sync::OnceCell::const_new();

async fn get_history_storage() -> &'static Arc<RwLock<WaveformHistory>> {
    WAVEFORM_HISTORY
        .get_or_init(|| async { Arc::new(RwLock::new(WaveformHistory::new())) })
        .await
}

// ============================================================================
// Public API
// ============================================================================

/// Record a waveform sample directly (called from device loop at 10Hz)
pub async fn record_sample_direct(sample: WaveformSample) {
    let storage = get_history_storage().await;
    let mut history = storage.write().await;
    history.add_sample(sample);
}

/// Get waveform samples since a given timestamp
pub async fn get_samples_since(since_timestamp: u64) -> Vec<WaveformSample> {
    let storage = get_history_storage().await;
    let history = storage.read().await;
    history.get_since(since_timestamp)
}

/// Get all waveform samples
pub async fn get_all_samples() -> Vec<WaveformSample> {
    let storage = get_history_storage().await;
    let history = storage.read().await;
    history.get_all()
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get waveform data since a given timestamp
/// If since_timestamp is 0, returns all available samples
#[tauri::command]
pub async fn get_waveform_data(since_timestamp: u64) -> Result<Vec<WaveformSample>, String> {
    if since_timestamp == 0 {
        Ok(get_all_samples().await)
    } else {
        Ok(get_samples_since(since_timestamp).await)
    }
}

/// Get current channel state (for both channels)
/// This provides a snapshot of the current parameters and input values
#[tauri::command]
pub async fn get_channel_state() -> Result<ChannelStateResponse, String> {
    // Get current intensity values from processing state
    let processing_state = crate::processing::get_processing_state().await;
    let processing_guard = processing_state.read().await;
    let (intensity_a, intensity_b) = processing_guard.get_current_intensities();

    // Get channel parameters for balance values
    let device_state = crate::device::get_device_state().await;
    let params_a = device_state.channel_a_params.read().await;
    let params_b = device_state.channel_b_params.read().await;

    // Build channel state structs
    let channel_a = ChannelState {
        intensity: intensity_a,
        frequency: params_a.frequency,
        frequency_balance: params_a.freq_balance,
        intensity_balance: params_a.int_balance,
        input_value: intensity_a, // TODO: Track actual input value separately
        input_source: "L0".to_string(), // TODO: Track actual input source
    };

    let channel_b = ChannelState {
        intensity: intensity_b,
        frequency: params_b.frequency,
        frequency_balance: params_b.freq_balance,
        intensity_balance: params_b.int_balance,
        input_value: intensity_b, // TODO: Track actual input value separately
        input_source: "R2".to_string(), // TODO: Track actual input source
    };

    Ok(ChannelStateResponse {
        channel_a,
        channel_b,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_sample(timestamp: u64, intensity_a: u8, intensity_b: u8) -> WaveformSample {
        WaveformSample {
            timestamp,
            channel_a_intensity: intensity_a,
            channel_b_intensity: intensity_b,
            channel_a_frequency: 100.0,
            channel_b_frequency: 50.0,
            channel_a_freq_balance: 128,
            channel_b_freq_balance: 128,
            channel_a_int_balance: 128,
            channel_b_int_balance: 128,
            channel_a_waveform: [100, 100, 100, 100],
            channel_b_waveform: [100, 100, 100, 100],
        }
    }

    #[test]
    fn test_waveform_history_capacity() {
        let mut history = WaveformHistory::new();

        // Add more than max_samples
        for i in 0..150 {
            history.add_sample(make_test_sample(i as u64, 50, 100));
        }

        // Should have capped at max_samples
        assert_eq!(history.len(), 100);

        // Oldest samples should have been removed
        let samples = history.get_all();
        assert_eq!(samples[0].timestamp, 50); // First 50 samples were dropped
    }

    #[test]
    fn test_get_since_timestamp() {
        let mut history = WaveformHistory::new();

        // Add samples with timestamps 0, 100, 200, 300, 400
        for i in 0..5 {
            history.add_sample(make_test_sample(i * 100, i as u8 * 10, i as u8 * 20));
        }

        // Get samples since timestamp 200
        let recent = history.get_since(200);
        assert_eq!(recent.len(), 3); // Should get 200, 300, 400
        assert_eq!(recent[0].timestamp, 200);
        assert_eq!(recent[1].timestamp, 300);
        assert_eq!(recent[2].timestamp, 400);
    }

    #[test]
    fn test_clear_history() {
        let mut history = WaveformHistory::new();

        // Add some samples
        for i in 0..10 {
            history.add_sample(make_test_sample(i, 50, 100));
        }

        assert_eq!(history.len(), 10);

        history.clear();

        assert_eq!(history.len(), 0);
        assert!(history.is_empty());
    }
}
