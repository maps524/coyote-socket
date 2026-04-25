//! Per-parameter shaping transforms.
//!
//! Each `ParameterLinkConfig` carries an ordered `Vec<TransformConfig>` that
//! the resolver applies after midpoint + curve, before range mapping. Two
//! categories of variants live in this enum:
//!
//! - **Generic primitives** (`Smooth`, `Scale`, `Clamp`, `Invert`, `Hold`,
//!   `Mix`) — composable shaping functions that read the post-curve value
//!   and at most one bus modifier axis. Resolver-layer; device-agnostic.
//! - **Buttplug semantic wrappers** (`Vibrate`, `Oscillate`, `Constrict`)
//!   — built atop the generics' shape but exposed as named, single-purpose
//!   variants so the Buttplug-era UX keeps the labels users recognize.
//!   Implementation lives in `transforms::buttplug` so the
//!   resolver-primitive boundary is enforced by file layout.
//!
//! **Transforms must NOT read the bus directly.** Per the GPT/Gemini
//! reviews captured in the plan doc, the resolver pre-fetches every
//! modifier axis at the link's `target_time` and passes the resolved
//! values in via `&[f64]`. Hidden bus reads inside `apply` would
//! reintroduce the time-travel bug where the base parameter resolves at
//! `now - delay_ms` while the transform's modifier reads `now`.
//!
//! Sub D introduces the variants + apply dispatch + tests but no caller
//! reads them yet. Sub E's resolver rewrite wires the
//! `ParameterLinkConfig.transforms` field into the per-tick loop and
//! sub F deletes the old `process_buttplug_pipeline`.

use serde::{Deserialize, Serialize};

pub mod buttplug;

/// Method for applying Constrict bounds. Re-exports the existing
/// `crate::buttplug::ConstrictionMethod` so saved presets that referenced
/// the old `buttplug::types::ConstrictionMethod` round-trip without
/// touching the wire format. Sub F is where this file becomes the new
/// home; until then we re-export rather than duplicate the enum.
pub use crate::buttplug::ConstrictionMethod;

/// One ordered shaping step inside a `ParameterLinkConfig.transforms`
/// vector. The variant payload carries everything the
/// `apply_transform` dispatch needs, except the modifier axis values
/// — those come from the resolver's pre-fetched modifier slice so a
/// transform can never escape its declared dependency set.
///
/// Serialization: tagged enum with a `type` discriminator + camelCase
/// field renames so the JSON shape on disk matches the frontend's
/// transform-editor model. `#[serde(rename_all = "kebab-case")]` on
/// the discriminator matches the rest of the project's wire-format
/// convention (`v2-balanced`, `s-curve`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum TransformConfig {
    // ====== Generic resolver-layer primitives ======
    /// Exponential moving average: `out = last + alpha*(in - last)` with
    /// `alpha = dt_ms / time_constant_ms` clamped to `[0, 1]`. The state
    /// holds `last_value` + `last_ts` so dt is recoverable per call.
    Smooth { time_constant_ms: f64 },

    /// Linear gain: `out = in * factor`.
    Scale { factor: f64 },

    /// Bounds the value: `out = in.clamp(min, max)`.
    Clamp { min: f64, max: f64 },

    /// Identity-flip: `out = 1.0 - in`. Pre-clamp; downstream `Clamp`
    /// can re-bound if needed.
    Invert,

    /// Peak-and-decay: outputs the running max, refreshed when a new
    /// peak arrives or the previous peak ages past `duration_ms`.
    /// Mirrors the `IntensityPeakHold` shape but per-parameter and
    /// transform-driven.
    Hold { duration_ms: u32 },

    /// Convex blend with another bus axis: `out = (1-w)*in + w*other`,
    /// where `other` is the resolver's pre-fetched value of `other_axis`
    /// at the link's `target_time`.
    Mix {
        #[serde(rename = "otherAxis")]
        other_axis: String,
        weight: f64,
    },

    // ====== Buttplug semantic wrappers (built atop the generics) ======
    /// High-frequency sinusoidal wobble around the input value. The
    /// modifier `speed_axis` (typically `bp:Vibrate_<i>`) drives
    /// frequency in `[0, 20]` Hz; `distance` sets amplitude.
    Vibrate {
        #[serde(rename = "speedAxis")]
        speed_axis: String,
        distance: f64,
    },

    /// Triangle-wave sweep around the input value. `speed_axis` drives
    /// frequency in `[0, max_speed_hz]`; `scale` sets amplitude.
    Oscillate {
        #[serde(rename = "speedAxis")]
        speed_axis: String,
        scale: f64,
        #[serde(rename = "maxSpeedHz")]
        max_speed_hz: f64,
    },

    /// Sawtooth directional sweep around the input value. `speed_axis`
    /// drives frequency in `[0, max_speed_hz]`; `direction_axis` carries a
    /// 0/1 bool (typically `bp:RotateDir_<i>`) where `>= 0.5` is clockwise
    /// (+sign) and `< 0.5` is counter-clockwise (-sign). `scale` sets
    /// amplitude. Mirrors the pre-refactor `process_buttplug_pipeline`'s
    /// Rotate stage so a saved Rotate-driven preset reproduces the same
    /// sweep on the new resolver path.
    Rotate {
        #[serde(rename = "speedAxis")]
        speed_axis: String,
        #[serde(rename = "directionAxis")]
        direction_axis: String,
        scale: f64,
        #[serde(rename = "maxSpeedHz")]
        max_speed_hz: f64,
    },

    /// Range-narrowing transform. The modifier `amount_axis` drives a
    /// 0..1 constriction strength: 0 leaves the full range alone, 1
    /// collapses to `min_floor`. `use_midpoint = false` centers the
    /// shrinking range on the input value (so the shrink follows the
    /// stroke); `true` pins it to 0.5. `method` chooses Downsample
    /// (remap into the bounds, preserves shape) vs Clamp (cut off at
    /// the bounds, can flat-spot).
    Constrict {
        #[serde(rename = "amountAxis")]
        amount_axis: String,
        #[serde(rename = "minFloor")]
        min_floor: f64,
        #[serde(rename = "useMidpoint")]
        use_midpoint: bool,
        method: ConstrictionMethod,
    },
}

