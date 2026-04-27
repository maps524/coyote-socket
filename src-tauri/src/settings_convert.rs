//! Conversion between persisted `settings::*` shapes and runtime `modulation::*` types.
//!
//! Lifted out of `websocket.rs` so the WS layer holds only network/protocol code.
//! Pre-refactor these functions sat next to the T-Code handler — purely by accident
//! of how the codebase grew. They have no dependency on the WebSocket server and
//! belong with the other settings glue.

use crate::modulation::{ChannelConfig, CurveType, ParameterLinkConfig, ParameterSourceType};
use crate::settings::{
    ButtplugLinksSettings, ChannelSettings, ParameterSourceSettings,
    ParameterSourceType as SettingsSourceType,
};
use crate::transforms::{ConstrictionMethod, TransformConfig};

/// Default Vibrate amplitude when `buttplug_links.vibrate.config.distance`
/// is missing. Matches the pre-refactor `FeatureTypeConfig::default`.
const DEFAULT_VIBRATE_DISTANCE: f64 = 0.2;
/// Default amplitude scale for Oscillate / Rotate when their respective
/// `*_scale` fields are absent. Matches the pre-refactor default.
const DEFAULT_MOTION_SCALE: f64 = 0.5;
/// Default sweep rate ceiling for Oscillate / Rotate when `*_max_speed` is
/// absent. Matches the pre-refactor default.
const DEFAULT_MOTION_MAX_HZ: f64 = 5.0;

/// Convert a `ParameterSourceSettings` (persisted shape) into a runtime
/// `ParameterLinkConfig`.
///
/// The runtime `transforms` vector is sourced in this order:
/// 1. The settings' own `transforms: Vec<TransformConfig>` field if
///    non-empty (sub G.2 — the new transforms editor writes here).
/// 2. Otherwise the legacy `buttplug_links` 4-stage pipeline gets
///    translated into an ordered transforms vector (Position →
///    Oscillate/Rotate → Vibrate → Constrict, mirroring the pre-refactor
///    stage order). The `Position` / `PositionWithDuration` link drives
///    the `source_axis` directly; everything else becomes a transform
///    with explicit modifier axes.
///
/// Sub G.3 retires the legacy `buttplug_links` path; saved Buttplug
/// presets get written through the new editor before then.
pub(crate) fn convert_parameter_source(source: &ParameterSourceSettings) -> ParameterLinkConfig {
    let curve = match source.curve.as_str() {
        "exponential" => CurveType::Exponential,
        "logarithmic" => CurveType::Logarithmic,
        "s-curve" => CurveType::SCurve,
        "inverse" => CurveType::Inverse,
        _ => CurveType::Linear,
    };

    let bp_position = source
        .buttplug_links
        .as_ref()
        .and_then(buttplug_links_position_axis);
    // Editor-supplied transforms win over the legacy buttplug_links
    // translation. An empty editor vector falls through to the legacy
    // path so saved Buttplug presets keep their behavior until sub G.3
    // rewrites them through the new editor.
    let transforms = if !source.transforms.is_empty() {
        source.transforms.clone()
    } else {
        source
            .buttplug_links
            .as_ref()
            .map(buttplug_links_to_transforms)
            .unwrap_or_default()
    };

    // Determine the effective source for the runtime config. Order of
    // precedence:
    // 1. Persisted source axis when source_type=Linked (T-Code / gamepad
    //    presets).
    // 2. Buttplug Position / PositionWithDuration axis when buttplug_links
    //    has one — this elevates a legacy Static-with-buttplug-links
    //    preset into the Linked path so transforms actually run.
    // 3. Otherwise honor source_type as-is.
    let (source_type, source_axis) = match source.source_type {
        SettingsSourceType::Linked => (
            ParameterSourceType::Linked,
            Some(source.source_axis.clone()),
        ),
        SettingsSourceType::Static => match bp_position.clone() {
            Some(axis) => (ParameterSourceType::Linked, Some(axis)),
            None => (ParameterSourceType::Static, None),
        },
    };

    // If the link has Buttplug position semantics, the effective source
    // axis is the bp: namespace — even if the persisted file kept the
    // T-Code axis around. Saved Buttplug presets typically already
    // store `bp:Position_0` etc. as the source axis, but legacy ones
    // may carry a stale "L0" — override.
    let source_axis = match (&source_type, bp_position) {
        (ParameterSourceType::Linked, Some(bp_axis)) => Some(bp_axis),
        _ => source_axis,
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
        transforms,
    }
}

