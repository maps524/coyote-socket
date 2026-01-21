use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::emit_axis_update;
use crate::processing::{
    get_processing_state, parse_tcode, ChannelInterplay, OutputOptions, ProcessingEngineType,
    WaveformData,
};

/// Update the output options from frontend
pub async fn set_output_options(
    interplay: Option<String>,
    engine: Option<String>,
    chase_delay_ms: Option<u32>,
) {
    let state = get_processing_state().await;
    let mut state_guard = state.write().await;

    let interplay_mode = interplay
        .map(|i| ChannelInterplay::from_str(&i))
        .unwrap_or(state_guard.options.channel_interplay);

    let engine_type = engine
        .map(|e| ProcessingEngineType::from_str(&e))
        .unwrap_or(state_guard.options.processing_engine);

    let delay = chase_delay_ms.unwrap_or(state_guard.options.chase_delay_ms);

    state_guard.set_options(OutputOptions {
        channel_interplay: interplay_mode,
        processing_engine: engine_type,
        chase_delay_ms: delay,
    });
}

/// Detected protocol for reporting to frontend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputProtocol {
    None,
    TCode,
    Buttplug,
}

impl InputProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            InputProtocol::None => "none",
            InputProtocol::TCode => "tcode",
            InputProtocol::Buttplug => "buttplug",
        }
    }
}

/// WebSocket server state
pub struct WebSocketServer {
    pub running: bool,
    shutdown_tx: Option<broadcast::Sender<()>>,
    pub detected_protocol: InputProtocol,
}

impl WebSocketServer {
    pub fn new() -> Self {
        Self {
            running: false,
            shutdown_tx: None,
            detected_protocol: InputProtocol::None,
        }
    }
}

// Global WebSocket server instance
pub static WEBSOCKET_SERVER: tokio::sync::OnceCell<Arc<Mutex<WebSocketServer>>> =
    tokio::sync::OnceCell::const_new();

pub async fn get_websocket_server() -> &'static Arc<Mutex<WebSocketServer>> {
    WEBSOCKET_SERVER
        .get_or_init(|| async { Arc::new(Mutex::new(WebSocketServer::new())) })
        .await
}

/// Start the WebSocket server on the specified port
pub async fn start_server(port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = get_websocket_server().await;
    let mut server_guard = server.lock().await;

    if server_guard.running {
        return Err("WebSocket server is already running".into());
    }

    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    crate::log_info!("WebSocket server listening on: {}", addr);

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    server_guard.shutdown_tx = Some(shutdown_tx.clone());
    server_guard.running = true;
    drop(server_guard);

    // Spawn the server loop
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown_tx.subscribe();

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, addr)) => {
                            crate::log_info!("New WebSocket connection from: {}", addr);
                            let shutdown_rx = shutdown_tx.subscribe();
                            tokio::spawn(handle_connection(stream, addr, shutdown_rx));
                        }
                        Err(e) => {
                            crate::log_error!("Failed to accept connection: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    crate::log_info!("WebSocket server shutting down");
                    break;
                }
            }
        }
    });

    Ok(())
}

/// Stop the WebSocket server
pub async fn stop_server() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = get_websocket_server().await;
    let mut server_guard = server.lock().await;

    if !server_guard.running {
        return Ok(());
    }

    if let Some(tx) = server_guard.shutdown_tx.take() {
        let _ = tx.send(());
    }

    server_guard.running = false;
    crate::log_info!("WebSocket server stopped");
    Ok(())
}

/// Detected protocol for a WebSocket connection
#[derive(Debug, Clone, Copy, PartialEq)]
enum DetectedProtocol {
    Unknown,
    TCode,
    Buttplug,
}

/// Detect protocol based on message content
fn detect_protocol(message: &str) -> DetectedProtocol {
    let trimmed = message.trim();

    // Buttplug messages are JSON arrays starting with '['
    if trimmed.starts_with('[') {
        return DetectedProtocol::Buttplug;
    }

    // T-Code commands start with axis letters: L, R, V, A, or D (for device info)
    if let Some(first_char) = trimmed.chars().next() {
        match first_char {
            'L' | 'R' | 'V' | 'A' | 'D' => return DetectedProtocol::TCode,
            _ => {}
        }
    }

    DetectedProtocol::Unknown
}

