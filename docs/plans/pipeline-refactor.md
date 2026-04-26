# Pipeline Refactor - Unified Input → Bus → Resolver → Engine

This plan reworks the input/processing pipeline into four clean layers with a single source of truth for sample data, one resolver path for all parameters, and a single InputSource interface for new input devices.

The current implementation grew incrementally — T-Code came first, gamepad and Buttplug were bolted on later, and Lovense piggybacked on Buttplug. As a result, Buttplug bypasses the curve/range/midpoint resolver, the Buttplug feature state lives in three separate top-level HashMaps, and settings→runtime sync exists in four places. This plan consolidates everything into one shape.

---

## Goals

1. **Unify input handling.** Every input source (T-Code, gamepad, Buttplug, Lovense) writes timestamped samples into one bus. Same shape, same history window, same lookup API.
2. **One resolver.** `ParameterLink` references a bus channel by name and runs a single curve/midpoint/range/delay/transform pipeline regardless of which input source produced the value.
3. **Symmetric controls.** Curve, range, midpoint, and delay knobs apply to Buttplug features the same way they apply to T-Code axes.
4. **Buffered lookback.** History window already exists on `AxisState`; wire it into the default resolver path so `delay_ms` and per-slot frequency are no longer special-cased.
5. **Smaller files, clearer ownership.** Split `websocket.rs`, lift settings conversion out of the WS layer, collapse the four settings-sync paths.
6. **Real-time visibility into every stage.** Frontend streams (a) raw input axis values, (b) post-resolver position per linked parameter (what the device actually receives, accounting for delay / midpoint / curve / transforms), and (c) device output. Each linked-parameter UI shows a live position line on its curve plot reflecting the transmitted value, not the raw input.

## Non-goals

- Changing the engine algorithms (V1/V2/V3, downsampler variants, peak-hold).
- Changing the device protocol layer (BF/B0 commands, V2 packets).
- Frontend store rewrite — this is a backend-shaped refactor; frontend changes are limited to keeping the wire format compatible.

---

## Today's pipeline (before)

```
INPUT SOURCES                    SHARED STATE                     RESOLVER              ENGINE/DRIVER
─────────────                    ────────────                     ────────              ─────────────
WS T-Code   ─┐
             ├─→ process_command ─→ axis_values: HashMap        ─┬─→ resolve_parameter ─→ Channel.next_raw_values
Gamepad     ─┘  (TCodeCommand)      (VecDeque history 500ms)     │   curve+mid+range      → V1/V2/V3 + downsampler
                                                                  │                         + peak_hold
                Channel.apply_tcode → Channel.v1/v2/v3            ┴─→ get_per_slot_freq ──→ device.rs scale + BF/B0
                (curve+mid applied   + downsampler                  (target_time)
                 here, again)        + peak_hold

WS Buttplug ──→ buttplug_features         ─┐
                buttplug_linear_commands   ├─→ process_buttplug_pipeline ───── BYPASSES resolver+engine ────┘
                buttplug_rotate_directions ┘  (own pipeline: pos/motion/vibrate/constrict)
HTTP Lovense ─→ same buttplug_features (lovense = adapter, not own path)
```

### Where the asymmetry lives

| File / line | Smell |
|---|---|
| `processing.rs:1546` `intensity_to_values(bp_val)` | Buttplug intensity short-circuits past curve/range/midpoint. |
| `processing.rs:1418-1428` | Three top-level Buttplug HashMaps next to `axis_values`. |
| `modulation.rs:66` `buttplug_links` on `ParameterSource` | Field present but resolver never reads it; real consumer is `Channel.buttplug_link`. |
| `processing.rs:1314` `Channel.apply_tcode` runs `apply_curve`+`apply_midpoint` eagerly. `modulation.rs:312` `resolve_parameter` runs them lazily for freq/balance. | Curve applied at two stages; intent is identical. |
| `device.rs:549` `scale_intensity` applies range late. `modulation.rs:351` `lerp(range_min, range_max, …)` applies range inside resolver for freq/balance. | Range applied in different layers depending on parameter. |
| `main.rs:608, 634, 723, 1048` four settings→state sync paths | `sync_settings_to_state`, `save_channel_settings`, `update_channel_config`, `update_parameter_source` each do a subset. |
| `main.rs:1086-1163` manual `ButtplugLinkConfig` construction | Duplicates `links.to_link_config()` already on `ButtplugLinksSettings`. |
| `websocket.rs` 760 lines | Server, protocol detect, T-Code handler, resolver helpers, settings→runtime conversion all in one file. |
| `modulation.rs:62` `delay_ms` field on `ParameterSource` | Defined; only consumed by `resolve_parameter_at_time` via caller-supplied `target_time`. The default `resolve_parameter` ignores it. |
| `gamepad.rs:928` `input-action` Tauri events | Gamepad buttons emit events to frontend, axes go to ProcessingState. Buttons can't drive parameters. |

---

## Target architecture (after)

Four layers, each with a clear contract.

```
LAYER 1: InputSource trait     LAYER 2: InputBus          LAYER 3: Resolver         LAYER 4: Engine + Driver
─────────────────────────      ──────────────────         ─────────────────         ────────────────────────
TCodeWsSource     ┐
GamepadSource     ├──→ bus.update(axis, val,  ─→ bus.value_at(axis,  ─→ ParameterLink.resolve(bus, now)  ─→ Channel.next_raw_values
ButtplugSource    │     ts, interval_ms)            target_time)         midpoint → curve →                → V1/V2/V3 + downsampler
LovenseSource (or │                                                       transforms (Oscillate,            + peak_hold
adapter inside    │      AxisState{value,                                Vibrate, Constrict) →
ButtplugSource)   │      ts, has_data,                                   range                          ─→ device.rs scale + BF/B0
                  ┘      history: VecDeque}
```

### Layer 1 — `InputSource`

```rust
pub trait InputSource: Send + Sync {
    fn id(&self) -> &str;                 // "tcode-ws", "gamepad-xinput:0", "buttplug-client", "lovense"
    fn axes(&self) -> &[AxisDescriptor];  // declares axis names + 0..1 semantics
    fn start(&mut self, bus: BusHandle) -> anyhow::Result<()>;  // owns its own task; pushes into bus
    fn stop(&mut self);
}

pub struct AxisDescriptor {
    pub name: String,        // "L0", "GP_LX", "bp:Vibrate_0", "bp:Position_0", "lovense:Pump"
    pub unit: AxisUnit,      // Normalized | Boolean | Counter
}
```

