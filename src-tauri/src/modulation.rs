// Parameter Modulation Module
// Handles dynamic parameter linking to T-Code axes with curve transformations

use serde::{Deserialize, Serialize};

use crate::transforms::{apply_transform, TransformConfig, TransformState};

/// Source type for a parameter value
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ParameterSourceType {
    Static,
    Linked,
}

/// Curve transformation types for linked parameters
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CurveType {
    Linear,
    Exponential,
    Logarithmic,
    SCurve,
    Inverse,
}

/// Behavior when a linked axis has no incoming data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NoInputBehavior {
    Hold,
    Default,
    Decay,
    Zero,
}

/// Configuration for a single parameter's link to a bus axis (or its
/// static-value fallback). The serialized half of the
/// `ParameterLinkConfig` + `ParameterLinkRuntime` split — fields here are
/// the user-facing knobs that round-trip through `settings.json` /
/// `presets.json`. Mutable transform / interpolation state lives on
/// `ParameterLinkRuntime` so it can never accidentally be persisted.
///
/// Pre-refactor name: `ParameterSource`. The rename matches the resolver
/// terminology in the plan doc; the struct shape post-sub-F is the
/// resolver-side input (no Buttplug-link payload — sub E translates
/// settings-side `buttplug_links` into `transforms` at load time).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterLinkConfig {
    #[serde(rename = "type")]
    pub source_type: ParameterSourceType,

    // For 'static' mode
    #[serde(skip_serializing_if = "Option::is_none")]
    pub static_value: Option<f64>,

    // For 'linked' mode
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_axis: Option<String>,
    pub range_min: f64,
    pub range_max: f64,
    pub curve: CurveType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve_strength: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub midpoint: Option<bool>, // If true, input is distance from center (0.5 → 0, 0 or 1 → 1)
    /// Input delay in ms. The resolver looks up axis history at `now - delay_ms`
    /// instead of the latest sample, so the channel "chases" live input. 0/None
    /// = real-time. Capped at AXIS_HISTORY_MS by the lookup window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u32>,

    /// Ordered list of post-curve / pre-range shaping transforms. Each
    /// entry's `resolve_modifiers()` builds its modifier slice (constants
    /// inline, axes pre-fetched at this link's `target_time`) before
    /// calling `apply_transform`. `#[serde(default)]` so saved presets
    /// that pre-date sub D deserialize cleanly with an empty vec.
    ///
    /// Sub D introduces the field + the transform variants but no
    /// caller reads it yet — sub E's resolver rewrite is the consumer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transforms: Vec<TransformConfig>,
}

impl ParameterLinkConfig {
    /// Create a static parameter source
    pub fn static_source(value: f64) -> Self {
        Self {
            source_type: ParameterSourceType::Static,
            static_value: Some(value),
            source_axis: None,
            range_min: 0.0,
            range_max: 0.0,
            curve: CurveType::Linear,
            curve_strength: None,
            midpoint: None,
            delay_ms: None,
            transforms: Vec::new(),
        }
    }

    /// The link's output range with its endpoints in ascending order.
    ///
    /// A transposed range (`range_min > range_max`) is a data-entry
    /// mistake, not a request to invert: the UI's own bound adjusters
    /// clamp each endpoint against the other so it cannot be produced
    /// through the app, and `curve: Inverse` is the supported way to
    /// make output fall as input rises. Left unordered it is actively
    /// dangerous — `200..0` maps a resting axis to full device output.
    ///
    /// Ordering here (and identically in `device::scale_intensity`)
    /// keeps every range-mapping path in the codebase reading a
    /// transposed range the same way, so a config that slips past the
    /// UI — a hand-edited `presets.json`, an older preset file — cannot
    /// make the resolver and the device path disagree.
    pub fn ordered_range(&self) -> (f64, f64) {
        if self.range_min <= self.range_max {
            (self.range_min, self.range_max)
        } else {
            (self.range_max, self.range_min)
        }
    }

    /// Create a linked parameter source
    pub fn linked_source(axis: &str, min: f64, max: f64, curve: CurveType) -> Self {
        Self {
            source_type: ParameterSourceType::Linked,
            static_value: None,
            source_axis: Some(axis.to_string()),
            range_min: min,
            range_max: max,
            curve,
            curve_strength: Some(2.0),
            midpoint: None,
            delay_ms: None,
            transforms: Vec::new(),
        }
    }
}

/// Mutable per-parameter runtime state. Holds whatever a transform pipeline
/// needs to remember between resolver ticks (interpolation positions,
/// oscillator phases, smoothing accumulators, hold peaks). One
/// `TransformState` slot per `TransformConfig` in the same index order;
/// `resolve_link_at_time` reconciles the slot count on entry so a config
/// edit that races a tick can't cause out-of-bounds indexing.
///
/// Lives on `Channel`, not on `ParameterLinkConfig`, so it can never be
/// serialized into a preset by accident.
#[derive(Debug, Clone, Default)]
pub struct ParameterLinkRuntime {
    pub transform_state: Vec<TransformState>,
}

