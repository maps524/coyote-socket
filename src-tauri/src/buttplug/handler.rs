/// Buttplug Message Handler
///
/// Processes incoming Buttplug messages and generates appropriate responses.
/// Handles handshake, device enumeration, and command processing.

use crate::buttplug::messages::*;
use crate::buttplug::types::ButtplugFeatureConfig;
use crate::processing::get_processing_state;
use crate::emit_buttplug_features;

/// Handle incoming Buttplug client message and generate response(s)
///
/// Most commands return a single response (usually Ok), but some return multiple:
/// - StartScanning returns: DeviceAdded + ScanningFinished + Ok
pub async fn handle_buttplug_message(
    msg: ButtplugClientMessage,
    config: &ButtplugFeatureConfig,
    protocol_version: u32,
) -> Vec<ButtplugServerMessage> {
    match msg {
        ButtplugClientMessage::RequestServerInfo(req) => {
            handle_request_server_info(req, protocol_version)
        }
        ButtplugClientMessage::StartScanning(req) => {
            handle_start_scanning(req, config, protocol_version)
        }
        ButtplugClientMessage::StopScanning(req) => {
            handle_stop_scanning(req)
        }
        ButtplugClientMessage::RequestDeviceList(req) => {
            handle_request_device_list(req, config)
        }
        ButtplugClientMessage::ScalarCmd(cmd) => {
            handle_scalar_cmd(cmd, config).await
        }
        ButtplugClientMessage::LinearCmd(cmd) => {
            handle_linear_cmd(cmd).await
        }
        ButtplugClientMessage::VibrateCmd(cmd) => {
            handle_vibrate_cmd(cmd).await
        }
        ButtplugClientMessage::RotateCmd(cmd) => {
            handle_rotate_cmd(cmd).await
        }
        ButtplugClientMessage::StopDeviceCmd(cmd) => {
            handle_stop_device_cmd(cmd).await
        }
        ButtplugClientMessage::StopAllDevices(cmd) => {
            handle_stop_all_devices(cmd).await
        }
        ButtplugClientMessage::Ping(req) => {
            handle_ping(req)
        }
    }
}

// ============================================================================
// HANDSHAKE
// ============================================================================

fn handle_request_server_info(
    req: RequestServerInfo,
    _protocol_version: u32,
) -> Vec<ButtplugServerMessage> {
    // Detect client version format and respond in matching format
    // v3 and earlier use MessageVersion, v4+ uses ProtocolVersionMajor/Minor

    let client_version = req.message_version.unwrap_or(3);

    if client_version >= 4 {
        // V4 format response
        vec![ButtplugServerMessage::ServerInfo(ServerInfo::V4(ServerInfoV4 {
            id: req.id,
            server_name: "CoyoteSocket".to_string(),
            protocol_version_major: 4,
            protocol_version_minor: 0,
            max_ping_time: 0,
        }))]
    } else {
        // V3 and earlier format response
        vec![ButtplugServerMessage::ServerInfo(ServerInfo::V3(ServerInfoV3 {
            id: req.id,
            server_name: "CoyoteSocket".to_string(),
            message_version: client_version.min(3), // We support up to v3 in this format
            max_ping_time: 0,
        }))]
    }
}

// ============================================================================
// DEVICE ENUMERATION
// ============================================================================

fn handle_start_scanning(
    req: StartScanning,
    config: &ButtplugFeatureConfig,
    protocol_version: u32,
) -> Vec<ButtplugServerMessage> {
    // Return device immediately (simulated scanning)
    vec![
        ButtplugServerMessage::DeviceAdded(create_device_added(config, protocol_version)),
        ButtplugServerMessage::ScanningFinished(ScanningFinished { id: 0 }),
        ButtplugServerMessage::Ok(ButtplugOk { id: req.id }),
    ]
}

fn handle_stop_scanning(req: StopScanning) -> Vec<ButtplugServerMessage> {
    vec![ButtplugServerMessage::Ok(ButtplugOk { id: req.id })]
}

fn handle_request_device_list(
    req: RequestDeviceList,
    config: &ButtplugFeatureConfig,
) -> Vec<ButtplugServerMessage> {
    let device = create_device_info(config, 2);
    vec![ButtplugServerMessage::DeviceList(DeviceList {
        id: req.id,
        devices: vec![device],
    })]
}

