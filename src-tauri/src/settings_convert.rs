//! Conversion between persisted `settings::*` shapes and runtime `modulation::*` types.
//!
//! Lifted out of `websocket.rs` so the WS layer holds only network/protocol code.
//! Pre-refactor these functions sat next to the T-Code handler — purely by accident
//! of how the codebase grew. They have no dependency on the WebSocket server and
//! belong with the other settings glue.
//!
//! Sub G.3 retired the legacy `buttplug_links` schema field along with its
//! field-by-field translation into `Vec<TransformConfig>`. The persisted
//! `transforms` vector is now the only source of truth; saved presets that
//! pre-date the editor load with an empty transforms list per the plan-doc
//! migration table (`docs/plans/pipeline-refactor.md` line 458 — "Drop key
//! entirely. New transforms list starts empty on the parameter; user adds
//! transforms in the new editor if they want them.").

use crate::modulation::{ChannelConfig, CurveType, ParameterLinkConfig, ParameterSourceType};
use crate::settings::{
    ChannelSettings, ParameterSourceSettings, ParameterSourceType as SettingsSourceType,
};

/// Convert a `ParameterSourceSettings` (persisted shape) into a runtime
/// `ParameterLinkConfig`. Direct field-by-field copy plus the curve string
/// → `CurveType` enum dispatch and the `delay_enabled` toggle gate.
pub(crate) fn convert_parameter_source(source: &ParameterSourceSettings) -> ParameterLinkConfig {
    let curve = match source.curve.as_str() {
        "exponential" => CurveType::Exponential,
        "logarithmic" => CurveType::Logarithmic,
        "s-curve" => CurveType::SCurve,
        "inverse" => CurveType::Inverse,
        _ => CurveType::Linear,
    };

    let (source_type, source_axis) = match source.source_type {
        SettingsSourceType::Linked => (
            ParameterSourceType::Linked,
            Some(source.source_axis.clone()),
        ),
        SettingsSourceType::Static => (ParameterSourceType::Static, None),
    };

    ParameterLinkConfig {
        source_type,
        // Static value stays around as a fallback if the link is later
        // cleared without rewriting the saved file.
        static_value: Some(source.static_value),
        source_axis,
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
        transforms: source.transforms.clone(),
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
    use crate::transforms::{ScalarInput, TransformConfig};

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
            transforms: Vec::new(),
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
    fn editor_supplied_transforms_pass_through_directly() {
        // Sub G.3: transforms are now the only source of truth for the
        // resolver-layer shaping vector. The persisted vector copies into
        // the runtime config one-to-one.
        let s = ParameterSourceSettings {
            transforms: vec![
                TransformConfig::Scale { factor: 0.5 },
                TransformConfig::Vibrate {
                    speed: ScalarInput::Axis("bp:Vibrate_0".into()),
                    distance: 0.3,
                },
            ],
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.transforms.len(), 2);
        assert!(matches!(
            &p.transforms[0],
            TransformConfig::Scale { factor } if (*factor - 0.5).abs() < 1e-9
        ));
        assert!(matches!(
            &p.transforms[1],
            TransformConfig::Vibrate { speed, distance }
                if *speed == ScalarInput::Axis("bp:Vibrate_0".into()) && (*distance - 0.3).abs() < 1e-9
        ));
    }

    #[test]
    fn empty_transforms_round_trips_to_empty_vec() {
        // Saved presets that omit the `transforms` key (older saves +
        // saves with no editor-attached transforms) deserialize with the
        // serde default of an empty vec.
        let p = convert_parameter_source(&base_settings());
        assert!(p.transforms.is_empty());
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
