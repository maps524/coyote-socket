/// Buttplug Protocol Message Types
///
/// Implements Buttplug v2 message format with serde serialization.
/// Messages are JSON wrapped in arrays: `[{"MessageType": {...}}]`

use serde::{Deserialize, Serialize};

// ============================================================================
// CLIENT MESSAGES (Client → Server)
// ============================================================================

/// Wrapper for incoming client messages
///
/// Buttplug messages are wrapped in an object with the message type as the key.
/// Example: `[{"RequestServerInfo": {"Id": 1, ...}}]`
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ButtplugClientMessageWrapper {
    RequestServerInfo { RequestServerInfo: RequestServerInfo },
    StartScanning { StartScanning: StartScanning },
    StopScanning { StopScanning: StopScanning },
    RequestDeviceList { RequestDeviceList: RequestDeviceList },
    ScalarCmd { ScalarCmd: ScalarCmd },
    LinearCmd { LinearCmd: LinearCmd },
    VibrateCmd { VibrateCmd: VibrateCmd },
    RotateCmd { RotateCmd: RotateCmd },
    StopDeviceCmd { StopDeviceCmd: StopDeviceCmd },
    StopAllDevices { StopAllDevices: StopAllDevices },
    Ping { Ping: Ping },
}

impl ButtplugClientMessageWrapper {
    /// Extract the inner message for processing
    pub fn into_message(self) -> ButtplugClientMessage {
        match self {
            Self::RequestServerInfo { RequestServerInfo: msg } => ButtplugClientMessage::RequestServerInfo(msg),
            Self::StartScanning { StartScanning: msg } => ButtplugClientMessage::StartScanning(msg),
            Self::StopScanning { StopScanning: msg } => ButtplugClientMessage::StopScanning(msg),
            Self::RequestDeviceList { RequestDeviceList: msg } => ButtplugClientMessage::RequestDeviceList(msg),
            Self::ScalarCmd { ScalarCmd: msg } => ButtplugClientMessage::ScalarCmd(msg),
            Self::LinearCmd { LinearCmd: msg } => ButtplugClientMessage::LinearCmd(msg),
            Self::VibrateCmd { VibrateCmd: msg } => ButtplugClientMessage::VibrateCmd(msg),
            Self::RotateCmd { RotateCmd: msg } => ButtplugClientMessage::RotateCmd(msg),
            Self::StopDeviceCmd { StopDeviceCmd: msg } => ButtplugClientMessage::StopDeviceCmd(msg),
            Self::StopAllDevices { StopAllDevices: msg } => ButtplugClientMessage::StopAllDevices(msg),
            Self::Ping { Ping: msg } => ButtplugClientMessage::Ping(msg),
        }
    }
}

/// Buttplug client message types
#[derive(Debug)]
pub enum ButtplugClientMessage {
    RequestServerInfo(RequestServerInfo),
    StartScanning(StartScanning),
    StopScanning(StopScanning),
    RequestDeviceList(RequestDeviceList),
    ScalarCmd(ScalarCmd),
    LinearCmd(LinearCmd),
    VibrateCmd(VibrateCmd),
    RotateCmd(RotateCmd),
    StopDeviceCmd(StopDeviceCmd),
    StopAllDevices(StopAllDevices),
    Ping(Ping),
}

impl ButtplugClientMessage {
    /// Get the message ID for response matching
    pub fn id(&self) -> u32 {
        match self {
            Self::RequestServerInfo(msg) => msg.id,
            Self::StartScanning(msg) => msg.id,
            Self::StopScanning(msg) => msg.id,
            Self::RequestDeviceList(msg) => msg.id,
            Self::ScalarCmd(msg) => msg.id,
            Self::LinearCmd(msg) => msg.id,
            Self::VibrateCmd(msg) => msg.id,
            Self::RotateCmd(msg) => msg.id,
            Self::StopDeviceCmd(msg) => msg.id,
            Self::StopAllDevices(msg) => msg.id,
            Self::Ping(msg) => msg.id,
        }
    }
}

// ----------------------------------------------------------------------------
// Handshake Messages
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RequestServerInfo {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "ClientName")]
    pub client_name: String,
    #[serde(rename = "MessageVersion")]
    pub message_version: Option<u32>,
}

// ----------------------------------------------------------------------------
// Device Enumeration Messages
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StartScanning {
    #[serde(rename = "Id")]
    pub id: u32,
}

