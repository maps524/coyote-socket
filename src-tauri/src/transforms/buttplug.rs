//! Semantic Buttplug-era transform implementations.
//!
//! Each function here is the apply-side body for one
//! `TransformConfig::{Vibrate, Oscillate, Constrict}` variant. The enum
//! itself + its dispatch table live in `transforms::mod`; this file holds
//! the device-flavored math so the resolver-primitive boundary
//! (generic shaping vs Buttplug-shaped wrappers) is enforced by file
//! layout. Sub F deletes `crate::buttplug::pipeline` once these
//! semantic wrappers + the resolver rewrite cover everything the old
//! `process_buttplug_pipeline` did.
//!
//! Modifier-axis convention: the resolver pre-fetches every axis named
//! in `TransformConfig::declared_axes()` at the link's `target_time`
//! and passes the values into `apply_transform` as a slice. Each
//! function below documents the slot indexes it reads. A misordered or
//! short slice falls through to a documented zero/identity default —
//! the resolver should never produce that, but a hot path can't crash
//! mid-tick.

use std::f64::consts::PI;

use super::{ConstrictionMethod, TransformState};
use crate::modulation::lerp;

/// Pull a transform's phase + last-target slot out of `state`, compute the
/// elapsed `dt_ms` since the previous call, and bump the slot's
/// last_target. Returns `None` and rewrites the slot to the right shape
/// (with `init`) if the wrong variant arrived — sub E's resolver init
/// guarantees the right slot, but the rewrite is defense-in-depth so a
/// misconfiguration recovers within one tick rather than killing the
/// device loop.
///
/// The first call (`last_target == 0`) reports `dt_ms = 0` so the very
/// first tick of a Vibrate / Oscillate slot doesn't immediately wobble
/// — phase advances on the second call onward.
fn step_phase_state<'a>(
    state: &'a mut TransformState,
    target_ts: u64,
    init: TransformState,
) -> Option<(&'a mut f64, u64)> {
    let needs_rewrite = !matches!(
        state,
        TransformState::Vibrate { .. }
            | TransformState::Oscillate { .. }
            | TransformState::Rotate { .. }
    )
        // additionally require the variant tag matches the init slot.
        || std::mem::discriminant(state) != std::mem::discriminant(&init);
    if needs_rewrite {
        *state = init;
        return None;
    }
    let (phase, last_target) = match state {
        TransformState::Vibrate { phase, last_target }
        | TransformState::Oscillate { phase, last_target }
        | TransformState::Rotate { phase, last_target } => (phase, last_target),
        _ => unreachable!("matches! guard above ensures Vibrate / Oscillate / Rotate"),
    };
    let dt_ms = if *last_target == 0 {
        0
    } else {
        target_ts.saturating_sub(*last_target)
    };
    *last_target = target_ts;
    Some((phase, dt_ms))
}

/// Sinusoidal wobble around `value`. `modifiers[0]` is the speed input
/// (typically the resolver's read of `bp:Vibrate_<i>` at `target_ts`)
/// in `[0, 1]`; it scales the frequency to `[0, 20]` Hz. `distance`
/// sets amplitude. Phase advances by `freq_hz * dt_seconds * 2π` per
/// call where `dt = target_ts - state.last_target` (so a missed tick
/// catches up rather than freezing).
pub fn apply_vibrate(
    distance: f64,
    value: f64,
    modifiers: &[f64],
    state: &mut TransformState,
    target_ts: u64,
) -> f64 {
    let speed = modifiers.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
    let Some((phase, dt_ms)) = step_phase_state(
        state,
        target_ts,
        TransformState::Vibrate {
            phase: 0.0,
            last_target: target_ts,
        },
    ) else {
        return value;
    };
    let freq_hz = speed * 20.0;
    *phase += freq_hz * (dt_ms as f64 / 1000.0) * 2.0 * PI;
    let offset = phase.sin() * distance;
    value + offset
}

