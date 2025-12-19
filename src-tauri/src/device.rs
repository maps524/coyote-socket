//! Device Controller - Manages the 10Hz update loop for sending commands to the Coyote device

use crate::bluetooth::get_bluetooth_manager;
use crate::emit_waveform_sample;
use crate::protocol::{generate_b0_command, convert_period, frequency_to_period};
use crate::waveform::WaveformSample;
use crate::websocket::{get_next_waveform_data, get_resolved_channel_params};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

/// Output data for a single channel (what was actually sent to the device)
#[derive(Debug, Clone, Serialize, Default)]
pub struct ChannelOutput {
    pub raw_intensity: u8,          // Pre-scaling intensity (0-200)
    pub scaled_intensity: u8,       // Post-scaling intensity (0-200)
    pub waveform: [u8; 4],          // 4 sub-values for the 100ms window (0-100 relative)
    pub frequency: f64,             // Frequency in Hz
    pub range_min: u8,              // Range min used for scaling (debug)
    pub range_max: u8,              // Range max used for scaling (debug)
}

/// Complete device output snapshot
#[derive(Debug, Clone, Serialize, Default)]
pub struct DeviceOutput {
    pub timestamp: u64,             // Unix timestamp in ms
    pub channel_a: ChannelOutput,
    pub channel_b: ChannelOutput,
    pub is_connected: bool,
}

/// Global storage for last device output
static LAST_OUTPUT: tokio::sync::OnceCell<Arc<RwLock<DeviceOutput>>> = tokio::sync::OnceCell::const_new();

async fn get_last_output_storage() -> &'static Arc<RwLock<DeviceOutput>> {
    LAST_OUTPUT
        .get_or_init(|| async { Arc::new(RwLock::new(DeviceOutput::default())) })
        .await
}

/// Get the last device output (for frontend display)
pub async fn get_last_device_output() -> DeviceOutput {
    let storage = get_last_output_storage().await;
    storage.read().await.clone()
}

/// Device parameters set from the frontend
#[derive(Debug, Clone)]
pub struct ChannelParams {
    pub frequency: f64,       // Hz (1-200)
    pub freq_balance: u8,     // 0-255
    pub int_balance: u8,      // 0-255
    pub range_min: u8,        // 0-200
    pub range_max: u8,        // 0-200
}

impl Default for ChannelParams {
    fn default() -> Self {
        Self {
            frequency: 100.0,    // 100Hz (10ms period) - balanced, distinct pulses
            freq_balance: 128,   // Neutral - balanced high/low frequency feeling
            int_balance: 128,    // Neutral - balanced pulse width
            range_min: 10,
            range_max: 20,
        }
    }
}

/// Global device state
pub struct DeviceState {
    pub running: AtomicBool,
    pub channel_a_params: RwLock<ChannelParams>,
    pub channel_b_params: RwLock<ChannelParams>,
}

impl DeviceState {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            channel_a_params: RwLock::new(ChannelParams::default()),
            channel_b_params: RwLock::new(ChannelParams::default()),
        }
    }
}

pub static DEVICE_STATE: tokio::sync::OnceCell<Arc<DeviceState>> = tokio::sync::OnceCell::const_new();

pub async fn get_device_state() -> &'static Arc<DeviceState> {
    DEVICE_STATE
        .get_or_init(|| async { Arc::new(DeviceState::new()) })
        .await
}

/// Start the 10Hz update loop
pub async fn start_device_loop() {
    let state = get_device_state().await;

    if state.running.swap(true, Ordering::SeqCst) {
        println!("[DEBUG] Device loop already running");
        return;
    }

    println!("[DEBUG] *** STARTING device 10Hz update loop ***");

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(100)); // 10Hz

        loop {
            ticker.tick().await;

            let state = get_device_state().await;
            if !state.running.load(Ordering::SeqCst) {
                println!("Device loop stopped");
                break;
            }

            // Note: axis updates are now pushed directly from T-Code/Buttplug handlers
            // when input is received, not tied to 10Hz device loop

            // Check if output is paused
            let is_paused = crate::settings::get_output_paused().await;
            if is_paused {
                // When paused, don't send any commands to device
                // The zero command was already sent when pausing
                continue;
            }

            // Send to device (may fail if not connected)
            if let Err(_e) = send_device_update().await {
                // Silently ignore errors - device may not be connected
                // The loop will keep trying
            }
        }
    });
}

/// Stop the 10Hz update loop
pub async fn stop_device_loop() {
    let state = get_device_state().await;
    state.running.store(false, Ordering::SeqCst);
    println!("Stopping device loop");
}

/// Send a zero command to immediately stop all output
/// This is called when pausing to ensure device stops immediately
pub async fn send_zero_command() {
    use crate::protocol::{generate_b0_command, convert_period, frequency_to_period};

    // Get Bluetooth manager
    let manager = match get_bluetooth_manager().await {
        Ok(m) => m,
        Err(_) => return, // Silently fail if no Bluetooth
    };

    let manager_guard = manager.lock().await;
    if !manager_guard.is_connected() {
        return; // Not connected, nothing to do
    }

    // Generate a B0 command with zero intensity
    let period = frequency_to_period(100.0);
    let period_converted = convert_period(period);

    let command = generate_b0_command(
        3, 3, // interpretation methods
        0,    // zero intensity channel A
        0,    // zero intensity channel B
        [period_converted, period_converted, period_converted, period_converted],
        [0, 0, 0, 0], // zero waveform
        [period_converted, period_converted, period_converted, period_converted],
        [0, 0, 0, 0], // zero waveform
    );

    // Send command, ignore errors
    let _ = manager_guard.write_command(&command).await;
    println!("[PAUSE] Sent zero command to stop output");
}

