use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::emit_axis_update;
use crate::processing::{
    get_processing_state, parse_tcode, ChannelId, OutputOptions, PeakFillStrategy,
    ProcessingEngineType, WaveformData,
};

/// Update the output options from frontend
pub async fn set_output_options(engine: Option<String>, peak_fill: Option<String>) {
    let state = get_processing_state().await;
    let mut state_guard = state.write().await;

    let engine_type = engine
        .map(|e| ProcessingEngineType::from_str(&e))
        .unwrap_or(state_guard.options.processing_engine);

    let fill = peak_fill
        .map(|s| PeakFillStrategy::from_str(&s))
        .unwrap_or(state_guard.options.peak_fill);

    state_guard.set_options(OutputOptions {
        processing_engine: engine_type,
        peak_fill: fill,
    });
}

/// Detected protocol for reporting to frontend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputProtocol {
    None,
    TCode,
    Buttplug,
    Lovense,
}

impl InputProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            InputProtocol::None => "none",
            InputProtocol::TCode => "tcode",
            InputProtocol::Buttplug => "buttplug",
            InputProtocol::Lovense => "lovense",
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

    // Bind to 0.0.0.0 so LAN clients (e.g. a game on the same machine reaching
    // us via the host's 192.168.x.x address, or the Lovense Remote app's
    // *-lovense.club wildcard DNS that resolves to the LAN IP) can connect.
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    crate::log_info!("WebSocket server listening on: {} (all interfaces)", addr);
    eprintln!("[net] listening on {} (all interfaces)", addr);

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
                            eprintln!("[net] accept from {}", addr);
                            crate::log_info!("Accepted TCP connection from: {}", addr);
                            let shutdown_rx = shutdown_tx.subscribe();
                            tokio::spawn(handle_connection(stream, addr, shutdown_rx));
                        }
                        Err(e) => {
                            eprintln!("[net] accept error: {}", e);
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

/// Handle a single connection. Peeks the first bytes to distinguish a
/// WebSocket upgrade (T-Code / Buttplug) from a plain HTTP request (Lovense
/// Standard API). The peek does not consume bytes, so the WebSocket handshake
/// re-reads the same buffer when we hand the stream to `accept_async`.
async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    shutdown_rx: broadcast::Receiver<()>,
) {
    let mut peek_buf = vec![0u8; 1024];
    let n = match peek_request_head(&stream, &mut peek_buf).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[net] {} peek failed: {}", addr, e);
            crate::log_warn!("peek failed for {}: {}", addr, e);
            return;
        }
    };

    let preview_full = String::from_utf8_lossy(&peek_buf[..n]);
    let preview_head: String = preview_full
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect();
    let is_ws = crate::lovense::is_websocket_upgrade(&peek_buf[..n]);
    eprintln!(
        "[net] {} peeked {} bytes — first line: {:?} → route={}",
        addr,
        n,
        preview_head,
        if is_ws { "websocket" } else { "http" }
    );
    crate::log_info!(
        "Peeked {} bytes from {}: '{}' → {}",
        n,
        addr,
        preview_head,
        if is_ws { "websocket" } else { "http" }
    );
    crate::logging::flush_now();

    if is_ws {
        let ws_stream = match accept_async(stream).await {
            Ok(ws) => ws,
            Err(e) => {
                eprintln!("[net] {} WS handshake failed: {}", addr, e);
                crate::log_warn!("WS handshake failed for {}: {}", addr, e);
                return;
            }
        };
        handle_auto_detect_connection(ws_stream, addr, shutdown_rx).await;
    } else {
        crate::lovense::handle_http_connection(stream, addr).await;
    }
}