Sources own their task. T-Code source owns the WS server thread. Gamepad source owns the gilrs/xinput poll loop. Buttplug source owns the Buttplug protocol handler. Lovense becomes an adapter inside the Buttplug source (same axis namespace) since its semantics map cleanly onto Buttplug feature concepts.

### Layer 2 — `InputBus`

Replaces `axis_values` + `buttplug_features` + `buttplug_linear_commands` + `buttplug_rotate_directions`.

```rust
pub struct InputBus {
    channels: HashMap<String, AxisState>,
}

impl InputBus {
    pub fn update(&mut self, axis: &str, value: f64, ts_monotonic_ms: u64, ramp_ms: Option<u32>);
    pub fn value(&self, axis: &str) -> Option<f64>;                    // latest
    pub fn value_at(&self, axis: &str, target_ts: u64) -> Option<f64>; // history lookup
    pub fn age_ms(&self, axis: &str, now: u64) -> Option<u64>;
    /// Frozen snapshot of the bus at a single instant. Resolver tick takes
    /// one snapshot and reads every required axis from it, so a tick that
    /// reads multiple axes (intensity + Vibrate transform modifier) sees a
    /// coherent frame instead of interleaved input writes.
    pub fn snapshot(&self) -> InputBusSnapshot;
}
```

`AxisState` already has the VecDeque, the trim logic, and the `value_at` lookup. Just generalize it:
- Buttplug `Vibrate_0` writes a 0..1 sample at the current ts.
- Buttplug `LinearCmd_0` writes a 0..1 sample with `ramp_ms` set to the duration. The bus stores it the same way as a T-Code command with an interval.
- Buttplug `Rotate_0` direction becomes a sign-encoded value: clockwise=+speed, counter=-speed, mapped to 0..1 via offset for storage. Or: store separately as `rotate-dir:0` with a Boolean unit.

**History window: fixed 2000ms global.** Earlier draft proposed dynamic per-axis sizing as `max(link_delay, engine_lookback)`. Both reviewers (GPT, Gemini) flagged this as a multi-reader race — writers don't know all readers, so trimming based on one reader's needs silently breaks another. Memory cost is trivial: `(f64 + u64) × 100Hz × 2s ≈ 3.2 KB/axis × ~20 axes ≈ 64 KB`. Skip the dynamism.

**Concurrency:** single `RwLock<InputBus>`. Input writers grab a write lock per sample (rare — input rates are hundreds of Hz at most across all sources combined). Engine tick grabs a read lock once per 100ms tick to call `snapshot()`, then releases. Lock contention is bounded; per-axis ring buffers (`crossbeam-queue`, `rtrb`) are overkill for ~20 axes and would complicate the snapshot story.

**Timebase:** monotonic time only (`Instant::now()` or a process-start epoch). Wall-clock timestamps from upstream sources are converted at the bus boundary. Mixing monotonic and wall-clock anywhere in the resolver path is a bug — DST, NTP step, system-clock changes all produce non-monotonic jumps that break history lookups.

### Layer 3 — Resolver (`ParameterLinkConfig` + `TransformState`)

One resolve function, one path. Reads the bus snapshot by axis name and runs:

```
input @ (now - delay_ms)  →  midpoint?  →  curve  →  transforms?  →  range  →  output
                                                       │
                                              transform modifier inputs
                                              (pre-fetched from snapshot
                                              at the same target_time)
```

**Config / runtime split.** Earlier draft put mutable phase state (`pos_dur_state`, `oscillate_phase`, `rotate_phase`, `vibrate_phase`) on the same struct as serialized config. Both reviewers flagged this as a Rust antipattern — settings would accidentally serialize phase state, and `&ParameterLink::resolve` would force `RefCell`/`Mutex` interior mutability. Split it:

```rust
// Serialized config — lives in settings.json. Immutable during resolve.
pub struct ParameterLinkConfig {
    pub source: ParameterSource,         // Static | Linked { axis }
    pub midpoint: bool,
    pub curve: CurveType,
    pub curve_strength: f64,
    pub range_min: f64,
    pub range_max: f64,
    pub delay_ms: u32,
    pub transforms: Vec<TransformConfig>, // declared, ordered, with declared axis dependencies
    pub no_input: NoInputBehavior,
}

// Mutable runtime state — lives on Channel, not in settings.
pub struct ParameterLinkRuntime {
    pub transform_state: Vec<TransformState>, // one slot per TransformConfig, same index
}

// Generic, composable shaping primitives. Buttplug semantic transforms
// (Vibrate / Oscillate / Constrict) are built from these in a separate
// transforms/buttplug.rs wrapper — keeps the resolver layer device-agnostic.
pub enum TransformConfig {
    // Generic (resolver-layer primitives)
    Smooth   { time_constant_ms: f64 },
    Scale    { factor: f64 },
    Clamp    { min: f64, max: f64 },
    Invert,
    Hold     { duration_ms: u32 },
    Mix      { other_axis: String, weight: f64 },

    // Semantic (Buttplug-era wrappers, built from generics)
    Vibrate    { speed_axis: String, distance: f64 },
    Oscillate  { speed_axis: String, scale: f64, max_speed_hz: f64 },
    Constrict  { amount_axis: String, min_floor: f64, use_midpoint: bool, method: ConstrictionMethod },
}

pub enum TransformState {
    None,
    Smooth   { last_value: f64, last_ts: u64 },
    Hold     { peak_value: f64, peak_ts: u64 },
    Vibrate  { phase: f64 },
    Oscillate{ phase: f64 },
    // …
}
```

**Pre-fetched modifier inputs.** Earlier draft had `Transform::apply(value, &bus, now)` doing arbitrary `bus.value(axis)` lookups inside the transform. Both reviewers flagged this as the biggest design risk: hidden dependencies, no validation, potential cycles, and a *time-travel bug* where the base parameter resolves at `now - delay_ms` but the transform reads modifier axes at `now`. Fix: Resolver pre-fetches every modifier value at the link's target time and passes them in as a pure function call.

