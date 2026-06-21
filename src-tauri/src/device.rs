//! Device Controller - Manages the 10Hz update loop for sending commands to the Coyote device

use crate::bluetooth::{get_bluetooth_manager, DeviceVersion}; // Add DeviceVersion
use crate::{emit_resolved_update, emit_waveform_sample};
// Add V2 related function references
use crate::processing::{get_processing_state, ProcessingEngineType};
use crate::protocol::{
    balance_to_v2_z, convert_period, freq_to_v2_xy, frequency_to_period, generate_b0_command,
    generate_bf_command, generate_v2_intensity, generate_v2_waveform,
};
use crate::resolver::{get_next_waveform_data, get_resolved_channel_params, ResolvedUpdatePayload};
use crate::waveform::WaveformSample;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

/// V2 intensity scale factor: maps 0-200 device range to 0-2047 V2 range
const V2_INTENSITY_SCALE: f32 = 2047.0 / 200.0;

/// Output data for a single channel (what was actually sent to the device)
#[derive(Debug, Clone, Serialize, Default)]
pub struct ChannelOutput {
    pub raw_intensity: u8,    // Pre-scaling intensity (0-200)
    pub scaled_intensity: u8, // Post-scaling intensity (0-200)
    pub waveform: [u8; 4],    // 4 sub-values for the 100ms window (0-100 relative)
    pub frequency: f64,       // Frequency in Hz
    pub range_min: u8,        // Range min used for scaling (debug)
    pub range_max: u8,        // Range max used for scaling (debug)
}

/// Complete device output snapshot
#[derive(Debug, Clone, Serialize, Default)]
pub struct DeviceOutput {
    pub timestamp: u64, // Unix timestamp in ms
    pub channel_a: ChannelOutput,
    pub channel_b: ChannelOutput,
    pub is_connected: bool,
}

/// Global storage for last device output
static LAST_OUTPUT: tokio::sync::OnceCell<Arc<RwLock<DeviceOutput>>> =
    tokio::sync::OnceCell::const_new();

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

/// Device parameters snapshot used for HMR recovery + waveform visualization.
/// Only the runtime values that the frontend needs to restore live here —
/// range_min/range_max now come from processing state (intensity source).
#[derive(Debug, Clone)]
pub struct ChannelParams {
    pub frequency: f64,   // Hz (1-200)
    pub freq_balance: u8, // 0-255
    pub int_balance: u8,  // 0-255
}

impl Default for ChannelParams {
    fn default() -> Self {
        Self {
            frequency: 100.0,
            freq_balance: 128,
            int_balance: 128,
        }
    }
}

/// Snapshot of the last BF command payload we sent, so we can skip rewriting
/// when nothing changed (BLE is expensive + the device persists BF anyway).
/// `None` after (re)connect forces the next tick to emit BF.
pub type BfSnapshot = (u8, u8, u8, u8, u8, u8);

/// Global device state
pub struct DeviceState {
    pub running: AtomicBool,
    pub channel_a_params: RwLock<ChannelParams>,
    pub channel_b_params: RwLock<ChannelParams>,
    pub last_bf_sent: RwLock<Option<BfSnapshot>>,
}

impl DeviceState {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            channel_a_params: RwLock::new(ChannelParams::default()),
            channel_b_params: RwLock::new(ChannelParams::default()),
            last_bf_sent: RwLock::new(None),
        }
    }
}

/// Clear BF tracking so the next device tick resends unconditionally.
/// Call on BLE (re)connect since BF state persists in device flash and may
/// diverge from what this process last sent.
pub async fn reset_bf_snapshot() {
    let state = get_device_state().await;
    *state.last_bf_sent.write().await = None;
}

