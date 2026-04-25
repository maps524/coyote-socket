//! WebSocket server: TCP listen, request peeking, WS upgrade, protocol
//! routing. Owns the singleton `WebSocketServer` and its detected-protocol
//! state.
//!
//! Lifted out of `websocket.rs` and trimmed: T-Code parsing lives in
//! `tcode_input`, resolver helpers in `resolver`, and the settings→runtime
//! conversion in `settings_convert`. What's left here is purely the network
//! layer.

use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::tcode_input::handle_tcode_message;

/// Detected protocol for reporting to the frontend.
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

/// Singleton state for the running WebSocket server.
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

pub static WEBSOCKET_SERVER: tokio::sync::OnceCell<Arc<Mutex<WebSocketServer>>> =
    tokio::sync::OnceCell::const_new();

pub async fn get_websocket_server() -> &'static Arc<Mutex<WebSocketServer>> {
    WEBSOCKET_SERVER
        .get_or_init(|| async { Arc::new(Mutex::new(WebSocketServer::new())) })
        .await
}

/// Start the WebSocket server on the specified port.
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

/// Stop the WebSocket server.
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

/// Detected protocol for an in-flight WebSocket connection. Internal
/// per-connection state — not the same as `InputProtocol`, which is the
/// app-wide flag for the frontend.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DetectedProtocol {
    Unknown,
    TCode,
    Buttplug,
}

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

/// Peek the first bytes off a TCP stream to distinguish a WebSocket
/// upgrade (T-Code / Buttplug) from a plain HTTP request (Lovense Standard
/// API). The peek does not consume bytes, so the WebSocket handshake
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

/// Handle a WebSocket connection with protocol auto-detection on the first
/// text message. Once the protocol is known, every subsequent message is
/// routed to the matching handler.
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

    set_detected_protocol(InputProtocol::None).await;
}

/// Parse + dispatch a Buttplug text frame. Returns the optional serialized
/// response.
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

            for client_msg in messages {
                let msg_responses =
                    handle_buttplug_message(client_msg, &config, protocol_version).await;
                responses.extend(msg_responses);
            }

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
            let error_response = vec![ButtplugServerMessage::Error(ButtplugError::message_error(
                0, e,
            ))];
            serialize_buttplug_messages(&error_response).ok()
        }
    }
}

/// True iff the WebSocket server is running.
pub async fn is_server_running() -> bool {
    let server = get_websocket_server().await;
    let server_guard = server.lock().await;
    server_guard.running
}

/// Read the currently detected input protocol (frontend reads via
/// `get_connection_status`).
pub async fn get_detected_protocol() -> InputProtocol {
    let server = get_websocket_server().await;
    let server_guard = server.lock().await;
    server_guard.detected_protocol
}

/// Mark the detected input protocol. Called when the WebSocket connection
/// auto-detects T-Code or Buttplug; called by the Lovense HTTP path on
/// first request.
pub async fn set_detected_protocol(protocol: InputProtocol) {
    let server = get_websocket_server().await;
    let mut server_guard = server.lock().await;
    server_guard.detected_protocol = protocol;
}
