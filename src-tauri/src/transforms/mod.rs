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
//! Sub D introduced the variants + apply dispatch + tests; sub E's
//! resolver rewrite wired `ParameterLinkConfig.transforms` into the
//! per-tick loop; sub F deleted the old `process_buttplug_pipeline` and
//! moved `ConstrictionMethod` here as the canonical home.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub mod buttplug;

/// A transform modifier input: either a fixed constant the user dials in,
/// or a live bus axis the resolver pre-fetches.
///
/// This replaces the bare `String` axis fields the Buttplug-wrapper
/// variants (`Vibrate`, `Oscillate`, `Rotate`, `Constrict`) used to
/// carry. Those fields were a holdover from the Buttplug pipeline, where
/// a device emitted speed / direction / amount as *separate* command
/// streams. When a user hand-builds a link there is no second stream, so
/// forcing an axis made the field a required-but-meaningless text box
/// (red until filled, silently `unwrap_or(0.0)` at runtime). A `Const`
/// covers the common "vibrate at a fixed rate" case; promoting to `Axis`
/// recovers the live-feed behavior.
///
/// ## Wire format
///
/// Serializes as a bare JSON **number** (`Const`) or **string** (`Axis`)
/// — no `{kind: ...}` wrapper. The two are unambiguous (a constant is
/// numeric, an axis name is a string) and JS distinguishes them with a
/// `typeof` check, so the editor model stays `number | string`.
///
/// Deserialization also accepts the legacy bare axis string saved by
/// pre-migration presets, so an old `"speedAxis": "bp:Vibrate_0"` loads
/// as `Axis("bp:Vibrate_0")` unchanged. An empty axis string resolves to
/// `0.0` (the same "unset" behavior the old `unwrap_or(0.0)` gave).
#[derive(Debug, Clone, PartialEq)]
pub enum ScalarInput {
    /// Fixed value dialed in by the user (slider / toggle).
    Const(f64),
    /// Live bus axis read at the link's `target_time`. Empty name = unset.
    Axis(String),
}

impl ScalarInput {
    /// Resolve to a modifier value. `Const` returns its literal; `Axis`
    /// fetches via the closure. An empty axis name short-circuits to
    /// `0.0` without calling `fetch`, matching the pre-migration "unset
    /// axis reads zero" behavior.
    pub fn value(&self, fetch: &mut impl FnMut(&str) -> f64) -> f64 {
        match self {
            ScalarInput::Const(v) => *v,
            ScalarInput::Axis(a) if a.is_empty() => 0.0,
            ScalarInput::Axis(a) => fetch(a),
        }
    }

    /// The bus axis this input depends on, if any. `Const` and empty
    /// `Axis` return `None` — they create no bus dependency.
    pub fn axis(&self) -> Option<&str> {
        match self {
            ScalarInput::Axis(a) if !a.is_empty() => Some(a),
            _ => None,
        }
    }
}

impl Serialize for ScalarInput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ScalarInput::Const(v) => serializer.serialize_f64(*v),
            ScalarInput::Axis(a) => serializer.serialize_str(a),
        }
    }
}