pub static DEVICE_STATE: tokio::sync::OnceCell<Arc<DeviceState>> =
    tokio::sync::OnceCell::const_new();

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

            // Always run send_device_update so telemetry events
            // (`waveform-sample`, `resolved-update`) fire regardless of
            // pause / connection state — the frontend's resolved-state
            // indicators read those events. Pause + not-connected are
            // checked inside; only the BLE writes are gated.
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
    use crate::protocol::{
        convert_period, frequency_to_period, generate_b0_command, generate_v2_intensity,
        generate_v2_waveform,
    };

    // Get Bluetooth manager
    let manager = match get_bluetooth_manager().await {
        Ok(m) => m,
        Err(_) => return, // Silently fail if no Bluetooth
    };

    let manager_guard = manager.lock().await;
    if !manager_guard.is_connected() {
        return; // Not connected, nothing to do
    }

    let device_version = manager_guard.device_version.unwrap_or(DeviceVersion::V3);

    match device_version {
        DeviceVersion::V3 => {
            // Generate a B0 command with zero intensity
            let period = frequency_to_period(100.0);
            let period_converted = convert_period(period);

            let command = generate_b0_command(
                3,
                3, // interpretation methods
                0, // zero intensity channel A
                0, // zero intensity channel B
                [
                    period_converted,
                    period_converted,
                    period_converted,
                    period_converted,
                ],
                [0, 0, 0, 0], // zero waveform
                [
                    period_converted,
                    period_converted,
                    period_converted,
                    period_converted,
                ],
                [0, 0, 0, 0], // zero waveform
            );

            let _ = manager_guard.write_command(&command).await;
        }
        DeviceVersion::V2 => {
            // Send zero intensity and neutral waveforms for V2
            let intensity_packet = generate_v2_intensity(0, 0);
            let zero_waveform = generate_v2_waveform(1, 100, 0);
            let _ = manager_guard
                .write_v2_data(&intensity_packet, &zero_waveform, &zero_waveform)
                .await;
        }
    }

    println!("[PAUSE] Sent zero command to stop output");
}

