//! Conversion between persisted `settings::*` shapes and runtime `modulation::*` types.
//!
//! Lifted out of `websocket.rs` so the WS layer holds only network/protocol code.
//! Pre-refactor these functions sat next to the T-Code handler — purely by accident
//! of how the codebase grew. They have no dependency on the WebSocket server and
//! belong with the other settings glue.

use crate::modulation::{ChannelConfig, CurveType, ParameterSource, ParameterSourceType};
use crate::settings::{
    ChannelSettings, ParameterSourceSettings, ParameterSourceType as SettingsSourceType,
};

/// Convert a `ParameterSourceSettings` (persisted shape) into a runtime `ParameterSource`.
pub fn convert_parameter_source(source: &ParameterSourceSettings) -> ParameterSource {
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
            delay_ms: if source.delay_enabled {
                Some(source.delay_ms)
            } else {
                None
            },
            buttplug_links: source.buttplug_links.clone(),
        },
        SettingsSourceType::Linked => ParameterSource {
            source_type: ParameterSourceType::Linked,
            // Static value stays around as a fallback if the link is later cleared
            // without rewriting the saved file.
            static_value: Some(source.static_value),
            source_axis: Some(source.source_axis.clone()),
            range_min: source.range_min,
            range_max: source.range_max,
            curve,
            curve_strength: Some(source.curve_strength),
            midpoint: if source.midpoint { Some(true) } else { None },
            delay_ms: if source.delay_enabled {
                Some(source.delay_ms)
            } else {
                None
            },
            buttplug_links: source.buttplug_links.clone(),
        },
    }
}

/// Convert `ChannelSettings` (persisted shape) into a runtime `ChannelConfig`
/// by converting each of the four `ParameterSource` slots.
pub fn convert_channel_settings(settings: &ChannelSettings) -> ChannelConfig {
    ChannelConfig {
        frequency: convert_parameter_source(&settings.frequency_source),
        frequency_balance: convert_parameter_source(&settings.frequency_balance_source),
        intensity_balance: convert_parameter_source(&settings.intensity_balance_source),
        intensity: convert_parameter_source(&settings.intensity_source),
    }
}