#[derive(Debug, Deserialize)]
pub struct StopScanning {
    #[serde(rename = "Id")]
    pub id: u32,
}

#[derive(Debug, Deserialize)]
pub struct RequestDeviceList {
    #[serde(rename = "Id")]
    pub id: u32,
}

// ----------------------------------------------------------------------------
// Device Command Messages
// ----------------------------------------------------------------------------

/// ScalarCmd - Universal command for scalar-type actuators (v3+)
/// Replaces VibrateCmd and handles Vibrate, Oscillate, Constrict, Position, etc.
#[derive(Debug, Deserialize)]
pub struct ScalarCmd {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "DeviceIndex")]
    pub device_index: u32,
    #[serde(rename = "Scalars")]
    pub scalars: Vec<ScalarValue>,
}

#[derive(Debug, Deserialize)]
pub struct ScalarValue {
    #[serde(rename = "Index")]
    pub index: u32,
    #[serde(rename = "Scalar")]
    pub scalar: f64,
    #[serde(rename = "ActuatorType")]
    pub actuator_type: String,
}

#[derive(Debug, Deserialize)]
pub struct LinearCmd {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "DeviceIndex")]
    pub device_index: u32,
    #[serde(rename = "Vectors")]
    pub vectors: Vec<LinearVector>,
}

#[derive(Debug, Deserialize)]
pub struct LinearVector {
    #[serde(rename = "Index")]
    pub index: u32,
    #[serde(rename = "Duration")]
    pub duration: u32,
    #[serde(rename = "Position")]
    pub position: f64,
}

#[derive(Debug, Deserialize)]
pub struct VibrateCmd {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "DeviceIndex")]
    pub device_index: u32,
    #[serde(rename = "Speeds")]
    pub speeds: Vec<VibrateSpeed>,
}

#[derive(Debug, Deserialize)]
pub struct VibrateSpeed {
    #[serde(rename = "Index")]
    pub index: u32,
    #[serde(rename = "Speed")]
    pub speed: f64,
}

#[derive(Debug, Deserialize)]
pub struct RotateCmd {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "DeviceIndex")]
    pub device_index: u32,
    #[serde(rename = "Rotations")]
    pub rotations: Vec<RotateSpeed>,
}

#[derive(Debug, Deserialize)]
pub struct RotateSpeed {
    #[serde(rename = "Index")]
    pub index: u32,
    #[serde(rename = "Speed")]
    pub speed: f64,
    #[serde(rename = "Clockwise")]
    pub clockwise: bool,
}

#[derive(Debug, Deserialize)]
pub struct StopDeviceCmd {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "DeviceIndex")]
    pub device_index: u32,
}

#[derive(Debug, Deserialize)]
pub struct StopAllDevices {
    #[serde(rename = "Id")]
    pub id: u32,
}

#[derive(Debug, Deserialize)]
pub struct Ping {
    #[serde(rename = "Id")]
    pub id: u32,
}

// ============================================================================
// SERVER MESSAGES (Server → Client)
// ============================================================================

/// Wrapper for outgoing server messages
///
/// When serialized, produces the correct Buttplug format:
/// `[{"ServerInfo": {"Id": 1, ...}}]`
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ButtplugServerMessageWrapper {
    ServerInfo { ServerInfo: ServerInfo },
    DeviceAdded { DeviceAdded: DeviceAdded },
    DeviceList { DeviceList: DeviceList },
    ScanningFinished { ScanningFinished: ScanningFinished },
    Ok { #[serde(rename = "Ok")] OkMessage: ButtplugOk },
    Error { Error: ButtplugError },
}

impl ButtplugServerMessageWrapper {
    /// Wrap a server message for serialization
    pub fn from_message(msg: ButtplugServerMessage) -> Self {
        match msg {
            ButtplugServerMessage::ServerInfo(m) => Self::ServerInfo { ServerInfo: m },
            ButtplugServerMessage::DeviceAdded(m) => Self::DeviceAdded { DeviceAdded: m },
            ButtplugServerMessage::DeviceList(m) => Self::DeviceList { DeviceList: m },
            ButtplugServerMessage::ScanningFinished(m) => Self::ScanningFinished { ScanningFinished: m },
            ButtplugServerMessage::Ok(m) => Self::Ok { OkMessage: m },
            ButtplugServerMessage::Error(m) => Self::Error { Error: m },
        }
    }
}

/// Buttplug server message types (inner messages)
#[derive(Debug, Clone)]
pub enum ButtplugServerMessage {
    ServerInfo(ServerInfo),
    DeviceAdded(DeviceAdded),
    DeviceList(DeviceList),
    ScanningFinished(ScanningFinished),
    Ok(ButtplugOk),
    Error(ButtplugError),
}

// ----------------------------------------------------------------------------
// Handshake Response
// ----------------------------------------------------------------------------

/// ServerInfo for v3 and earlier (uses MessageVersion)
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfoV3 {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "ServerName")]
    pub server_name: String,
    #[serde(rename = "MessageVersion")]
    pub message_version: u32,
    #[serde(rename = "MaxPingTime")]
    pub max_ping_time: u32,
}