/// Create DeviceAdded message based on feature configuration
fn create_device_added(
    config: &ButtplugFeatureConfig,
    protocol_version: u32,
) -> DeviceAdded {
    let device_messages = build_device_messages(config, protocol_version);

    DeviceAdded {
        id: 0, // Server-initiated event
        device_name: "CoyoteSocket".to_string(),
        device_index: 0,
        device_messages,
    }
}

/// Create DeviceInfo for DeviceList response
fn create_device_info(config: &ButtplugFeatureConfig, protocol_version: u32) -> DeviceInfo {
    let device_messages = build_device_messages(config, protocol_version);

    DeviceInfo {
        device_name: "CoyoteSocket".to_string(),
        device_index: 0,
        device_messages,
    }
}

/// Build device message capabilities based on feature configuration (v3 format)
///
/// V3 uses arrays of feature attributes with FeatureDescriptor, StepCount, and ActuatorType.
/// ScalarCmd is the universal command for vibration/oscillation/constriction features.
///
/// ScalarCmd feature order: Position, Vibrate, Oscillate, Constrict
/// This puts Position at lower indices (0-1) for easier mapping.
fn build_device_messages(
    config: &ButtplugFeatureConfig,
    _protocol_version: u32,
) -> DeviceMessagesV3 {
    let mut messages = DeviceMessagesV3::default();
    let mut scalar_features: Vec<DeviceMessageAttributeV3> = Vec::new();

    // Position features -> ScalarCmd with ActuatorType: "Position" (indices 0-1)
    if config.position > 0 {
        for i in 0..config.position {
            scalar_features.push(DeviceMessageAttributeV3::new(
                &format!("Position {}", i + 1),
                100,
                "Position",
            ));
        }
    }

    // Vibrate features -> ScalarCmd with ActuatorType: "Vibrate"
    if config.vibrate > 0 {
        for i in 0..config.vibrate {
            scalar_features.push(DeviceMessageAttributeV3::new(
                &format!("Vibrator {}", i + 1),
                100, // 100 steps for fine control
                "Vibrate",
            ));
        }
    }

    // Oscillate features -> ScalarCmd with ActuatorType: "Oscillate"
    if config.oscillate > 0 {
        for i in 0..config.oscillate {
            scalar_features.push(DeviceMessageAttributeV3::new(
                &format!("Oscillator {}", i + 1),
                100,
                "Oscillate",
            ));
        }
    }

    // Constrict features -> ScalarCmd with ActuatorType: "Constrict"
    if config.constrict > 0 {
        for i in 0..config.constrict {
            scalar_features.push(DeviceMessageAttributeV3::new(
                &format!("Constrictor {}", i + 1),
                100,
                "Constrict",
            ));
        }
    }

    if !scalar_features.is_empty() {
        messages.scalar_cmd = Some(scalar_features);
    }

    // PositionWithDuration features -> LinearCmd
    if config.position_with_duration > 0 {
        let mut linear_features: Vec<DeviceMessageAttributeV3> = Vec::new();
        for i in 0..config.position_with_duration {
            linear_features.push(DeviceMessageAttributeV3::new(
                &format!("Linear {}", i + 1),
                100,
                "Position",
            ));
        }
        messages.linear_cmd = Some(linear_features);
    }

    // Rotate features -> RotateCmd
    if config.rotate > 0 {
        let mut rotate_features: Vec<DeviceMessageAttributeV3> = Vec::new();
        for i in 0..config.rotate {
            rotate_features.push(DeviceMessageAttributeV3::new(
                &format!("Rotator {}", i + 1),
                100,
                "Rotate",
            ));
        }
        messages.rotate_cmd = Some(rotate_features);
    }

    // Always include StopDeviceCmd
    messages.stop_device_cmd = Some(NullMessageAttributes::new());

    messages
}

// ============================================================================
// DEVICE COMMANDS
// ============================================================================