impl ParameterLinkRuntime {
    /// Build a runtime whose `transform_state` slots match `cfg.transforms`
    /// in length and variant. Used by `apply_channel_config_to_state` to
    /// reset state when a channel's config changes — without the reset, a
    /// preset switch would carry over the previous channel's oscillator
    /// phases / smoothing accumulators into the new transforms.
    pub fn for_config(cfg: &ParameterLinkConfig) -> Self {
        Self {
            transform_state: cfg.transforms.iter().map(|t| t.initial_state()).collect(),
        }
    }
}

/// Per-channel bundle of `ParameterLinkRuntime` slots — one per parameter
/// the resolver writes (`frequency`, `frequency_balance`,
/// `intensity_balance`, `intensity`). Mirrors the field layout of
/// `ChannelConfig` so each `ParameterLinkConfig` has a co-indexed runtime
/// neighbor.
#[derive(Debug, Clone, Default)]
pub struct ChannelLinkRuntime {
    pub frequency: ParameterLinkRuntime,
    pub frequency_balance: ParameterLinkRuntime,
    pub intensity_balance: ParameterLinkRuntime,
    pub intensity: ParameterLinkRuntime,
}

impl ChannelLinkRuntime {
    /// Build a runtime whose four parameter slots match `config`'s four
    /// `transforms` lists. Called by `apply_channel_config_to_state` so
    /// each config write to a channel resets phase / smoothing /
    /// hold-peak state in lockstep with the new transform list.
    pub fn for_config(config: &ChannelConfig) -> Self {
        Self {
            frequency: ParameterLinkRuntime::for_config(&config.frequency),
            frequency_balance: ParameterLinkRuntime::for_config(&config.frequency_balance),
            intensity_balance: ParameterLinkRuntime::for_config(&config.intensity_balance),
            intensity: ParameterLinkRuntime::for_config(&config.intensity),
        }
    }
}

/// Complete configuration for a single channel's parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConfig {
    pub frequency: ParameterLinkConfig,
    pub frequency_balance: ParameterLinkConfig,
    pub intensity_balance: ParameterLinkConfig,
    pub intensity: ParameterLinkConfig,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            frequency: ParameterLinkConfig::static_source(100.0),
            frequency_balance: ParameterLinkConfig::static_source(128.0),
            intensity_balance: ParameterLinkConfig::static_source(128.0),
            intensity: ParameterLinkConfig::linked_source("L0", 10.0, 20.0, CurveType::Linear),
        }
    }
}

impl ChannelConfig {
    /// Create default configuration for Channel A (linked to L0)
    pub fn channel_a_default() -> Self {
        Self {
            frequency: ParameterLinkConfig::static_source(100.0),
            frequency_balance: ParameterLinkConfig::static_source(128.0),
            intensity_balance: ParameterLinkConfig::static_source(128.0),
            intensity: ParameterLinkConfig::linked_source("L0", 10.0, 20.0, CurveType::Linear),
        }
    }

    /// Create default configuration for Channel B (linked to R2)
    pub fn channel_b_default() -> Self {
        Self {
            frequency: ParameterLinkConfig::static_source(100.0),
            frequency_balance: ParameterLinkConfig::static_source(128.0),
            intensity_balance: ParameterLinkConfig::static_source(128.0),
            intensity: ParameterLinkConfig::linked_source("R2", 10.0, 20.0, CurveType::Linear),
        }
    }
}

/// Retention window for per-axis sample history. Generous enough to cover the
/// per-slot lookup window plus the maximum supported input delay (1000ms),
/// with headroom so backdated lookups at the edge can still find a sample.
pub const AXIS_HISTORY_MS: u64 = 1500;

/// One entry in the per-axis history ring. `interval_ms` is the ramp duration
/// from the original T-Code command (preserved so tick-side replay can feed
/// the V2 ramp the same semantics it'd see at command-arrival time).
#[derive(Debug, Clone, Copy)]
pub struct AxisSample {
    pub timestamp: u64,
    pub value: f64,
    pub interval_ms: Option<u32>,
}

/// State tracking for a single T-Code axis. `value`/`timestamp` hold the most
/// recent sample (cheap hot-path access); `history` preserves timestamped
/// samples for per-slot lookups and delayed-replay ingest.
#[derive(Debug, Clone)]
pub struct AxisState {
    pub value: f64,     // 0.0-1.0 normalized (most recent)
    pub timestamp: u64, // When last updated (milliseconds)
    pub has_data: bool, // Has received any data this session
    /// Ordered-by-time ring of recent samples. Trimmed to AXIS_HISTORY_MS.
    pub history: std::collections::VecDeque<AxisSample>,
}