/// ServerInfo for v4+ (uses ProtocolVersionMajor/Minor)
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfoV4 {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "ServerName")]
    pub server_name: String,
    #[serde(rename = "ProtocolVersionMajor")]
    pub protocol_version_major: u32,
    #[serde(rename = "ProtocolVersionMinor")]
    pub protocol_version_minor: u32,
    #[serde(rename = "MaxPingTime")]
    pub max_ping_time: u32,
}

/// Unified ServerInfo that can be either v3 or v4 format
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ServerInfo {
    V3(ServerInfoV3),
    V4(ServerInfoV4),
}

// ----------------------------------------------------------------------------
// Device Enumeration Responses
// ----------------------------------------------------------------------------

/// V3 format DeviceMessages - commands are arrays of feature attributes
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceMessagesV3 {
    #[serde(rename = "ScalarCmd", skip_serializing_if = "Option::is_none")]
    pub scalar_cmd: Option<Vec<DeviceMessageAttributeV3>>,
    #[serde(rename = "LinearCmd", skip_serializing_if = "Option::is_none")]
    pub linear_cmd: Option<Vec<DeviceMessageAttributeV3>>,
    #[serde(rename = "RotateCmd", skip_serializing_if = "Option::is_none")]
    pub rotate_cmd: Option<Vec<DeviceMessageAttributeV3>>,
    #[serde(rename = "StopDeviceCmd", skip_serializing_if = "Option::is_none")]
    pub stop_device_cmd: Option<NullMessageAttributes>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceAdded {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "DeviceName")]
    pub device_name: String,
    #[serde(rename = "DeviceIndex")]
    pub device_index: u32,
    #[serde(rename = "DeviceMessages")]
    pub device_messages: DeviceMessagesV3,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceList {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "Devices")]
    pub devices: Vec<DeviceInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    #[serde(rename = "DeviceName")]
    pub device_name: String,
    #[serde(rename = "DeviceIndex")]
    pub device_index: u32,
    #[serde(rename = "DeviceMessages")]
    pub device_messages: DeviceMessagesV3,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanningFinished {
    #[serde(rename = "Id")]
    pub id: u32,
}

/// V3 device message attributes (array of per-feature attributes)
/// Format: [{"FeatureDescriptor": "...", "StepCount": N, "ActuatorType": "..."}]
#[derive(Debug, Clone, Serialize)]
pub struct DeviceMessageAttributeV3 {
    #[serde(rename = "FeatureDescriptor")]
    pub feature_descriptor: String,
    #[serde(rename = "StepCount")]
    pub step_count: u32,
    #[serde(rename = "ActuatorType")]
    pub actuator_type: String,
}

impl DeviceMessageAttributeV3 {
    pub fn new(descriptor: &str, step_count: u32, actuator_type: &str) -> Self {
        Self {
            feature_descriptor: descriptor.to_string(),
            step_count,
            actuator_type: actuator_type.to_string(),
        }
    }
}

/// V2 device message attributes (single object with FeatureCount)
#[derive(Debug, Clone, Serialize)]
pub struct DeviceMessageAttributeV2 {
    #[serde(rename = "FeatureCount", skip_serializing_if = "Option::is_none")]
    pub feature_count: Option<u32>,
}

impl DeviceMessageAttributeV2 {
    pub fn with_features(count: u32) -> Self {
        Self { feature_count: Some(count) }
    }

    pub fn empty() -> Self {
        Self { feature_count: None }
    }
}

/// Empty attributes for StopDeviceCmd
#[derive(Debug, Clone, Serialize)]
pub struct NullMessageAttributes {}

impl NullMessageAttributes {
    pub fn new() -> Self {
        Self {}
    }
}

