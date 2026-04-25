// Lovense Standard API HTTP handler.
//
// Implements the same protocol as a Lovense Remote local server (POST /command
// with JSON bodies). Shares the WebSocket port with the T-Code/Buttplug server
// — `websocket::handle_connection` peeks the TCP stream and routes plain HTTP
// requests here when no WebSocket Upgrade header is present.
//
// Lovense actions are mapped onto the existing Buttplug feature state map so
// channel routing, parameter linking, and the UI all reuse the Buttplug
// pipeline. The mapping mirrors LR_spoofer's `coyote-socket` profile.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::buttplug::ButtplugFeatureConfig;
use crate::emit_buttplug_features;
use crate::lovense::messages::{
    build_get_toy_name_response, build_get_toys_response, CommandEnvelope, FunctionRequest,
    OkReply, PatternRequest, PositionRequest,
};
use crate::processing::get_processing_state;

// ----------------------------------------------------------------------------
// Session model — every new command bumps the counter, in-flight loops exit
// when their captured snapshot no longer matches.
// ----------------------------------------------------------------------------
static SESSION: AtomicU64 = AtomicU64::new(0);

fn bump_session() -> u64 {
    SESSION.fetch_add(1, Ordering::SeqCst) + 1
}

fn current_session() -> u64 {
    SESSION.load(Ordering::SeqCst)
}

fn is_live(snapshot: u64) -> bool {
    current_session() == snapshot
}

// ----------------------------------------------------------------------------
// Action grammar
//
// Per the Lovense Standard API, raw strength scales are per-action:
//   Vibrate / Rotate / Thrusting / Fingering / Suction / Oscillate / All: 0..20
//   Pump, Depth: 0..3
//   Stroke, Position: 0..100
// ----------------------------------------------------------------------------
fn action_scale(action: &str) -> f64 {
    match action {
        "Pump" | "Depth" => 3.0,
        "Stroke" | "Position" => 100.0,
        _ => 20.0,
    }
}

fn normalize(action: &str, raw: f64) -> f64 {
    (raw / action_scale(action)).clamp(0.0, 1.0)
}

const SHORT_TO_FULL: &[(&str, &str)] = &[
    ("v", "Vibrate"),
    ("r", "Rotate"),
    ("p", "Pump"),
    ("t", "Thrusting"),
    ("f", "Fingering"),
    ("s", "Suction"),
    ("d", "Depth"),
    ("o", "Oscillate"),
    ("a", "All"),
];

fn short_to_full(short: &str) -> Option<&'static str> {
    SHORT_TO_FULL
        .iter()
        .find(|(s, _)| *s == short)
        .map(|(_, f)| *f)
}

/// Buttplug actuator family that a Lovense action maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Actuator {
    Vibrate,
    Oscillate,
    Constrict,
    Rotate,
    Linear,
}

impl Actuator {
    fn feature_prefix(self) -> &'static str {
        match self {
            Actuator::Vibrate => "Vibrate",
            Actuator::Oscillate => "Oscillate",
            Actuator::Constrict => "Constrict",
            Actuator::Rotate => "Rotate",
            Actuator::Linear => "PositionWithDuration",
        }
    }
}

fn actuators_for(action: &str) -> &'static [Actuator] {
    match action {
        "Vibrate" | "Fingering" => &[Actuator::Vibrate],
        "Oscillate" | "Thrusting" => &[Actuator::Oscillate],
        "Pump" | "Suction" => &[Actuator::Constrict],
        "Rotate" => &[Actuator::Rotate],
        "Depth" | "Stroke" | "Position" => &[Actuator::Linear],
        "All" => &[Actuator::Vibrate, Actuator::Rotate, Actuator::Oscillate],
        _ => &[],
    }
}

// ----------------------------------------------------------------------------
// Apply / clear actuators against the shared Buttplug feature state.
// We write to every feature index that the active ButtplugFeatureConfig
// advertises so users with channel A bound to *_0 and channel B bound to *_1
// both receive the value.
// ----------------------------------------------------------------------------
fn feature_count(config: &ButtplugFeatureConfig, actuator: Actuator) -> usize {
    match actuator {
        Actuator::Vibrate => config.vibrate,
        Actuator::Oscillate => config.oscillate,
        Actuator::Constrict => config.constrict,
        Actuator::Rotate => config.rotate,
        Actuator::Linear => config.position_with_duration,
    }
    .max(1)
}