/// Translate `ButtplugLinksSettings` → ordered `Vec<TransformConfig>`.
/// Stage order matches `process_buttplug_pipeline`:
///   1. Position / PositionWithDuration — handled as `source_axis`, not
///      a transform; not emitted here.
///   2. Motion — `Oscillate` or `Rotate` (mutually exclusive in the
///      settings shape).
///   3. Vibrate.
///   4. Constrict.
fn buttplug_links_to_transforms(links: &ButtplugLinksSettings) -> Vec<TransformConfig> {
    let mut out = Vec::new();

    if let Some(motion) = links.motion.as_ref() {
        match motion.feature_type.as_str() {
            "Oscillate" => out.push(TransformConfig::Oscillate {
                speed_axis: format!("bp:Oscillate_{}", motion.feature_index),
                scale: motion.config.oscillate_scale.unwrap_or(DEFAULT_MOTION_SCALE),
                max_speed_hz: motion
                    .config
                    .oscillate_max_speed
                    .unwrap_or(DEFAULT_MOTION_MAX_HZ),
            }),
            "Rotate" => out.push(TransformConfig::Rotate {
                speed_axis: format!("bp:Rotate_{}", motion.feature_index),
                direction_axis: format!("bp:RotateDir_{}", motion.feature_index),
                scale: motion.config.rotate_scale.unwrap_or(DEFAULT_MOTION_SCALE),
                max_speed_hz: motion
                    .config
                    .rotate_max_speed
                    .unwrap_or(DEFAULT_MOTION_MAX_HZ),
            }),
            // Unknown motion subtype: skip rather than fail load.
            _ => {}
        }
    }

    if let Some(vib) = links.vibrate.as_ref() {
        out.push(TransformConfig::Vibrate {
            speed_axis: format!("bp:Vibrate_{}", vib.feature_index),
            distance: vib.config.distance.unwrap_or(DEFAULT_VIBRATE_DISTANCE),
        });
    }

    if let Some(con) = links.constrict.as_ref() {
        let method = con
            .config
            .constrict_method
            .as_deref()
            .map(|m| match m {
                "downsample" | "Downsample" => ConstrictionMethod::Downsample,
                "clamp" | "Clamp" => ConstrictionMethod::Clamp,
                _ => ConstrictionMethod::Downsample,
            })
            .unwrap_or(ConstrictionMethod::Downsample);
        out.push(TransformConfig::Constrict {
            amount_axis: format!("bp:Constrict_{}", con.feature_index),
            min_floor: con.config.constrict_min_floor.unwrap_or(0.0),
            use_midpoint: con.config.constrict_use_midpoint.unwrap_or(false),
            method,
        });
    }

    out
}