/// Triangle-wave sweep around `value`. `modifiers[0]` is the speed
/// input in `[0, 1]`; it scales the frequency to `[0, max_speed_hz]`
/// Hz. `scale` sets amplitude (the wave swings ±scale around `value`).
///
/// The wave shape mirrors the pre-refactor
/// `process_buttplug_pipeline`'s Oscillate stage: phase modulo 1.0,
/// triangle = `1 - |2*phase - 1|`, offset = `(triangle - 0.5) * 2 *
/// scale`, then add to `value`. Match-for-match so a saved preset that
/// previously routed through Oscillate and now routes through this
/// transform produces the same output.
pub fn apply_oscillate(
    scale: f64,
    max_speed_hz: f64,
    value: f64,
    modifiers: &[f64],
    state: &mut TransformState,
    target_ts: u64,
) -> f64 {
    let speed = modifiers.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
    let Some((phase, dt_ms)) = step_phase_state(
        state,
        target_ts,
        TransformState::Oscillate {
            phase: 0.0,
            last_target: target_ts,
        },
    ) else {
        return value;
    };
    let freq_hz = speed * max_speed_hz;
    *phase += freq_hz * (dt_ms as f64 / 1000.0);

    let phase_norm = *phase % 1.0;
    let triangle = 1.0 - (2.0 * phase_norm - 1.0).abs();
    let offset = (triangle - 0.5) * 2.0 * scale;
    value + offset
}

/// Sawtooth directional sweep around `value`. `modifiers[0]` is the
/// speed input in `[0, 1]`; it scales the frequency to
/// `[0, max_speed_hz]` Hz. `modifiers[1]` is the direction axis: a value
/// `>= 0.5` is clockwise (+sign), `< 0.5` is counter-clockwise (-sign).
/// `scale` sets amplitude (the wave swings `0..scale` in the chosen
/// direction).
///
/// Mirrors the pre-refactor `process_buttplug_pipeline`'s Rotate stage:
/// `phase % 1.0 * scale * direction`, added to `value`. The modifier
/// slot order matches `declared_axes()` (`speed_axis` first,
/// `direction_axis` second) so the resolver pre-fetch always lands the
/// values in the right slot.
pub fn apply_rotate(
    scale: f64,
    max_speed_hz: f64,
    value: f64,
    modifiers: &[f64],
    state: &mut TransformState,
    target_ts: u64,
) -> f64 {
    let speed = modifiers.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
    let direction = if modifiers.get(1).copied().unwrap_or(1.0) >= 0.5 {
        1.0
    } else {
        -1.0
    };
    let Some((phase, dt_ms)) = step_phase_state(
        state,
        target_ts,
        TransformState::Rotate {
            phase: 0.0,
            last_target: target_ts,
        },
    ) else {
        return value;
    };
    let freq_hz = speed * max_speed_hz;
    *phase += freq_hz * (dt_ms as f64 / 1000.0);

    let sawtooth = *phase % 1.0;
    let offset = sawtooth * scale * direction;
    value + offset
}