/// Handle a single WebSocket connection with protocol auto-detection
async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    shutdown_rx: broadcast::Receiver<()>,
) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("WebSocket handshake failed for {}: {}", addr, e);
            return;
        }
    };

    // Handle connection with auto-detection
    handle_auto_detect_connection(ws_stream, addr, shutdown_rx).await;
}

/// Handle WebSocket connection with protocol auto-detection on first message
async fn handle_auto_detect_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<TcpStream>,
    addr: SocketAddr,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    let (mut write, mut read) = ws_stream.split();
    let mut protocol = DetectedProtocol::Unknown;

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Auto-detect protocol on first text message if not yet determined
                        if protocol == DetectedProtocol::Unknown {
                            protocol = detect_protocol(&text);
                            match protocol {
                                DetectedProtocol::Buttplug => {
                                    set_detected_protocol(InputProtocol::Buttplug).await;
                                }
                                DetectedProtocol::TCode => {
                                    set_detected_protocol(InputProtocol::TCode).await;
                                }
                                DetectedProtocol::Unknown => {
                                    // Default to T-Code for backward compatibility
                                    protocol = DetectedProtocol::TCode;
                                    set_detected_protocol(InputProtocol::TCode).await;
                                }
                            }
                        }

                        // Route message to appropriate handler
                        match protocol {
                            DetectedProtocol::TCode | DetectedProtocol::Unknown => {
                                let response = handle_tcode_message(&text).await;
                                if let Some(resp) = response {
                                    if let Err(e) = write.send(Message::Text(resp)).await {
                                        eprintln!("Failed to send T-Code response: {}", e);
                                        break;
                                    }
                                }
                            }
                            DetectedProtocol::Buttplug => {
                                if let Some(response) = handle_buttplug_text_message(&text).await {
                                    if let Err(e) = write.send(Message::Text(response)).await {
                                        eprintln!("Failed to send Buttplug response: {}", e);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if let Err(e) = write.send(Message::Pong(data)).await {
                            eprintln!("Failed to send pong: {}", e);
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        eprintln!("WebSocket error from {}: {}", addr, e);
                        break;
                    }
                    None => {
                        break;
                    }
                    Some(Ok(Message::Binary(_))) => {
                        // Binary messages not currently handled
                    }
                    _ => {}
                }
            }
            _ = shutdown_rx.recv() => {
                let _ = write.send(Message::Close(None)).await;
                break;
            }
        }
    }

    // Reset detected protocol when connection closes
    set_detected_protocol(InputProtocol::None).await;
}

/// Handle a Buttplug text message and return optional response
async fn handle_buttplug_text_message(text: &str) -> Option<String> {
    use crate::buttplug::handler::handle_buttplug_message;
    use crate::buttplug::messages::{
        parse_buttplug_messages, serialize_buttplug_messages, ButtplugError, ButtplugServerMessage,
    };
    use crate::buttplug::types::ButtplugFeatureConfig;

    let protocol_version = 2; // Support Buttplug v2
    let config = ButtplugFeatureConfig::default(); // TODO: Load from settings

    match parse_buttplug_messages(text) {
        Ok(messages) => {
            let mut responses = Vec::new();

            // Process each message
            for client_msg in messages {
                let msg_responses =
                    handle_buttplug_message(client_msg, &config, protocol_version).await;
                responses.extend(msg_responses);
            }

            // Serialize and return responses
            if !responses.is_empty() {
                match serialize_buttplug_messages(&responses) {
                    Ok(json) => Some(json),
                    Err(e) => {
                        eprintln!("Failed to serialize Buttplug response: {}", e);
                        None
                    }
                }
            } else {
                None
            }
        }
        Err(e) => {
            eprintln!("Failed to parse Buttplug message: {}", e);
            // Return error response
            let error_response = vec![ButtplugServerMessage::Error(ButtplugError::message_error(
                0, e,
            ))];
            serialize_buttplug_messages(&error_response).ok()
        }
    }
}