// Debug counter for logging
use std::sync::atomic::AtomicU64;
static DEBUG_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Send a single update to the device
async fn send_device_update() -> Result<(), String> {
    let count = DEBUG_COUNTER.fetch_add(1, Ordering::Relaxed);

    // Get Bluetooth manager
    let manager = get_bluetooth_manager().await
        .map_err(|e| format!("Bluetooth error: {}", e))?;

    let manager_guard = manager.lock().await;
    let is_connected = manager_guard.is_connected();

    if !is_connected {
        // Still update output storage to show disconnected state
        let storage = get_last_output_storage().await;
        let mut output = storage.write().await;
        output.is_connected = false;
        output.timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        if count % 100 == 0 {
            println!("[DEBUG] Device loop running but not connected (count: {})", count);
        }
        return Err("Not connected".to_string());
    }

    // Get next waveform data (consumes 4 values from queue per channel)
    let (waveform_a, waveform_b) = get_next_waveform_data().await;

    // Get resolved channel parameters (handles both static and linked sources)
    // This resolves frequency, freqBalance, intBalance with midpoint/curve transformations
    let (params_a, params_b) = get_resolved_channel_params().await;

    // Convert frequency to period
    let period_a = frequency_to_period(params_a.frequency);
    let period_b = frequency_to_period(params_b.frequency);
    let period_a_converted = convert_period(period_a);
    let period_b_converted = convert_period(period_b);

    // Capture all values we need
    let freq_a = params_a.frequency;
    let freq_b = params_b.frequency;
    let range_min_a = params_a.range_min;
    let range_max_a = params_a.range_max;
    let range_min_b = params_b.range_min;
    let range_max_b = params_b.range_max;

    // Apply range scaling to intensity values
    // For static intensity: use the value directly (user set an explicit value)
    // For linked intensity: apply range scaling (maps T-Code 0-100% to range)
    let scaled_a = if params_a.intensity_is_static {
        waveform_a.intensity  // Static: use directly without scaling
    } else {
        scale_intensity(waveform_a.intensity, range_min_a, range_max_a)
    };
    let scaled_b = if params_b.intensity_is_static {
        waveform_b.intensity  // Static: use directly without scaling
    } else {
        scale_intensity(waveform_b.intensity, range_min_b, range_max_b)
    };

    // Get timestamp for both output storage and waveform recording
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Store output data for frontend visualization
    let storage = get_last_output_storage().await;
    {
        let mut output = storage.write().await;
        output.timestamp = timestamp;
        output.is_connected = true;
        output.channel_a = ChannelOutput {
            raw_intensity: waveform_a.intensity,
            scaled_intensity: scaled_a,
            waveform: waveform_a.waveform_intensity,
            frequency: freq_a,
            range_min: range_min_a,
            range_max: range_max_a,
        };
        output.channel_b = ChannelOutput {
            raw_intensity: waveform_b.intensity,
            scaled_intensity: scaled_b,
            waveform: waveform_b.waveform_intensity,
            frequency: freq_b,
            range_min: range_min_b,
            range_max: range_max_b,
        };
    }

    // Create waveform sample for visualization
    let sample = WaveformSample {
        timestamp,
        channel_a_intensity: scaled_a,
        channel_b_intensity: scaled_b,
        channel_a_frequency: freq_a,
        channel_b_frequency: freq_b,
        channel_a_freq_balance: params_a.freq_balance,
        channel_b_freq_balance: params_b.freq_balance,
        channel_a_int_balance: params_a.int_balance,
        channel_b_int_balance: params_b.int_balance,
        channel_a_waveform: waveform_a.waveform_intensity,
        channel_b_waveform: waveform_b.waveform_intensity,
    };

    // Emit to frontend in real-time (push-based)
    emit_waveform_sample(sample.clone());

    // Also record for history buffer (used by get_waveform_data command)
    crate::waveform::record_sample_direct(sample).await;

    // Generate B0 command with proper waveform intensity arrays
    let command = generate_b0_command(
        3, 3, // interpretation methods
        scaled_a,
        scaled_b,
        [period_a_converted, period_a_converted, period_a_converted, period_a_converted],
        waveform_a.waveform_intensity,
        [period_b_converted, period_b_converted, period_b_converted, period_b_converted],
        waveform_b.waveform_intensity,
    );

    // Send command
    match manager_guard.write_command(&command).await {
        Ok(_) => Ok(()),
        Err(e) => {
            println!("[DEBUG] Write FAILED: {}", e);
            Err(format!("Write error: {}", e))
        }
    }
}

/// Scale intensity based on range limits
fn scale_intensity(intensity: u8, min: u8, max: u8) -> u8 {
    if max <= min {
        return min;
    }
    let range = (max - min) as f64;
    let scaled = min as f64 + (intensity as f64 * range / 200.0);
    scaled.round().clamp(0.0, 200.0) as u8
}

/// Update channel A parameters
pub async fn update_channel_a_params(params: ChannelParams) {
    let state = get_device_state().await;
    let mut guard = state.channel_a_params.write().await;
    *guard = params;
}

/// Update channel B parameters
pub async fn update_channel_b_params(params: ChannelParams) {
    let state = get_device_state().await;
    let mut guard = state.channel_b_params.write().await;
    *guard = params;
}

/// Get channel A parameters (for HMR state recovery)
pub async fn get_channel_a_params() -> ChannelParams {
    let state = get_device_state().await;
    state.channel_a_params.read().await.clone()
}

/// Get channel B parameters (for HMR state recovery)
pub async fn get_channel_b_params() -> ChannelParams {
    let state = get_device_state().await;
    state.channel_b_params.read().await.clone()
}