impl Default for AxisState {
    fn default() -> Self {
        Self {
            value: 0.0,
            timestamp: 0,
            has_data: false,
            history: std::collections::VecDeque::with_capacity(64),
        }
    }
}

impl AxisState {
    /// Create a new axis state with a value
    #[cfg(test)]
    pub fn new(value: f64, timestamp: u64) -> Self {
        let v = value.clamp(0.0, 1.0);
        let mut history = std::collections::VecDeque::with_capacity(64);
        history.push_back(AxisSample { timestamp, value: v, interval_ms: None });
        Self {
            value: v,
            timestamp,
            has_data: true,
            history,
        }
    }

    /// Update the axis value and append to history, trimming old samples.
    pub fn update(&mut self, value: f64, timestamp: u64, interval_ms: Option<u32>) {
        let v = value.clamp(0.0, 1.0);
        self.value = v;
        self.timestamp = timestamp;
        self.has_data = true;
        self.history.push_back(AxisSample { timestamp, value: v, interval_ms });
        let cutoff = timestamp.saturating_sub(AXIS_HISTORY_MS);
        while let Some(front) = self.history.front() {
            if front.timestamp < cutoff {
                self.history.pop_front();
            } else {
                break;
            }
        }
    }

    /// Look up the axis value at a historical timestamp using nearest-prior
    /// sample (zero-order hold). Returns the current `value` if the target
    /// is newer than all samples, or `None` if history is empty.
    pub fn value_at(&self, target_time: u64) -> Option<f64> {
        if self.history.is_empty() {
            return if self.has_data { Some(self.value) } else { None };
        }
        // Scan from newest back; first sample with ts <= target wins.
        let mut best: Option<f64> = None;
        for s in self.history.iter().rev() {
            if s.timestamp <= target_time {
                best = Some(s.value);
                break;
            }
        }
        // target predates all samples → clamp to oldest available.
        best.or_else(|| self.history.front().map(|s| s.value))
    }

    /// Iterate samples with timestamps in `(after, up_to]`, oldest-first.
    /// Used by the device tick to replay TCode samples into engine state at
    /// the (possibly delayed) effective time.
    pub fn samples_in_range(&self, after: u64, up_to: u64) -> impl Iterator<Item = &AxisSample> {
        self.history
            .iter()
            .filter(move |s| s.timestamp > after && s.timestamp <= up_to)
    }
}

/// Apply curve transformation to normalized input (0.0-1.0)
pub fn apply_curve(input: f64, curve: &CurveType, strength: f64) -> f64 {
    let input = input.clamp(0.0, 1.0);
    match curve {
        CurveType::Linear => input,
        CurveType::Exponential => input.powf(strength),
        CurveType::Logarithmic => input.powf(1.0 / strength),
        CurveType::SCurve => smoothstep(input),
        CurveType::Inverse => 1.0 - input,
    }
}

/// Apply midpoint transformation
/// Converts input so center (0.5) becomes 0, and edges (0 or 1) become 1
/// Formula: abs(input - 0.5) * 2
pub fn apply_midpoint(input: f64) -> f64 {
    (input - 0.5).abs() * 2.0
}

/// Smoothstep function for S-curve (3t^2 - 2t^3)
fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear interpolation
pub fn lerp(min: f64, max: f64, t: f64) -> f64 {
    min + (max - min) * t.clamp(0.0, 1.0)
}

/// Output of one resolver pass over a `ParameterLinkConfig`. Carries the
/// staged values for both engine consumption (`device_value`) and frontend
/// telemetry (`raw_input`, `normalized_pre_range`, `target_time_ms`,
/// `source_axis`). Sub G's `resolved-update` Tauri event projects this
/// shape onto the wire format directly — defining it once now keeps the
/// frontend wire format stable as more callers pick up the resolver.
///
/// `#[allow(dead_code)]` covers the telemetry fields until sub G wires
/// the event emission. Today only `device_value` and
/// `normalized_pre_range` have live readers (the engine path + the
/// resolver tests).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ResolvedSample {
    /// Pre-curve, pre-transform input value. For Static parameters this
    /// is the static value (which for non-intensity slots is in device
    /// units, not 0..1). For Linked parameters it's the bus axis read at
    /// `target_time_ms`, post no-input handling.
    pub raw_input: f64,
    /// Post-curve, post-transforms value clamped to `[0, 1]`. The UI
    /// renders this as the live "where the resolver thinks we are" dot
    /// on the curve plot. For Static parameters it equals `raw_input`
    /// (no shaping applied).
    pub normalized_pre_range: f64,
    /// Final value the engine / device consumes. For Linked parameters
    /// this is `lerp(range_min, range_max, normalized_pre_range)`. For
    /// Static parameters it's the static value as-is.
    pub device_value: f64,
    /// `current_time_ms - delay_ms` for Linked, or the resolver's `now`
    /// for Static. Lets the frontend align dots on the curve plot with
    /// the delayed input position.
    pub target_time_ms: u64,
    /// The bus axis name when Linked; `None` for Static. Sub G's
    /// frontend uses this to pick which input-monitor card the resolved
    /// dot belongs on.
    pub source_axis: Option<String>,
}