```rust
pub fn resolve(
    cfg: &ParameterLinkConfig,
    runtime: &mut ParameterLinkRuntime,
    snapshot: &InputBusSnapshot,    // frozen view; one per tick
    now: u64,
    no_input_decay_ms: u32,
) -> ResolvedSample {
    let target = now.saturating_sub(cfg.delay_ms as u64);

    let raw = match &cfg.source {
        ParameterSource::Static(v) => return ResolvedSample::static_value(*v),
        ParameterSource::Linked { axis } => {
            snapshot.value_at(axis, target).unwrap_or_else(|| handle_no_input(cfg, no_input_decay_ms))
        }
    };

    let mid    = if cfg.midpoint { apply_midpoint(raw) } else { raw };
    let curved = apply_curve(mid, &cfg.curve, cfg.curve_strength);
    let mut v  = curved;

    for (i, tcfg) in cfg.transforms.iter().enumerate() {
        // Pre-fetch every axis the transform declares — at the SAME target time.
        let modifiers: Vec<f64> = tcfg.declared_axes()
            .map(|axis| snapshot.value_at(axis, target).unwrap_or(0.0))
            .collect();
        v = apply_transform(tcfg, &mut runtime.transform_state[i], v, &modifiers, target);
    }

    let scaled = lerp(cfg.range_min, cfg.range_max, v.clamp(0.0, 1.0));
    ResolvedSample {
        normalized_pre_range: v.clamp(0.0, 1.0),  // 0..1, post-curve, post-transforms — for UI position-line
        device_value: scaled,                     // post-range — what the engine consumes
        raw_input: raw,                           // pre-curve, pre-transforms — for debug/telemetry
        target_time: target,
    }
}
```

**Dependency declaration + validation.** Each `TransformConfig` exposes `declared_axes()` returning the bus channels it depends on. Combined with the link's primary `source.axis`, this gives the link's full dependency set. On config apply, validate: (a) every declared axis exists in the bus's known-axes registry, (b) no cycle (a transform on link X can't reference an axis driven by link X's own output — links don't currently feed the bus, but defending against future "feedback" misuse is cheap).

**Buttplug's pipeline stages** (Vibrate, Oscillate, Constrict) become declared semantic transforms with explicit modifier axes. Today's `process_buttplug_pipeline` collapses into "this channel's intensity link has these transforms in this order." `buttplug_links` field comes off `ParameterSource`.

### Layer 4 — Engine + Driver (mostly unchanged)

`Channel.next_raw_values` still picks an engine variant and runs V1/V2/V3 + downsampler + peak_hold. The change: it consumes resolved 0..200 samples per slot rather than calling `apply_tcode` (which embedded the curve/midpoint). The engine becomes parameter-agnostic — it doesn't know whether intensity came from T-Code, gamepad, or Buttplug.

Static-source short-circuit moves into the resolver (returns the constant). Engine no longer needs that branch.

`device.rs::scale_intensity` goes away — range is applied inside the resolver now, output is already in device units.

---

## State ownership after the refactor

```rust
pub struct ProcessingState {
    pub input_bus: RwLock<InputBus>,    // all sample history, all sources; locked-once-per-tick reads
    pub channels: [Channel; 2],
    pub options: OutputOptions,
    pub no_input_behavior: NoInputBehavior,
    pub no_input_decay_ms: u32,
}

pub struct Channel {
    pub id: ChannelId,
    pub config: ChannelConfig,           // ParameterLinkConfig × 4 — serialized, immutable during resolve
    pub link_runtime: ChannelLinkRuntime, // ParameterLinkRuntime × 4 — mutable transform/phase state
    pub engine: EngineState,             // V2ChannelState + V3ChannelState (V1 dropped — see Step 4.5)
    pub downsampler: Downsampler,
    pub peak_hold: IntensityPeakHold,
    // (no more buttplug_link / buttplug_state — replaced by transforms in config + transform_state in link_runtime)
}

pub struct ChannelLinkRuntime {
    pub frequency: ParameterLinkRuntime,
    pub frequency_balance: ParameterLinkRuntime,
    pub intensity_balance: ParameterLinkRuntime,
    pub intensity: ParameterLinkRuntime,
}
```

**Config / runtime separation is enforced at the type level.** `ChannelConfig` (and `ParameterLinkConfig` inside it) is `Serialize + Deserialize` and is what hits `settings.json`. `ChannelLinkRuntime` is not serialized — it lives only in memory and is reset on engine restart, preset switch, or transform-config change. This makes it impossible to accidentally persist phase state into user settings.

---

## Refactor steps (smallest blast radius first)

Each step compiles, tests, and ships independently.

### Step 1 — Wire `delay_ms` into the default resolver

**Files:** `modulation.rs`.

Change `resolve_parameter` to compute `target = now - delay_ms` and call `value_at(target)` instead of using `state.value` directly. One-line semantic change inside the existing function. Frontend already exposes `delay_ms` on the parameter source UI; resolver just starts honoring it.

**Risk:** very low. `delay_ms` defaults to 0 → no behavior change for existing configs.

**Validation:** add a unit test that sets `delay_ms = 100`, pushes two samples 100ms apart, asserts the resolver returns the older value.

### Step 2 — Move settings→runtime conversion out of `websocket.rs`

**Files:** new `settings_convert.rs`, edits in `main.rs` and `websocket.rs`.

Pure rename + move of `convert_parameter_source` and `convert_channel_settings`. No logic change. Drop the orphaned import paths.

### Step 3 — Collapse the four settings-sync paths

**Files:** `main.rs`.

`update_parameter_source`, `save_channel_settings`, `update_channel_config`, `sync_settings_to_state` all converge on a single `apply_channel_config_to_state(channel_id, ChannelConfig, Option<ButtplugLinkConfig>)` helper. Delete the manual `ButtplugLinkConfig` construction in `update_parameter_source` (~100 lines); call `links.to_link_config()`.

**Validation:** existing settings round-trip tests cover this.

### Step 4 — Split `websocket.rs`

**Files:** new `net.rs` (TCP listen + WS upgrade + protocol detect), new `tcode_input.rs` (T-Code handler), resolver helpers (`get_resolved_channel_params`, `get_per_slot_frequencies`) move into a new `resolver.rs` next to `modulation.rs`.

`websocket.rs` is deleted or shrunk to a re-export shim if other crates import paths from it.

### Step 4.5 — Drop V1 engine

**Files:** `processing.rs`, `device.rs`, `settings.rs`, frontend engine selector UI.

Both reviewers concluded V1's queue-based ramping is structurally misaligned with a sample-driven pipeline and shouldn't be preserved. Reproducing it as a Transform leaks engine logic into the resolver layer — wrong abstraction.

Delete `V1ChannelState`, the `V1` enum variant, and V1-specific branches in `device.rs` / `Channel::next_raw_values`. Any preset whose `processing_engine == "v1"` migrates to `"v2-balanced"` on settings load. UI engine dropdown loses the V1 entry.

**Risk:** users who specifically set V1 will feel a change. Mitigation: V1 was already flagged by the project owner as inaccurate ("inaccurate anyway for a lot of reasons"), and V2-Balanced is the closest substitute. Document in release notes.