impl<'de> Deserialize<'de> for ScalarInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct InputVisitor;

        impl<'de> de::Visitor<'de> for InputVisitor {
            type Value = ScalarInput;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a number (constant) or a string (bus axis name)")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<ScalarInput, E> {
                Ok(ScalarInput::Axis(v.to_string()))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<ScalarInput, E> {
                Ok(ScalarInput::Axis(v))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<ScalarInput, E> {
                Ok(ScalarInput::Const(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<ScalarInput, E> {
                Ok(ScalarInput::Const(v as f64))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<ScalarInput, E> {
                Ok(ScalarInput::Const(v as f64))
            }
        }

        deserializer.deserialize_any(InputVisitor)
    }
}

/// Method for applying Constrict bounds. Sub F moved this enum from
/// `crate::buttplug::types` into the transforms layer so the resolver
/// owns its own type. Saved presets that referenced the old wire path
/// round-trip identically — `#[serde(rename_all = ...)]` is unchanged
/// from the pre-refactor definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstrictionMethod {
    /// Remap 0.0-1.0 input to constrained range (preserves relative position).
    Downsample,
    /// Cut off values outside bounds (can cause flat spots).
    Clamp,
}

impl Default for ConstrictionMethod {
    fn default() -> Self {
        ConstrictionMethod::Downsample
    }
}

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
    /// High-frequency sinusoidal wobble around the input value. `speed`
    /// (a constant or the resolver's read of e.g. `bp:Vibrate_<i>`)
    /// drives frequency in `[0, 20]` Hz; `distance` sets amplitude. The
    /// `speedAxis` serde alias keeps pre-migration presets loading.
    Vibrate {
        #[serde(alias = "speedAxis")]
        speed: ScalarInput,
        distance: f64,
    },

    /// Triangle-wave sweep around the input value. `speed` drives
    /// frequency in `[0, max_speed_hz]`; `scale` sets amplitude.
    Oscillate {
        #[serde(alias = "speedAxis")]
        speed: ScalarInput,
        scale: f64,
        #[serde(rename = "maxSpeedHz")]
        max_speed_hz: f64,
    },

    /// Sawtooth directional sweep around the input value. `speed` drives
    /// frequency in `[0, max_speed_hz]`; `direction` carries a 0/1 value
    /// where `>= 0.5` is clockwise (+sign) and `< 0.5` is
    /// counter-clockwise (-sign) — typically a constant CW/CCW toggle,
    /// but linkable to e.g. `bp:RotateDir_<i>`. `scale` sets amplitude.
    /// Mirrors the pre-refactor `process_buttplug_pipeline`'s Rotate
    /// stage so a saved Rotate-driven preset reproduces the same sweep.
    Rotate {
        #[serde(alias = "speedAxis")]
        speed: ScalarInput,
        #[serde(alias = "directionAxis")]
        direction: ScalarInput,
        scale: f64,
        #[serde(rename = "maxSpeedHz")]
        max_speed_hz: f64,
    },

    /// Range-narrowing transform. `amount` drives a 0..1 constriction
    /// strength: 0 leaves the full range alone, 1 collapses to
    /// `min_floor`. `use_midpoint = false` centers the shrinking range
    /// on the input value (so the shrink follows the stroke); `true`
    /// pins it to 0.5. `method` chooses Downsample (remap into the
    /// bounds, preserves shape) vs Clamp (cut off at the bounds, can
    /// flat-spot).
    Constrict {
        #[serde(alias = "amountAxis")]
        amount: ScalarInput,
        #[serde(rename = "minFloor")]
        min_floor: f64,
        #[serde(rename = "useMidpoint")]
        use_midpoint: bool,
        method: ConstrictionMethod,
    },
}

impl TransformConfig {
    /// Build the modifier slice `apply_transform` expects, in the fixed
    /// per-variant slot order it reads (`Rotate` = `[speed, direction]`,
    /// etc.). Each `ScalarInput` slot resolves to its constant or, for an
    /// `Axis`, the value `fetch` returns for that axis name. `Mix`'s
    /// single `other_axis` is always a bus read (empty name → 0.0).
    ///
    /// Unlike the old `declared_axes()` this keeps slot positions stable
    /// regardless of which inputs are constants — a `Rotate` with a const
    /// speed and a linked direction still lands the speed at slot 0 and
    /// the direction at slot 1. The resolver passes
    /// `|axis| bus.value_at(axis, target).unwrap_or(0.0)`; no transform
    /// touches the bus directly.
    pub fn resolve_modifiers(&self, mut fetch: impl FnMut(&str) -> f64) -> Vec<f64> {
        match self {
            // Pure shaping primitives that read no modifier values.
            Self::Smooth { .. }
            | Self::Scale { .. }
            | Self::Clamp { .. }
            | Self::Invert
            | Self::Hold { .. } => Vec::new(),
            Self::Mix { other_axis, .. } => {
                let v = if other_axis.is_empty() {
                    0.0
                } else {
                    fetch(other_axis)
                };
                vec![v]
            }
            Self::Vibrate { speed, .. } | Self::Oscillate { speed, .. } => {
                vec![speed.value(&mut fetch)]
            }
            Self::Rotate {
                speed, direction, ..
            } => vec![speed.value(&mut fetch), direction.value(&mut fetch)],
            Self::Constrict { amount, .. } => vec![amount.value(&mut fetch)],
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
/// through `state`. `modifiers` carries the resolver's resolved
/// values from `cfg.resolve_modifiers()` in slot order; an empty
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

    // ---------- resolve_modifiers ----------

    /// A `fetch` closure that maps every axis name to a fixed sentinel so
    /// tests can tell an axis-sourced slot apart from a constant one.
    fn fetch_const(v: f64) -> impl FnMut(&str) -> f64 {
        move |_axis: &str| v
    }

    #[test]
    fn resolve_modifiers_empty_for_pure_primitives() {
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
                cfg.resolve_modifiers(fetch_const(0.9)).is_empty(),
                "{:?} reads no modifiers",
                cfg
            );
        }
    }

    #[test]
    fn resolve_modifiers_fetches_axis_inputs() {
        // Mix's other_axis is always a bus read.
        let mix = TransformConfig::Mix {
            other_axis: "L1".into(),
            weight: 0.3,
        };
        assert_eq!(mix.resolve_modifiers(fetch_const(0.7)), vec![0.7]);

        let vibrate = TransformConfig::Vibrate {
            speed: ScalarInput::Axis("bp:Vibrate_0".into()),
            distance: 0.2,
        };
        assert_eq!(vibrate.resolve_modifiers(fetch_const(0.5)), vec![0.5]);

        // Rotate keeps slot order — speed at [0], direction at [1] — even
        // when fetched from the bus, so `apply_rotate` reads them right.
        let rotate = TransformConfig::Rotate {
            speed: ScalarInput::Axis("bp:Rotate_0".into()),
            direction: ScalarInput::Axis("bp:RotateDir_0".into()),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        assert_eq!(rotate.resolve_modifiers(fetch_const(0.8)), vec![0.8, 0.8]);
    }

    #[test]
    fn resolve_modifiers_uses_constants_without_fetching() {
        // A Const slot returns its literal and never calls `fetch` — the
        // closure here panics if touched, proving constants short-circuit.
        let panic_fetch = |_axis: &str| -> f64 { panic!("const must not fetch the bus") };
        let vibrate = TransformConfig::Vibrate {
            speed: ScalarInput::Const(0.25),
            distance: 0.2,
        };
        assert_eq!(vibrate.resolve_modifiers(panic_fetch), vec![0.25]);
    }

    #[test]
    fn resolve_modifiers_keeps_slot_order_for_mixed_const_and_axis() {
        // The whole point of the fixed-slot contract: a const speed and a
        // linked direction must still land speed at [0], direction at [1].
        let rotate = TransformConfig::Rotate {
            speed: ScalarInput::Const(0.3),
            direction: ScalarInput::Axis("bp:RotateDir_0".into()),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        // Direction axis fetches to 1.0; speed is the constant 0.3.
        assert_eq!(rotate.resolve_modifiers(fetch_const(1.0)), vec![0.3, 1.0]);
    }

    #[test]
    fn resolve_modifiers_empty_axis_reads_zero() {
        // An unset (empty) axis name resolves to 0.0 without fetching,
        // matching the pre-migration "unset axis reads zero" behavior.
        let panic_fetch = |_axis: &str| -> f64 { panic!("empty axis must not fetch") };
        let vibrate = TransformConfig::Vibrate {
            speed: ScalarInput::Axis(String::new()),
            distance: 0.2,
        };
        assert_eq!(vibrate.resolve_modifiers(panic_fetch), vec![0.0]);
    }

    #[test]
    fn scalar_input_round_trips_const_as_number_and_axis_as_string() {
        // Wire format: Const → bare number, Axis → bare string. A legacy
        // bare axis string also deserializes back to Axis.
        let c: ScalarInput = serde_json::from_str("0.5").unwrap();
        assert_eq!(c, ScalarInput::Const(0.5));
        let a: ScalarInput = serde_json::from_str("\"bp:Vibrate_0\"").unwrap();
        assert_eq!(a, ScalarInput::Axis("bp:Vibrate_0".into()));
        assert_eq!(serde_json::to_string(&ScalarInput::Const(0.5)).unwrap(), "0.5");
        assert_eq!(
            serde_json::to_string(&ScalarInput::Axis("L1".into())).unwrap(),
            "\"L1\""
        );
    }

    #[test]
    fn legacy_speed_axis_key_deserializes_into_speed() {
        // Pre-migration presets stored `speedAxis` as a bare string. The
        // serde alias + ScalarInput's string visitor load it as an Axis.
        let json = r#"{"type":"vibrate","speedAxis":"bp:Vibrate_0","distance":0.3}"#;
        let cfg: TransformConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            cfg,
            TransformConfig::Vibrate {
                speed: ScalarInput::Axis("bp:Vibrate_0".into()),
                distance: 0.3,
            }
        );
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
                amount: ScalarInput::Axis("bp:Constrict_0".into()),
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
                speed: ScalarInput::Axis("x".into()),
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
                speed: ScalarInput::Axis("x".into()),
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
                speed: ScalarInput::Axis("x".into()),
                direction: ScalarInput::Axis("y".into()),
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