impl ResolvedSample {
    /// Build the Static-source result. Skips bus reads, transforms, and
    /// range mapping — the static value is the device value verbatim.
    fn from_static(value: f64, target_time_ms: u64) -> Self {
        Self {
            raw_input: value,
            normalized_pre_range: value,
            device_value: value,
            target_time_ms,
            source_axis: None,
        }
    }
}

/// Resolve a `ParameterLinkConfig` at a specific point in time, threading
/// per-call mutable transform state through `runtime`.
///
/// Pipeline order (matches the plan doc's resolver layer):
/// `bus.value_at(target - delay)  →  midpoint?  →  curve  →
///  transforms (with pre-fetched modifiers)  →  lerp(range_min, range_max)`.
///
/// Transforms run in declaration order; for each transform the resolver
/// calls `resolve_modifiers()` (constants inline, axes read at the link's
/// `lookup_time`) and passes the values into `apply_transform` as a slice.
/// A misordered or short slice falls through to a documented zero/identity
/// default rather than panicking.
///
/// `runtime.transform_state` length is reconciled against
/// `cfg.transforms` on entry — a length mismatch (config edit that
/// hasn't been replayed through `apply_channel_config_to_state` yet, or
/// a fresh runtime) re-seeds with `initial_state()` so the resolver
/// never indexes past the slice. Callers don't have to remember to
/// re-init.
pub fn resolve_link_at_time(
    cfg: &ParameterLinkConfig,
    runtime: &mut ParameterLinkRuntime,
    bus: crate::input_bus::InputBusSnapshot<'_>,
    no_input_behavior: &NoInputBehavior,
    current_time_ms: u64,
    no_input_decay_ms: u32,
    target_time_ms: u64,
) -> ResolvedSample {
    match cfg.source_type {
        ParameterSourceType::Static => {
            ResolvedSample::from_static(cfg.static_value.unwrap_or(0.0), target_time_ms)
        }
        ParameterSourceType::Linked => {
            let axis_name = cfg.source_axis.as_deref();
            let axis_state = axis_name.and_then(|a| bus.get(a));
            let delay = cfg.delay_ms.unwrap_or(0) as u64;
            let lookup_time = target_time_ms.saturating_sub(delay);

            let raw = match axis_state {
                Some(state) if state.has_data => {
                    // Staleness check is against CURRENT time (not target), so a
                    // stale axis produces the same no-input response for every
                    // slot within one tick.
                    let age_ms = current_time_ms.saturating_sub(state.timestamp);
                    if age_ms > 1000 {
                        handle_no_input(no_input_behavior, cfg, state, age_ms, no_input_decay_ms)
                    } else {
                        state.value_at(lookup_time).unwrap_or(state.value)
                    }
                }
                _ => handle_no_input_no_state(no_input_behavior, cfg),
            };

            let midpoint_value = if cfg.midpoint.unwrap_or(false) {
                apply_midpoint(raw)
            } else {
                raw
            };
            let strength = cfg.curve_strength.unwrap_or(2.0);
            let curved = apply_curve(midpoint_value, &cfg.curve, strength);

            // Reconcile runtime slot count with config. The resolver never
            // index-out-of-bounds here even if `apply_channel_config_to_state`
            // hasn't refreshed the runtime yet (e.g. early-tick race after a
            // config swap).
            if runtime.transform_state.len() != cfg.transforms.len() {
                runtime.transform_state =
                    cfg.transforms.iter().map(|t| t.initial_state()).collect();
            }

            let mut shaped = curved;
            for (i, tcfg) in cfg.transforms.iter().enumerate() {
                // Resolve every modifier slot — constants inline, axes
                // pre-fetched at the SAME `lookup_time` as the base read so
                // a delayed link's transforms see modifiers from the same
                // instant the base value came from. No transform reads the
                // bus directly.
                let modifiers: Vec<f64> =
                    tcfg.resolve_modifiers(|axis| bus.value_at(axis, lookup_time).unwrap_or(0.0));
                shaped = apply_transform(
                    tcfg,
                    &mut runtime.transform_state[i],
                    shaped,
                    &modifiers,
                    lookup_time,
                );
            }

            let normalized = shaped.clamp(0.0, 1.0);
            let (range_min, range_max) = cfg.ordered_range();
            let device_value = lerp(range_min, range_max, normalized);

            ResolvedSample {
                raw_input: raw,
                normalized_pre_range: normalized,
                device_value,
                target_time_ms: lookup_time,
                source_axis: axis_name.map(|s| s.to_string()),
            }
        }
    }
}