**Why before the bus refactor:** doing this first means Step 5+ doesn't have to keep V1's `(value 0..1, interval_ms)` apply path alive while everything else moves to sample-based. Cleaner diff.

### Steps 5+6+7 (bundled) — Bus + Transforms + Drop `buttplug_links`

**Single PR.** Earlier draft proposed a feature flag for the Buttplug-pipeline change. Gemini's review caught the trap: Step 5 deletes the input state Step 6's old code path depends on, so a flag would force keeping two parallel state shapes — exactly the dual-architecture mess this refactor exists to remove. Validate in a beta branch instead, ship as one cohesive change.

**Files:** `processing.rs`, `buttplug/handler.rs`, `buttplug/state.rs`, new `transforms.rs`, new `transforms/buttplug.rs`, `buttplug/pipeline.rs` (deleted), `modulation.rs`, `settings.rs`, frontend `types/modulation.ts`.

**Substeps within the PR:**

1. **Introduce `InputBus`** with `update`, `value`, `value_at`, `age_ms`, `snapshot`. Initially backed by the existing `axis_values` HashMap.
2. **Lift the three Buttplug HashMaps** (`buttplug_features`, `buttplug_linear_commands`, `buttplug_rotate_directions`) off `ProcessingState` into the bus. Buttplug handler writes to `bus.update("bp:Vibrate_0", v, ts, None)` etc. Lovense adapter writes to the same namespace.
3. **Split `ParameterLink`** into `ParameterLinkConfig` + `ParameterLinkRuntime`. Runtime lives on `Channel.link_runtime`.
4. **Implement `TransformConfig` enum** (generic primitives in `transforms.rs`, semantic Buttplug wrappers in `transforms/buttplug.rs`). Each transform declares its modifier axes.
5. **Rewrite resolver** to take `&InputBusSnapshot` and pre-fetch all transform modifier axes at the link's `target_time`. No transform reads the bus directly.
6. **Remove the intensity short-circuit** at the old `processing.rs:1546`. Buttplug-driven intensity now flows through curve/midpoint/range like every other source.
7. **Settings load handles dead keys via serde, not migration logic.** `buttplug_links` field is gone from the new types so serde drops it on load. Renamed fields use `#[serde(alias = ...)]`. V1 enum value falls back to default via a single deserialize wrapper. No `migrations/` module. See "Settings handling" section below for the full per-key policy.
8. **Drop `buttplug_links`** from `ParameterSource` once nothing reads it. `ButtplugLinksSettings` either disappears or shrinks to a UI-grouping shim on the settings type.
9. **Wire the resolved-state event stream** (see "Frontend streaming" section) so the UI can show post-mutation position lines.

**Risk:** behavior change for Buttplug-driven sessions. Even with identity defaults, Constrict's bounds math interacts with range scaling differently when range isn't `[0, 200]`. Mitigation: beta-branch validation, explicit feel-test against a saved preset, release notes.

**Validation:**
- Buttplug integration tests still pass after the bus rewrite.
- Lovense → bus axes round-trips correctly.
- Curve/range knobs visibly affect Buttplug-driven intensity (regression test: set curve = exponential, confirm response curve changes).
- Snapshot consistency test: write to two axes from a fake input source between snapshot reads, assert the resolver sees the older snapshot's values.
- Settings load test: load a saved preset with old `buttplug_links` and `processing_engine: "v1"` keys, confirm it deserializes without error, the dead keys are silently dropped, V1 falls back to default engine, and other fields (curves, ranges, delays) carry through unchanged.

### Step 8 — Introduce `InputSource` trait

**Files:** new `input/mod.rs`, `input/tcode.rs`, `input/gamepad.rs`, `input/buttplug.rs` (lovense lives inside).

Refactor existing input handling into trait implementations. Mostly cosmetic — the pipeline is already unified at this point. The win is that adding a new input source (e.g., MIDI, OSC, audio amplitude) is a self-contained file rather than scattered changes.

---

## Deletion manifest

This refactor must leave **no remnants** of the old pipeline. Every item below is deleted outright in the listed step, not migrated, not refactored, not renamed-and-kept-around. PRs that introduce these changes must also delete the listed items in the same diff — no "we'll clean up later" entries.

### Files deleted entirely

| File | Lines | Step | Why |
|---|---:|---|---|
| `src-tauri/src/buttplug/pipeline.rs` | 311 | 5+6+7 | Replaced by composable transforms in `transforms.rs` + `transforms/buttplug.rs`. Different shape — hardcoded 4-stage pipeline → ordered list of transforms. |
| `src-tauri/src/buttplug/state.rs` | 210 | 5+6+7 | `ButtplugChannelState` / `ButtplugFeatureValues` / `PositionDurationState` superseded by per-Transform `TransformState` + bus snapshot reads. |
| `src-tauri/src/buttplug/types.rs` | 191 | 5+6+7 | `ButtplugLinkConfig` / `FeatureTypeConfig` deleted entirely. `ConstrictionMethod` moves to `transforms/buttplug.rs` as a `TransformConfig::Constrict` field. |
| `src-tauri/src/websocket.rs` | 760 | 4 | Split into `net.rs` + `tcode_input.rs` + resolver helpers move to `resolver.rs`. The kitchen-sink file is gone, no shim. |
| `src/lib/stores/inputPosition.ts` | — | 5+6+7 | Replaced by `resolvedState.ts` + `inputBus.ts`. New stores have different shape (per-parameter resolved samples vs flat axis map). No re-export. |

### Structs / types deleted

| Symbol | File | Step | Replacement (or none) |
|---|---|---|---|
| `V1ChannelState` | processing.rs | 4.5 | None. V1 engine retired. |
| `ProcessingEngineType::V1` variant | processing.rs | 4.5 | None. Variant removed from enum (forces compile-time errors at every match site). |
| `ProcessingState.buttplug_features` | processing.rs | 5+6+7 | `InputBus` channels keyed `bp:Vibrate_<n>`, `bp:Position_<n>`, etc. |
| `ProcessingState.buttplug_linear_commands` | processing.rs | 5+6+7 | `InputBus` channel `bp:LinearCmd_<n>` with `ramp_ms` carrying duration. |
| `ProcessingState.buttplug_rotate_directions` | processing.rs | 5+6+7 | `InputBus` channel `bp:RotateDir_<n>` (boolean encoded). |
| `Channel.buttplug_link` | processing.rs | 5+6+7 | Lives on `ParameterLinkConfig.transforms` per-parameter. |
| `Channel.buttplug_state` | processing.rs | 5+6+7 | Lives on `ParameterLinkRuntime.transform_state` per-parameter. |
| `ParameterSource.buttplug_links` field | modulation.rs | 5+6+7 | None. Transforms live on `ParameterLinkConfig`, not on the source. |
| `ButtplugLinksSettings` | settings.rs | 5+6+7 | None. Settings UI groups transforms inline on the parameter, not via this nested type. |
| `BufferedCommand` (V3 lookahead) | processing.rs | 5+6+7 | V3's command buffer becomes a thin wrapper over bus history with effective-time computed from arrival + lookahead. The struct may survive renamed; if so, document it — otherwise delete. |
| `Channel.apply_tcode` method | processing.rs | 5+6+7 | None. Engine takes resolved per-slot samples; nothing applies T-Code-shaped commands directly to engine state. |
| `ResolvedChannelParams` (websocket.rs) | websocket.rs | 5+6+7 | Replaced by `ResolvedSample` returned from the unified resolver. |
| `IntensityPeakHold` (kept structurally) | processing.rs | — | Stays as-is; lives on `Channel`. Note here so it isn't accidentally deleted. |

