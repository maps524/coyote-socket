//! T-Code message handler. Parses T-Code commands off a WebSocket text
//! frame, applies them to `ProcessingState`, and pushes an axis-update
//! event to the frontend.
//!
//! Lifted out of `websocket.rs` so the WebSocket layer (`net.rs`) is left
//! with only connection / framing / routing logic.

use crate::emit_axis_update;
use crate::processing::{get_processing_state, parse_tcode};

/// Handle an inbound T-Code text message and return an optional response
/// (used for the `D0` / `D1` / `D2` device-info handshakes).
pub async fn handle_tcode_message(message: &str) -> Option<String> {
    let message = message.trim();

    // Device info handshakes
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
        let state = get_processing_state().await;
        let mut state_guard = state.write().await;
        state_guard.stop();
        return None;
    }

    let commands = parse_tcode(message);
    if !commands.is_empty() {
        let state = get_processing_state().await;
        let mut state_guard = state.write().await;

        for cmd in &commands {
            state_guard.process_command(cmd);
            crate::diagnostic::record_input(&cmd.axis, cmd.value, cmd.interval_ms);
        }

        let (channel_a, channel_b) = state_guard.get_current_intensities();
        let axes: std::collections::HashMap<String, f64> = state_guard
            .axis_values
            .iter()
            .map(|(k, v)| (k.clone(), v.value))
            .collect();
        drop(state_guard);

        emit_axis_update(axes, channel_a, channel_b);
    }

    None
}
