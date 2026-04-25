// Lovense Standard API request/response shapes.
//
// Spec: https://developer.lovense.com/docs/standard-solutions/standard-api.html
// Reference impl: ../../../../LR_spoofer/index.js

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Common header for command requests. We only inspect `command`/`type` to
/// dispatch — concrete fields are decoded per-handler.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandEnvelope {
    pub command: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionRequest {
    pub action: Option<String>,
    #[serde(default)]
    pub time_sec: Option<f64>,
    #[serde(default)]
    pub loop_running_sec: Option<f64>,
    #[serde(default)]
    pub loop_pause_sec: Option<f64>,
    #[serde(default)]
    pub stop_previous: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternRequest {
    pub rule: Option<String>,
    pub strength: Option<String>,
    #[serde(default)]
    pub time_sec: Option<f64>,
    #[serde(default)]
    pub stop_previous: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PositionRequest {
    pub value: Option<Value>,
}

/// Standard OK reply.
#[derive(Debug, Clone, Serialize)]
pub struct OkReply {
    pub code: u32,
    #[serde(rename = "type")]
    pub kind: &'static str,
}

impl OkReply {
    pub const fn ok() -> Self {
        Self {
            code: 200,
            kind: "ok",
        }
    }
}

/// Toy descriptor reported in `GetToys` / `GetToyName` responses.
#[derive(Debug, Clone, Serialize)]
pub struct ToyData {
    pub id: &'static str,
    pub status: &'static str,
    pub version: &'static str,
    pub name: &'static str,
    pub battery: u8,
    #[serde(rename = "nickName")]
    pub nick_name: &'static str,
    #[serde(rename = "shortFunctionNames")]
    pub short_function_names: &'static [&'static str],
    #[serde(rename = "fullFunctionNames")]
    pub full_function_names: &'static [&'static str],
}

pub const SPOOFED_TOY: ToyData = ToyData {
    id: "f082c00246fa",
    status: "1",
    version: "",
    name: "max",
    battery: 60,
    nick_name: "",
    short_function_names: &["v", "r", "p", "t", "f", "s", "d", "o"],
    full_function_names: &[
        "Vibrate",
        "Rotate",
        "Pump",
        "Thrusting",
        "Fingering",
        "Suction",
        "Depth",
        "Oscillate",
    ],
};

/// Build the GetToys response payload (matches Lovense Remote schema).
pub fn build_get_toys_response() -> serde_json::Value {
    // The spec serializes the toys map as a JSON-encoded *string* nested inside
    // the response. Replicate that quirk so unmodified game clients accept it.
    let toys_map = serde_json::json!({
        SPOOFED_TOY.id: {
            "id": SPOOFED_TOY.id,
            "status": SPOOFED_TOY.status,
            "version": SPOOFED_TOY.version,
            "name": SPOOFED_TOY.name,
            "battery": SPOOFED_TOY.battery,
            "nickName": SPOOFED_TOY.nick_name,
            "shortFunctionNames": SPOOFED_TOY.short_function_names,
            "fullFunctionNames": SPOOFED_TOY.full_function_names,
        }
    });
    serde_json::json!({
        "code": 200,
        "data": {
            "toys": serde_json::to_string(&toys_map).unwrap_or_default(),
            "platform": "ios",
            "appType": "remote",
        },
        "type": "OK",
    })
}

pub fn build_get_toy_name_response() -> serde_json::Value {
    serde_json::json!({
        "code": 200,
        "data": [SPOOFED_TOY.name],
        "type": "OK",
    })
}