### Functions deleted

| Function | Step | Replacement |
|---|---|---|
| `process_buttplug_pipeline` | 5+6+7 | `apply_transform` per transform, driven by `ParameterLinkConfig.transforms`. |
| `ProcessingState::set_buttplug_feature` | 5+6+7 | `bus.update("bp:Vibrate_0", ...)`. |
| `ProcessingState::set_buttplug_linear_cmd` | 5+6+7 | `bus.update("bp:LinearCmd_0", ..., Some(duration_ms))`. |
| `ProcessingState::set_buttplug_rotate_direction` | 5+6+7 | `bus.update("bp:RotateDir_0", ...)`. |
| `ProcessingState::get_buttplug_feature_values` | 5+6+7 | `bus.snapshot()`. |
| `ProcessingState::get_buttplug_features` | 5+6+7 | `bus.snapshot()` filtered to `bp:*`. |
| `ProcessingState::clear_all_buttplug_features` | 5+6+7 | `bus.clear_prefix("bp:")`. |
| `ProcessingState::has_buttplug_input` | 5+6+7 | `bus.has_any_with_prefix("bp:")`. |
| `ProcessingState::set_buttplug_link_config` | 5+6+7 | `apply_channel_config` (via `ChannelConfig.{intensity}.transforms`). |
| `convert_parameter_source` | 2 (move) → 5+6+7 (rewrite) | `ParameterLinkConfig::from_settings` in `settings_convert.rs`. The Step 2 move stages the rewrite; the bundled phase replaces it. Old function name is gone. |
| `convert_channel_settings` | 2 (move) → 5+6+7 (rewrite) | `ChannelConfig::from_settings` in `settings_convert.rs`. |
| `get_resolved_channel_params` | 5+6+7 | Per-tick resolver pass over `[Channel; 2]`. |
| `get_per_slot_frequencies` | 5+6+7 | Same per-tick resolver pass; produces 4-slot freq alongside intensity. |
| `apply_saved_settings_to_processing` | 3 → 5+6+7 | `apply_channel_config_to_state` (one helper, no per-field ceremony). |
| `sync_settings_to_state` | 3 | Same helper. |
| `update_parameter_source` (Tauri command + 100-line ButtplugLinkConfig builder in main.rs) | 3 (shipped) | Replaced by single Tauri command `apply_channel_config { channel, config }`. |
| `update_buttplug_links` (Tauri command) | 3 (shipped) | Folded into `apply_channel_config` alongside `update_parameter_source`. |
| `Channel.apply_tcode` | 5+6+7 | None. |
| `device.rs::scale_intensity` | 5+6+7 | None. Range scaling is part of the resolver now; engine output is already in device units. |

### Tauri commands deleted (frontend contract)

| Command | Step | Replacement |
|---|---|---|
| `update_parameter_source` | 3 (shipped) | `apply_channel_config { channel, config: ChannelConfig }`. Frontend `ChannelControl.svelte` (4 callsites) + `GeneralTab.svelte` (1 callsite) rewritten to call `apply_channel_config` with the full channel config payload. |
| `update_buttplug_links` | 3 (shipped) | Same — Buttplug-specific link updates fold into `apply_channel_config`. |
| `update_channel_config` | 3 | Same — debounced fast-path uses the same command, distinguished by a `persist: bool` flag. |
| `save_channel_settings` | 3 | Same — `apply_channel_config { ..., persist: true }`. |

### Tauri events deleted

| Event | Step | Replacement |
|---|---|---|
| `axis-update` | 5+6+7 | `bus-update` (per-write, source-tagged) + `resolved-update` (per-tick, per-parameter). Frontend listeners in `inputPosition.ts:197` and `InputMonitor.svelte:158` are deleted; new subscribers wire to the new events. |
| `buttplug-features` | 5+6+7 | `bus-update` with `bp:*` axis keys. Listener in `InputMonitor.svelte:170` is deleted. |

### Frontend stores deleted

| Store | Step | Replacement |
|---|---|---|
| `inputPosition.ts` | 5+6+7 | `resolvedState.ts` (per-parameter resolved samples) + `inputBus.ts` (raw axis values, source-tagged). |
| `inputSource.ts` (if redundant with `connectionState`) | 5+6+7 | Verify during refactor — fold into `connectionState.ts` if its only role is exposing detected protocol + Buttplug feature mirror. |

### Settings schema fields deleted

| Field | Step | Replacement |
|---|---|---|
| `ChannelSettings.intensity_source.buttplug_links` | 5+6+7 | None. Schema version bumps; old field is dropped on save. See "Migration audit" below for whether old configs are translated or discarded. |
| `OutputSettings.processing_engine == "v1"` (the V1 enum value) | 4.5 | None. See migration audit. |

---

## Settings handling — drop dead keys, auto-rename, no migration logic

**Policy:** no translation layer. No modals. No "needs reconfig" prompts. Most settings flow through unchanged because most fields are unchanged. Where a field is gone, drop it on load. Where a field is renamed, do the rename mechanically. Where a field's *meaning* changed (rare here), the old value is dropped and the new system's default takes over.

This avoids the failure mode the maintainer called out: agentic upgrades that leave translation logic, "legacy compat" branches, and half-mapped configs littering the codebase forever.

### Per-key disposition