// Debug counter for logging
use std::sync::atomic::AtomicU64;
static DEBUG_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Send a single update to the device
async fn send_device_update() -> Result<(), String> {
    let count = DEBUG_COUNTER.fetch_add(1, Ordering::Relaxed);

    // Get Bluetooth manager
    let manager = get_bluetooth_manager()
        .await
        .map_err(|e| format!("Bluetooth error: {}", e))?;

    let mut manager_guard = manager.lock().await;
    let is_connected = manager_guard.is_connected();
    let is_paused = crate::settings::get_output_paused().await;
    // Get the current engine settings
    let engine_type = {
        let state = get_processing_state().await.read().await;
        state.options.processing_engine
    };

    // Sub G.3.0+ note: this function runs every tick regardless of
    // connection / pause. Telemetry (`waveform-sample`,
    // `resolved-update`) and engine advancement (V2 ramp, V3 lookahead,
    // bp:-stash for the resolver telemetry pass) are unconditional;
    // the BLE write at the end is the only path gated on
    // `is_connected && !is_paused`. Frontend indicators stay live
    // even when no device is hooked up or output is paused.
    if count % 100 == 0 && !is_connected {
        println!(
            "[DEBUG] Device loop running but not connected (count: {})",
            count
        );
    }

    // Get next waveform data (consumes 4 values from queue per channel).
    // For `bp:`-routed intensity links this also stashes the resolver's
    // `ResolvedSample` on each channel so the telemetry pass below can
    // read it without re-running the resolver (and double-advancing
    // phase / smoothing state).
    let (waveform_a, waveform_b) = get_next_waveform_data().await;

    // Per-slot frequency arrays for V3 B0. Window starts 100ms before now so
    // the 4 slots land at now-100, now-75, now-50, now-25 (matching the axis
    // history timeline). For V2 we keep the scalar `params_a.frequency`.
    //
    // This MUST run before `get_resolved_channel_params`: the per-slot pass
    // is the authoritative advancer of each channel's `link_runtime.frequency`
    // transform state (four chronological 25ms sub-steps) and stashes its
    // latest sample on the channel. The telemetry pass below then reads that
    // stash instead of re-resolving frequency, which would advance a stateful
    // frequency transform a fifth, out-of-order time per tick.
    let window_start_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        - 100;
    let (freq_slots_a_hz, freq_slots_b_hz) =
        crate::resolver::get_per_slot_frequencies(window_start_ms).await;

    // Resolved channel parameters + telemetry snapshots in one pass.
    // Frequency is sourced from the per-slot stash above; balance always
    // runs through the resolver; intensity is sourced from the bp-path stash
    // for `bp:` links and synthesized from the engine output / static value
    // otherwise.
    let ((params_a, params_b), (resolved_a, resolved_b)) =
        get_resolved_channel_params().await;

    // V2 path uses the scalar `params_a/b.frequency` directly via
    // `freq_to_v2_xy`; V3 uses the per-slot arrays computed above. No need
    // for a single-value period conversion anymore.

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
        waveform_a.intensity // Static: use directly without scaling
    } else {
        scale_intensity(waveform_a.intensity, range_min_a, range_max_a)
    };
    let scaled_b = if params_b.intensity_is_static {
        waveform_b.intensity // Static: use directly without scaling
    } else {
        scale_intensity(waveform_b.intensity, range_min_b, range_max_b)
    };

    // Per-channel device intensity cap ("soft mode"). Hardware enforces this
    // via BF below, but clamp here too — covers the brief window before the
    // first BF lands after a (re)connect.
    let general = crate::settings::get_settings().await.general;
    let max_a = general.channel_a_max_intensity.min(200);
    let max_b = general.channel_b_max_intensity.min(200);
    let scaled_a = scaled_a.min(max_a);
    let scaled_b = scaled_b.min(max_b);

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
        output.is_connected = is_connected;
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
    emit_waveform_sample(sample);

    // Sub G: emit per-tick resolver snapshot. Pairs with
    // `waveform-sample` — the latter carries engine output (post
    // V2-ramp / V3-lookahead, post-range, what BLE sees), this one
    // carries the resolver's view (post-curve, post-transforms,
    // pre-engine). UI cards subscribe and render the position line on
    // each linked-parameter curve plot.
    emit_resolved_update(ResolvedUpdatePayload {
        timestamp_ms: timestamp,
        channel_a: resolved_a,
        channel_b: resolved_b,
    });

    // Diagnostic tick recording (no-op when capture is off). Records the
    // computed engine output regardless of connection state, so a capture
    // works without a device hooked up.
    crate::diagnostic::record_tick(
        is_connected,
        scaled_a,
        scaled_b,
        waveform_a.waveform_intensity,
        waveform_b.waveform_intensity,
        waveform_a.raw_values,
        waveform_b.raw_values,
        freq_a,
        freq_b,
    );

    // BLE write path requires an actual connection AND output not paused.
    // Telemetry above (waveform-sample + resolved-update) already fired —
    // only BLE writes are gated.
    if !is_connected || is_paused {
        return Ok(());
    }

    // Get device version (defaulting to V3 just in case)
    let device_version = manager_guard.device_version.unwrap_or(DeviceVersion::V3);

    match device_version {
        DeviceVersion::V3 => {
            // Send BF (balance + soft limits) before B0 whenever the snapshot
            // changed (includes first tick after connect where snapshot = None).
            // No rate throttle: DG-LAB docs give no write-frequency limit for
            // BF. The natural upper bound on write rate is the 10Hz device
            // tick combined with the frontend's 50ms debounce on slider
            // changes — effectively ≤10 BF writes/sec during a drag, zero
            // when idle. Soft limits come from the per-channel cap setting
            // ("soft mode") — hardware enforces them across reconnects via
            // device flash; software also clamps `scaled_*` above as a
            // belt-and-suspenders for the brief pre-BF window.
            let desired_bf: BfSnapshot = (
                max_a,
                max_b,
                params_a.freq_balance,
                params_b.freq_balance,
                params_a.int_balance,
                params_b.int_balance,
            );
            let device_state_ref = get_device_state().await;
            let needs_bf = {
                let last = device_state_ref.last_bf_sent.read().await;
                last.as_ref() != Some(&desired_bf)
            };
            if needs_bf {
                let bf_cmd = generate_bf_command(
                    desired_bf.0,
                    desired_bf.1,
                    desired_bf.2,
                    desired_bf.3,
                    desired_bf.4,
                    desired_bf.5,
                );
                match manager_guard.write_command(&bf_cmd).await {
                    Ok(_) => {
                        *device_state_ref.last_bf_sent.write().await = Some(desired_bf);
                    }
                    Err(e) => {
                        // Dead-handle = OS closed our BLE link (device dropped /
                        // adapter reset). Stop the loop hammering a closed
                        // socket; mark disconnected, kick off reconnect, bail.
                        if crate::bluetooth::is_handle_dead(&*e) {
                            crate::log_warn!(
                                "[V3] BF write hit dead BLE handle: {} — flipping to disconnected, will auto-reconnect",
                                e
                            );
                            manager_guard.mark_disconnected();
                            drop(manager_guard);
                            crate::emit_connection_changed("bluetooth", false, None);
                            crate::bluetooth::spawn_reconnect_attempt();
                            return Err(format!("BLE handle closed: {}", e));
                        }
                        println!("[DEBUG] V3 BF Write FAILED: {}", e);
                        // Transient error — fall through to B0.
                    }
                }
            }

            // Per-slot period arrays from sub-100ms freq resolution.
            let slot_periods_a: [u8; 4] =
                std::array::from_fn(|i| convert_period(frequency_to_period(freq_slots_a_hz[i])));
            let slot_periods_b: [u8; 4] =
                std::array::from_fn(|i| convert_period(frequency_to_period(freq_slots_b_hz[i])));

            let command = generate_b0_command(
                3,
                3,
                scaled_a,
                scaled_b,
                slot_periods_a,
                waveform_a.waveform_intensity,
                slot_periods_b,
                waveform_b.waveform_intensity,
            );

            match manager_guard.write_command(&command).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    if crate::bluetooth::is_handle_dead(&*e) {
                        crate::log_warn!(
                            "[V3] B0 write hit dead BLE handle: {} — flipping to disconnected, will auto-reconnect",
                            e
                        );
                        manager_guard.mark_disconnected();
                        drop(manager_guard);
                        crate::emit_connection_changed("bluetooth", false, None);
                        crate::bluetooth::spawn_reconnect_attempt();
                        return Err(format!("BLE handle closed: {}", e));
                    }
                    crate::log_error!("[V3] B0 Write FAILED: {}", e);
                    Err(format!("Write error: {}", e))
                }
            }
        }
        DeviceVersion::V2 => {
            // V2 Engine Effect Deep Customization

            // Auxiliary closure: Calculate the waveform coefficients of a single channel (0.0 - 1.0)
            let calc_coeff = |waveform: &[u8; 4]| -> f32 {
                // Convert waveform data from 0-100 to floating point numbers from 0.0-1.0
                let vals: Vec<f32> = waveform.iter().map(|&x| x as f32 / 100.0).collect();
                let max = vals.iter().fold(0.0f32, |a, &b| a.max(b));
                let avg = vals.iter().sum::<f32>() / 4.0;
                let min = vals.iter().fold(1.0f32, |a, &b| a.min(b));

                match engine_type {
                    // [V2 Smooth Soft Mode] -Delicate, smooth
                    ProcessingEngineType::V2Smooth => avg,

                    // [V2 Balanced Mode] - Moderate Force (Recommended)
                    ProcessingEngineType::V2Balanced => (avg + max) / 2.0,

                    // [V2 Detailed Details/Aggressive Mode] - Powerful Explosive Force
                    ProcessingEngineType::V2Detailed => max,

                    // [V2 Dynamic / V2 Sustained] - Intelligently Preserves the Shaking Sensation
                    // V2Sustained reuses Dynamic's shaping for the V2-hardware
                    // perceived-intensity coefficient. The sustained part lives
                    // in the V3-host master `intensity` byte, which V2 hardware
                    // doesn't read; on V2 hardware the engines feel identical.
                    ProcessingEngineType::V2Dynamic | ProcessingEngineType::V2Sustained => {
                        let range = max - min;
                        if range > 0.3 {
                            max
                        } else {
                            avg
                        }
                    }

                    // [V3 Predictive Mode] - Smooth Follow Simulation
                    ProcessingEngineType::V3Predictive => {
                        vals[0] * 0.1 + vals[1] * 0.2 + vals[2] * 0.3 + vals[3] * 0.4
                    }
                }
            };

            // Calculate coefficients for A/B channels separately
            let coeff_a = calc_coeff(&waveform_a.waveform_intensity);
            let coeff_b = calc_coeff(&waveform_b.waveform_intensity);

            // Apply coefficients and map to V2 intensity range (0-2047)
            let v2_int_a = (scaled_a as f32 * coeff_a * V2_INTENSITY_SCALE).min(2047.0) as u16;
            let v2_int_b = (scaled_b as f32 * coeff_b * V2_INTENSITY_SCALE).min(2047.0) as u16;

            // Generate and send data
            let intensity_packet = generate_v2_intensity(v2_int_a, v2_int_b);

            let (xa, ya) = freq_to_v2_xy(params_a.frequency);
            let za = balance_to_v2_z(params_a.int_balance);
            let waveform_a_packet = generate_v2_waveform(xa, ya, za);

            let (xb, yb) = freq_to_v2_xy(params_b.frequency);
            let zb = balance_to_v2_z(params_b.int_balance);
            let waveform_b_packet = generate_v2_waveform(xb, yb, zb);

            match manager_guard
                .write_v2_data(&intensity_packet, &waveform_a_packet, &waveform_b_packet)
                .await
            {
                Ok(_) => Ok(()),
                Err(e) => {
                    if crate::bluetooth::is_handle_dead(&*e) {
                        crate::log_warn!(
                            "[V2] write hit dead BLE handle: {} — flipping to disconnected, will auto-reconnect",
                            e
                        );
                        manager_guard.mark_disconnected();
                        drop(manager_guard);
                        crate::emit_connection_changed("bluetooth", false, None);
                        crate::bluetooth::spawn_reconnect_attempt();
                        return Err(format!("BLE handle closed: {}", e));
                    }
                    Err(format!("Write error: {}", e))
                }
            }
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
