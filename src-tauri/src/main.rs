// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter, Manager};

mod bluetooth;
mod buttplug;
mod device;
mod logging;
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

/// Full connection status for HMR recovery
#[derive(Clone, Serialize)]
pub struct ConnectionStatus {
    pub websocket_running: bool,
    pub detected_input_protocol: String,  // "none", "tcode", or "buttplug"
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
    pub channel_interplay: String,
    pub chase_delay_ms: u32,
}

/// Emit axis values to the frontend (called from device loop at 10Hz)
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
pub fn emit_connection_changed(connection_type: &str, connected: bool, device_address: Option<String>) {
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

        let payload = OutputPauseChangedPayload {
            paused,
            timestamp,
        };

        let _ = handle.emit("output-pause-changed", payload);
    }
}

/// Emit waveform sample to frontend (called from device loop at 10Hz when sending to device)
pub fn emit_waveform_sample(sample: waveform::WaveformSample) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit("waveform-sample", sample);
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
use device::{start_device_loop, stop_device_loop, update_channel_a_params, update_channel_b_params, get_channel_a_params, get_channel_b_params, ChannelParams, get_last_device_output, DeviceOutput};
use settings::{
    AppSettings, ChannelSettings as SettingsChannelSettings, ConnectionSettings,
    BluetoothSettings, OutputSettings, KeyboardShortcuts, GeneralSettings,
    ChannelPreset,
};
use waveform::{get_waveform_data, get_channel_state};
use websocket::{start_server, stop_server, is_server_running, get_current_intensities, set_output_options, apply_saved_settings_to_processing};
use modulation::ParameterSource;

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
                Err(e) => Err(format!("Failed to get adapters: {}", e))
            }
        }
        Err(e) => Err(format!("Failed to initialize Bluetooth: {}", e))
    }
}

#[tauri::command]
async fn scan_bluetooth_devices(adapter_index: usize) -> Result<Vec<BluetoothDevice>, String> {
    match get_bluetooth_manager().await {
        Ok(manager) => {
            let mut manager = manager.lock().await;
            match manager.scan_devices(adapter_index).await {
                Ok(devices) => Ok(devices.into_iter().map(|d| BluetoothDevice {
                    address: d.address,
                    name: d.name,
                    rssi: d.rssi,
                }).collect()),
                Err(e) => Err(format!("Failed to scan devices: {}", e))
            }
        }
        Err(e) => Err(format!("Failed to initialize Bluetooth: {}", e))
    }
}

/// Get the list of discovered Bluetooth devices from the last scan (no new scan)
#[tauri::command]
async fn get_discovered_bluetooth_devices() -> Result<Vec<BluetoothDevice>, String> {
    match get_bluetooth_manager().await {
        Ok(manager) => {
            let manager = manager.lock().await;
            let devices = manager.get_discovered_devices();
            Ok(devices.into_iter().map(|d| BluetoothDevice {
                address: d.address,
                name: d.name,
                rssi: d.rssi,
            }).collect())
        }
        Err(e) => Err(format!("Failed to get Bluetooth manager: {}", e))
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
        },
        Err(e) => Err(format!("Failed to start WebSocket server: {}", e))
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
        },
        Err(e) => Err(format!("Failed to stop WebSocket server: {}", e))
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
async fn get_buttplug_features() -> Result<HashMap<String, f64>, String> {
    use crate::processing::get_processing_state;
    let state = get_processing_state().await;
    let state_guard = state.read().await;
    Ok(state_guard.get_buttplug_features())
}