impl TransformConfig {
    /// Bus axes this transform reads modifier values from. The resolver
    /// pre-fetches each at the link's `target_time` and passes the
    /// resolved values into `apply_transform` in declaration order, so
    /// the transform never touches the bus directly.
    pub fn declared_axes(&self) -> Vec<&str> {
        match self {
            // Pure shaping primitives that read no modifier axes.
            Self::Smooth { .. }
            | Self::Scale { .. }
            | Self::Clamp { .. }
            | Self::Invert
            | Self::Hold { .. } => Vec::new(),
            Self::Mix { other_axis, .. } => vec![other_axis.as_str()],
            Self::Vibrate { speed_axis, .. } | Self::Oscillate { speed_axis, .. } => {
                vec![speed_axis.as_str()]
            }
            Self::Rotate {
                speed_axis,
                direction_axis,
                ..
            } => vec![speed_axis.as_str(), direction_axis.as_str()],
            Self::Constrict { amount_axis, .. } => vec![amount_axis.as_str()],
        }
    }

    /// Build the matching `TransformState` slot for this config. Used by
    /// sub E's `ParameterLinkRuntime::for_config` (not yet wired) to
    /// initialize the runtime vector when a config changes. Variants
    /// that carry no per-tick state return `TransformState::None`.
    pub fn initial_state(&self) -> TransformState {
        match self {
            Self::Smooth { .. } => TransformState::Smooth {
                last_value: 0.0,
                last_ts: 0,
            },
            Self::Hold { .. } => TransformState::Hold {
                peak_value: 0.0,
                peak_ts: 0,
            },
            Self::Vibrate { .. } => TransformState::Vibrate {
                phase: 0.0,
                last_target: 0,
            },
            Self::Oscillate { .. } => TransformState::Oscillate {
                phase: 0.0,
                last_target: 0,
            },
            Self::Rotate { .. } => TransformState::Rotate {
                phase: 0.0,
                last_target: 0,
            },
            Self::Scale { .. }
            | Self::Clamp { .. }
            | Self::Invert
            | Self::Mix { .. }
            | Self::Constrict { .. } => TransformState::None,
        }
    }
}

/// Per-transform mutable state. One variant per `TransformConfig` variant
/// that needs to remember anything between resolver ticks (interpolation
/// positions, oscillator phases, smoothing accumulators, hold peaks).
/// Stateless variants (`Scale`, `Clamp`, `Invert`, `Mix`, `Constrict`)
/// share the `None` slot so the runtime vector stays a single uniform
/// `Vec<TransformState>` indexed alongside `Vec<TransformConfig>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum TransformState {
    /// Identity slot used by stateless transforms and as the default
    /// for fresh `ParameterLinkRuntime` instances.
    #[default]
    None,
    Smooth {
        last_value: f64,
        last_ts: u64,
    },
    Hold {
        peak_value: f64,
        peak_ts: u64,
    },
    Vibrate {
        phase: f64,
        last_target: u64,
    },
    Oscillate {
        phase: f64,
        last_target: u64,
    },
    Rotate {
        phase: f64,
        last_target: u64,
    },
}

