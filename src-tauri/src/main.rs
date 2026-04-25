// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter, Manager};

mod bluetooth;
mod buttplug;
mod device;
mod diagnostic;
mod gamepad;
mod logging;
mod lovense;
mod modulation;
mod processing;
mod protocol;
mod settings;
mod waveform;
mod websocket;

// Global AppHandle for emitting events from anywhere
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// Store the AppHandle for global access
pub fn set_app_handle(handle: AppHandle) {
    let _ = APP_HANDLE.set(handle);
}

/// Get the stored AppHandle
pub fn get_app_handle() -> Option<&'static AppHandle> {
    APP_HANDLE.get()
}

/// Payload for axis update events
#[derive(Clone, Serialize)]
pub struct AxisUpdatePayload {
    pub axes: HashMap<String, f64>,
    pub channel_a: f64,
    pub channel_b: f64,
    pub timestamp: u64,
}

/// Payload for connection status change events
#[derive(Clone, Serialize)]
pub struct ConnectionChangedPayload {
    pub connection_type: String, // "websocket" or "bluetooth"
    pub connected: bool,
    pub device_address: Option<String>,
    pub timestamp: u64,
}

/// Payload for output pause state change events
#[derive(Clone, Serialize)]
pub struct OutputPauseChangedPayload {
    pub paused: bool,
    pub timestamp: u64,
}

/// Payload for Buttplug feature update events
#[derive(Clone, Serialize)]
pub struct ButtplugFeaturesPayload {
    pub features: HashMap<String, f64>,
    pub timestamp: u64,
}

/// Payload for battery level change events
#[derive(Clone, Serialize)]
pub struct BatteryChangedPayload {
    pub level: u8,
    pub timestamp: u64,
}

/// Full connection status for HMR recovery
#[derive(Clone, Serialize)]
pub struct ConnectionStatus {
    pub websocket_running: bool,
    pub detected_input_protocol: String, // "none", "tcode", "buttplug", or "lovense"
    pub bluetooth_connected: bool,
    pub bluetooth_device_address: Option<String>,
    pub battery_level: Option<u8>,
    pub discovered_devices: Vec<BluetoothDevice>,
}

/// Full application state for HMR recovery (stateless frontend)
#[derive(Clone, Serialize)]
pub struct FullAppState {
    pub connection: ConnectionStatus,
    pub channel_a: ChannelStateSnapshot,
    pub channel_b: ChannelStateSnapshot,
    pub output_options: OutputOptionsSnapshot,
    pub timestamp: u64,
}

#[derive(Clone, Serialize)]
pub struct ChannelStateSnapshot {
    pub frequency: f64,
    pub freq_balance: u8,
    pub int_balance: u8,
    pub range_min: u8,
    pub range_max: u8,
    pub current_intensity: f64,
}

#[derive(Clone, Serialize)]
pub struct OutputOptionsSnapshot {
    pub processing_engine: String,
    pub peak_fill: String,
}

/// Emit axis values to the frontend.
///
/// Fired from `websocket::handle_tcode_message` per inbound T-Code command, so
/// the event rate matches the input stream (typically 10-60+ Hz depending on
/// the sender) rather than the 10Hz device tick. Consumers should treat the
/// cadence as "whenever input arrives" and not assume a fixed rate.
pub fn emit_axis_update(axes: HashMap<String, f64>, channel_a: f64, channel_b: f64) {
    if let Some(handle) = get_app_handle() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let payload = AxisUpdatePayload {
            axes,
            channel_a,
            channel_b,
            timestamp,
        };

        let _ = handle.emit("axis-update", payload);
    }
}

/// Emit connection status change to frontend
pub fn emit_connection_changed(
    connection_type: &str,
    connected: bool,
    device_address: Option<String>,
) {
    if let Some(handle) = get_app_handle() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let payload = ConnectionChangedPayload {
            connection_type: connection_type.to_string(),
            connected,
            device_address,
            timestamp,
        };

        let _ = handle.emit("connection-changed", payload);
    }
}

/// Emit output pause state change to frontend
pub fn emit_output_pause_changed(paused: bool) {
    if let Some(handle) = get_app_handle() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let payload = OutputPauseChangedPayload { paused, timestamp };

        let _ = handle.emit("output-pause-changed", payload);
    }
}

/// Emit waveform sample to frontend (called from device loop at 10Hz when sending to device)
pub fn emit_waveform_sample(sample: waveform::WaveformSample) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit("waveform-sample", sample);
    }
}

/// Emit battery level change to frontend. Fired after the initial read on
/// connect and from the 30s polling task in `bluetooth::start_battery_monitor`.
pub fn emit_battery_changed(level: u8) {
    if let Some(handle) = get_app_handle() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let payload = BatteryChangedPayload { level, timestamp };
        let _ = handle.emit("battery-changed", payload);
    }
}

/// Forward a backend log line to the frontend LogsPanel. Called from the
/// ring-buffer logger so every `log_info!`/`log_error!` etc. shows up in
/// the in-app panel, not just the on-disk file. No-ops before the app
/// handle is initialized (early startup logs still hit the file).
pub fn emit_backend_log(level: &str, message: &str) {
    if let Some(handle) = get_app_handle() {
        #[derive(serde::Serialize, Clone)]
        struct BackendLog<'a> {
            level: &'a str,
            message: &'a str,
        }
        let _ = handle.emit("backend-log", BackendLog { level, message });
    }
}