/// Peek bytes off the TCP stream until we have either the end of the HTTP
/// header block (`\r\n\r\n`) or the buffer is full. peek() does not advance
/// the read pointer, so the bytes remain available to the next consumer.
async fn peek_request_head(stream: &TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::time::Duration;

    // Bound the wait so a connection that never sends anything doesn't hang
    // a worker forever. 5s mirrors typical WS / HTTP client timeouts.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_len = 0usize;

    loop {
        // peek waits for at least one byte to be available, then returns
        // however much is currently in the kernel buffer.
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(last_len);
        }
        let remaining = deadline - now;
        let n = match tokio::time::timeout(remaining, stream.peek(buf)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Ok(last_len),
        };
        if n == 0 {
            return Ok(last_len);
        }
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") || n == buf.len() {
            return Ok(n);
        }
        if n == last_len {
            // No new bytes arrived since the last call; back off briefly so
            // we don't busy-spin while the client is mid-send.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        last_len = n;
    }
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
            crate::diagnostic::record_input(&cmd.axis, cmd.value, cmd.interval_ms);
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
    use crate::processing::{current_time_ms, ChannelId};

    let state = get_processing_state().await;
    let state_guard = state.read().await;
    let now = current_time_ms();

    // Resolve a single channel's parameters — identical logic for A and B, so
    // we close over `state_guard` once and call it per channel.
    let resolve_one = |id: ChannelId| -> ResolvedChannelParams {
        let ch = state_guard.channel(id);
        let freq = resolve_parameter(
            &ch.config.frequency,
            &state_guard.axis_values,
            &state_guard.no_input_behavior,
            now,
            state_guard.no_input_decay_ms,
        );
        let freq_bal = resolve_parameter(
            &ch.config.frequency_balance,
            &state_guard.axis_values,
            &state_guard.no_input_behavior,
            now,
            state_guard.no_input_decay_ms,
        );
        let int_bal = resolve_parameter(
            &ch.config.intensity_balance,
            &state_guard.axis_values,
            &state_guard.no_input_behavior,
            now,
            state_guard.no_input_decay_ms,
        );
        let intensity_is_static = ch.config.intensity.source_type == ParameterSourceType::Static;
        ResolvedChannelParams {
            frequency: freq.clamp(1.0, 200.0),
            freq_balance: freq_bal.clamp(0.0, 255.0) as u8,
            int_balance: int_bal.clamp(0.0, 255.0) as u8,
            range_min: ch.config.intensity.range_min as u8,
            range_max: ch.config.intensity.range_max as u8,
            intensity_is_static,
        }
    };

    let params_a = resolve_one(ChannelId::A);
    let params_b = resolve_one(ChannelId::B);

    (params_a, params_b)
}

/// Resolve per-slot frequencies (Hz) for both channels at 25ms intervals
/// inside the current 100ms window. Each slot walks the axis history at
/// `window_start + slot*25` so fast axis motion produces true sub-100ms
/// frequency sweeps instead of four copies of the same value.
///
/// Returns `(chan_a_hz, chan_b_hz)` with 4 entries each, already clamped to
/// the protocol range 1-200 Hz. Callers feed these through
/// `frequency_to_period` → `convert_period` for the device command.
pub async fn get_per_slot_frequencies(window_start: u64) -> ([f64; 4], [f64; 4]) {
    use crate::modulation::resolve_parameter_at_time;
    use crate::processing::current_time_ms;

    let state = get_processing_state().await;
    let state_guard = state.read().await;
    let now = current_time_ms();

    let resolve_slots = |id: ChannelId| -> [f64; 4] {
        let src = &state_guard.channel(id).config.frequency;
        std::array::from_fn(|i| {
            let target = window_start + (i as u64) * 25;
            resolve_parameter_at_time(
                src,
                &state_guard.axis_values,
                &state_guard.no_input_behavior,
                now,
                state_guard.no_input_decay_ms,
                target,
            )
            .clamp(1.0, 200.0)
        })
    };

    (resolve_slots(ChannelId::A), resolve_slots(ChannelId::B))
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
pub fn convert_parameter_source(
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
            curve: curve.clone(),
            curve_strength: Some(source.curve_strength),
            midpoint: if source.midpoint { Some(true) } else { None },
            delay_ms: if source.delay_enabled { Some(source.delay_ms) } else { None },
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
            delay_ms: if source.delay_enabled { Some(source.delay_ms) } else { None },
            buttplug_links: source.buttplug_links.clone(),
        },
    }
}

/// Convert ChannelSettings (from settings) to ChannelConfig (for runtime)
pub fn convert_channel_settings(
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
    state_guard.channel_mut(ChannelId::A).config = channel_a_config;
    state_guard.channel_mut(ChannelId::B).config = channel_b_config;

    // Restore output options so Engine + peak_fill variant survive restart.
    state_guard.options.processing_engine = all_settings.output.processing_engine;
    state_guard.options.peak_fill = all_settings.output.peak_fill;
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