async fn handle_scalar_cmd(cmd: ScalarCmd, config: &ButtplugFeatureConfig) -> Vec<ButtplugServerMessage> {
    // Validate device index
    if cmd.device_index != 0 {
        return vec![ButtplugServerMessage::Error(ButtplugError::device_error(
            cmd.id,
            format!("Invalid device index: {}", cmd.device_index),
        ))];
    }

    // Compute offsets for each actuator type in ScalarCmd
    // Order: Position (if any), Vibrate, Oscillate, Constrict
    let position_offset = 0usize;
    let vibrate_offset = config.position;  // Starts after Position features
    let oscillate_offset = vibrate_offset + config.vibrate;
    let constrict_offset = oscillate_offset + config.oscillate;

    // Store feature values in PROCESSING_STATE
    // Key format: "{ActuatorType}_{TypeSpecificIndex}" e.g., "Vibrate_0", "Position_0"
    let state = get_processing_state().await;
    let features = {
        let mut state_guard = state.write().await;
        for scalar in &cmd.scalars {
            // Convert global ScalarCmd index to type-specific index using saturating_sub to prevent underflow
            let global_index = scalar.index as usize;
            let type_specific_index = match scalar.actuator_type.as_str() {
                "Position" => global_index.saturating_sub(position_offset),
                "Vibrate" => global_index.saturating_sub(vibrate_offset),
                "Oscillate" => global_index.saturating_sub(oscillate_offset),
                "Constrict" => global_index.saturating_sub(constrict_offset),
                _ => global_index, // Unknown type, use as-is
            };
            let feature_key = format!("{}_{}", scalar.actuator_type, type_specific_index);
            let value = scalar.scalar.clamp(0.0, 1.0);
            state_guard.set_buttplug_feature(feature_key, value);
        }
        state_guard.get_buttplug_features()
    };

    // Emit to frontend
    emit_buttplug_features(features);

    vec![ButtplugServerMessage::Ok(ButtplugOk { id: cmd.id })]
}

async fn handle_linear_cmd(cmd: LinearCmd) -> Vec<ButtplugServerMessage> {
    // Validate device index
    if cmd.device_index != 0 {
        return vec![ButtplugServerMessage::Error(ButtplugError::device_error(
            cmd.id,
            format!("Invalid device index: {}", cmd.device_index),
        ))];
    }

    // Store feature values in PROCESSING_STATE
    // LinearCmd includes position AND duration for smooth movement
    let state = get_processing_state().await;
    let features = {
        let mut state_guard = state.write().await;
        for vector in &cmd.vectors {
            // Store with duration for PositionWithDuration pipeline processing
            state_guard.set_buttplug_linear_cmd(
                vector.index as usize,
                vector.position,
                vector.duration,
            );
            // Also store position in features for UI display
            // Use "PositionWithDuration" to match frontend terminology
            let feature_key = format!("PositionWithDuration_{}", vector.index);
            state_guard.set_buttplug_feature(feature_key, vector.position);
        }
        state_guard.get_buttplug_features()
    };

    // Emit to frontend
    emit_buttplug_features(features);

    vec![ButtplugServerMessage::Ok(ButtplugOk { id: cmd.id })]
}

async fn handle_vibrate_cmd(cmd: VibrateCmd) -> Vec<ButtplugServerMessage> {
    // Validate device index
    if cmd.device_index != 0 {
        return vec![ButtplugServerMessage::Error(ButtplugError::device_error(
            cmd.id,
            format!("Invalid device index: {}", cmd.device_index),
        ))];
    }

    // Store feature values in PROCESSING_STATE
    let state = get_processing_state().await;
    let features = {
        let mut state_guard = state.write().await;
        for speed in &cmd.speeds {
            let feature_key = format!("Vibrate_{}", speed.index);
            let value = speed.speed.clamp(0.0, 1.0);
            state_guard.set_buttplug_feature(feature_key, value);
        }
        state_guard.get_buttplug_features()
    };

    // Emit to frontend
    emit_buttplug_features(features);

    vec![ButtplugServerMessage::Ok(ButtplugOk { id: cmd.id })]
}

async fn handle_rotate_cmd(cmd: RotateCmd) -> Vec<ButtplugServerMessage> {
    // Validate device index
    if cmd.device_index != 0 {
        return vec![ButtplugServerMessage::Error(ButtplugError::device_error(
            cmd.id,
            format!("Invalid device index: {}", cmd.device_index),
        ))];
    }

    // Store feature values in PROCESSING_STATE
    // RotateCmd includes speed AND direction (clockwise)
    let state = get_processing_state().await;
    let features = {
        let mut state_guard = state.write().await;
        for rotation in &cmd.rotations {
            let feature_key = format!("Rotate_{}", rotation.index);
            let value = rotation.speed.clamp(0.0, 1.0);
            state_guard.set_buttplug_feature(feature_key, value);
            // Store direction for pipeline processing
            state_guard.set_buttplug_rotate_direction(rotation.index as usize, rotation.clockwise);
        }
        state_guard.get_buttplug_features()
    };

    // Emit to frontend
    emit_buttplug_features(features);

    vec![ButtplugServerMessage::Ok(ButtplugOk { id: cmd.id })]
}

