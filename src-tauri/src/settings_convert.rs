//! Conversion between persisted `settings::*` shapes and runtime `modulation::*` types.
//!
//! Lifted out of `websocket.rs` so the WS layer holds only network/protocol code.
//! Pre-refactor these functions sat next to the T-Code handler — purely by accident
//! of how the codebase grew. They have no dependency on the WebSocket server and
//! belong with the other settings glue.

use crate::modulation::{ChannelConfig, CurveType, ParameterLinkConfig, ParameterSourceType};
use crate::settings::{
    ChannelSettings, ParameterSourceSettings, ParameterSourceType as SettingsSourceType,
};

/// Convert a `ParameterSourceSettings` (persisted shape) into a runtime
/// `ParameterLinkConfig`. The `buttplug_links` field on the persisted side
/// stays — its consumer is `apply_channel_config_to_state` reading
/// `ChannelSettings.intensity_source.buttplug_links` directly to populate
/// `Channel.buttplug_link`. Sub C's drop is on the *runtime* mirror only;
/// sub F removes the persisted side once the transform model lands.
pub(crate) fn convert_parameter_source(source: &ParameterSourceSettings) -> ParameterLinkConfig {
    let curve = match source.curve.as_str() {
        "exponential" => CurveType::Exponential,
        "logarithmic" => CurveType::Logarithmic,
        "s-curve" => CurveType::SCurve,
        "inverse" => CurveType::Inverse,
        _ => CurveType::Linear,
    };

    match source.source_type {
        SettingsSourceType::Static => ParameterLinkConfig {
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
        },
        SettingsSourceType::Linked => ParameterLinkConfig {
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
        },
    }
}

/// Convert `ChannelSettings` (persisted shape) into a runtime `ChannelConfig`
/// by converting each of the four `ParameterLinkConfig` slots.
pub(crate) fn convert_channel_settings(settings: &ChannelSettings) -> ChannelConfig {
    ChannelConfig {
        frequency: convert_parameter_source(&settings.frequency_source),
        frequency_balance: convert_parameter_source(&settings.frequency_balance_source),
        intensity_balance: convert_parameter_source(&settings.intensity_balance_source),
        intensity: convert_parameter_source(&settings.intensity_source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_settings() -> ParameterSourceSettings {
        ParameterSourceSettings {
            source_type: SettingsSourceType::Linked,
            static_value: 25.0,
            source_axis: "L0".to_string(),
            range_min: 10.0,
            range_max: 90.0,
            curve: "linear".to_string(),
            curve_strength: 2.0,
            midpoint: false,
            delay_enabled: false,
            delay_ms: 0,
            buttplug_links: None,
        }
    }

    #[test]
    fn static_source_preserves_static_value_and_clears_axis() {
        let s = ParameterSourceSettings {
            source_type: SettingsSourceType::Static,
            static_value: 42.0,
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.source_type, ParameterSourceType::Static);
        assert_eq!(p.static_value, Some(42.0));
        assert_eq!(p.source_axis, None);
    }

    #[test]
    fn linked_source_preserves_axis_and_keeps_static_as_fallback() {
        let s = ParameterSourceSettings {
            source_axis: "R2".to_string(),
            static_value: 7.0,
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.source_type, ParameterSourceType::Linked);
        assert_eq!(p.source_axis.as_deref(), Some("R2"));
        // Static value persists as a fallback for the Linked → Static switch.
        assert_eq!(p.static_value, Some(7.0));
    }

    #[test]
    fn delay_enabled_false_drops_delay_ms() {
        // delay_ms non-zero but toggle off — the runtime side should see
        // None so the resolver bypasses delay logic entirely.
        let s = ParameterSourceSettings {
            delay_enabled: false,
            delay_ms: 200,
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.delay_ms, None);
    }

    #[test]
    fn delay_enabled_true_carries_delay_ms() {
        let s = ParameterSourceSettings {
            delay_enabled: true,
            delay_ms: 150,
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.delay_ms, Some(150));
    }

    #[test]
    fn midpoint_false_becomes_none_not_some_false() {
        // `midpoint: bool` on settings → `midpoint: Option<bool>` on runtime.
        // Convention is None for "off" so resolve_parameter's
        // `unwrap_or(false)` is the only path that reads it.
        let s = ParameterSourceSettings {
            midpoint: false,
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.midpoint, None);
    }

    #[test]
    fn midpoint_true_becomes_some_true() {
        let s = ParameterSourceSettings {
            midpoint: true,
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.midpoint, Some(true));
    }

    #[test]
    fn curve_string_dispatch_matches_each_variant() {
        let cases = [
            ("linear", CurveType::Linear),
            ("exponential", CurveType::Exponential),
            ("logarithmic", CurveType::Logarithmic),
            ("s-curve", CurveType::SCurve),
            ("inverse", CurveType::Inverse),
            // Unknown value falls back to Linear (matches the resolver's
            // "default = identity" expectation).
            ("garbage", CurveType::Linear),
        ];
        for (input, expected) in cases {
            let s = ParameterSourceSettings {
                curve: input.to_string(),
                ..base_settings()
            };
            let p = convert_parameter_source(&s);
            assert_eq!(
                p.curve, expected,
                "curve string {:?} should map to {:?}",
                input, expected
            );
        }
    }
}