async fn write_actuator(actuator: Actuator, value: f64) {
    let config = ButtplugFeatureConfig::default();
    let count = feature_count(&config, actuator);
    let value = value.clamp(0.0, 1.0);

    let state = get_processing_state().await;
    let features = {
        let mut guard = state.write().await;
        for i in 0..count {
            let key = format!("{}_{}", actuator.feature_prefix(), i);
            if matches!(actuator, Actuator::Linear) {
                // Linear features go through both pipelines: a LinearCmd record
                // (so the smooth-move pipeline picks it up) and the feature
                // map (so the UI displays the current target).
                guard.set_buttplug_linear_cmd(i, value, 200);
            }
            guard.set_buttplug_feature(key, value);
        }
        guard.get_buttplug_features()
    };
    emit_buttplug_features(features);
}

async fn apply_action(action: &str, raw_strength: f64) {
    let normalized = normalize(action, raw_strength);
    for actuator in actuators_for(action) {
        write_actuator(*actuator, normalized).await;
    }
}

async fn zero_actuators(actuators: &[Actuator]) {
    for actuator in actuators {
        write_actuator(*actuator, 0.0).await;
    }
}

async fn stop_all() {
    let state = get_processing_state().await;
    let features = {
        let mut guard = state.write().await;
        guard.clear_all_buttplug_features();
        guard.get_buttplug_features()
    };
    emit_buttplug_features(features);
}

// ----------------------------------------------------------------------------
// Loop runners — spawn one task per action so multiple actuators in a single
// command (e.g. Vibrate:2,Rotate:3) run concurrently.
// ----------------------------------------------------------------------------
fn spawn_function_loop(
    snapshot: u64,
    action: String,
    raw: f64,
    duration_ms: u64,
    loop_run_ms: u64,
    loop_pause_ms: u64,
) {
    tokio::spawn(async move {
        if loop_run_ms == 0 {
            apply_action(&action, raw).await;
            if duration_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
                if is_live(snapshot) {
                    apply_action(&action, 0.0).await;
                }
            }
            return;
        }

        let start = std::time::Instant::now();
        loop {
            if !is_live(snapshot) {
                return;
            }
            if duration_ms > 0 && start.elapsed().as_millis() as u64 >= duration_ms {
                break;
            }
            apply_action(&action, raw).await;
            tokio::time::sleep(std::time::Duration::from_millis(loop_run_ms)).await;
            if !is_live(snapshot) {
                return;
            }
            apply_action(&action, 0.0).await;
            tokio::time::sleep(std::time::Duration::from_millis(loop_pause_ms)).await;
        }
        if is_live(snapshot) {
            apply_action(&action, 0.0).await;
        }
    });
}

fn spawn_pattern_loop(
    snapshot: u64,
    short_code: String,
    raw_strengths: Vec<f64>,
    duration_ms: u64,
    interval_ms: u64,
) {
    tokio::spawn(async move {
        let Some(action) = short_to_full(&short_code) else {
            return;
        };
        if raw_strengths.is_empty() || interval_ms == 0 {
            return;
        }
        let start = std::time::Instant::now();
        let mut i = 0usize;
        loop {
            if !is_live(snapshot) {
                return;
            }
            if duration_ms > 0 && start.elapsed().as_millis() as u64 >= duration_ms {
                break;
            }
            let raw = raw_strengths[i % raw_strengths.len()];
            apply_action(action, raw).await;
            i += 1;
            tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
        }
        if is_live(snapshot) {
            apply_action(action, 0.0).await;
        }
    });
}

// ----------------------------------------------------------------------------
// Command dispatch
// ----------------------------------------------------------------------------
async fn dispatch_command(body: &str) -> Value {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return serde_json::json!({
                "code": 400,
                "type": "error",
                "message": format!("invalid JSON: {}", e),
            });
        }
    };

    // Heartbeat: real clients send {"type":"ping"} every ~2s.
    let envelope: CommandEnvelope = serde_json::from_value(parsed.clone()).unwrap_or(CommandEnvelope {
        command: None,
        kind: None,
    });
    if envelope.command.is_none() && envelope.kind.is_some() {
        return serde_json::to_value(OkReply::ok()).unwrap();
    }

    let Some(command) = envelope.command else {
        return serde_json::json!({
            "code": 400,
            "type": "error",
            "message": "missing command",
        });
    };

    match command.as_str() {
        "GetToys" | "GetToyName" => {
            if command == "GetToyName" {
                build_get_toy_name_response()
            } else {
                build_get_toys_response()
            }
        }
        "Ping" => serde_json::to_value(OkReply::ok()).unwrap(),
        "Stop" => {
            bump_session();
            stop_all().await;
            serde_json::to_value(OkReply::ok()).unwrap()
        }
        "Position" => {
            let req: PositionRequest = serde_json::from_value(parsed).unwrap_or(PositionRequest {
                value: None,
            });
            let raw = req
                .value
                .as_ref()
                .and_then(|v| match v {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.parse::<f64>().ok(),
                    _ => None,
                })
                .unwrap_or(0.0);
            apply_action("Position", raw).await;
            serde_json::to_value(OkReply::ok()).unwrap()
        }
        "PatternV2" | "Preset" => {
            // PatternV2 (Setup/Play/InitPlay/Stop/SyncTime) and named Presets
            // are not yet synthesized — the real Lovense Remote app generates
            // these waveforms internally. Reply OK so clients keep talking.
            serde_json::to_value(OkReply::ok()).unwrap()
        }
        "Function" => {
            let req: FunctionRequest =
                serde_json::from_value(parsed).unwrap_or(FunctionRequest {
                    action: None,
                    time_sec: None,
                    loop_running_sec: None,
                    loop_pause_sec: None,
                    stop_previous: None,
                });
            handle_function(req).await;
            serde_json::to_value(OkReply::ok()).unwrap()
        }
        "Pattern" => {
            let req: PatternRequest = serde_json::from_value(parsed).unwrap_or(PatternRequest {
                rule: None,
                strength: None,
                time_sec: None,
                stop_previous: None,
            });
            handle_pattern(req).await;
            serde_json::to_value(OkReply::ok()).unwrap()
        }
        other => {
            crate::log_warn!("Lovense: unknown command '{}', replying OK", other);
            serde_json::to_value(OkReply::ok()).unwrap()
        }
    }
}