/// Pick the bus axis for the link's Position / PositionWithDuration slot,
/// if any. Returns the `bp:` namespace name the resolver reads as the
/// link's source. `None` means there is no Position-shaped link, so
/// `source_axis` falls back to whatever the persisted settings shape said.
fn buttplug_links_position_axis(links: &ButtplugLinksSettings) -> Option<String> {
    let pos = links.position.as_ref()?;
    match pos.feature_type.as_str() {
        "Position" => Some(format!("bp:Position_{}", pos.feature_index)),
        "PositionWithDuration" => Some(format!("bp:PositionWithDuration_{}", pos.feature_index)),
        _ => None,
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
            transforms: Vec::new(),
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

    fn bp_link(
        feature_type: &str,
        feature_index: u32,
        config: crate::settings::ButtplugFeatureConfigSettings,
    ) -> crate::settings::ButtplugFeatureLinkSettings {
        crate::settings::ButtplugFeatureLinkSettings {
            feature_type: feature_type.to_string(),
            feature_index,
            config,
        }
    }

    #[test]
    fn buttplug_position_link_elevates_static_to_linked_with_bp_axis() {
        // A Static source with a Position buttplug link — legacy preset
        // shape — should switch to Linked + bp:Position_<i> so the
        // resolver actually reads from the bus.
        let mut links = ButtplugLinksSettings::default();
        links.position = Some(bp_link("Position", 1, Default::default()));
        let s = ParameterSourceSettings {
            source_type: SettingsSourceType::Static,
            buttplug_links: Some(links),
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.source_type, ParameterSourceType::Linked);
        assert_eq!(p.source_axis.as_deref(), Some("bp:Position_1"));
        assert!(p.transforms.is_empty(), "Position is the source, not a transform");
    }

    #[test]
    fn buttplug_position_with_duration_uses_pwd_namespace() {
        let mut links = ButtplugLinksSettings::default();
        links.position = Some(bp_link("PositionWithDuration", 0, Default::default()));
        let s = ParameterSourceSettings {
            source_type: SettingsSourceType::Linked,
            buttplug_links: Some(links),
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.source_axis.as_deref(), Some("bp:PositionWithDuration_0"));
    }

    #[test]
    fn buttplug_links_emit_transforms_in_pipeline_stage_order() {
        // Sub D's transforms vec runs in declaration order. The legacy
        // pipeline order was Position → Motion → Vibrate → Constrict.
        // Position becomes the source axis; the rest get emitted as
        // transforms in motion-then-vibrate-then-constrict order so
        // saved presets keep the same effective shape.
        let mut links = ButtplugLinksSettings::default();
        links.position = Some(bp_link("PositionWithDuration", 0, Default::default()));
        links.motion = Some(bp_link(
            "Oscillate",
            0,
            crate::settings::ButtplugFeatureConfigSettings {
                oscillate_scale: Some(0.4),
                oscillate_max_speed: Some(7.0),
                ..Default::default()
            },
        ));
        links.vibrate = Some(bp_link(
            "Vibrate",
            1,
            crate::settings::ButtplugFeatureConfigSettings {
                distance: Some(0.3),
                ..Default::default()
            },
        ));
        links.constrict = Some(bp_link(
            "Constrict",
            0,
            crate::settings::ButtplugFeatureConfigSettings {
                constrict_min_floor: Some(0.1),
                constrict_use_midpoint: Some(true),
                constrict_method: Some("Clamp".to_string()),
                ..Default::default()
            },
        ));
        let s = ParameterSourceSettings {
            source_type: SettingsSourceType::Linked,
            buttplug_links: Some(links),
            ..base_settings()
        };
        let p = convert_parameter_source(&s);

        assert_eq!(p.transforms.len(), 3);
        assert!(matches!(
            &p.transforms[0],
            TransformConfig::Oscillate { speed_axis, scale, max_speed_hz }
                if speed_axis == "bp:Oscillate_0" && (*scale - 0.4).abs() < 1e-9 && (*max_speed_hz - 7.0).abs() < 1e-9
        ));
        assert!(matches!(
            &p.transforms[1],
            TransformConfig::Vibrate { speed_axis, distance }
                if speed_axis == "bp:Vibrate_1" && (*distance - 0.3).abs() < 1e-9
        ));
        assert!(matches!(
            &p.transforms[2],
            TransformConfig::Constrict { amount_axis, min_floor, use_midpoint, method }
                if amount_axis == "bp:Constrict_0"
                && (*min_floor - 0.1).abs() < 1e-9
                && *use_midpoint
                && *method == ConstrictionMethod::Clamp
        ));
    }

    #[test]
    fn buttplug_motion_rotate_emits_rotate_transform_with_paired_axes() {
        // Rotate is the new sub E variant — speed_axis + direction_axis.
        // The conversion produces both axis names in the bp: namespace
        // so the resolver's pre-fetch lands speed at modifier[0] and
        // direction at modifier[1] (matches `apply_rotate`'s contract).
        let mut links = ButtplugLinksSettings::default();
        links.motion = Some(bp_link(
            "Rotate",
            2,
            crate::settings::ButtplugFeatureConfigSettings {
                rotate_scale: Some(0.6),
                rotate_max_speed: Some(8.0),
                ..Default::default()
            },
        ));
        let s = ParameterSourceSettings {
            source_type: SettingsSourceType::Linked,
            buttplug_links: Some(links),
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.transforms.len(), 1);
        assert!(matches!(
            &p.transforms[0],
            TransformConfig::Rotate {
                speed_axis,
                direction_axis,
                scale,
                max_speed_hz,
            } if speed_axis == "bp:Rotate_2"
                && direction_axis == "bp:RotateDir_2"
                && (*scale - 0.6).abs() < 1e-9
                && (*max_speed_hz - 8.0).abs() < 1e-9
        ));
    }

    #[test]
    fn missing_buttplug_links_leaves_transforms_empty() {
        // The pre-refactor default — no Buttplug semantics — produces
        // the same shape as before sub E (empty transforms vec).
        let s = base_settings();
        let p = convert_parameter_source(&s);
        assert!(p.transforms.is_empty());
        assert_eq!(p.source_axis.as_deref(), Some("L0"));
    }

    #[test]
    fn buttplug_motion_unknown_subtype_skips_rather_than_panics() {
        // Defensive: a saved file that names an unknown motion subtype
        // (future variant, typo) should drop the motion stage but still
        // emit Vibrate / Constrict ones if present.
        let mut links = ButtplugLinksSettings::default();
        links.motion = Some(bp_link("MysteryWave", 0, Default::default()));
        links.vibrate = Some(bp_link("Vibrate", 0, Default::default()));
        let s = ParameterSourceSettings {
            source_type: SettingsSourceType::Linked,
            buttplug_links: Some(links),
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.transforms.len(), 1);
        assert!(matches!(&p.transforms[0], TransformConfig::Vibrate { .. }));
    }

    #[test]
    fn editor_supplied_transforms_win_over_legacy_buttplug_links() {
        // Sub G.2: when both `transforms` and `buttplug_links` are
        // present, the editor-supplied vector wins. Old saves only
        // carry `buttplug_links`; new saves write `transforms` directly
        // and the legacy field becomes a stale shadow until sub G.3
        // drops it.
        let mut links = ButtplugLinksSettings::default();
        links.vibrate = Some(bp_link("Vibrate", 0, Default::default()));
        let s = ParameterSourceSettings {
            source_type: SettingsSourceType::Linked,
            transforms: vec![TransformConfig::Scale { factor: 0.5 }],
            buttplug_links: Some(links),
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.transforms.len(), 1);
        assert!(matches!(
            &p.transforms[0],
            TransformConfig::Scale { factor } if (*factor - 0.5).abs() < 1e-9
        ));
    }

    #[test]
    fn empty_transforms_falls_back_to_buttplug_links_translation() {
        // Older saves omit the `transforms` field entirely (serde
        // default = empty vec). The convert layer falls back to the
        // legacy `buttplug_links → transforms` translation so saved
        // Buttplug presets keep their behavior until they get rewritten
        // through the new editor.
        let mut links = ButtplugLinksSettings::default();
        links.vibrate = Some(bp_link("Vibrate", 0, Default::default()));
        let s = ParameterSourceSettings {
            source_type: SettingsSourceType::Linked,
            transforms: Vec::new(),
            buttplug_links: Some(links),
            ..base_settings()
        };
        let p = convert_parameter_source(&s);
        assert_eq!(p.transforms.len(), 1);
        assert!(matches!(&p.transforms[0], TransformConfig::Vibrate { .. }));
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