/// Handle incoming T-Code message and return optional response
async fn handle_tcode_message(message: &str) -> Option<String> {
    let message = message.trim();

    // Handle D commands (device info)
    if message.contains("D0") {
        return Some("v2.0\r\n".to_string());
    } else if message.contains("D1") {
        return Some("T-Code v0.3\r\n".to_string());
    } else if message.contains("D2") {
        let axis_info = "L0 0 9999 Up\n\
                         R0 0 9999 Twist\n\
                         R1 0 9999 Roll\n\
                         R2 0 9999 Pitch\n\
                         V0 0 9999 Vibe1\n\
                         V1 0 9999 Vibe2\n\
                         V2 0 9999 Vibe3\n\
                         V3 0 9999 Vibe4\n\
                         A0 0 9999 Valve\n\
                         A1 0 9999 Suck\n\
                         \r\n";
        return Some(axis_info.to_string());
    } else if message.contains("DSTOP") {
        // Stop all channels
        let state = get_processing_state().await;
        let mut state_guard = state.write().await;
        state_guard.stop();
        return None;
    }

    // Parse T-Code commands
    let commands = parse_tcode(message);
    if !commands.is_empty() {
        let state = get_processing_state().await;
        let mut state_guard = state.write().await;

        for cmd in &commands {
            state_guard.process_command(cmd);
        }

        // Get current intensities and axis values for logging and frontend push
        let (channel_a, channel_b) = state_guard.get_current_intensities();
        // Convert AxisState map to simple f64 values for frontend
        let axes: std::collections::HashMap<String, f64> = state_guard
            .axis_values
            .iter()
            .map(|(k, v)| (k.clone(), v.value))
            .collect();
        drop(state_guard); // Release lock before async call

        // Push axis update to frontend immediately
        emit_axis_update(axes, channel_a, channel_b);
    }

    None
}

/// Get the next waveform data for both channels (called at 10Hz by device.rs)
pub async fn get_next_waveform_data() -> (WaveformData, WaveformData) {
    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    state_guard.get_next_waveform_data()
}

/// Get the current intensity values (for UI display, returns normalized 0.0-1.0)
pub async fn get_current_intensities() -> (f64, f64) {
    let state = get_processing_state().await;
    let state_guard = state.read().await;
    state_guard.get_current_intensities()
}

/// Resolved channel parameters for device output
#[derive(Debug, Clone)]
#[allow(dead_code)] // freq_balance and int_balance reserved for BF command support
pub struct ResolvedChannelParams {
    pub frequency: f64,   // Hz (1-200)
    pub freq_balance: u8, // 0-255
    pub int_balance: u8,  // 0-255
    pub range_min: u8,    // 0-200 (only used for linked intensity)
    pub range_max: u8,    // 0-200 (only used for linked intensity)
    pub intensity_is_static: bool,
}