async fn handle_function(req: FunctionRequest) {
    let duration_ms = (req.time_sec.unwrap_or(0.0) * 1000.0).max(0.0) as u64;
    let loop_run_ms = (req.loop_running_sec.unwrap_or(0.0) * 1000.0).max(0.0) as u64;
    let loop_pause_ms = (req.loop_pause_sec.unwrap_or(0.0) * 1000.0).max(0.0) as u64;
    let action_str = req.action.unwrap_or_default();
    let stop_prev = req.stop_previous.map(|v| v != 0).unwrap_or(true);

    let parts: Vec<(String, f64)> = action_str
        .split(',')
        .filter_map(|part| {
            let mut split = part.splitn(2, ':');
            let name = split.next()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let raw = split
                .next()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            Some((name, raw))
        })
        .collect();

    let mut new_actuators: Vec<Actuator> = Vec::new();
    for (name, _) in &parts {
        if name == "Stop" {
            continue;
        }
        for a in actuators_for(name) {
            if !new_actuators.contains(a) {
                new_actuators.push(*a);
            }
        }
    }

    if stop_prev {
        bump_session();
        // Zero any actuator that was driven by the prior session but isn't
        // touched by the new one — otherwise it sticks at its last value.
        let stale: Vec<Actuator> = [
            Actuator::Vibrate,
            Actuator::Oscillate,
            Actuator::Constrict,
            Actuator::Rotate,
            Actuator::Linear,
        ]
        .into_iter()
        .filter(|a| !new_actuators.contains(a))
        .collect();
        zero_actuators(&stale).await;
    }
    let snapshot = current_session();

    for (name, raw) in parts {
        if name == "Stop" {
            stop_all().await;
            continue;
        }
        spawn_function_loop(snapshot, name, raw, duration_ms, loop_run_ms, loop_pause_ms);
    }
}

async fn handle_pattern(req: PatternRequest) {
    let rule = parse_rule(req.rule.as_deref().unwrap_or(""));
    let short_codes: Vec<String> = rule
        .get("F")
        .map(|s| s.as_str())
        .unwrap_or("a")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let interval_ms = rule
        .get("S")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(100)
        .max(100);
    let strengths: Vec<f64> = req
        .strength
        .unwrap_or_default()
        .split(';')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        .collect();
    let duration_ms = (req.time_sec.unwrap_or(0.0) * 1000.0).max(0.0) as u64;
    let stop_prev = req.stop_previous.map(|v| v != 0).unwrap_or(true);

    let mut new_actuators: Vec<Actuator> = Vec::new();
    for short in &short_codes {
        if let Some(full) = short_to_full(short) {
            for a in actuators_for(full) {
                if !new_actuators.contains(a) {
                    new_actuators.push(*a);
                }
            }
        }
    }

    if stop_prev {
        bump_session();
        let stale: Vec<Actuator> = [
            Actuator::Vibrate,
            Actuator::Oscillate,
            Actuator::Constrict,
            Actuator::Rotate,
            Actuator::Linear,
        ]
        .into_iter()
        .filter(|a| !new_actuators.contains(a))
        .collect();
        zero_actuators(&stale).await;
    }
    let snapshot = current_session();

    for short in short_codes {
        spawn_pattern_loop(
            snapshot,
            short,
            strengths.clone(),
            duration_ms,
            interval_ms,
        );
    }
}