// ----------------------------------------------------------------------------
// Generic Responses
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ButtplugOk {
    #[serde(rename = "Id")]
    pub id: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ButtplugError {
    #[serde(rename = "Id")]
    pub id: u32,
    #[serde(rename = "ErrorCode")]
    pub error_code: u32,
    #[serde(rename = "ErrorMessage")]
    pub error_message: String,
}

// Error code constants
impl ButtplugError {
    pub const ERROR_UNKNOWN: u32 = 0;
    pub const ERROR_HANDSHAKE: u32 = 1;
    pub const ERROR_PING: u32 = 2;
    pub const ERROR_MSG: u32 = 3;
    pub const ERROR_DEVICE: u32 = 4;

    pub fn unknown(id: u32, msg: String) -> Self {
        Self { id, error_code: Self::ERROR_UNKNOWN, error_message: msg }
    }

    pub fn handshake(id: u32, msg: String) -> Self {
        Self { id, error_code: Self::ERROR_HANDSHAKE, error_message: msg }
    }

    pub fn message_error(id: u32, msg: String) -> Self {
        Self { id, error_code: Self::ERROR_MSG, error_message: msg }
    }

    pub fn device_error(id: u32, msg: String) -> Self {
        Self { id, error_code: Self::ERROR_DEVICE, error_message: msg }
    }
}

// ============================================================================
// UTILITY FUNCTIONS
// ============================================================================

/// Parse incoming JSON array of Buttplug messages
pub fn parse_buttplug_messages(raw: &str) -> Result<Vec<ButtplugClientMessage>, String> {
    let wrappers: Vec<ButtplugClientMessageWrapper> = serde_json::from_str(raw)
        .map_err(|e| format!("Failed to parse JSON: {}", e))?;

    Ok(wrappers.into_iter().map(|w| w.into_message()).collect())
}

/// Serialize outgoing messages to JSON array format
pub fn serialize_buttplug_messages(messages: &[ButtplugServerMessage]) -> Result<String, String> {
    let wrappers: Vec<ButtplugServerMessageWrapper> = messages
        .iter()
        .map(|m| ButtplugServerMessageWrapper::from_message(m.clone()))
        .collect();

    serde_json::to_string(&wrappers)
        .map_err(|e| format!("Failed to serialize JSON: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request_server_info() {
        let json = r#"[{"RequestServerInfo": {"Id": 1, "ClientName": "Test", "MessageVersion": 2}}]"#;
        let messages = parse_buttplug_messages(json).unwrap();
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            ButtplugClientMessage::RequestServerInfo(msg) => {
                assert_eq!(msg.id, 1);
                assert_eq!(msg.client_name, "Test");
                assert_eq!(msg.message_version, Some(2));
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_serialize_server_info_v3() {
        let messages = vec![ButtplugServerMessage::ServerInfo(ServerInfo::V3(ServerInfoV3 {
            id: 1,
            server_name: "CoyoteSocket".to_string(),
            message_version: 3,
            max_ping_time: 0,
        }))];

        let json = serialize_buttplug_messages(&messages).unwrap();
        assert!(json.contains("ServerInfo"));
        assert!(json.contains("CoyoteSocket"));
        assert!(json.contains("MessageVersion"));
    }

    #[test]
    fn test_serialize_server_info_v4() {
        let messages = vec![ButtplugServerMessage::ServerInfo(ServerInfo::V4(ServerInfoV4 {
            id: 1,
            server_name: "CoyoteSocket".to_string(),
            protocol_version_major: 4,
            protocol_version_minor: 0,
            max_ping_time: 0,
        }))];

        let json = serialize_buttplug_messages(&messages).unwrap();
        assert!(json.contains("ServerInfo"));
        assert!(json.contains("CoyoteSocket"));
        assert!(json.contains("ProtocolVersionMajor"));
    }

    #[test]
    fn test_parse_linear_cmd() {
        let json = r#"[{"LinearCmd": {"Id": 4, "DeviceIndex": 0, "Vectors": [{"Index": 0, "Duration": 500, "Position": 0.75}]}}]"#;
        let messages = parse_buttplug_messages(json).unwrap();
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            ButtplugClientMessage::LinearCmd(msg) => {
                assert_eq!(msg.device_index, 0);
                assert_eq!(msg.vectors.len(), 1);
                assert_eq!(msg.vectors[0].position, 0.75);
            }
            _ => panic!("Wrong message type"),
        }
    }
}