/// Get resolved channel parameters (handles both static and linked sources)
/// This resolves frequency, freqBalance, and intBalance from their configured sources
pub async fn get_resolved_channel_params() -> (ResolvedChannelParams, ResolvedChannelParams) {
    use crate::modulation::{resolve_parameter, ParameterSourceType};
    use crate::processing::current_time_ms;

    let state = get_processing_state().await;
    let state_guard = state.read().await;
    let now = current_time_ms();

    // Resolve Channel A parameters
    let freq_a = resolve_parameter(
        &state_guard.channel_a_config.frequency,
        &state_guard.axis_values,
        &state_guard.no_input_behavior,
        now,
        state_guard.no_input_decay_ms,
    );
    let freq_bal_a = resolve_parameter(
        &state_guard.channel_a_config.frequency_balance,
        &state_guard.axis_values,
        &state_guard.no_input_behavior,
        now,
        state_guard.no_input_decay_ms,
    );
    let int_bal_a = resolve_parameter(
        &state_guard.channel_a_config.intensity_balance,
        &state_guard.axis_values,
        &state_guard.no_input_behavior,
        now,
        state_guard.no_input_decay_ms,
    );
    let a_intensity_static =
        state_guard.channel_a_config.intensity.source_type == ParameterSourceType::Static;
    let range_min_a = state_guard.channel_a_config.intensity.range_min;
    let range_max_a = state_guard.channel_a_config.intensity.range_max;

    // Resolve Channel B parameters
    let freq_b = resolve_parameter(
        &state_guard.channel_b_config.frequency,
        &state_guard.axis_values,
        &state_guard.no_input_behavior,
        now,
        state_guard.no_input_decay_ms,
    );
    let freq_bal_b = resolve_parameter(
        &state_guard.channel_b_config.frequency_balance,
        &state_guard.axis_values,
        &state_guard.no_input_behavior,
        now,
        state_guard.no_input_decay_ms,
    );
    let int_bal_b = resolve_parameter(
        &state_guard.channel_b_config.intensity_balance,
        &state_guard.axis_values,
        &state_guard.no_input_behavior,
        now,
        state_guard.no_input_decay_ms,
    );
    let b_intensity_static =
        state_guard.channel_b_config.intensity.source_type == ParameterSourceType::Static;
    let range_min_b = state_guard.channel_b_config.intensity.range_min;
    let range_max_b = state_guard.channel_b_config.intensity.range_max;

    let params_a = ResolvedChannelParams {
        frequency: freq_a.clamp(1.0, 200.0),
        freq_balance: (freq_bal_a.clamp(0.0, 255.0) as u8),
        int_balance: (int_bal_a.clamp(0.0, 255.0) as u8),
        range_min: range_min_a as u8,
        range_max: range_max_a as u8,
        intensity_is_static: a_intensity_static,
    };

    let params_b = ResolvedChannelParams {
        frequency: freq_b.clamp(1.0, 200.0),
        freq_balance: (freq_bal_b.clamp(0.0, 255.0) as u8),
        int_balance: (int_bal_b.clamp(0.0, 255.0) as u8),
        range_min: range_min_b as u8,
        range_max: range_max_b as u8,
        intensity_is_static: b_intensity_static,
    };

    (params_a, params_b)
}

/// Get all axis values with no-input behavior applied
/// If an axis hasn't received data for over 1 second, apply the configured behavior
pub async fn get_axis_values_from_processing() -> std::collections::HashMap<String, f64> {
    use crate::modulation::NoInputBehavior;
    use crate::processing::current_time_ms;

    let state = get_processing_state().await;
    let state_guard = state.read().await;
    let now = current_time_ms();
    let stale_threshold_ms = 1000u64; // 1 second
    let decay_ms = state_guard.no_input_decay_ms as u64;

    state_guard
        .axis_values
        .iter()
        .map(|(k, v)| {
            let age_ms = now.saturating_sub(v.timestamp);

            let value = if age_ms > stale_threshold_ms {
                // Data is stale, apply no_input_behavior
                match state_guard.no_input_behavior {
                    NoInputBehavior::Hold => v.value,
                    NoInputBehavior::Default | NoInputBehavior::Zero => 0.0,
                    NoInputBehavior::Decay => {
                        // Linear decay from current value to 0 over decay_ms
                        let decay_progress =
                            ((age_ms - stale_threshold_ms) as f64 / decay_ms as f64).min(1.0);
                        v.value * (1.0 - decay_progress)
                    }
                }
            } else {
                v.value
            };

            (k.clone(), value)
        })
        .collect()
}

/// Check if the server is running
pub async fn is_server_running() -> bool {
    let server = get_websocket_server().await;
    let server_guard = server.lock().await;
    server_guard.running
}

/// Get the currently detected input protocol
pub async fn get_detected_protocol() -> InputProtocol {
    let server = get_websocket_server().await;
    let server_guard = server.lock().await;
    server_guard.detected_protocol
}

/// Set the detected input protocol (called when protocol is auto-detected)
pub async fn set_detected_protocol(protocol: InputProtocol) {
    let server = get_websocket_server().await;
    let mut server_guard = server.lock().await;
    server_guard.detected_protocol = protocol;
}