#[tauri::command]
async fn update_output_options(interplay: Option<String>, engine: Option<String>, chase_delay_ms: Option<u32>) -> Result<String, String> {
    set_output_options(interplay, engine, chase_delay_ms).await;
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
                    // Start the 10Hz device update loop
                    start_device_loop().await;

                    // Emit connection changed event for stateless frontend
                    emit_connection_changed("bluetooth", true, Some(address.clone()));

                    let battery_info = battery_level
                        .map(|l| format!(" (Battery: {}%)", l))
                        .unwrap_or_default();
                    Ok(format!("Connected to Bluetooth device: {}{}", address, battery_info))
                }
                Err(e) => Err(format!("Failed to connect: {}", e))
            }
        }
        Err(e) => Err(format!("Failed to initialize Bluetooth: {}", e))
    }
}

#[tauri::command]
async fn disconnect_bluetooth_device() -> Result<String, String> {
    // Stop the device loop first
    stop_device_loop().await;

    match get_bluetooth_manager().await {
        Ok(manager) => {
            let mut manager = manager.lock().await;
            match manager.disconnect_device().await {
                Ok(_) => {
                    // Emit connection changed event for stateless frontend
                    emit_connection_changed("bluetooth", false, None);
                    Ok("Disconnected from Bluetooth device".to_string())
                },
                Err(e) => Err(format!("Failed to disconnect: {}", e))
            }
        }
        Err(e) => Err(format!("Failed to initialize Bluetooth: {}", e))
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
                Err(e) => Err(format!("Failed to send command: {}", e))
            }
        }
        Err(e) => Err(format!("Bluetooth not available: {}", e))
    }
}

#[tauri::command]
async fn send_device_response(response: String) -> Result<String, String> {
    Ok(format!("Sent response: {}", response))
}

#[tauri::command]
async fn update_channel_params(
    channel: String,
    frequency: f64,
    freq_balance: u8,
    int_balance: u8,
    range_min: u8,
    range_max: u8,
) -> Result<String, String> {
    let params = ChannelParams {
        frequency,
        freq_balance,
        int_balance,
        range_min,
        range_max,
    };

    match channel.as_str() {
        "A" | "a" => {
            update_channel_a_params(params).await;
            Ok("Updated Channel A parameters".to_string())
        }
        "B" | "b" => {
            update_channel_b_params(params).await;
            Ok("Updated Channel B parameters".to_string())
        }
        _ => Err(format!("Unknown channel: {}", channel))
    }
}