/// Resolve a `ParameterLinkConfig` at the current instant. Thin wrapper
/// over `resolve_link_at_time` with `target_time = current_time_ms` so
/// the per-call delay shift, staleness check, transform pipeline, and
/// range mapping all live in one place.
pub fn resolve_link(
    cfg: &ParameterLinkConfig,
    runtime: &mut ParameterLinkRuntime,
    bus: crate::input_bus::InputBusSnapshot<'_>,
    no_input_behavior: &NoInputBehavior,
    current_time_ms: u64,
    no_input_decay_ms: u32,
) -> ResolvedSample {
    resolve_link_at_time(
        cfg,
        runtime,
        bus,
        no_input_behavior,
        current_time_ms,
        no_input_decay_ms,
        current_time_ms,
    )
}

/// Handle no-input behavior when axis state exists but is stale
fn handle_no_input(
    behavior: &NoInputBehavior,
    source: &ParameterLinkConfig,
    state: &AxisState,
    age_ms: u64,
    decay_ms: u32,
) -> f64 {
    match behavior {
        NoInputBehavior::Hold => state.value,
        NoInputBehavior::Default => source.static_value.unwrap_or(0.0),
        NoInputBehavior::Zero => 0.0,
        NoInputBehavior::Decay => {
            // Decay from last value to zero over decay_ms
            let decay_progress = (age_ms as f64 / decay_ms as f64).min(1.0);
            state.value * (1.0 - decay_progress)
        }
    }
}