/// Apply one transform to `value`, threading any per-tick state mutations
/// through `state`. `modifiers` carries the resolver's pre-fetched
/// values for `cfg.declared_axes()` in declaration order; an empty
/// slice means the transform reads no bus axes (any access via
/// `modifiers.first()` falls through to a documented default rather
/// than panicking, so a misordered resolver is loud at the call site
/// but doesn't crash mid-tick).
///
/// `target_time_ms` is the link's resolved target time
/// (`now - delay_ms`); transforms that need dt compute it from their
/// own state (`Smooth.last_ts`, `Vibrate.last_target`, etc.).
pub fn apply_transform(
    cfg: &TransformConfig,
    state: &mut TransformState,
    value: f64,
    modifiers: &[f64],
    target_time_ms: u64,
) -> f64 {
    match cfg {
        TransformConfig::Smooth { time_constant_ms } => {
            apply_smooth(*time_constant_ms, value, state, target_time_ms)
        }
        TransformConfig::Scale { factor } => value * factor,
        TransformConfig::Clamp { min, max } => value.clamp(*min, *max),
        TransformConfig::Invert => 1.0 - value,
        TransformConfig::Hold { duration_ms } => {
            apply_hold(*duration_ms, value, state, target_time_ms)
        }
        TransformConfig::Mix { weight, .. } => {
            let other = modifiers.first().copied().unwrap_or(0.0);
            let w = weight.clamp(0.0, 1.0);
            value * (1.0 - w) + other * w
        }
        TransformConfig::Vibrate { distance, .. } => {
            buttplug::apply_vibrate(*distance, value, modifiers, state, target_time_ms)
        }
        TransformConfig::Oscillate {
            scale,
            max_speed_hz,
            ..
        } => buttplug::apply_oscillate(
            *scale,
            *max_speed_hz,
            value,
            modifiers,
            state,
            target_time_ms,
        ),
        TransformConfig::Rotate {
            scale,
            max_speed_hz,
            ..
        } => buttplug::apply_rotate(
            *scale,
            *max_speed_hz,
            value,
            modifiers,
            state,
            target_time_ms,
        ),
        TransformConfig::Constrict {
            min_floor,
            use_midpoint,
            method,
            ..
        } => buttplug::apply_constrict(*min_floor, *use_midpoint, *method, value, modifiers),
    }
}

fn apply_smooth(
    time_constant_ms: f64,
    value: f64,
    state: &mut TransformState,
    target_ts: u64,
) -> f64 {
    let TransformState::Smooth { last_value, last_ts } = state else {
        // Caller used the wrong state slot (initial_state should have
        // produced `Smooth`). Rewrite the slot rather than panic — sub E's
        // resolver rebuild can recover on the next tick.
        *state = TransformState::Smooth {
            last_value: value,
            last_ts: target_ts,
        };
        return value;
    };
    if *last_ts == 0 {
        // First sample: prime the filter with the input rather than
        // ramping from zero, so the very first tick doesn't undershoot.
        *last_value = value;
        *last_ts = target_ts;
        return value;
    }
    let dt_ms = target_ts.saturating_sub(*last_ts) as f64;
    // Guard against zero / negative time constants — they'd produce
    // unbounded alpha and immediately collapse the filter to the input.
    let alpha = (dt_ms / time_constant_ms.max(1.0)).clamp(0.0, 1.0);
    let smoothed = *last_value + alpha * (value - *last_value);
    *last_value = smoothed;
    *last_ts = target_ts;
    smoothed
}