| Key | Disposition | Notes |
|---|---|---|
| `output.processing_engine == "v1"` | **Drop value.** Field stays; if value is `"v1"` on load, treat as missing → engine selector falls back to default (V2-Balanced). | No translation. The default kicks in naturally. User can pick a different engine in the UI. |
| `channel_*.intensity_source.buttplug_links` | **Drop key entirely.** | Old Buttplug-pipeline config is gone. New transforms list starts empty on the parameter; user adds transforms in the new editor if they want them. |
| `channel_*.{frequency,frequency_balance,intensity_balance,intensity}_source` → corresponding `ParameterLinkConfig` | **Auto-pass-through.** | Same field names (`source_type`, `static_value`, `source_axis`, `range_min`, `range_max`, `curve`, `curve_strength`, `midpoint`, `delay_ms`). Rust `serde` deserializes directly into the new struct. No code needed. |
| Any rename (e.g., `intensity_source` → `intensity_link` if we choose to) | **`#[serde(rename = "old_name")]` or `#[serde(alias = "old_name")]` on the new field.** | Mechanical, single-line per field, lives in the type definition. No standalone migration code. |
| Output settings (peak fill, etc.), connection, bluetooth, shortcuts, gamepad bindings, preset names | **Keep as-is.** | Unchanged. |
| Diagnostic capture CSV format | **Replace, no read-back.** | Capture is debug output, not user-visible saved state. New format ships fresh. |
| Frontend `inputPosition` store | **Replace, no read-back.** | In-memory only; reload picks up new stores. |

### How this gets implemented

- New types use `serde(default)` on every optional field so missing keys produce sensible defaults rather than load errors.
- Renamed fields use `#[serde(alias = "<old_name>")]` so old saves deserialize transparently.
- Removed fields just aren't on the new struct — `serde` ignores unknown fields by default (verify the relevant `#[serde]` attrs on the parent types don't enable `deny_unknown_fields`; if they do, remove that for the upgrade boundary).
- For the V1 case, the `ProcessingEngineType` enum drops the `V1` variant entirely. Saved `"v1"` strings fail to deserialize with the strict enum, so wrap deserialization in a default fallback: `#[serde(deserialize_with = "engine_or_default")]` where the function returns the default on parse error. Twenty lines, lives next to the enum, no dedicated migration module.

### What does **not** exist after this refactor