/// Handle no-input behavior when no axis state exists
fn handle_no_input_no_state(behavior: &NoInputBehavior, source: &ParameterLinkConfig) -> f64 {
    match behavior {
        NoInputBehavior::Hold => 0.0,
        NoInputBehavior::Default => source.static_value.unwrap_or(0.0),
        NoInputBehavior::Zero => 0.0,
        NoInputBehavior::Decay => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_curve_linear() {
        assert_eq!(apply_curve(0.5, &CurveType::Linear, 2.0), 0.5);
    }

    #[test]
    fn test_apply_curve_exponential() {
        let result = apply_curve(0.5, &CurveType::Exponential, 2.0);
        assert!((result - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_apply_curve_inverse() {
        assert_eq!(apply_curve(0.3, &CurveType::Inverse, 2.0), 0.7);
    }

    #[test]
    fn test_smoothstep() {
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
        assert!((smoothstep(0.5) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_lerp() {
        assert_eq!(lerp(0.0, 100.0, 0.5), 50.0);
        assert_eq!(lerp(10.0, 20.0, 1.0), 20.0);
    }

    #[test]
    fn ordered_range_sorts_a_transposed_range() {
        let normal = ParameterLinkConfig::linked_source("L0", 0.0, 200.0, CurveType::Linear);
        assert_eq!(normal.ordered_range(), (0.0, 200.0));

        let transposed = ParameterLinkConfig::linked_source("L0", 200.0, 0.0, CurveType::Linear);
        assert_eq!(transposed.ordered_range(), (0.0, 200.0));

        let degenerate = ParameterLinkConfig::linked_source("L0", 120.0, 120.0, CurveType::Linear);
        assert_eq!(degenerate.ordered_range(), (120.0, 120.0));
    }

    #[test]
    fn test_axis_state_update() {
        let mut state = AxisState::default();
        assert!(!state.has_data);

        state.update(0.75, 1000, None);
        assert!(state.has_data);
        assert_eq!(state.value, 0.75);
        assert_eq!(state.timestamp, 1000);
    }

    use crate::input_bus::InputBus;
    use crate::transforms::{ScalarInput, TransformConfig, TransformState};

    fn resolve_value(
        cfg: &ParameterLinkConfig,
        bus: &InputBus,
        behavior: &NoInputBehavior,
        now: u64,
        decay_ms: u32,
    ) -> f64 {
        let mut runtime = ParameterLinkRuntime::for_config(cfg);
        resolve_link(cfg, &mut runtime, bus, behavior, now, decay_ms).device_value
    }

    fn resolve_value_at_time(
        cfg: &ParameterLinkConfig,
        bus: &InputBus,
        behavior: &NoInputBehavior,
        now: u64,
        decay_ms: u32,
        target: u64,
    ) -> f64 {
        let mut runtime = ParameterLinkRuntime::for_config(cfg);
        resolve_link_at_time(cfg, &mut runtime, bus, behavior, now, decay_ms, target).device_value
    }

    #[test]
    fn test_resolve_static_parameter() {
        let source = ParameterLinkConfig::static_source(42.0);
        let bus = InputBus::new();
        let result = resolve_value(&source, &bus, &NoInputBehavior::Hold, 0, 1000);
        assert_eq!(result, 42.0);
    }

    #[test]
    fn test_resolve_linked_parameter() {
        let source = ParameterLinkConfig::linked_source("L0", 0.0, 100.0, CurveType::Linear);
        let mut bus = InputBus::new();
        bus.update("L0", 0.5, 100, None);

        let result = resolve_value(&source, &bus, &NoInputBehavior::Hold, 200, 1000);
        assert_eq!(result, 50.0);
    }

    /// A transposed range must not invert the mapping — the resolver
    /// orders the endpoints, matching `device::scale_intensity`.
    #[test]
    fn resolve_orders_a_transposed_range_instead_of_inverting() {
        let source = ParameterLinkConfig::linked_source("L0", 200.0, 0.0, CurveType::Linear);
        let mut bus = InputBus::new();
        bus.update("L0", 0.25, 100, None);

        let result = resolve_value(&source, &bus, &NoInputBehavior::Hold, 200, 1000);
        assert!(
            (result - 50.0).abs() < 1e-9,
            "expected 200..0 to map like 0..200 (50.0), got {}",
            result
        );

        // ...and an axis at rest must sit at the bottom of the range, not
        // the top. This is the safety property.
        bus.update("L0", 0.0, 300, None);
        let at_rest = resolve_value(&source, &bus, &NoInputBehavior::Hold, 400, 1000);
        assert!(
            at_rest.abs() < 1e-9,
            "a resting axis on a transposed range must resolve to 0, got {}",
            at_rest
        );
    }

    #[test]
    fn test_resolve_parameter_honors_delay_ms() {
        // delay_ms=100, two samples 100ms apart. At now=200, delayed lookup
        // should land on the older sample (ts=100, value=0.2), not the
        // newer one (ts=200, value=0.8). With delay_ms=0 we must see the
        // newer sample.
        let mut source = ParameterLinkConfig::linked_source("L0", 0.0, 1.0, CurveType::Linear);
        source.delay_ms = Some(100);

        let mut bus = InputBus::new();
        bus.update("L0", 0.2, 100, None);
        bus.update("L0", 0.8, 200, None);

        let delayed = resolve_value(&source, &bus, &NoInputBehavior::Hold, 200, 1000);
        assert!(
            (delayed - 0.2).abs() < 1e-9,
            "delayed lookup expected 0.2, got {}",
            delayed
        );

        source.delay_ms = Some(0);
        let live = resolve_value(&source, &bus, &NoInputBehavior::Hold, 200, 1000);
        assert!(
            (live - 0.8).abs() < 1e-9,
            "live lookup expected 0.8, got {}",
            live
        );
    }

    #[test]
    fn test_resolve_parameter_delay_beyond_history_clamps_to_oldest() {
        // delay_ms set so target_time falls before the oldest history sample.
        // value_at scans newest→oldest, finds nothing ≤ target, then falls
        // through to history.front() (oldest available).
        let mut source = ParameterLinkConfig::linked_source("L0", 0.0, 1.0, CurveType::Linear);
        source.delay_ms = Some((AXIS_HISTORY_MS + 200) as u32);

        let mut bus = InputBus::new();
        // Both samples land within the AXIS_HISTORY_MS window relative to
        // t=1100, so neither is trimmed.
        bus.update("L0", 0.3, 1000, None);
        bus.update("L0", 0.7, 1100, None);

        let result = resolve_value(&source, &bus, &NoInputBehavior::Hold, 1100, 1000);
        // target = 1100 - delay. No sample has ts ≤ target, so fallback
        // returns the oldest still in history (t=1000 / value=0.3).
        assert!(
            (result - 0.3).abs() < 1e-9,
            "delay beyond history should clamp to oldest sample, got {}",
            result
        );
    }

    #[test]
    fn test_resolve_parameter_staleness_anchored_on_now_not_target() {
        // delay_ms is non-zero, but the axis has been silent long enough to
        // trigger staleness. The age check uses current_time_ms (not the
        // delayed target), so the no-input handler fires regardless of how
        // delay would have shifted the lookup window.
        let mut source = ParameterLinkConfig::linked_source("L0", 0.0, 1.0, CurveType::Linear);
        source.delay_ms = Some(500);

        let mut bus = InputBus::new();
        bus.update("L0", 0.4, 100, None);

        // age_ms = 1500 - 100 = 1400 > 1000 → staleness path runs.
        let result = resolve_value(&source, &bus, &NoInputBehavior::Hold, 1500, 1000);
        // Hold returns state.value = 0.4 (last received).
        assert!(
            (result - 0.4).abs() < 1e-9,
            "stale-axis Hold should return last value despite delay, got {}",
            result
        );
    }

    #[test]
    fn test_resolve_parameter_delay_underflow_saturates_at_zero() {
        // current_time_ms < delay_ms (boot-time edge). saturating_sub returns
        // 0 instead of wrapping. With a single sample at t=100, target=0
        // matches no sample (sample.ts > target), so the value_at fallback
        // hits history.front() = oldest = the sole sample. No panic.
        let mut source = ParameterLinkConfig::linked_source("L0", 0.0, 1.0, CurveType::Linear);
        source.delay_ms = Some(500);

        let mut bus = InputBus::new();
        bus.update("L0", 0.6, 100, None);

        let result = resolve_value(&source, &bus, &NoInputBehavior::Hold, 50, 1000);
        assert!(
            (result - 0.6).abs() < 1e-9,
            "delay underflow should clamp to oldest sample, got {}",
            result
        );
    }

    #[test]
    fn test_resolve_link_at_time_matches_resolve_link_when_target_is_now() {
        // The wrapper relationship: resolve_link(now) must equal
        // resolve_link_at_time(now, target=now). Guards the collapse
        // from two bodies into one.
        let mut source = ParameterLinkConfig::linked_source("R2", 10.0, 90.0, CurveType::Linear);
        source.delay_ms = Some(40);

        let mut bus = InputBus::new();
        bus.update("R2", 0.1, 50, None);
        bus.update("R2", 0.9, 150, None);

        let now = 200u64;
        let via_wrapper = resolve_value(&source, &bus, &NoInputBehavior::Hold, now, 1000);
        let via_inner = resolve_value_at_time(&source, &bus, &NoInputBehavior::Hold, now, 1000, now);
        assert_eq!(via_wrapper, via_inner);
    }

    #[test]
    fn test_channel_link_runtime_default_starts_with_empty_transform_state() {
        // Default ChannelLinkRuntime is the "no transforms anywhere"
        // shape used by `Channel::new` before a config is applied.
        // `apply_channel_config_to_state` is what fills in slots that
        // match the config's transform list.
        let runtime = ChannelLinkRuntime::default();
        for slot in [
            &runtime.frequency,
            &runtime.frequency_balance,
            &runtime.intensity_balance,
            &runtime.intensity,
        ] {
            assert!(
                slot.transform_state.is_empty(),
                "fresh ParameterLinkRuntime must start with no transform slots"
            );
        }
    }

    #[test]
    fn test_channel_link_runtime_for_config_seeds_slots_per_transform() {
        // `for_config` is what `apply_channel_config_to_state` calls when
        // it swaps a channel's config. Each slot's `transform_state`
        // length must match `cfg.transforms` length, and each slot's
        // variant must match the corresponding `initial_state()`.
        let mut intensity =
            ParameterLinkConfig::linked_source("bp:Position_0", 0.0, 200.0, CurveType::Linear);
        intensity.transforms = vec![
            TransformConfig::Vibrate {
                speed: ScalarInput::Axis("bp:Vibrate_0".into()),
                distance: 0.2,
            },
            TransformConfig::Constrict {
                amount: ScalarInput::Axis("bp:Constrict_0".into()),
                min_floor: 0.0,
                use_midpoint: false,
                method: crate::transforms::ConstrictionMethod::Downsample,
            },
        ];
        let cfg = ChannelConfig {
            frequency: ParameterLinkConfig::static_source(100.0),
            frequency_balance: ParameterLinkConfig::static_source(128.0),
            intensity_balance: ParameterLinkConfig::static_source(128.0),
            intensity,
        };

        let runtime = ChannelLinkRuntime::for_config(&cfg);
        assert!(runtime.frequency.transform_state.is_empty());
        assert_eq!(runtime.intensity.transform_state.len(), 2);
        assert!(matches!(
            runtime.intensity.transform_state[0],
            TransformState::Vibrate { .. }
        ));
        assert!(matches!(
            runtime.intensity.transform_state[1],
            TransformState::None
        ));
    }

    #[test]
    fn test_resolve_link_runs_transforms_and_lerps_into_range() {
        // Vibrate + range mapping: the resolver applies the transform
        // post-curve, then `lerp(range_min, range_max, normalized)`.
        // Speed=0 so the wobble offset is 0 → output sits at
        // lerp(0, 200, 0.5) = 100. Confirms the device_value path
        // honors the range when transforms are attached.
        let mut cfg =
            ParameterLinkConfig::linked_source("bp:Position_0", 0.0, 200.0, CurveType::Linear);
        cfg.transforms = vec![TransformConfig::Vibrate {
            speed: ScalarInput::Axis("bp:Vibrate_0".into()),
            distance: 0.2,
        }];
        let mut runtime = ParameterLinkRuntime::for_config(&cfg);
        let mut bus = InputBus::new();
        bus.update("bp:Position_0", 0.5, 100, None);
        bus.update("bp:Vibrate_0", 0.0, 100, None);

        let resolved = resolve_link(&cfg, &mut runtime, &bus, &NoInputBehavior::Hold, 100, 1000);
        assert!((resolved.normalized_pre_range - 0.5).abs() < 1e-9);
        assert!((resolved.device_value - 100.0).abs() < 1e-9);
        assert_eq!(resolved.source_axis.as_deref(), Some("bp:Position_0"));
    }

    #[test]
    fn test_resolve_link_range_change_visibly_affects_buttplug_driven_intensity() {
        // Acceptance for sub E: a Buttplug-namespaced intensity link
        // honors `range_min` / `range_max`, so adjusting the range
        // sliders changes the device output. Pre-sub-E the Buttplug
        // pipeline short-circuit always emitted `output * 200`, ignoring
        // the user's range. Same input, two range configs, different
        // device values.
        let mut cfg =
            ParameterLinkConfig::linked_source("bp:Position_0", 0.0, 200.0, CurveType::Linear);
        // No transforms attached; the unified resolver handles the
        // range-mapping for plain Buttplug-driven links too.
        cfg.transforms = Vec::new();
        let mut runtime = ParameterLinkRuntime::for_config(&cfg);
        let mut bus = InputBus::new();
        bus.update("bp:Position_0", 0.5, 100, None);

        let full = resolve_link(&cfg, &mut runtime, &bus, &NoInputBehavior::Hold, 100, 1000);
        // Full 0..200 range, input 0.5 → 100.
        assert!((full.device_value - 100.0).abs() < 1e-9);

        // Tighten range to 40..120; input 0.5 → 80.
        cfg.range_min = 40.0;
        cfg.range_max = 120.0;
        let mut runtime2 = ParameterLinkRuntime::for_config(&cfg);
        let narrowed =
            resolve_link(&cfg, &mut runtime2, &bus, &NoInputBehavior::Hold, 100, 1000);
        assert!((narrowed.device_value - 80.0).abs() < 1e-9);
        assert_ne!(full.device_value, narrowed.device_value);
    }

    #[test]
    fn test_resolve_link_vibrate_constrict_chain_narrows_around_wobbled_value() {
        // Sub D's centering shift: Constrict centers on the post-prior-
        // transforms value (the wobble) rather than the un-wobbled base.
        // Pin the chain Vibrate → Constrict so a future regression that
        // re-anchors Constrict back to `state.base_position` would fail
        // here. With speed=0 → Vibrate offset = 0 → wobbled value =
        // input value → Constrict centers on input. Output should sit
        // inside `[input - effective/2, input + effective/2]`.
        use crate::transforms::ConstrictionMethod;

        let mut cfg =
            ParameterLinkConfig::linked_source("bp:Position_0", 0.0, 1.0, CurveType::Linear);
        cfg.transforms = vec![
            TransformConfig::Vibrate {
                speed: ScalarInput::Axis("bp:Vibrate_0".into()),
                distance: 0.2,
            },
            TransformConfig::Constrict {
                amount: ScalarInput::Axis("bp:Constrict_0".into()),
                min_floor: 0.0,
                use_midpoint: false,
                method: ConstrictionMethod::Downsample,
            },
        ];
        let mut runtime = ParameterLinkRuntime::for_config(&cfg);

        let mut bus = InputBus::new();
        bus.update("bp:Position_0", 0.5, 100, None);
        bus.update("bp:Vibrate_0", 0.0, 100, None);
        bus.update("bp:Constrict_0", 0.5, 100, None);

        let resolved =
            resolve_link(&cfg, &mut runtime, &bus, &NoInputBehavior::Hold, 100, 1000);
        // Vibrate priming returns input → wobbled = 0.5. Constrict at
        // strength=0.5 with use_midpoint=false centers on 0.5,
        // effective range = 0.5 → bounds [0.25, 0.75]. Downsample
        // remaps input 0.5 → 0.5. Output = 0.5.
        assert!((resolved.normalized_pre_range - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_resolve_link_reseeds_runtime_when_slot_count_drifts() {
        // A config swap that races the resolver: cfg gains a new
        // transform but runtime hasn't been reset yet. The resolver
        // re-seeds rather than panicking on the index, so the worst
        // case is one tick of phase reset (not a crash).
        let mut cfg =
            ParameterLinkConfig::linked_source("L0", 0.0, 1.0, CurveType::Linear);
        cfg.transforms = vec![TransformConfig::Vibrate {
            speed: ScalarInput::Axis("bp:Vibrate_0".into()),
            distance: 0.2,
        }];
        // Runtime starts empty (mismatched length).
        let mut runtime = ParameterLinkRuntime::default();

        let mut bus = InputBus::new();
        bus.update("L0", 0.5, 100, None);
        bus.update("bp:Vibrate_0", 0.0, 100, None);

        // Should not panic.
        let _ = resolve_link(&cfg, &mut runtime, &bus, &NoInputBehavior::Hold, 100, 1000);
        assert_eq!(runtime.transform_state.len(), 1);
        assert!(matches!(
            runtime.transform_state[0],
            TransformState::Vibrate { .. }
        ));
    }
}