async fn handle_stop_device_cmd(cmd: StopDeviceCmd) -> Vec<ButtplugServerMessage> {
    // Validate device index
    if cmd.device_index != 0 {
        return vec![ButtplugServerMessage::Error(ButtplugError::device_error(
            cmd.id,
            format!("Invalid device index: {}", cmd.device_index),
        ))];
    }

    // Clear all Buttplug features for this device
    let state = get_processing_state().await;
    let features = {
        let mut state_guard = state.write().await;
        state_guard.clear_all_buttplug_features();
        state_guard.get_buttplug_features()
    };

    // Emit to frontend (empty map)
    emit_buttplug_features(features);

    vec![ButtplugServerMessage::Ok(ButtplugOk { id: cmd.id })]
}

async fn handle_stop_all_devices(cmd: StopAllDevices) -> Vec<ButtplugServerMessage> {
    // Clear all Buttplug features
    let state = get_processing_state().await;
    let features = {
        let mut state_guard = state.write().await;
        state_guard.clear_all_buttplug_features();
        state_guard.get_buttplug_features()
    };

    // Emit to frontend (empty map)
    emit_buttplug_features(features);

    vec![ButtplugServerMessage::Ok(ButtplugOk { id: cmd.id })]
}

// ============================================================================
// MISC
// ============================================================================

fn handle_ping(req: Ping) -> Vec<ButtplugServerMessage> {
    vec![ButtplugServerMessage::Ok(ButtplugOk { id: req.id })]
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handshake_v3() {
        let req = RequestServerInfo {
            id: 1,
            client_name: "Test".to_string(),
            message_version: Some(3),
        };

        let responses = handle_request_server_info(req, 3);
        assert_eq!(responses.len(), 1);

        match &responses[0] {
            ButtplugServerMessage::ServerInfo(ServerInfo::V3(info)) => {
                assert_eq!(info.id, 1);
                assert_eq!(info.server_name, "CoyoteSocket");
                assert_eq!(info.message_version, 3);
            }
            _ => panic!("Wrong response type"),
        }
    }

    #[test]
    fn test_handshake_v4() {
        let req = RequestServerInfo {
            id: 1,
            client_name: "Test".to_string(),
            message_version: Some(4),
        };

        let responses = handle_request_server_info(req, 4);
        assert_eq!(responses.len(), 1);

        match &responses[0] {
            ButtplugServerMessage::ServerInfo(ServerInfo::V4(info)) => {
                assert_eq!(info.id, 1);
                assert_eq!(info.server_name, "CoyoteSocket");
                assert_eq!(info.protocol_version_major, 4);
                assert_eq!(info.protocol_version_minor, 0);
            }
            _ => panic!("Wrong response type"),
        }
    }

    #[test]
    fn test_start_scanning() {
        let config = ButtplugFeatureConfig::default();
        let req = StartScanning { id: 2 };

        let responses = handle_start_scanning(req, &config, 2);
        assert_eq!(responses.len(), 3); // DeviceAdded + ScanningFinished + Ok
    }

    #[test]
    fn test_build_device_messages_v3() {
        let config = ButtplugFeatureConfig::default();
        let messages = build_device_messages(&config, 3);

        // V3 format should include ScalarCmd (for vibrate/oscillate/constrict), LinearCmd, RotateCmd, StopDeviceCmd
        // Note: Position is disabled by default (position=0), clients use LinearCmd instead
        assert!(messages.scalar_cmd.is_some());
        assert!(messages.linear_cmd.is_some());
        assert!(messages.rotate_cmd.is_some());
        assert!(messages.stop_device_cmd.is_some());

        // Check feature counts - default config has position=0, vibrate=2, oscillate=2, constrict=2
        // ScalarCmd order: Vibrate, Oscillate, Constrict (no Position)
        let scalar = messages.scalar_cmd.unwrap();
        assert_eq!(scalar.len(), 6); // 2 vibrate + 2 oscillate + 2 constrict
        assert_eq!(scalar[0].actuator_type, "Vibrate");
        assert_eq!(scalar[2].actuator_type, "Oscillate");
        assert_eq!(scalar[4].actuator_type, "Constrict");

        let linear = messages.linear_cmd.unwrap();
        assert_eq!(linear.len(), 2); // 2 linear features (PositionWithDuration)
        assert_eq!(linear[0].actuator_type, "Position");

        let rotate = messages.rotate_cmd.unwrap();
        assert_eq!(rotate.len(), 2); // 2 rotators
        assert_eq!(rotate[0].actuator_type, "Rotate");
    }
}