/// Range-narrowing transform. `modifiers[0]` is the constriction
/// strength in `[0, 1]`: 0 leaves the full range alone, 1 collapses
/// to `min_floor`. `use_midpoint = false` centers the shrinking range
/// on `value` so the bounds follow the input; `true` pins to 0.5.
/// `method` chooses Downsample (remap `[0, 1]` into `[min_bound,
/// max_bound]`, preserves shape) vs Clamp (cut off at the bounds, can
/// flat-spot near saturation).
///
/// Stateless — the variant slot stays `TransformState::None`, no
/// per-call mutation.
pub fn apply_constrict(
    min_floor: f64,
    use_midpoint: bool,
    method: ConstrictionMethod,
    value: f64,
    modifiers: &[f64],
) -> f64 {
    let constriction = modifiers.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);

    // effective range: at constriction=0 → 1.0 (full); at 1.0 → min_floor.
    let effective = lerp(1.0, min_floor, constriction);
    let center = if use_midpoint { 0.5 } else { value };
    let half_range = effective * 0.5;
    let min_bound = (center - half_range).max(0.0);
    let max_bound = (center + half_range).min(1.0);

    match method {
        ConstrictionMethod::Downsample => {
            let normalized = value.clamp(0.0, 1.0);
            min_bound + normalized * (max_bound - min_bound)
        }
        ConstrictionMethod::Clamp => value.clamp(min_bound, max_bound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transforms::{apply_transform, TransformConfig};

    #[test]
    fn vibrate_first_call_passes_value_through() {
        // First call has no prior target_ts, so dt = 0 → phase stays
        // at 0 → sin(0) = 0 → output equals input. Prevents an
        // initial-tick wobble that would surprise users.
        let cfg = TransformConfig::Vibrate {
            speed_axis: "bp:Vibrate_0".into(),
            distance: 0.2,
        };
        let mut state = cfg.initial_state();
        let out = apply_transform(&cfg, &mut state, 0.5, &[1.0], 1_000);
        assert!((out - 0.5).abs() < 1e-9);
    }

    #[test]
    fn vibrate_offsets_output_after_phase_advance() {
        let cfg = TransformConfig::Vibrate {
            speed_axis: "bp:Vibrate_0".into(),
            distance: 0.2,
        };
        let mut state = cfg.initial_state();
        // Prime at t=1000 (no offset).
        apply_transform(&cfg, &mut state, 0.5, &[1.0], 1_000);
        // 25ms at speed=1.0 → freq=20Hz → phase += 20 * 0.025 * 2π ≈ π.
        // sin(π) ≈ 0, so this lands close to the input. Pick a dt that
        // produces a clearer non-zero offset: 12.5ms → phase ≈ π/2 → sin=1.
        let out = apply_transform(&cfg, &mut state, 0.5, &[1.0], 1_012);
        assert!(
            (out - 0.5).abs() > 0.05,
            "vibrate should add a non-trivial offset by phase ~π/2, got {}",
            out
        );
    }

    #[test]
    fn vibrate_speed_zero_holds_phase_so_offset_stays_zero() {
        let cfg = TransformConfig::Vibrate {
            speed_axis: "bp:Vibrate_0".into(),
            distance: 0.5,
        };
        let mut state = cfg.initial_state();
        apply_transform(&cfg, &mut state, 0.4, &[0.0], 1_000);
        for ts in (1_010..=1_500).step_by(10) {
            let out = apply_transform(&cfg, &mut state, 0.4, &[0.0], ts);
            assert!(
                (out - 0.4).abs() < 1e-12,
                "speed=0 should freeze phase, got {} at ts={}",
                out,
                ts
            );
        }
    }

    #[test]
    fn oscillate_first_call_lands_at_phase_zero_trough() {
        // The triangle wave's value at phase 0 is the trough, so even
        // with dt=0 (no phase advance) the offset = (0-0.5)*2*scale =
        // -scale. This is intentional: phase 0 is the start of the
        // sweep cycle, not a "pass-through" identity. Vibrate's sin(0)
        // = 0 gives identity on the first call; Oscillate's triangle(0)
        // = 0 does not. Document the asymmetry rather than mask it.
        let cfg = TransformConfig::Oscillate {
            speed_axis: "bp:Oscillate_0".into(),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        let mut state = cfg.initial_state();
        let out = apply_transform(&cfg, &mut state, 0.5, &[1.0], 1_000);
        assert!(
            (out - 0.1).abs() < 1e-9,
            "oscillate at phase 0 should land at value-scale, got {}",
            out
        );
    }

    #[test]
    fn oscillate_advances_phase_with_speed_and_dt() {
        let cfg = TransformConfig::Oscillate {
            speed_axis: "bp:Oscillate_0".into(),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        let mut state = cfg.initial_state();
        // Prime at t=1000.
        apply_transform(&cfg, &mut state, 0.5, &[1.0], 1_000);
        // 100ms at max speed=5Hz → phase += 5 * 0.1 = 0.5.
        // triangle(phase=0.5) = 1.0 → offset = (1-0.5)*2*0.4 = 0.4.
        let out = apply_transform(&cfg, &mut state, 0.5, &[1.0], 1_100);
        assert!(
            (out - 0.9).abs() < 1e-9,
            "oscillate at phase 0.5 should land at value+scale, got {}",
            out
        );
    }

    #[test]
    fn constrict_downsample_remaps_around_midpoint() {
        // constriction=0.5, min_floor=0.0, use_midpoint=true:
        // effective = 0.5, bounds = [0.25, 0.75].
        // Input 0.0 → 0.25; 0.5 → 0.5; 1.0 → 0.75.
        let cfg = TransformConfig::Constrict {
            amount_axis: "bp:Constrict_0".into(),
            min_floor: 0.0,
            use_midpoint: true,
            method: ConstrictionMethod::Downsample,
        };
        let mut state = cfg.initial_state();
        assert!((apply_transform(&cfg, &mut state, 0.0, &[0.5], 0) - 0.25).abs() < 1e-9);
        assert!((apply_transform(&cfg, &mut state, 0.5, &[0.5], 0) - 0.5).abs() < 1e-9);
        assert!((apply_transform(&cfg, &mut state, 1.0, &[0.5], 0) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn constrict_clamp_cuts_off_at_bounds() {
        let cfg = TransformConfig::Constrict {
            amount_axis: "bp:Constrict_0".into(),
            min_floor: 0.0,
            use_midpoint: true,
            method: ConstrictionMethod::Clamp,
        };
        let mut state = cfg.initial_state();
        // bounds = [0.25, 0.75]. Inside-band input survives; outside snaps.
        assert!((apply_transform(&cfg, &mut state, 0.1, &[0.5], 0) - 0.25).abs() < 1e-9);
        assert!((apply_transform(&cfg, &mut state, 0.5, &[0.5], 0) - 0.5).abs() < 1e-9);
        assert!((apply_transform(&cfg, &mut state, 0.9, &[0.5], 0) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn rotate_first_call_passes_value_through() {
        // First call: dt_ms = 0 → phase stays at 0 → sawtooth(0) = 0 →
        // offset = 0 → output = input. Same identity guarantee Vibrate
        // gives on its priming tick, so a Rotate-driven preset doesn't
        // jump on the first frame.
        let cfg = TransformConfig::Rotate {
            speed_axis: "bp:Rotate_0".into(),
            direction_axis: "bp:RotateDir_0".into(),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        let mut state = cfg.initial_state();
        let out = apply_transform(&cfg, &mut state, 0.5, &[1.0, 1.0], 1_000);
        assert!((out - 0.5).abs() < 1e-9);
    }

    #[test]
    fn rotate_advances_phase_clockwise() {
        let cfg = TransformConfig::Rotate {
            speed_axis: "bp:Rotate_0".into(),
            direction_axis: "bp:RotateDir_0".into(),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        let mut state = cfg.initial_state();
        // Prime at t=1000 (no offset).
        apply_transform(&cfg, &mut state, 0.5, &[1.0, 1.0], 1_000);
        // 100ms at speed=1, max=5Hz → phase += 0.5. sawtooth(0.5) = 0.5.
        // offset = 0.5 * 0.4 * 1 (clockwise) = 0.2 → output = 0.7.
        let out = apply_transform(&cfg, &mut state, 0.5, &[1.0, 1.0], 1_100);
        assert!(
            (out - 0.7).abs() < 1e-9,
            "rotate clockwise should land at value+0.2, got {}",
            out
        );
    }

    #[test]
    fn rotate_direction_axis_below_threshold_flips_sign() {
        // direction modifier < 0.5 → counter-clockwise → offset is
        // subtracted. Same speed / dt as the clockwise test, opposite sign.
        let cfg = TransformConfig::Rotate {
            speed_axis: "bp:Rotate_0".into(),
            direction_axis: "bp:RotateDir_0".into(),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        let mut state = cfg.initial_state();
        apply_transform(&cfg, &mut state, 0.5, &[1.0, 0.0], 1_000);
        let out = apply_transform(&cfg, &mut state, 0.5, &[1.0, 0.0], 1_100);
        assert!(
            (out - 0.3).abs() < 1e-9,
            "rotate ccw should land at value-0.2, got {}",
            out
        );
    }

    #[test]
    fn rotate_missing_direction_modifier_defaults_clockwise() {
        // Resolver pre-fetches `declared_axes()` in order. If the bus
        // lacks the direction axis the slot still gets a value (default
        // 0.0 from the resolver's `unwrap_or(0.0)`), which is < 0.5 and
        // would go counter-clockwise. The transform's own
        // `modifiers.get(1)` fallback (`1.0` = clockwise) only kicks in
        // when the slice is short — defense-in-depth for a misordered
        // resolver.
        let cfg = TransformConfig::Rotate {
            speed_axis: "bp:Rotate_0".into(),
            direction_axis: "bp:RotateDir_0".into(),
            scale: 0.4,
            max_speed_hz: 5.0,
        };
        let mut state = cfg.initial_state();
        // Pass only the speed slot — direction is missing entirely.
        apply_transform(&cfg, &mut state, 0.5, &[1.0], 1_000);
        let out = apply_transform(&cfg, &mut state, 0.5, &[1.0], 1_100);
        assert!(
            (out - 0.7).abs() < 1e-9,
            "missing direction slot should default to clockwise, got {}",
            out
        );
    }

    #[test]
    fn constrict_zero_strength_leaves_input_alone() {
        // constriction=0.0 → effective=1.0 → bounds=[0,1] (when
        // use_midpoint=true) or centered on the input. Either way,
        // a downsample pass with input 0.4 should hit ~0.4.
        let cfg = TransformConfig::Constrict {
            amount_axis: "bp:Constrict_0".into(),
            min_floor: 0.0,
            use_midpoint: true,
            method: ConstrictionMethod::Downsample,
        };
        let mut state = cfg.initial_state();
        let out = apply_transform(&cfg, &mut state, 0.4, &[0.0], 0);
        assert!((out - 0.4).abs() < 1e-9, "expected 0.4, got {}", out);
    }
}