#[tauri::command]
async fn get_battery_level() -> Result<u8, String> {
    match get_bluetooth_manager().await {
        Ok(manager) => {
            let manager = manager.lock().await;
            if !manager.is_connected() {
                return Ok(0); // Return 0 if not connected
            }
            match manager.read_battery().await {
                Ok(level) => Ok(level),
                Err(e) => {
                    println!("Failed to read battery: {}", e);
                    Ok(0)
                }
            }
        }
        Err(_) => Ok(0)
    }
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
            let devices = manager.get_discovered_devices()
                .into_iter()
                .map(|d| BluetoothDevice {
                    address: d.address,
                    name: d.name,
                    rssi: d.rssi,
                })
                .collect();
            (connected, address, devices)
        }
        Err(_) => (false, None, Vec::new())
    };

    let battery = if bt_connected {
        match get_bluetooth_manager().await {
            Ok(manager) => {
                let manager = manager.lock().await;
                manager.read_battery().await.ok()
            }
            Err(_) => None
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
    use crate::processing::{get_processing_state, ProcessingEngineType, ChannelInterplay};

    // Get connection status
    let connection = get_connection_status().await?;

    // Get channel params from device state
    let params_a = get_channel_a_params().await;
    let params_b = get_channel_b_params().await;

    // Get current intensities
    let (intensity_a, intensity_b) = get_current_intensities().await;

    // Get output options from processing state
    let (engine_str, interplay_str, chase_delay) = {
        let state = get_processing_state().await;
        let state_guard = state.read().await;
        let engine = match state_guard.options.processing_engine {
            ProcessingEngineType::V1 => "v1",
            ProcessingEngineType::V2Smooth => "v2-smooth",
            ProcessingEngineType::V2Balanced => "v2-balanced",
            ProcessingEngineType::V2Detailed => "v2-detailed",
            ProcessingEngineType::V2Dynamic => "v2-dynamic",
            ProcessingEngineType::V3Predictive => "v3-predictive",
        };
        let interplay = match state_guard.options.channel_interplay {
            ChannelInterplay::None => "none",
            ChannelInterplay::Mirror => "mirror",
            ChannelInterplay::MirrorInverted => "mirror-inverted",
            ChannelInterplay::Chase => "chase",
            ChannelInterplay::ChaseInverted => "chase-inverted",
            ChannelInterplay::Alternating => "alternating",
        };
        (engine.to_string(), interplay.to_string(), state_guard.options.chase_delay_ms)
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    Ok(FullAppState {
        connection,
        channel_a: ChannelStateSnapshot {
            frequency: params_a.frequency,
            freq_balance: params_a.freq_balance,
            int_balance: params_a.int_balance,
            range_min: params_a.range_min,
            range_max: params_a.range_max,
            current_intensity: intensity_a,
        },
        channel_b: ChannelStateSnapshot {
            frequency: params_b.frequency,
            freq_balance: params_b.freq_balance,
            int_balance: params_b.int_balance,
            range_min: params_b.range_min,
            range_max: params_b.range_max,
            current_intensity: intensity_b,
        },
        output_options: OutputOptionsSnapshot {
            processing_engine: engine_str,
            channel_interplay: interplay_str,
            chase_delay_ms: chase_delay,
        },
        timestamp,
    })
}

// ============================================================================
// Settings Commands
// ============================================================================

#[tauri::command]
async fn get_app_settings() -> Result<AppSettings, String> {
    let settings = settings::get_settings().await;

    // Sync Buttplug link configs to processing state on settings load
    let state = processing::get_processing_state().await;
    let mut state_guard = state.write().await;

    // Sync Channel A buttplug links
    if let Some(ref bp_links) = settings.channel_a.intensity_source.buttplug_links {
        state_guard.set_buttplug_link_config('A', bp_links.to_link_config());
    }

    // Sync Channel B buttplug links
    if let Some(ref bp_links) = settings.channel_b.intensity_source.buttplug_links {
        state_guard.set_buttplug_link_config('B', bp_links.to_link_config());
    }

    drop(state_guard);
    Ok(settings)
}

#[tauri::command]
async fn save_app_settings(settings: AppSettings) -> Result<String, String> {
    settings::update_settings(settings).await?;
    Ok("Settings saved".to_string())
}

#[tauri::command]
async fn save_channel_settings(
    channel: String,
    channel_settings: SettingsChannelSettings,
) -> Result<String, String> {
    // Extract current values for device params based on parameter source types
    let frequency = if channel_settings.frequency_source.source_type == settings::ParameterSourceType::Static {
        channel_settings.frequency_source.static_value
    } else {
        // When linked, use a reasonable default or middle of range
        (channel_settings.frequency_source.range_min + channel_settings.frequency_source.range_max) / 2.0
    };

    let freq_balance = if channel_settings.frequency_balance_source.source_type == settings::ParameterSourceType::Static {
        channel_settings.frequency_balance_source.static_value as u8
    } else {
        128
    };

    let int_balance = if channel_settings.intensity_balance_source.source_type == settings::ParameterSourceType::Static {
        channel_settings.intensity_balance_source.static_value as u8
    } else {
        128
    };

    // Intensity range comes from intensity source
    let range_min = channel_settings.intensity_source.range_min as u8;
    let range_max = channel_settings.intensity_source.range_max as u8;

    // Update device parameters so they take effect immediately
    let params = ChannelParams {
        frequency,
        freq_balance,
        int_balance,
        range_min,
        range_max,
    };

    match channel.as_str() {
        "A" | "a" => {
            // Sync Buttplug link config to processing state if present
            if let Some(ref bp_links) = channel_settings.intensity_source.buttplug_links {
                let bp_config = bp_links.to_link_config();
                let state = processing::get_processing_state().await;
                let mut state_guard = state.write().await;
                state_guard.set_buttplug_link_config('A', bp_config);
            }
            settings::update_channel_a(channel_settings).await?;
            update_channel_a_params(params).await;
        }
        "B" | "b" => {
            // Sync Buttplug link config to processing state if present
            if let Some(ref bp_links) = channel_settings.intensity_source.buttplug_links {
                let bp_config = bp_links.to_link_config();
                let state = processing::get_processing_state().await;
                let mut state_guard = state.write().await;
                state_guard.set_buttplug_link_config('B', bp_config);
            }
            settings::update_channel_b(channel_settings).await?;
            update_channel_b_params(params).await;
        }
        _ => return Err(format!("Unknown channel: {}", channel)),
    }
    Ok("Channel settings saved".to_string())
}

#[tauri::command]
async fn save_output_settings(
    channel_interplay: String,
    processing_engine: String,
    chase_delay_ms: Option<u32>,
) -> Result<String, String> {
    let delay = chase_delay_ms.unwrap_or(100);

    // Update processing options immediately
    set_output_options(
        Some(channel_interplay.clone()),
        Some(processing_engine.clone()),
        Some(delay),
    ).await;

    // Persist to settings file
    let output_settings = OutputSettings {
        channel_interplay: processing::ChannelInterplay::from_str(&channel_interplay),
        processing_engine: processing::ProcessingEngineType::from_str(&processing_engine),
        chase_delay_ms: delay,
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
async fn save_general_settings(
    no_input_behavior: String,
    no_input_decay_ms: u32,
    update_rate_ms: u32,
    save_rate_ms: u32,
    show_tcode_monitor: bool,
    processing_engine: String,
) -> Result<String, String> {
    use crate::modulation::NoInputBehavior;
    use crate::processing::{ProcessingEngineType, get_processing_state};

    let engine = match processing_engine.as_str() {
        "v1" => ProcessingEngineType::V1,
        "v2-smooth" => ProcessingEngineType::V2Smooth,
        "v2-balanced" => ProcessingEngineType::V2Balanced,
        "v2-detailed" => ProcessingEngineType::V2Detailed,
        "v2-dynamic" => ProcessingEngineType::V2Dynamic,
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

    // Get current output_paused state to preserve it
    let current_output_paused = settings::get_output_paused().await;

    let general_settings = GeneralSettings {
        no_input_behavior,
        no_input_decay_ms,
        update_rate_ms,
        save_rate_ms,
        show_tcode_monitor,
        processing_engine: engine,
        output_paused: current_output_paused,
    };
    settings::update_general(general_settings).await?;
    Ok("General settings saved".to_string())
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
    use crate::processing::get_processing_state;
    use crate::buttplug::{ButtplugLinkConfig, FeatureTypeConfig, ConstrictionMethod};

    println!("[update_parameter_source] channel={}, parameter={}, source_type={:?}, source_axis={:?}, has_buttplug_links={:?}",
        channel, parameter, source.source_type, source.source_axis, source.buttplug_links.is_some());

    // Extract buttplug links before moving source
    let buttplug_links = source.buttplug_links.clone();

    let state = get_processing_state().await;
    let mut state = state.write().await;

    let channel_char = match channel.as_str() {
        "A" | "a" => 'A',
        "B" | "b" => 'B',
        _ => return Err(format!("Unknown channel: {}", channel)),
    };

    match (channel.as_str(), parameter.as_str()) {
        ("A" | "a", "intensity") => {
            state.channel_a_config.intensity = source;
        }
        ("A" | "a", "frequency") => {
            state.channel_a_config.frequency = source;
        }
        ("A" | "a", "frequency_balance") => {
            state.channel_a_config.frequency_balance = source;
        }
        ("A" | "a", "intensity_balance") => {
            state.channel_a_config.intensity_balance = source;
        }
        ("B" | "b", "intensity") => {
            state.channel_b_config.intensity = source;
        }
        ("B" | "b", "frequency") => {
            state.channel_b_config.frequency = source;
        }
        ("B" | "b", "frequency_balance") => {
            state.channel_b_config.frequency_balance = source;
        }
        ("B" | "b", "intensity_balance") => {
            state.channel_b_config.intensity_balance = source;
        }
        _ => return Err(format!("Unknown channel/parameter: {}/{}", channel, parameter)),
    }

    // Also update buttplug link config if present (for intensity parameter)
    if parameter == "intensity" {
        if let Some(links) = buttplug_links {
            // Determine position feature type - Position vs PositionWithDuration
            let (position_feature, pos_dur_feature) = match links.position.as_ref() {
                Some(pos) if pos.feature_type == "PositionWithDuration" => {
                    (None, Some(pos.feature_index as usize))
                }
                Some(pos) => {
                    (Some(pos.feature_index as usize), None)
                }
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
                rotate_feature: links.motion.as_ref()
                    .filter(|l| l.feature_type == "Rotate")
                    .map(|l| l.feature_index as usize),
                rotate_config: links.motion.as_ref()
                    .filter(|l| l.feature_type == "Rotate")
                    .map(|l| FeatureTypeConfig {
                        distance: None,
                        scale: l.config.rotate_scale,
                        max_speed: l.config.rotate_max_speed,
                        min_floor: None,
                        use_midpoint: None,
                        method: None,
                    }),
                oscillate_feature: links.motion.as_ref()
                    .filter(|l| l.feature_type == "Oscillate")
                    .map(|l| l.feature_index as usize),
                oscillate_config: links.motion.as_ref()
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
                    let method = l.config.constrict_method.as_ref()
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
            println!("[update_parameter_source] Also updated Buttplug links for channel {}", channel);
        }
    }

    Ok(format!("Updated {} {} parameter source", channel, parameter))
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

    println!("[update_buttplug_links] Updated Buttplug links for channel {}", channel);
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
    Ok(format!("Preset renamed from '{}' to '{}'", old_name, new_name))
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

    let path = logging::get_log_path()
        .ok_or_else(|| "Logger not initialized".to_string())?;

    let file = File::open(&path)
        .map_err(|e| format!("Failed to open log file: {}", e))?;

    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines()
        .filter_map(|l| l.ok())
        .collect();

    let count = lines.unwrap_or(100);
    let start = all_lines.len().saturating_sub(count);

    Ok(all_lines[start..].to_vec())
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
        splash.close().map_err(|e| format!("Failed to close splashscreen: {}", e))?;
    }

    // Show the main window (don't force focus to avoid stealing it during dev rebuilds)
    if let Some(main_window) = app.get_webview_window("main") {
        main_window.show().map_err(|e| format!("Failed to show main window: {}", e))?;
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
            get_buttplug_features,
            update_output_options,
            connect_bluetooth_device,
            disconnect_bluetooth_device,
            send_coyote_command,
            send_device_response,
            update_channel_params,
            get_battery_level,
            get_device_output,
            // State query commands (HMR recovery)
            get_connection_status,
            get_full_state,
            close_splashscreen,
            // Settings commands
            get_app_settings,
            save_app_settings,
            save_channel_settings,
            save_output_settings,
            save_connection_settings,
            get_websocket_port,
            save_bluetooth_settings,
            save_shortcuts,
            save_general_settings,
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
            // Waveform commands
            get_waveform_data,
            get_channel_state,
            // Logging commands
            get_log_path,
            read_logs
        ])
        .setup(|app| {
            // Initialize the ring buffer logger
            // Log file will be at <app_dir>/coyote-socket.log
            let app_dir = app.path().app_data_dir().ok();
            logging::init_logger(app_dir);
            log_info!("CoyoteSocket starting up");

            // Store the AppHandle for global event emission
            set_app_handle(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}