/// Emit Buttplug feature values to frontend
pub fn emit_buttplug_features(features: HashMap<String, f64>) {
    if let Some(handle) = get_app_handle() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let payload = ButtplugFeaturesPayload {
            features,
            timestamp,
        };

        let _ = handle.emit("buttplug-features", payload);
    }
}

use bluetooth::get_bluetooth_manager;
use device::{
    get_channel_a_params, get_channel_b_params, get_last_device_output, start_device_loop,
    stop_device_loop, update_channel_a_params, update_channel_b_params, ChannelParams,
    DeviceOutput,
};
use modulation::ParameterSource;
use settings::{
    AppSettings, BluetoothSettings, ChannelPreset, ChannelSettings as SettingsChannelSettings,
    ConnectionSettings, GamepadBindings, GeneralSettings, KeyboardShortcuts, OutputSettings,
};
use websocket::{
    apply_saved_settings_to_processing, get_current_intensities, is_server_running,
    set_output_options, start_server, stop_server,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BluetoothDevice {
    address: String,
    name: Option<String>,
    rssi: Option<i16>,
}

#[tauri::command]
async fn get_bluetooth_adapters() -> Result<Vec<String>, String> {
    match get_bluetooth_manager().await {
        Ok(manager) => {
            let manager = manager.lock().await;
            match manager.get_adapters().await {
                Ok(adapters) => Ok(adapters.into_iter().map(|a| a.name).collect()),
                Err(e) => Err(format!("Failed to get adapters: {}", e)),
            }
        }
        Err(e) => Err(format!("Failed to initialize Bluetooth: {}", e)),
    }
}

#[tauri::command]
async fn scan_bluetooth_devices(adapter_index: usize) -> Result<Vec<BluetoothDevice>, String> {
    match get_bluetooth_manager().await {
        Ok(manager) => {
            let mut manager = manager.lock().await;
            match manager.scan_devices(adapter_index).await {
                Ok(devices) => Ok(devices
                    .into_iter()
                    .map(|d| BluetoothDevice {
                        address: d.address,
                        name: d.name,
                        rssi: d.rssi,
                    })
                    .collect()),
                Err(e) => Err(format!("Failed to scan devices: {}", e)),
            }
        }
        Err(e) => Err(format!("Failed to initialize Bluetooth: {}", e)),
    }
}

/// Get the list of discovered Bluetooth devices from the last scan (no new scan)
#[tauri::command]
async fn get_discovered_bluetooth_devices() -> Result<Vec<BluetoothDevice>, String> {
    match get_bluetooth_manager().await {
        Ok(manager) => {
            let manager = manager.lock().await;
            let devices = manager.get_discovered_devices();
            Ok(devices
                .into_iter()
                .map(|d| BluetoothDevice {
                    address: d.address,
                    name: d.name,
                    rssi: d.rssi,
                })
                .collect())
        }
        Err(e) => Err(format!("Failed to get Bluetooth manager: {}", e)),
    }
}

#[tauri::command]
async fn start_websocket_server(port: u16) -> Result<String, String> {
    // Check if already running
    if is_server_running().await {
        return Ok("WebSocket server is already running".to_string());
    }

    match start_server(port).await {
        Ok(_) => {
            // Apply saved settings to ProcessingState
            apply_saved_settings_to_processing().await;

            // Start the 10Hz update loop for UI visualization
            // (works even without Bluetooth device connected)
            start_device_loop().await;

            // Emit connection changed event for stateless frontend
            emit_connection_changed("websocket", true, None);

            Ok(format!("WebSocket server started on port {}", port))
        }
        Err(e) => Err(format!("Failed to start WebSocket server: {}", e)),
    }
}

#[tauri::command]
async fn stop_websocket_server() -> Result<String, String> {
    // Stop the UI update loop
    stop_device_loop().await;

    match stop_server().await {
        Ok(_) => {
            // Emit connection changed event for stateless frontend
            emit_connection_changed("websocket", false, None);
            Ok("WebSocket server stopped".to_string())
        }
        Err(e) => Err(format!("Failed to stop WebSocket server: {}", e)),
    }
}

#[tauri::command]
async fn get_websocket_status() -> Result<bool, String> {
    Ok(is_server_running().await)
}

#[tauri::command]
async fn get_channel_intensities() -> Result<(f64, f64), String> {
    Ok(get_current_intensities().await)
}

#[tauri::command]
async fn get_axis_values() -> Result<HashMap<String, f64>, String> {
    use crate::websocket::get_axis_values_from_processing;
    Ok(get_axis_values_from_processing().await)
}

#[tauri::command]
async fn update_output_options(
    engine: Option<String>,
    peak_fill: Option<String>,
) -> Result<String, String> {
    set_output_options(engine, peak_fill).await;
    Ok("Output options updated".to_string())
}

#[tauri::command]
async fn connect_bluetooth_device(adapter_index: usize, address: String) -> Result<String, String> {
    match get_bluetooth_manager().await {
        Ok(manager) => {
            let mut manager = manager.lock().await;
            match manager.connect_device(adapter_index, &address).await {
                Ok(_) => {
                    // Read battery level right after connecting
                    let battery_level = match manager.read_battery().await {
                        Ok(level) => {
                            println!("[BATTERY] Device battery level: {}%", level);
                            Some(level)
                        }
                        Err(e) => {
                            println!("[BATTERY] Failed to read battery: {}", e);
                            None
                        }
                    };

                    drop(manager); // Release lock before starting loop

                    // Clear BF snapshot so the first tick resends balance +
                    // soft-limit params (device flash persists BF across
                    // reconnects, so we can't trust what's there).
                    device::reset_bf_snapshot().await;

                    // Start the 10Hz device update loop
                    start_device_loop().await;

                    // Emit connection changed event for stateless frontend
                    emit_connection_changed("bluetooth", true, Some(address.clone()));

                    // Push the initial battery reading and spawn the 30s
                    // refresh task so the frontend stays in sync without
                    // polling us for it.
                    if let Some(level) = battery_level {
                        emit_battery_changed(level);
                    }
                    bluetooth::start_battery_monitor();

                    let battery_info = battery_level
                        .map(|l| format!(" (Battery: {}%)", l))
                        .unwrap_or_default();
                    Ok(format!(
                        "Connected to Bluetooth device: {}{}",
                        address, battery_info
                    ))
                }
                Err(e) => Err(format!("Failed to connect: {}", e)),
            }
        }
        Err(e) => Err(format!("Failed to initialize Bluetooth: {}", e)),
    }
}

#[tauri::command]
async fn disconnect_bluetooth_device() -> Result<String, String> {
    // Stop the device loop first
    stop_device_loop().await;

    // Clear BF snapshot so the next reconnect rewrites from scratch rather
    // than assuming the device still holds our prior values.
    device::reset_bf_snapshot().await;

    match get_bluetooth_manager().await {
        Ok(manager) => {
            let mut manager = manager.lock().await;
            match manager.disconnect_device().await {
                Ok(_) => {
                    // Emit connection changed event for stateless frontend
                    emit_connection_changed("bluetooth", false, None);
                    Ok("Disconnected from Bluetooth device".to_string())
                }
                Err(e) => Err(format!("Failed to disconnect: {}", e)),
            }
        }
        Err(e) => Err(format!("Failed to initialize Bluetooth: {}", e)),
    }
}

#[tauri::command]
async fn send_coyote_command(command_data: Vec<u8>) -> Result<String, String> {
    match get_bluetooth_manager().await {
        Ok(manager) => {
            let manager = manager.lock().await;
            if !manager.is_connected() {
                return Err("No device connected".to_string());
            }
            match manager.write_command(&command_data).await {
                Ok(_) => Ok(format!("Sent {} bytes to device", command_data.len())),
                Err(e) => Err(format!("Failed to send command: {}", e)),
            }
        }
        Err(e) => Err(format!("Bluetooth not available: {}", e)),
    }
}

#[tauri::command]
async fn send_device_response(response: String) -> Result<String, String> {
    Ok(format!("Sent response: {}", response))
}

#[tauri::command]
async fn get_device_output() -> Result<DeviceOutput, String> {
    Ok(get_last_device_output().await)
}

// ============================================================================
// State Query Commands (for HMR-resilient stateless frontend)
// ============================================================================

/// Get current connection status for both WebSocket and Bluetooth
#[tauri::command]
async fn get_connection_status() -> Result<ConnectionStatus, String> {
    use crate::websocket::get_detected_protocol;

    let ws_running = is_server_running().await;
    let detected_protocol = get_detected_protocol().await.as_str().to_string();

    let (bt_connected, bt_address, discovered) = match get_bluetooth_manager().await {
        Ok(manager) => {
            let manager = manager.lock().await;
            let connected = manager.is_connected();
            let address = manager.get_connected_device_address();
            let devices = manager
                .get_discovered_devices()
                .into_iter()
                .map(|d| BluetoothDevice {
                    address: d.address,
                    name: d.name,
                    rssi: d.rssi,
                })
                .collect();
            (connected, address, devices)
        }
        Err(_) => (false, None, Vec::new()),
    };

    let battery = if bt_connected {
        match get_bluetooth_manager().await {
            Ok(manager) => {
                let manager = manager.lock().await;
                manager.read_battery().await.ok()
            }
            Err(_) => None,
        }
    } else {
        None
    };

    Ok(ConnectionStatus {
        websocket_running: ws_running,
        detected_input_protocol: detected_protocol,
        bluetooth_connected: bt_connected,
        bluetooth_device_address: bt_address,
        battery_level: battery,
        discovered_devices: discovered,
    })
}

/// Get full application state for HMR recovery
/// This returns all live state so the frontend can restore after reload
#[tauri::command]
async fn get_full_state() -> Result<FullAppState, String> {
    use crate::processing::{get_processing_state, ProcessingEngineType};

    let connection = get_connection_status().await?;

    // Device-side mirror for frequency/balance; ranges come from processing state.
    let params_a_dev = get_channel_a_params().await;
    let params_b_dev = get_channel_b_params().await;

    let (intensity_a, intensity_b) = get_current_intensities().await;

    // Pull engine/peak_fill + per-channel ranges from processing state in one
    // read lock. Ranges live on the intensity ParameterSource, not on device
    // params anymore.
    let (engine_str, peak_fill_str, range_a, range_b) = {
        let state = get_processing_state().await;
        let state_guard = state.read().await;
        let engine = match state_guard.options.processing_engine {
            ProcessingEngineType::V1 => "v1",
            ProcessingEngineType::V2Smooth => "v2-smooth",
            ProcessingEngineType::V2Balanced => "v2-balanced",
            ProcessingEngineType::V2Detailed => "v2-detailed",
            ProcessingEngineType::V2Dynamic => "v2-dynamic",
            ProcessingEngineType::V2Sustained => "v2-sustained",
            ProcessingEngineType::V3Predictive => "v3-predictive",
        };
        let range_for = |id: crate::processing::ChannelId| -> (u8, u8) {
            let src = &state_guard.channel(id).config.intensity;
            (src.range_min as u8, src.range_max as u8)
        };
        (
            engine.to_string(),
            state_guard.options.peak_fill.as_str().to_string(),
            range_for(crate::processing::ChannelId::A),
            range_for(crate::processing::ChannelId::B),
        )
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    Ok(FullAppState {
        connection,
        channel_a: ChannelStateSnapshot {
            frequency: params_a_dev.frequency,
            freq_balance: params_a_dev.freq_balance,
            int_balance: params_a_dev.int_balance,
            range_min: range_a.0,
            range_max: range_a.1,
            current_intensity: intensity_a,
        },
        channel_b: ChannelStateSnapshot {
            frequency: params_b_dev.frequency,
            freq_balance: params_b_dev.freq_balance,
            int_balance: params_b_dev.int_balance,
            range_min: range_b.0,
            range_max: range_b.1,
            current_intensity: intensity_b,
        },
        output_options: OutputOptionsSnapshot {
            processing_engine: engine_str,
            peak_fill: peak_fill_str,
        },
        timestamp,
    })
}

// ============================================================================
// Settings Commands
// ============================================================================

/// Sync a full AppSettings to ProcessingState: channel configs + buttplug link configs.
/// Single source of truth for settings→runtime conversion used by load and save paths.
async fn sync_settings_to_state(settings: &AppSettings) {
    let state = processing::get_processing_state().await;
    let mut state_guard = state.write().await;

    use crate::processing::ChannelId;
    state_guard.channel_mut(ChannelId::A).config =
        crate::websocket::convert_channel_settings(&settings.channel_a);
    state_guard.channel_mut(ChannelId::B).config =
        crate::websocket::convert_channel_settings(&settings.channel_b);

    if let Some(ref bp_links) = settings.channel_a.intensity_source.buttplug_links {
        state_guard.set_buttplug_link_config('A', bp_links.to_link_config());
    }
    if let Some(ref bp_links) = settings.channel_b.intensity_source.buttplug_links {
        state_guard.set_buttplug_link_config('B', bp_links.to_link_config());
    }
}

#[tauri::command]
async fn get_app_settings() -> Result<AppSettings, String> {
    let settings = settings::get_settings().await;
    sync_settings_to_state(&settings).await;
    Ok(settings)
}

#[tauri::command]
async fn save_app_settings(settings: AppSettings) -> Result<String, String> {
    settings::update_settings(settings.clone()).await?;
    sync_settings_to_state(&settings).await;
    Ok("Settings saved".to_string())
}

#[tauri::command]
async fn save_channel_settings(
    channel: String,
    channel_settings: SettingsChannelSettings,
) -> Result<String, String> {
    // Snapshot static fallback values for HMR recovery. When a source is
    // linked, seed the mirror with a sensible middle-of-range so reload shows
    // something reasonable until the next T-Code tick repopulates it.
    let frequency = if channel_settings.frequency_source.source_type
        == settings::ParameterSourceType::Static
    {
        channel_settings.frequency_source.static_value
    } else {
        (channel_settings.frequency_source.range_min + channel_settings.frequency_source.range_max)
            / 2.0
    };

    let freq_balance = if channel_settings.frequency_balance_source.source_type
        == settings::ParameterSourceType::Static
    {
        channel_settings.frequency_balance_source.static_value as u8
    } else {
        128
    };

    let int_balance = if channel_settings.intensity_balance_source.source_type
        == settings::ParameterSourceType::Static
    {
        channel_settings.intensity_balance_source.static_value as u8
    } else {
        128
    };

    let params = ChannelParams {
        frequency,
        freq_balance,
        int_balance,
    };

    // Convert settings → runtime ChannelConfig before moving channel_settings.
    // Disk write first; on failure processing state stays consistent with disk.
    let new_config = crate::websocket::convert_channel_settings(&channel_settings);
    let bp_config = channel_settings
        .intensity_source
        .buttplug_links
        .as_ref()
        .map(|bp| bp.to_link_config());

    match channel.as_str() {
        "A" | "a" => {
            settings::update_channel_a(channel_settings).await?;

            let state = processing::get_processing_state().await;
            let mut state_guard = state.write().await;
            state_guard.channel_mut(processing::ChannelId::A).config = new_config;
            if let Some(cfg) = bp_config {
                state_guard.set_buttplug_link_config('A', cfg);
            }
            drop(state_guard);

            update_channel_a_params(params).await;
        }
        "B" | "b" => {
            settings::update_channel_b(channel_settings).await?;

            let state = processing::get_processing_state().await;
            let mut state_guard = state.write().await;
            state_guard.channel_mut(processing::ChannelId::B).config = new_config;
            if let Some(cfg) = bp_config {
                state_guard.set_buttplug_link_config('B', cfg);
            }
            drop(state_guard);

            update_channel_b_params(params).await;
        }
        _ => return Err(format!("Unknown channel: {}", channel)),
    }
    Ok("Channel settings saved".to_string())
}

/// Lightweight processing-state-only update for a channel. No disk I/O.
/// Used by the fast (50ms) frontend debounce so device output reflects UI
/// changes immediately; `save_channel_settings` runs behind it for persistence.
#[tauri::command]
async fn update_channel_config(
    channel: String,
    channel_settings: SettingsChannelSettings,
) -> Result<String, String> {
    let new_config = crate::websocket::convert_channel_settings(&channel_settings);
    let bp_config = channel_settings
        .intensity_source
        .buttplug_links
        .as_ref()
        .map(|bp| bp.to_link_config());

    let channel_id = processing::ChannelId::from_str(&channel)
        .ok_or_else(|| format!("Unknown channel: {}", channel))?;

    let state = processing::get_processing_state().await;
    let mut state_guard = state.write().await;
    state_guard.channel_mut(channel_id).config = new_config;
    if let Some(cfg) = bp_config {
        state_guard.set_buttplug_link_config(channel_id.as_char(), cfg);
    }
    drop(state_guard);

    Ok(format!("Updated channel {} config", channel))
}

#[tauri::command]
async fn save_output_settings(
    processing_engine: String,
    peak_fill: Option<String>,
) -> Result<String, String> {
    let fill = peak_fill
        .as_deref()
        .map(processing::PeakFillStrategy::from_str)
        .unwrap_or_default();

    // Update processing options immediately
    set_output_options(
        Some(processing_engine.clone()),
        Some(fill.as_str().to_string()),
    )
    .await;

    // Persist to settings file
    let output_settings = OutputSettings {
        processing_engine: processing::ProcessingEngineType::from_str(&processing_engine),
        peak_fill: fill,
    };
    settings::update_output(output_settings).await?;
    Ok("Output settings saved".to_string())
}

#[tauri::command]
async fn save_connection_settings(
    websocket_port: u16,
    auto_open: bool,
    show_tcode_monitor: bool,
) -> Result<String, String> {
    let connection_settings = ConnectionSettings {
        websocket_port,
        auto_open,
        show_tcode_monitor,
    };
    settings::update_connection(connection_settings).await?;
    Ok("Connection settings saved".to_string())
}

/// Get just the WebSocket port from settings (for frontend to use with connect)
#[tauri::command]
async fn get_websocket_port() -> Result<u16, String> {
    let settings = settings::get_settings().await;
    Ok(settings.connection.websocket_port)
}

#[tauri::command]
async fn save_bluetooth_settings(
    selected_interface: usize,
    auto_scan: bool,
    auto_connect: bool,
    saved_devices: Vec<settings::SavedBluetoothDevice>,
    last_device: Option<String>,
) -> Result<String, String> {
    let bluetooth_settings = BluetoothSettings {
        selected_interface,
        auto_scan,
        auto_connect,
        saved_devices,
        last_device,
    };
    settings::update_bluetooth(bluetooth_settings).await?;
    Ok("Bluetooth settings saved".to_string())
}

#[tauri::command]
async fn save_shortcuts(
    channel_a_freq_up: String,
    channel_a_freq_down: String,
    channel_a_int_up: String,
    channel_a_int_down: String,
    channel_a_freq_bal_up: String,
    channel_a_freq_bal_down: String,
    channel_a_int_bal_up: String,
    channel_a_int_bal_down: String,
    channel_b_freq_up: String,
    channel_b_freq_down: String,
    channel_b_int_up: String,
    channel_b_int_down: String,
    channel_b_freq_bal_up: String,
    channel_b_freq_bal_down: String,
    channel_b_int_bal_up: String,
    channel_b_int_bal_down: String,
    help: String,
    settings_key: String,
    toggle_output_pause: Option<String>,
) -> Result<String, String> {
    let shortcuts = KeyboardShortcuts {
        channel_a_freq_up,
        channel_a_freq_down,
        channel_a_int_up,
        channel_a_int_down,
        channel_a_freq_bal_up,
        channel_a_freq_bal_down,
        channel_a_int_bal_up,
        channel_a_int_bal_down,
        channel_b_freq_up,
        channel_b_freq_down,
        channel_b_int_up,
        channel_b_int_down,
        channel_b_freq_bal_up,
        channel_b_freq_bal_down,
        channel_b_int_bal_up,
        channel_b_int_bal_down,
        help,
        settings: settings_key,
        toggle_output_pause: toggle_output_pause.unwrap_or_else(|| " ".to_string()),
    };
    settings::update_shortcuts(shortcuts).await?;
    Ok("Shortcuts saved".to_string())
}

#[tauri::command]
async fn save_gamepad_bindings(bindings: GamepadBindings) -> Result<String, String> {
    settings::update_gamepad_bindings(bindings.clone()).await?;
    gamepad::set_active_bindings(bindings).await;
    Ok("Gamepad bindings saved".to_string())
}

#[tauri::command]
async fn get_gamepad_bindings() -> Result<GamepadBindings, String> {
    Ok(settings::get_gamepad_bindings().await)
}

#[tauri::command]
async fn set_gamepad_engine(engine: String) -> Result<String, String> {
    let parsed = gamepad::GamepadEngine::from_str(&engine);
    gamepad::set_engine(parsed).await;
    // Persist the choice to general settings.
    let mut general = settings::get_settings().await.general;
    general.gamepad_engine = parsed.as_str().to_string();
    settings::update_general(general).await?;
    Ok(format!("Gamepad engine set to {}", parsed.as_str()))
}

#[tauri::command]
async fn save_general_settings(
    no_input_behavior: String,
    no_input_decay_ms: u32,
    update_rate_ms: u32,
    save_rate_ms: u32,
    show_tcode_monitor: bool,
    processing_engine: String,
) -> Result<String, String> {
    use crate::modulation::NoInputBehavior;
    use crate::processing::{get_processing_state, ProcessingEngineType};

    let engine = match processing_engine.as_str() {
        "v1" => ProcessingEngineType::V1,
        "v2-smooth" => ProcessingEngineType::V2Smooth,
        "v2-balanced" => ProcessingEngineType::V2Balanced,
        "v2-detailed" => ProcessingEngineType::V2Detailed,
        "v2-dynamic" => ProcessingEngineType::V2Dynamic,
        "v2-sustained" => ProcessingEngineType::V2Sustained,
        "v3-predictive" => ProcessingEngineType::V3Predictive,
        _ => ProcessingEngineType::V1,
    };

    // Parse no_input_behavior string to enum
    let behavior = match no_input_behavior.as_str() {
        "hold" => NoInputBehavior::Hold,
        "default" => NoInputBehavior::Default,
        "decay" => NoInputBehavior::Decay,
        "zero" => NoInputBehavior::Zero,
        _ => NoInputBehavior::Hold,
    };

    // Apply to running ProcessingState
    {
        let state = get_processing_state().await;
        let mut state_guard = state.write().await;
        state_guard.no_input_behavior = behavior;
        state_guard.no_input_decay_ms = no_input_decay_ms;
    }

    // Get current output_paused + gamepad_engine to preserve them
    let current = settings::get_settings().await.general;

    let general_settings = GeneralSettings {
        no_input_behavior,
        no_input_decay_ms,
        update_rate_ms,
        save_rate_ms,
        show_tcode_monitor,
        processing_engine: engine,
        output_paused: current.output_paused,
        gamepad_engine: current.gamepad_engine,
        gamepad_stick_sensitivity: current.gamepad_stick_sensitivity,
        gamepad_button_repeat_delay_ms: current.gamepad_button_repeat_delay_ms,
        gamepad_button_repeat_interval_ms: current.gamepad_button_repeat_interval_ms,
        channel_a_max_intensity: current.channel_a_max_intensity,
        channel_b_max_intensity: current.channel_b_max_intensity,
    };
    settings::update_general(general_settings).await?;
    Ok("General settings saved".to_string())
}

/// Set per-channel device intensity cap ("soft mode"). 0-200 (clamped).
/// Persisted in general settings; device.rs reads it per tick to set the BF
/// soft limit and clamp scaled values.
#[tauri::command]
async fn set_channel_max_intensity(channel: String, value: u8) -> Result<String, String> {
    let clamped = value.min(200);
    let mut general = settings::get_settings().await.general;
    match channel.as_str() {
        "A" | "a" => general.channel_a_max_intensity = clamped,
        "B" | "b" => general.channel_b_max_intensity = clamped,
        _ => return Err(format!("Unknown channel: {}", channel)),
    }
    settings::update_general(general).await?;
    // Force BF resend on next tick so the new cap reaches the device immediately.
    device::reset_bf_snapshot().await;
    Ok(format!("Channel {} max intensity set to {}", channel, clamped))
}

#[tauri::command]
async fn set_gamepad_stick_sensitivity(value: f64) -> Result<String, String> {
    let mut general = settings::get_settings().await.general;
    general.gamepad_stick_sensitivity = value.clamp(0.05, 5.0);
    settings::update_general(general).await?;
    Ok("Stick sensitivity saved".to_string())
}

#[tauri::command]
async fn set_gamepad_button_repeat(delay_ms: u32, interval_ms: u32) -> Result<String, String> {
    let mut general = settings::get_settings().await.general;
    general.gamepad_button_repeat_delay_ms = delay_ms.clamp(50, 5000);
    general.gamepad_button_repeat_interval_ms = interval_ms.clamp(20, 2000);
    settings::update_general(general).await?;
    Ok("Button repeat saved".to_string())
}

// ============================================================================
// Output Pause Commands
// ============================================================================

#[tauri::command]
async fn get_output_paused() -> Result<bool, String> {
    Ok(settings::get_output_paused().await)
}

#[tauri::command]
async fn set_output_paused(paused: bool) -> Result<String, String> {
    // When pausing, we need to send a zero signal first
    // This is handled by the device loop checking the pause state
    settings::set_output_paused(paused).await?;
    emit_output_pause_changed(paused);

    if paused {
        // Send immediate zero command to stop all output
        device::send_zero_command().await;
    }

    Ok(format!("Output paused: {}", paused))
}

#[tauri::command]
async fn toggle_output_paused() -> Result<bool, String> {
    let new_state = settings::toggle_output_paused().await?;
    emit_output_pause_changed(new_state);

    if new_state {
        // Send immediate zero command to stop all output
        device::send_zero_command().await;
    }

    Ok(new_state)
}

// ============================================================================
// Parameter Modulation Commands
// ============================================================================

#[tauri::command]
async fn update_parameter_source(
    channel: String,
    parameter: String,
    source: ParameterSource,
) -> Result<String, String> {
    use crate::buttplug::{ButtplugLinkConfig, ConstrictionMethod, FeatureTypeConfig};
    use crate::processing::get_processing_state;

    println!("[update_parameter_source] channel={}, parameter={}, source_type={:?}, source_axis={:?}, has_buttplug_links={:?}",
        channel, parameter, source.source_type, source.source_axis, source.buttplug_links.is_some());

    // Extract buttplug links before moving source
    let buttplug_links = source.buttplug_links.clone();

    let state = get_processing_state().await;
    let mut state = state.write().await;

    let channel_id = crate::processing::ChannelId::from_str(&channel)
        .ok_or_else(|| format!("Unknown channel: {}", channel))?;
    let channel_char = channel_id.as_char();

    {
        let config = &mut state.channel_mut(channel_id).config;
        match parameter.as_str() {
            "intensity" => config.intensity = source,
            "frequency" => config.frequency = source,
            "frequency_balance" => config.frequency_balance = source,
            "intensity_balance" => config.intensity_balance = source,
            _ => {
                return Err(format!(
                    "Unknown channel/parameter: {}/{}",
                    channel, parameter
                ))
            }
        }
    }

    // Also update buttplug link config if present (for intensity parameter)
    if parameter == "intensity" {
        if let Some(links) = buttplug_links {
            // Determine position feature type - Position vs PositionWithDuration
            let (position_feature, pos_dur_feature) = match links.position.as_ref() {
                Some(pos) if pos.feature_type == "PositionWithDuration" => {
                    (None, Some(pos.feature_index as usize))
                }
                Some(pos) => (Some(pos.feature_index as usize), None),
                None => (None, None),
            };

            let config = ButtplugLinkConfig {
                position_feature,
                pos_dur_feature,
                vibrate_feature: links.vibrate.as_ref().map(|l| l.feature_index as usize),
                vibrate_config: links.vibrate.as_ref().map(|l| FeatureTypeConfig {
                    distance: l.config.distance,
                    scale: None,
                    max_speed: None,
                    min_floor: None,
                    use_midpoint: None,
                    method: None,
                }),
                rotate_feature: links
                    .motion
                    .as_ref()
                    .filter(|l| l.feature_type == "Rotate")
                    .map(|l| l.feature_index as usize),
                rotate_config: links
                    .motion
                    .as_ref()
                    .filter(|l| l.feature_type == "Rotate")
                    .map(|l| FeatureTypeConfig {
                        distance: None,
                        scale: l.config.rotate_scale,
                        max_speed: l.config.rotate_max_speed,
                        min_floor: None,
                        use_midpoint: None,
                        method: None,
                    }),
                oscillate_feature: links
                    .motion
                    .as_ref()
                    .filter(|l| l.feature_type == "Oscillate")
                    .map(|l| l.feature_index as usize),
                oscillate_config: links
                    .motion
                    .as_ref()
                    .filter(|l| l.feature_type == "Oscillate")
                    .map(|l| FeatureTypeConfig {
                        distance: None,
                        scale: l.config.oscillate_scale,
                        max_speed: l.config.oscillate_max_speed,
                        min_floor: None,
                        use_midpoint: None,
                        method: None,
                    }),
                constrict_feature: links.constrict.as_ref().map(|l| l.feature_index as usize),
                constrict_config: links.constrict.as_ref().map(|l| {
                    let method = l
                        .config
                        .constrict_method
                        .as_ref()
                        .map(|m| match m.as_str() {
                            "clamp" => ConstrictionMethod::Clamp,
                            _ => ConstrictionMethod::Downsample,
                        })
                        .unwrap_or(ConstrictionMethod::Downsample);
                    FeatureTypeConfig {
                        distance: None,
                        scale: None,
                        max_speed: None,
                        min_floor: l.config.constrict_min_floor,
                        use_midpoint: l.config.constrict_use_midpoint,
                        method: Some(method),
                    }
                }),
            };
            state.set_buttplug_link_config(channel_char, config);
            println!(
                "[update_parameter_source] Also updated Buttplug links for channel {}",
                channel
            );
        }
    }

    Ok(format!(
        "Updated {} {} parameter source",
        channel, parameter
    ))
}

/// Update Buttplug link configuration for a channel
/// This connects the frontend UI configuration to the backend processing pipeline
#[tauri::command]
async fn update_buttplug_links(
    channel: String,
    links: settings::ButtplugLinksSettings,
) -> Result<String, String> {
    use crate::processing::get_processing_state;

    // Convert from settings format to runtime format using the method
    let config = links.to_link_config();

    let channel_char = match channel.as_str() {
        "A" | "a" => 'A',
        "B" | "b" => 'B',
        _ => return Err(format!("Unknown channel: {}", channel)),
    };

    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    state_guard.set_buttplug_link_config(channel_char, config);

    println!(
        "[update_buttplug_links] Updated Buttplug links for channel {}",
        channel
    );
    Ok(format!("Updated Buttplug links for channel {}", channel))
}

// ============================================================================
// Preset Commands
// ============================================================================

#[tauri::command]
async fn get_presets() -> Result<Vec<ChannelPreset>, String> {
    Ok(settings::get_presets().await)
}

#[tauri::command]
async fn save_preset(preset: ChannelPreset) -> Result<String, String> {
    settings::save_preset(preset).await?;
    Ok("Preset saved".to_string())
}

#[tauri::command]
async fn delete_preset(name: String) -> Result<String, String> {
    settings::delete_preset(&name).await?;
    Ok(format!("Preset '{}' deleted", name))
}

#[tauri::command]
async fn rename_preset(old_name: String, new_name: String) -> Result<String, String> {
    settings::rename_preset(&old_name, &new_name).await?;
    Ok(format!(
        "Preset renamed from '{}' to '{}'",
        old_name, new_name
    ))
}

// ============================================================================
// Logging Commands
// ============================================================================

/// Get the path to the log file (for agents/debugging)
#[tauri::command]
fn get_log_path() -> Result<String, String> {
    logging::get_log_path()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Logger not initialized".to_string())
}

/// Read the last N lines from the log file (default 100)
#[tauri::command]
fn read_logs(lines: Option<usize>) -> Result<Vec<String>, String> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let path = logging::get_log_path().ok_or_else(|| "Logger not initialized".to_string())?;

    let file = File::open(&path).map_err(|e| format!("Failed to open log file: {}", e))?;

    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

    let count = lines.unwrap_or(100);
    let start = all_lines.len().saturating_sub(count);

    Ok(all_lines[start..].to_vec())
}

// ============================================================================
// Diagnostic Capture Commands
// ============================================================================

/// Start a diagnostic capture session. Returns the planned output CSV path.
/// `duration_ms` is the auto-stop window (default 30000 if None).
///
/// Async on purpose: `diagnostic::start` schedules the auto-stop with
/// `tokio::spawn`, which requires being called from a tokio runtime
/// context. Sync Tauri commands run on a non-tokio worker and would panic.
#[tauri::command]
async fn start_diagnostic_capture(duration_ms: Option<u64>) -> Result<String, String> {
    let dur = duration_ms.unwrap_or(30_000);
    diagnostic::start(dur).map(|p| p.to_string_lossy().to_string())
}

/// Stop the active diagnostic capture and flush to CSV. Returns the file path.
#[tauri::command]
fn stop_diagnostic_capture() -> Result<String, String> {
    diagnostic::stop().map(|p| p.to_string_lossy().to_string())
}

/// Read-only status of the diagnostic capture system.
#[tauri::command]
fn get_diagnostic_status() -> diagnostic::DiagnosticStatus {
    diagnostic::status()
}

// ============================================================================
// Window Management
// ============================================================================

#[tauri::command]
async fn close_splashscreen(window: tauri::Window) -> Result<(), String> {
    // Get the app handle from the window
    let app = window.app_handle();

    // Close the splashscreen
    if let Some(splash) = app.get_webview_window("splashscreen") {
        splash
            .close()
            .map_err(|e| format!("Failed to close splashscreen: {}", e))?;
    }

    // Show the main window (don't force focus to avoid stealing it during dev rebuilds)
    if let Some(main_window) = app.get_webview_window("main") {
        main_window
            .show()
            .map_err(|e| format!("Failed to show main window: {}", e))?;
    }

    Ok(())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_bluetooth_adapters,
            scan_bluetooth_devices,
            get_discovered_bluetooth_devices,
            start_websocket_server,
            stop_websocket_server,
            get_websocket_status,
            get_channel_intensities,
            get_axis_values,
            update_output_options,
            connect_bluetooth_device,
            disconnect_bluetooth_device,
            send_coyote_command,
            send_device_response,
            get_device_output,
            // State query commands (HMR recovery)
            get_connection_status,
            get_full_state,
            close_splashscreen,
            // Settings commands
            get_app_settings,
            save_app_settings,
            save_channel_settings,
            update_channel_config,
            save_output_settings,
            save_connection_settings,
            get_websocket_port,
            save_bluetooth_settings,
            save_shortcuts,
            save_gamepad_bindings,
            get_gamepad_bindings,
            set_gamepad_engine,
            set_gamepad_stick_sensitivity,
            set_gamepad_button_repeat,
            save_general_settings,
            set_channel_max_intensity,
            // Output pause commands
            get_output_paused,
            set_output_paused,
            toggle_output_paused,
            // Parameter modulation commands
            update_parameter_source,
            update_buttplug_links,
            // Preset commands
            get_presets,
            save_preset,
            delete_preset,
            rename_preset,
            // Logging commands
            get_log_path,
            read_logs,
            // Diagnostic capture commands
            start_diagnostic_capture,
            stop_diagnostic_capture,
            get_diagnostic_status
        ])
        .setup(|app| {
            // Initialize the ring buffer logger
            // Log file will be at <app_dir>/coyote-socket.log
            let app_dir = app.path().app_data_dir().ok();
            logging::init_logger(app_dir);
            // Diagnostic CSV captures live next to the executable so both
            // dev (target/debug) and release builds drop the file in an
            // easy-to-find location. Falls back to cwd if exe path can't
            // be resolved.
            let diag_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            diagnostic::init(diag_dir);
            log_info!("CoyoteSocket starting up");

            // Store the AppHandle for global event emission
            set_app_handle(app.handle().clone());

            // Initialize gamepad bindings from settings + start the configured
            // engine (gilrs / xinput / off). Runs in a detached task so setup
            // stays sync.
            tauri::async_runtime::spawn(async {
                let app_settings = settings::get_settings().await;
                let bindings = app_settings.gamepad_bindings.clone();
                gamepad::init_active_bindings(bindings).await;
                let engine = gamepad::GamepadEngine::from_str(
                    &app_settings.general.gamepad_engine,
                );
                gamepad::set_engine(engine).await;
            });

            // DEV_URL override: when set (e.g. http://localhost:1421), redirect
            // config-created windows to the Vite dev server. Allows release
            // builds run under the dev-server skill's shadow-copy hot-swap to
            // pick up frontend HMR without re-bundling assets.
            if let Ok(dev_url) = std::env::var("DEV_URL") {
                let base = dev_url.trim_end_matches('/').to_string();
                log_info!("DEV_URL override active: {}", base);
                if let Some(main) = app.get_webview_window("main") {
                    if let Ok(url) = base.parse::<tauri::Url>() {
                        let _ = main.navigate(url);
                    }
                }
                if let Some(splash) = app.get_webview_window("splashscreen") {
                    if let Ok(url) = format!("{}/splashscreen.html", base).parse::<tauri::Url>() {
                        let _ = splash.navigate(url);
                    }
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