/// Convert a ParameterSourceSettings (from settings) to ParameterSource (for runtime)
fn convert_parameter_source(
    source: &crate::settings::ParameterSourceSettings,
) -> crate::modulation::ParameterSource {
    use crate::modulation::{CurveType, ParameterSource, ParameterSourceType};
    use crate::settings::ParameterSourceType as SettingsSourceType;

    let curve = match source.curve.as_str() {
        "exponential" => CurveType::Exponential,
        "logarithmic" => CurveType::Logarithmic,
        "s-curve" => CurveType::SCurve,
        "inverse" => CurveType::Inverse,
        _ => CurveType::Linear,
    };

    match source.source_type {
        SettingsSourceType::Static => ParameterSource {
            source_type: ParameterSourceType::Static,
            static_value: Some(source.static_value),
            source_axis: None,
            range_min: source.range_min,
            range_max: source.range_max,
            curve,
            curve_strength: Some(source.curve_strength),
            midpoint: if source.midpoint { Some(true) } else { None },
            buttplug_links: source.buttplug_links.clone(),
        },
        SettingsSourceType::Linked => ParameterSource {
            source_type: ParameterSourceType::Linked,
            static_value: Some(source.static_value), // Keep as fallback
            source_axis: Some(source.source_axis.clone()),
            range_min: source.range_min,
            range_max: source.range_max,
            curve,
            curve_strength: Some(source.curve_strength),
            midpoint: if source.midpoint { Some(true) } else { None },
            buttplug_links: source.buttplug_links.clone(),
        },
    }
}

/// Convert ChannelSettings (from settings) to ChannelConfig (for runtime)
fn convert_channel_settings(
    settings: &crate::settings::ChannelSettings,
) -> crate::modulation::ChannelConfig {
    use crate::modulation::ChannelConfig;

    ChannelConfig {
        frequency: convert_parameter_source(&settings.frequency_source),
        frequency_balance: convert_parameter_source(&settings.frequency_balance_source),
        intensity_balance: convert_parameter_source(&settings.intensity_balance_source),
        intensity: convert_parameter_source(&settings.intensity_source),
    }
}

/// Apply saved settings to the running ProcessingState
/// Call this when the server starts to restore user preferences
pub async fn apply_saved_settings_to_processing() {
    use crate::modulation::NoInputBehavior;
    use crate::settings;

    // Get saved settings
    let all_settings = settings::get_settings().await;
    let saved = all_settings.general;

    // Parse no_input_behavior string to enum
    let behavior = match saved.no_input_behavior.as_str() {
        "hold" => NoInputBehavior::Hold,
        "default" => NoInputBehavior::Default,
        "decay" => NoInputBehavior::Decay,
        "zero" => NoInputBehavior::Zero,
        _ => NoInputBehavior::Hold,
    };

    // Convert channel settings to runtime configs
    let channel_a_config = convert_channel_settings(&all_settings.channel_a);
    let channel_b_config = convert_channel_settings(&all_settings.channel_b);

    // Apply to ProcessingState
    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    state_guard.no_input_behavior = behavior;
    state_guard.no_input_decay_ms = saved.no_input_decay_ms;
    state_guard.channel_a_config = channel_a_config;
    state_guard.channel_b_config = channel_b_config;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tcode_simple() {
        let commands = parse_tcode("L0500");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].axis, "L0");
        assert!((commands[0].value - 0.5).abs() < 0.01);
        assert!(commands[0].interval_ms.is_none());
    }

    #[test]
    fn test_parse_tcode_with_interval() {
        let commands = parse_tcode("R2750I1000");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].axis, "R2");
        assert!((commands[0].value - 0.25).abs() < 0.01); // 750/1000 = 0.75, inverted = 0.25
        assert_eq!(commands[0].interval_ms, Some(1000));
    }

    #[test]
    fn test_parse_tcode_multiple() {
        let commands = parse_tcode("L0500 R2250");
        assert_eq!(commands.len(), 2);
    }
}