fn parse_rule(rule: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for part in rule.split(';') {
        let mut split = part.splitn(2, ':');
        let Some(k) = split.next() else { continue };
        let v = split.next().unwrap_or("").trim_end_matches('#').to_string();
        if !k.is_empty() {
            out.insert(k.to_string(), v);
        }
    }
    out
}

// ----------------------------------------------------------------------------
// HTTP transport
//
// Hand-rolled because we share a TCP listener with the WebSocket server and
// only need POST /command + a couple of GETs. No keep-alive — every request
// is followed by a connection close, matching the Lovense Remote app's own
// behavior.
// ----------------------------------------------------------------------------

const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;

struct ParsedRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> std::io::Result<ParsedRequest> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    let mut header_end: Option<usize> = None;

    while header_end.is_none() {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed before headers",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
        if let Some(idx) = find_subsequence(&buf, b"\r\n\r\n") {
            header_end = Some(idx + 4);
        }
    }
    let header_end = header_end.unwrap();
    let header_str = std::str::from_utf8(&buf[..header_end - 4])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut lines = header_str.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    for line in lines {
        let mut split = line.splitn(2, ':');
        let name = split.next().unwrap_or("").trim();
        let value = split.next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().unwrap_or(0);
        }
    }

    if content_length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        ));
    }

    let mut body: Vec<u8> = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Ok(ParsedRequest {
        method,
        path,
        body,
    })
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn write_json_response(stream: &mut TcpStream, body: &Value) -> std::io::Result<()> {
    let body_bytes = serde_json::to_vec(body).unwrap_or_default();
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body_bytes.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(&body_bytes).await?;
    stream.flush().await?;
    Ok(())
}

async fn write_status_response(stream: &mut TcpStream) -> std::io::Result<()> {
    let body = serde_json::json!({
        "service": "coyote-socket",
        "status": "running",
        "protocol": "lovense-standard-api",
    });
    write_json_response(stream, &body).await
}

/// Inspect the peeked TCP bytes and decide whether this is a WebSocket
/// upgrade (let tungstenite handle it) or a plain HTTP request (route to
/// the Lovense handler).
pub fn is_websocket_upgrade(peek: &[u8]) -> bool {
    let lower = peek.to_ascii_lowercase();
    // Naive but sufficient: real WS clients always send "Upgrade: websocket".
    find_subsequence(&lower, b"upgrade: websocket").is_some()
}

/// Handle a connection that we've already classified as plain HTTP.
pub async fn handle_http_connection(mut stream: TcpStream, addr: SocketAddr) {
    let request = match read_request(&mut stream).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[lovense] {} read failed: {}", addr, e);
            crate::log_warn!("Lovense: failed to read request from {}: {}", addr, e);
            crate::logging::flush_now();
            return;
        }
    };

    if request.method == "POST" {
        let body_str = match std::str::from_utf8(&request.body) {
            Ok(s) => s,
            Err(_) => {
                eprintln!("[lovense] {} non-utf8 body", addr);
                crate::log_warn!("Lovense: non-utf8 body from {}", addr);
                crate::logging::flush_now();
                let _ = write_json_response(
                    &mut stream,
                    &serde_json::json!({"code": 400, "type": "error", "message": "non-utf8 body"}),
                )
                .await;
                return;
            }
        };
        let preview: String = body_str.chars().take(300).collect();
        eprintln!("[lovense] POST {} from {} body={}", request.path, addr, preview);
        crate::log_info!("Lovense POST {} from {}: {}", request.path, addr, preview);
        crate::websocket::set_detected_protocol(crate::websocket::InputProtocol::Lovense).await;
        let reply = dispatch_command(body_str).await;
        let reply_preview = serde_json::to_string(&reply).unwrap_or_default();
        let reply_short: String = reply_preview.chars().take(200).collect();
        eprintln!("[lovense] -> {} reply={}", addr, reply_short);
        crate::log_info!("Lovense -> {} reply: {}", addr, reply_short);
        crate::logging::flush_now();
        if let Err(e) = write_json_response(&mut stream, &reply).await {
            eprintln!("[lovense] {} write reply failed: {}", addr, e);
            crate::log_warn!("Lovense: write reply failed for {}: {}", addr, e);
            crate::logging::flush_now();
        }
        return;
    }

    // GET / or anything else — return a small status payload so probes work
    // and so curl-ing the port returns something useful.
    eprintln!("[lovense] {} {} from {}", request.method, request.path, addr);
    crate::log_info!("Lovense {} {} from {}", request.method, request.path, addr);
    crate::logging::flush_now();
    let _ = write_status_response(&mut stream).await;
}