- No `migrations/` module.
- No `pre-v<N>.bak` backup file mechanism. (If users want backups, they back up `presets.json` themselves; we don't pretend to manage versions.)
- No "Action needed after upgrade" UI panel.
- No legacy fallback branches in the resolver, settings loader, or input handlers.
- No `#[deprecated]` symbols re-exported "for compat."

### Validation

The first PR after the bundled phase ships should grep the entire repo for the old symbol names and confirm zero hits:

```
buttplug_link
buttplug_features
buttplug_linear_commands
buttplug_rotate_directions
process_buttplug_pipeline
ButtplugChannelState
ButtplugFeatureValues
ButtplugLinkConfig
ButtplugLinksSettings
V1ChannelState
V1
apply_tcode
process_command
update_parameter_source
update_buttplug_links
convert_parameter_source
convert_channel_settings
get_resolved_channel_params
get_per_slot_frequencies
apply_saved_settings_to_processing
sync_settings_to_state
ResolvedChannelParams
scale_intensity
axis-update
buttplug-features
inputPosition
```

Any hit other than in the deletion-manifest section of this doc and in `git log` history is a bug.

---

## Frontend streaming

The frontend needs three live data streams to render the pipeline state. Two exist today; one is new and is the main UX win of this refactor.

### Streams

```
1. INPUT BUS STREAM    — every source's raw axis values, source-tagged
                         (TCode L0, GP_LX, bp:Vibrate_0, lovense:Pump, …)
                         Cadence: per-input-event (pushed when bus.update fires)
                         Today equivalent: `axis-update` event (T-Code only).

2. RESOLVED STREAM     — per-linked-parameter post-resolver values.
                         For each linked ParameterLink (channel × parameter):
                           - raw_input         (bus value at target_time, pre-curve)
                           - normalized_pre_range (0..1, post-delay+midpoint+curve+transforms)
                           - device_value      (post-range, in device units)
                           - target_time       (now - delay_ms) for time-axis alignment
                         Cadence: 10Hz tick (matches engine tick rate; RAF-smoothed in UI).
                         Today equivalent: NONE — UI only sees raw input + final device output.

3. DEVICE OUTPUT STREAM — what the engine actually transmitted.
                         Per-channel: scaled_intensity, frequency, freq_balance,
                         int_balance, waveform[4], range_min, range_max.
                         Cadence: 10Hz tick (matches `waveform-sample` event today).
                         Today equivalent: `waveform-sample` event — keep.
```

### Why the resolved stream matters

Today the UI shows the raw axis position on each linked-parameter card. That's misleading — the actual transmitted value depends on `delay_ms`, midpoint, curve, transforms (Vibrate offset, Constrict bounds, etc.) and range mapping. A user adjusting a curve has no live feedback that the curve is doing what they expect; they have to flick the input and infer from the device's reaction.

After the refactor, every linked-parameter card renders:

- The **input dot** at the raw axis value (where the user's controller is right now).
- A **delayed input dot** at `bus.value_at(axis, now - delay_ms)` (where the resolver is reading from). Same axis as the curve's X.
- The **resolved position line** at `(delayed_input, normalized_pre_range)` — lands on the curve plot, accounts for midpoint + curve + transforms. This is what the engine will see.
- The **device-units bar** showing the post-range scaled value next to the range slider.

So the curve plot becomes a live tool: users see where their input currently lands on the curve, accounting for every shaping stage. Same idea applies to frequency / balance / range — every linked field gets a live position line of what's actually being sent.

### Backend wire format

One new Tauri event:

```rust
#[derive(Clone, Serialize)]
pub struct ResolvedUpdatePayload {
    pub timestamp_ms: u64,
    pub channel_a: ChannelResolvedSnapshot,
    pub channel_b: ChannelResolvedSnapshot,
}

#[derive(Clone, Serialize)]
pub struct ChannelResolvedSnapshot {
    pub frequency: ResolvedSampleSnapshot,
    pub frequency_balance: ResolvedSampleSnapshot,
    pub intensity_balance: ResolvedSampleSnapshot,
    pub intensity: ResolvedSampleSnapshot,
}

#[derive(Clone, Serialize)]
pub struct ResolvedSampleSnapshot {
    pub raw_input: f64,             // 0..1, what came off the bus before any shaping
    pub normalized_pre_range: f64,  // 0..1, post-curve+transforms — for plotting on the curve
    pub device_value: f64,          // device units (0-200 for intensity, 1-200 Hz for frequency, …)
    pub target_time_ms: u64,        // now - delay_ms; lets UI align dots with delayed input
    pub source_axis: Option<String>,// for static parameters: None
}
```

Emitted at the same 10Hz tick as the device update. Total payload ≈ 8 floats × 2 channels × 10 Hz ≈ 640 bytes/sec. Trivial.

### Frontend store

Replace today's `inputPosition.ts` smoothing with a unified `resolvedState.ts` store keyed by `(channel, parameter)`. Each linked-parameter component subscribes to its own slot. RAF-interpolation between 10Hz updates stays.

Also publish an `inputBus.ts` store fed by a `bus-update` event (one event per input write, source-tagged) so the input-monitor panels (T-Code monitor, gamepad axis viewer, Buttplug feature inspector) read from one place. Replaces today's `axis-update` for the unified case.

### Implementation note on cadence

The 10Hz tick rate already drives `waveform-sample`. The new `resolved-update` is computed in the same place (`device.rs` send path) since the resolver is already running there to produce engine input. Just emit the resolved snapshot alongside the waveform sample. No extra resolver pass.

For input-bus updates, fire on every `bus.update(...)` write. Bursty inputs (T-Code at 60Hz, gamepad at ~80ms tick) are fine — frontend smooths via RAF.

## Tradeoffs and open questions

### Resolved (decided)

1. **Per-parameter transforms.** Transforms live on `ParameterLinkConfig` (per-parameter), not on `ChannelConfig` (per-channel). Matches the "any parameter can be linked to any axis" symmetry already established for T-Code.

2. **Lovense as adapter inside Buttplug source.** Lovense semantics map cleanly onto Buttplug feature names. The bus namespace stays stable (`bp:Vibrate_0` is fed by Buttplug or Lovense interchangeably), no duplicated stage logic.

3. **History window: fixed 2000ms global, not dynamic.** Earlier idea was `max(link_delay, engine_lookback)` per-axis. Both reviewers flagged the multi-reader race: writer doesn't know all readers, so trimming based on one reader silently breaks another. Fixed 2000ms costs ~3.2 KB/axis × ~20 axes ≈ 64 KB. Skip the dynamism.

4. **Drop V1 in Step 4.5.** Both reviewers agreed: V1's queue-based ramping is structurally misaligned with sample-driven pipeline. Recovering it as a Transform leaks engine logic into the resolver. Just remove it; V1-using presets migrate to V2-Balanced.

5. **No correctness work for buttons today.** Gamepad buttons route to `input-action` Tauri events for frontend hotkeys; nothing is silently broken. Future-only: routing buttons through the bus would require adding event/peak-hold semantics to `AxisState` (continuous-only `f64` history would miss press→release transitions between polls). Defer to a follow-up plan.

6. **Diagnostic capture as bus subscriber.** After the refactor, `diagnostic::record_input` becomes one hook on the bus that records every sample from every source. Replaces today's inline call from `handle_tcode_message`.

7. **Generic + semantic transform split.** `TransformConfig` enum splits into generic resolver-layer primitives (`Smooth`, `Scale`, `Clamp`, `Invert`, `Hold`, `Mix`) and semantic Buttplug-era wrappers (`Vibrate`, `Oscillate`, `Constrict`) defined in `transforms/buttplug.rs`. Keeps the resolver layer device-agnostic; Buttplug-specific feel lives in one file.

### Open

- **Engine input shape.** Step 4.5 drops V1; remaining engines (V2 family, V3) all consume resolved per-slot samples cleanly. Confirm during Step 5+6+7 that no V2/V3 path still secretly assumes T-Code-shaped commands.

- **Snapshot lock contention.** Single `RwLock<InputBus>` model assumes input write rate is bounded. Worst case is a Buttplug client sending feature updates at hundreds of Hz across 8 features simultaneously while the engine tick is reading. Measure during beta; if contention shows up in profiles, switch to a sharded lock or a copy-on-write snapshot.

- **Rollback after upgrade.** No migration code means no automatic backup file either. If a user rolls back to a prior release, the new preset file may have lost the `buttplug_links` key. Acceptable: pre-upgrade users back up `presets.json` themselves if they care about reverting. Document in release notes. Don't add migration scaffolding just to support rollback.

- **Input event ordering inside one tick.** If two input sources write to the same axis between snapshots (e.g., gamepad axis L0 + T-Code L0 both bound to channel A), latest-write-wins. Consider whether axes should be source-namespaced (`tcode:L0` vs `gp:L0`) instead of sharing a single `L0` slot. Currently the doc treats them as one namespace; if conflicts get reported in the wild, namespacing is a low-cost change.

---

## Status (as of branch `worktree-pipeline-refactor`)

| Step | Status | Commits |
|---|---|---|
| 1 — `delay_ms` resolver wiring | ✅ shipped | `9b5a6e8`, `663249a` |
| 2 — Lift settings_convert out of `websocket.rs` | ✅ shipped | `9eebd24`, `7c22882` |
| 3 — Collapse 4 sync paths into `apply_channel_config` | ✅ shipped | `f4fcb02`, `de5819e` |
| 4 — Split `websocket.rs` into `net.rs` + `tcode_input.rs` + `resolver.rs` | ✅ shipped | `09173ca`, `86f1885` |
| 4.5 — Drop V1 engine | ✅ shipped | `a30d115`, `6379c22` |
| **5+6+7 (bundled)** — Bus + Transforms + drop `buttplug_links` | 🚧 in progress | subs A-D shipped — see substep table below |
| 8 — Introduce `InputSource` trait | ⏳ pending | — |

Each shipped step landed with three reviewer passes (correctness / DRY / design gaps) and follow-up commits addressing in-scope findings.

### Bundled phase substeps

The plan called for Steps 5+6+7 as a single PR. In practice the work is large enough (the original 1800–2400 line estimate is accurate) that breaking it into reviewable sub-commits inside the same logical PR is the only sane way to ship without losing the thread. Order:

| Sub | Status | Description |
|---|---|---|
| A — Introduce `InputBus` foundation | ✅ shipped (`df8a715`, `80ec841`) | New `input_bus.rs` module with `update`, `value`, `value_at`, `age_ms`, `get`, `iter`, `clear`, `clear_prefix`, `has_any_with_prefix`, `latest_timestamp`. `ProcessingState.axis_values` renamed to `input_bus`. Resolver signatures take `&InputBus`. Six resolver tests rebuilt; eight bus tests added. |
| B — Lift the 3 buttplug HashMaps into the bus | ✅ shipped (`1b12a36`, `3b19e3a`) | Dropped `ProcessingState.buttplug_features`, `buttplug_linear_commands`, `buttplug_rotate_directions`. Rewrote `set_buttplug_*` / `get_buttplug_*` / `has_buttplug_input` / `clear_all_buttplug_features` as bus-backed shims under the `bp:` namespace. Pipeline + state timestamps converted from `Instant` to `u64 ms`. Per-channel `Channel.last_buttplug_replay_ts: u64` watermark replaces the post-tick `clear()` of `linear_commands`. Five new tests pin the contract. |
| C — Split `ParameterSource` → `ParameterLinkConfig` + `ParameterLinkRuntime` | ✅ shipped (`7539bc7`, `db17255`) | Renamed `ParameterSource` → `ParameterLinkConfig`. Dropped vestigial `buttplug_links` field from runtime mirror. Added `ParameterLinkRuntime { transform_state: Vec<TransformState> }`, `ChannelLinkRuntime` (4 slots), `Channel.link_runtime`. `TransformState::None` placeholder lives in `modulation.rs` until sub D moves it. `Channel.buttplug_link` + `buttplug_state` flagged with `TODO(sub F)` markers. |
| D — `TransformConfig` enum + transform variants | ✅ shipped (`695cd9a`, `f37aa41`) | New `src-tauri/src/transforms/` module. `mod.rs` holds the `TransformConfig` + `TransformState` enums and `apply_transform` dispatch over the generic primitives (`Smooth`, `Scale`, `Clamp`, `Invert`, `Hold`, `Mix`); `transforms/buttplug.rs` holds the semantic wrappers (`Vibrate`, `Oscillate`, `Constrict`) reproducing pre-refactor pipeline math. `declared_axes()` + `initial_state()` per variant. `ParameterLinkConfig` gains `transforms: Vec<TransformConfig>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`. 20 new tests; module gated `#[allow(dead_code)]` for the staging window. **Constrict centering on `value` (post-prior-transforms) is a deliberate semantic shift from the pre-refactor `state.base_position` — captured in sub G migration notes for the beta feel-test.** |
| E — Resolver rewrite to take `InputBusSnapshot` + pre-fetched modifiers | ✅ shipped (`b575447`, follow-up TBD) | Dropped the intensity short-circuit at the old `processing.rs:1546`. **Buttplug-namespaced** intensity (axis under `bp:`) flows through `resolve_link` → midpoint / curve / transforms / range. Added `TransformConfig::Rotate { speed_axis, direction_axis, scale, max_speed_hz }`. Wired `ButtplugLinksSettings` → ordered `Vec<TransformConfig>` in `settings_convert::convert_parameter_source`. Per-batch `arrival_ts_ms` argument on `set_buttplug_feature` / `set_buttplug_linear_cmd` / `set_buttplug_rotate_direction` — handlers compute one ts per logical command. `Channel.buttplug_link` consumer dropped (field stays for sub F, marked `#[deprecated]`). T-Code intensity keeps the V2/V3 engine path; transforms attached to non-`bp:` links are silently ignored until sub G unifies the engine + resolver paths. `ResolvedSample { raw_input, normalized_pre_range, device_value, target_time_ms, source_axis }` defined once for sub G's wire format. |
| F — Delete `buttplug/pipeline.rs`, `buttplug/state.rs`, most of `buttplug/types.rs` | ✅ shipped (`<TBD>`) | `git rm src-tauri/src/buttplug/pipeline.rs src-tauri/src/buttplug/state.rs`. Trimmed `buttplug/types.rs` down to `ButtplugFeatureType` + `ButtplugFeatureConfig` (descriptor-only). Moved canonical `ConstrictionMethod` into `transforms/mod.rs`. Dropped `Channel.buttplug_link`, `Channel.buttplug_state`, `set_buttplug_link_config`, `get_buttplug_feature_values`, `has_buttplug_input`, `BUTTPLUG_MAX_FEATURES`. Dropped `ButtplugLinksSettings::to_link_config`. Removed the `bp_config` write in `apply_channel_config_to_state`. Tests dropped: 5 in `pipeline.rs` (deleted with file), 2 in `processing.rs` (`test_buttplug_linear_cmd_watermark_*`, `test_buttplug_feature_values_round_trip_*`) — replaced by resolver / bus / handler-level coverage. |
| G — Frontend resolved-state event stream | ⏳ pending | New Tauri event `resolved-update` carrying per-parameter `ResolvedSampleSnapshot`. Replaces `axis-update` for the unified case. New stores `resolvedState.ts` + `inputBus.ts` (replacing `inputPosition.ts`). Linked-parameter UI cards render the post-curve position line. |

Sub A is the only sub that ships without behavior change. Subs B–G land semantic changes; per the plan they validate as a coherent set on a beta branch before merge to `main`.

---

## Ship order

| Phase | Steps | Risk | Ship as |
|---|---|---|---|
| **Cleanups** | 1, 2, 3, 4 | Low | Each its own PR, in listed order (each shrinks the next) |
| **V1 removal** | 4.5 | Medium (user-visible) | Own PR with release-notes entry |
| **Architecture** | 5+6+7 (bundled) + frontend resolved-stream | High | One PR, validated on a beta branch before merge. No feature flag (Step 5 deletes the state Step 6's old code path needs). |
| **Polish** | 8 | Low | Ship whenever |

**Why no flag on the bundled phase:** Step 5 lifts `buttplug_features` / `buttplug_linear_commands` / `buttplug_rotate_directions` off `ProcessingState` into the bus. The old `process_buttplug_pipeline` reads from those exact fields. A flag would require keeping both state shapes in parallel — exactly the dual-architecture mess this refactor exists to remove. Beta-branch validation is the safety net instead.

Total estimated diff: ~1800–2400 lines moved/changed across backend + frontend, with the bulk in the 5+6+7 bundle.

## Pre-flight checks before starting the bundled phase

Before opening the 5+6+7 PR, the following preconditions should be in place from earlier steps:

1. `delay_ms` is wired into the default resolver path (Step 1) so a resolved-stream emit during Step 6 already honors delay.
2. Settings→runtime conversion lives outside `websocket.rs` (Step 2) so the bus rewrite doesn't have to also untangle conversion code.
3. The four sync paths are collapsed into one helper (Step 3) so there's a single entry point to reroute through `apply_channel_config_to_state`.
4. `websocket.rs` is split (Step 4) so the resolver helpers (`get_resolved_channel_params`, `get_per_slot_frequencies`) are already in `resolver.rs` and can be rewritten in isolation.
5. V1 is gone (Step 4.5) so the bus rewrite doesn't have to keep the queue-based apply path alive.
6. A beta branch is set up with at least one Buttplug-driven preset and one T-Code-driven preset saved, so post-merge feel-tests have known fixtures.