fn apply_hold(
    duration_ms: u32,
    value: f64,
    state: &mut TransformState,
    target_ts: u64,
) -> f64 {
    let TransformState::Hold { peak_value, peak_ts } = state else {
        *state = TransformState::Hold {
            peak_value: value,
            peak_ts: target_ts,
        };
        return value;
    };
    let age_ms = target_ts.saturating_sub(*peak_ts);
    if value >= *peak_value || age_ms > duration_ms as u64 {
        // New peak (or held value aged out) — restart the hold window.
        *peak_value = value;
        *peak_ts = target_ts;
    }
    *peak_value
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- declared_axes ----------

    #[test]
    fn declared_axes_empty_for_pure_primitives() {
        for cfg in [
            TransformConfig::Smooth {
                time_constant_ms: 100.0,
            },
            TransformConfig::Scale { factor: 2.0 },
            TransformConfig::Clamp { min: 0.0, max: 1.0 },
            TransformConfig::Invert,
            TransformConfig::Hold { duration_ms: 200 },
        ] {
            assert!(
                cfg.declared_axes().is_empty(),
                "{:?} reads no bus axes",
                cfg
            );
        }
    }

    #[test]
    fn declared_axes_reports_modifier_axis_names() {
        let mix = TransformConfig::Mix {
            other_axis: "L1".into(),
            weight: 0.3,
        };
        assert_eq!(mix.declared_axes(), vec!["L1"]);

        let vibrate = TransformConfig::Vibrate {
            speed_axis: "bp:Vibrate_0".into(),
            distance: 0.2,
        };
        assert_eq!(vibrate.declared_axes(), vec!["bp:Vibrate_0"]);

        let oscillate = TransformConfig::Oscillate {
            speed_axis: "bp:Oscillate_1".into(),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        assert_eq!(oscillate.declared_axes(), vec!["bp:Oscillate_1"]);

        // Rotate declares two axes — speed first, direction second. The
        // resolver's pre-fetch must preserve that order so
        // `apply_rotate` reads `modifiers[0]` as speed and
        // `modifiers[1]` as direction.
        let rotate = TransformConfig::Rotate {
            speed_axis: "bp:Rotate_0".into(),
            direction_axis: "bp:RotateDir_0".into(),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        assert_eq!(
            rotate.declared_axes(),
            vec!["bp:Rotate_0", "bp:RotateDir_0"]
        );

        let constrict = TransformConfig::Constrict {
            amount_axis: "bp:Constrict_0".into(),
            min_floor: 0.1,
            use_midpoint: true,
            method: ConstrictionMethod::Downsample,
        };
        assert_eq!(constrict.declared_axes(), vec!["bp:Constrict_0"]);
    }

    // ---------- initial_state ----------

    #[test]
    fn initial_state_matches_variant_shape() {
        // Stateless variants share the None slot so the runtime
        // vector layout stays uniform.
        for cfg in [
            TransformConfig::Scale { factor: 2.0 },
            TransformConfig::Clamp { min: 0.0, max: 1.0 },
            TransformConfig::Invert,
            TransformConfig::Mix {
                other_axis: "L0".into(),
                weight: 0.5,
            },
            TransformConfig::Constrict {
                amount_axis: "bp:Constrict_0".into(),
                min_floor: 0.0,
                use_midpoint: false,
                method: ConstrictionMethod::Clamp,
            },
        ] {
            assert_eq!(cfg.initial_state(), TransformState::None);
        }
        // Stateful variants get their matching slot, primed at zero.
        assert!(matches!(
            TransformConfig::Smooth {
                time_constant_ms: 100.0
            }
            .initial_state(),
            TransformState::Smooth {
                last_value: 0.0,
                last_ts: 0,
            }
        ));
        assert!(matches!(
            TransformConfig::Hold { duration_ms: 200 }.initial_state(),
            TransformState::Hold {
                peak_value: 0.0,
                peak_ts: 0,
            }
        ));
        assert!(matches!(
            TransformConfig::Vibrate {
                speed_axis: "x".into(),
                distance: 0.1,
            }
            .initial_state(),
            TransformState::Vibrate {
                phase: 0.0,
                last_target: 0,
            }
        ));
        assert!(matches!(
            TransformConfig::Oscillate {
                speed_axis: "x".into(),
                scale: 0.5,
                max_speed_hz: 5.0,
            }
            .initial_state(),
            TransformState::Oscillate {
                phase: 0.0,
                last_target: 0,
            }
        ));
        assert!(matches!(
            TransformConfig::Rotate {
                speed_axis: "x".into(),
                direction_axis: "y".into(),
                scale: 0.5,
                max_speed_hz: 5.0,
            }
            .initial_state(),
            TransformState::Rotate {
                phase: 0.0,
                last_target: 0,
            }
        ));
    }

    // ---------- generic apply ----------

    #[test]
    fn scale_multiplies_input_by_factor() {
        let cfg = TransformConfig::Scale { factor: 0.5 };
        let mut state = cfg.initial_state();
        assert_eq!(apply_transform(&cfg, &mut state, 0.6, &[], 0), 0.3);
    }

    #[test]
    fn clamp_bounds_input() {
        let cfg = TransformConfig::Clamp { min: 0.2, max: 0.8 };
        let mut state = cfg.initial_state();
        assert_eq!(apply_transform(&cfg, &mut state, 0.1, &[], 0), 0.2);
        assert_eq!(apply_transform(&cfg, &mut state, 0.5, &[], 0), 0.5);
        assert_eq!(apply_transform(&cfg, &mut state, 0.9, &[], 0), 0.8);
    }

    #[test]
    fn invert_flips_around_one() {
        let cfg = TransformConfig::Invert;
        let mut state = cfg.initial_state();
        assert_eq!(apply_transform(&cfg, &mut state, 0.3, &[], 0), 0.7);
        assert_eq!(apply_transform(&cfg, &mut state, 1.0, &[], 0), 0.0);
    }

    #[test]
    fn mix_blends_value_with_modifier_axis() {
        let cfg = TransformConfig::Mix {
            other_axis: "L1".into(),
            weight: 0.25,
        };
        let mut state = cfg.initial_state();
        // value=0.4, other=0.8, weight=0.25 → 0.4*0.75 + 0.8*0.25 = 0.5
        let out = apply_transform(&cfg, &mut state, 0.4, &[0.8], 0);
        assert!((out - 0.5).abs() < 1e-9);
    }

    #[test]
    fn mix_with_missing_modifier_treats_other_as_zero() {
        // Defensive default: a misordered resolver gives an empty slice.
        // Better to fall through to "other = 0" than to panic mid-tick.
        let cfg = TransformConfig::Mix {
            other_axis: "L1".into(),
            weight: 0.5,
        };
        let mut state = cfg.initial_state();
        let out = apply_transform(&cfg, &mut state, 0.6, &[], 0);
        assert!((out - 0.3).abs() < 1e-9);
    }

    #[test]
    fn smooth_first_sample_passes_through() {
        // Priming the filter with the first input prevents an
        // undershoot ramp from zero on the very first tick.
        let cfg = TransformConfig::Smooth {
            time_constant_ms: 200.0,
        };
        let mut state = cfg.initial_state();
        let out = apply_transform(&cfg, &mut state, 0.5, &[], 1000);
        assert_eq!(out, 0.5);
    }

    #[test]
    fn smooth_decays_toward_input_with_time_constant_alpha() {
        let cfg = TransformConfig::Smooth {
            time_constant_ms: 100.0,
        };
        let mut state = cfg.initial_state();
        // Prime at 0.0
        apply_transform(&cfg, &mut state, 0.0, &[], 1000);
        // Step to 1.0 with dt = 50ms → alpha = 0.5 → output = 0.5
        let half = apply_transform(&cfg, &mut state, 1.0, &[], 1050);
        assert!((half - 0.5).abs() < 1e-9, "expected 0.5, got {}", half);
        // Another 50ms at 1.0 → alpha = 0.5 again → output = 0.75
        let three_quarters = apply_transform(&cfg, &mut state, 1.0, &[], 1100);
        assert!(
            (three_quarters - 0.75).abs() < 1e-9,
            "expected 0.75, got {}",
            three_quarters
        );
    }

    #[test]
    fn smooth_clamps_alpha_when_dt_exceeds_time_constant() {
        // dt > time_constant means alpha would overshoot 1.0 and
        // produce a negative undershoot on the next sample. Guard
        // by clamping alpha at 1.0 (instant follow).
        let cfg = TransformConfig::Smooth {
            time_constant_ms: 50.0,
        };
        let mut state = cfg.initial_state();
        apply_transform(&cfg, &mut state, 0.0, &[], 0);
        let out = apply_transform(&cfg, &mut state, 1.0, &[], 500); // dt=500, tc=50
        assert!((out - 1.0).abs() < 1e-9);
    }

    #[test]
    fn hold_outputs_peak_until_age_exceeds_duration() {
        let cfg = TransformConfig::Hold { duration_ms: 200 };
        let mut state = cfg.initial_state();

        // First sample primes the peak.
        assert_eq!(apply_transform(&cfg, &mut state, 0.7, &[], 1000), 0.7);
        // Lower input keeps the peak as long as we're inside the window.
        assert_eq!(apply_transform(&cfg, &mut state, 0.3, &[], 1100), 0.7);
        // Higher input replaces the peak immediately.
        assert_eq!(apply_transform(&cfg, &mut state, 0.9, &[], 1150), 0.9);
        // After the window passes, the next lower input takes over.
        assert_eq!(apply_transform(&cfg, &mut state, 0.4, &[], 1500), 0.4);
    }
}
