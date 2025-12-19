# Buttplug Feature Pipeline - Implementation Plan

This document outlines the implementation of Buttplug integration using a composable processing pipeline where each Buttplug output type serves a distinct role in shaping the final output.

## Core Concept

Instead of treating Buttplug features as simple intensity sources, we interpret each output type as a **processing stage** that can be composed together:

```
[Position] → [Motion] → [Vibrate] → [Constrict] → Final Output
   base       pattern    wobble      bound
```

**Pipeline stages:**
1. **Position** (Position/PositionWithDuration) - Sets base value
2. **Motion** (Rotate OR Oscillate - mutually exclusive) - Adds movement pattern
3. **Vibrate** - Adds high-frequency wobble
4. **Constrict** - Bounds final range (downsamples to bounds)

Users can link multiple feature types to a single channel parameter, and each contributes differently to the output.

---

## Buttplug Output Type Definitions

### Position (Base Value)
Sets the exact target position. Acts as the "root" or "center point" for other modifiers.

```
Input:  0.0 - 1.0
Output: Sets base value directly
```

When no Position is linked, base defaults to 0.5 (midpoint of range).

### PositionWithDuration (Smooth Movement)
Moves from current position to target position over specified duration. New commands overwrite in-progress movements.

```
Input:  position (0.0-1.0) + duration (ms)
Output: Smoothly interpolated base value over time
```

**State:**
- `start_time`: Timestamp when command started
- `start_position`: Position when movement began
- `target_position`: Destination
- `duration_ms`: How long the movement should take

**Each tick:** (current position computed on the fly)
```
elapsed = now - start_time
progress = min(elapsed / duration_ms, 1.0)
current = lerp(start_position, target_position, progress)
```

### Vibrate (Wobble Modulation)
Adds oscillating "wobble" around a center point. Speed controls rate, with configurable distance/amplitude.

```
Input:  speed (0.0-1.0)
Config: distance (0.0-1.0) - max amplitude of wobble
Output: center ± (distance * sin(phase))
```

**Center Point Logic:**
- If Position/PositionWithDuration is linked → wobble around position value
- If no Position linked → wobble around midpoint (0.5)

**Parameters:**
- `speed`: 0.0 = no vibration, 1.0 = max frequency (e.g., 20Hz)
- `distance` (UI config in link panel): How far from center the wobble extends

**Each tick:**
```
center = has_position_linked ? position_value : 0.5
frequency_hz = speed * 20  // 0-20 Hz range
phase += frequency_hz * dt * 2π
offset = sin(phase) * distance
output = center + offset
```

### Rotate (Directional Sweep)
Repeating motion in ONE direction (← or →), then snaps back. Like a sawtooth wave.

```
Input:  speed (0.0-1.0), clockwise (bool)
Config: scale (0.0-1.0) - how much of range to sweep
Config: max_speed (Hz) - max sweep rate (default: 5)
Output: Sawtooth pattern from base
```

**Buttplug RotateCmd format:** Each actuator sends `speed` (0.0-1.0) and `clockwise` (bool), which maps directly to our inputs.

**Behavior:**
- `clockwise = true`: Sweeps base → base+scale, snaps back
- `clockwise = false`: Sweeps base → base-scale, snaps back

**Each tick:**
```
cycle_hz = speed * max_speed  // 0-5 Hz sweep rate (configurable)
phase = (time * cycle_hz) % 1.0  // 0-1 sawtooth
direction = clockwise ? 1 : -1
offset = phase * scale * direction
output = base + offset
```

### Oscillate (Alternating Sweep)
Repeating back-and-forth motion (←→). Like a triangle or sine wave across the full configured range.

```
Input:  speed (0.0-1.0)
Config: scale (0.0-1.0) - portion of range to cover
Config: max_speed (Hz) - max sweep rate (default: 5)
Output: Triangle wave centered on base
```

**Each tick:**
```
cycle_hz = speed * max_speed  // 0-5 Hz oscillation rate (configurable)
phase = (time * cycle_hz) % 1.0
// Triangle wave: 0→1→0 over one cycle
triangle = 1 - abs(2 * phase - 1)
offset = (triangle - 0.5) * scale * 2
output = base + offset
```

### Constrict (Range Limiter)
Bounds the output range. At 0, output is constrained to a narrow band. At 1, full range is available.

```
Input:  constriction (0.0-1.0)
Config: min_floor (0.0-1.0) - what "0" constriction means
Config: use_midpoint (bool) - center around midpoint vs position
Config: method ('downsample' | 'clamp') - how to apply bounds (default: downsample)
Output: Remaps or clamps previous output to constrained range
```

**Center Point Logic:**
- If Position is linked AND `use_midpoint = false` → bound around position value
- If `use_midpoint = true` OR no Position linked → bound around 0.5

**UI:** When Position feature is also linked, show toggle:
```
Constrict Midpoint: [ | ]  (off = use position, on = use 0.5)
```

**Method Options:**
- **Downsample (default)**: Remap 0.0-1.0 input to constrained range (preserves relative position)
- **Clamp**: Cut off values outside bounds (can cause flat spots)

**Behavior:**
```
effective = lerp(min_floor, 1.0, constriction_input)
center = use_midpoint ? 0.5 : position_value
half_range = effective * 0.5
min_bound = max(center - half_range, 0.0)
max_bound = min(center + half_range, 1.0)

if method == 'downsample':
    // Remap input (0.0-1.0) to constrained range
    output = min_bound + (input_value * (max_bound - min_bound))
else:  // clamp
    output = clamp(input_value, min_bound, max_bound)
```

**Example:**
- Constrict = 0, min_floor = 0.2 → output range is 20% of full
- Constrict = 1 → output range is 100% of full

### LED (Hidden UI Effect)
Controls visual glow/emphasis on UI elements. Does not affect device output.

**This is a hidden feature** - not listed in feature configuration, not user-controllable. Only activated if a Buttplug client sends LED commands.

```
Input:  brightness (0-255 or 0.0-1.0)
Output: CSS glow intensity (brightness and size)
```

**Performance:** Uses `requestAnimationFrame` for smooth updates. CSS changes target `filter` and `box-shadow` properties to avoid layout thrashing and full element/window repaints.

**Targeted UI Elements:**
- Channel sliders (primary purple, secondary blue colors)
- Logo and lightning bolt
- Input and output pills in navbar

---

## Processing Pipeline

### Pipeline Order

When multiple feature types are linked to a channel parameter, they process in this fixed order:

```
1. [Position/PositionWithDuration] → Base value (0.0-1.0)
         ↓
2. [Motion: Rotate OR Oscillate] → Adds movement pattern
         ↓
3. [Vibrate] → Adds high-frequency wobble
         ↓
4. [Constrict] → Downsamples to final range
         ↓
   Final Output (0.0-1.0)
```

### Pipeline Rules

1. **Only one of each type per parameter** - Can't link Vibrate1 AND Vibrate2 to same param
2. **Motion types are mutually exclusive** - Can't link BOTH Rotate AND Oscillate (selecting one disables the other)
3. **All types optional** - Missing stages pass through unchanged
4. **Order is fixed** - Position → Motion → Vibrate → Constrict
5. **Constrict always last** - Downsamples output to final bounds

### Default Values (when type not linked)

| Stage | Type | Default Behavior |
|-------|------|------------------|
| Position | Position | 0.5 (midpoint) |
| Position | PositionWithDuration | Same as Position |
| Motion | Oscillate | No pattern (pass-through) |
| Motion | Rotate | No pattern (pass-through) |
| Wobble | Vibrate | No modulation (pass-through) |
| Bound | Constrict | Full range (1.0) |

---

## Feature Management

### Settings UI: Buttplug Section

New settings section where users configure which features to advertise:

```
┌─ Buttplug Features ─────────────────────────────┐
│                                                  │
│  📍 Position          [1] [2] [3×] [+]          │
│  ⏱️ PositionWithDur   [1] [2] [+]               │
│  📳 Vibrate           [1] [2] [+]               │
│  ↔️ Oscillate         [1] [2] [+]               │
│  🔄 Rotate            [1] [2] [+]               │
│  ⊘ Constrict         [1] [2] [+]               │
│                                                  │
│  Total Features: 13                              │
│  [Reset to Defaults]                             │
│                                                  │
└──────────────────────────────────────────────────┘
```

**Notes:**
- Icons shown left of type names for visual association
- Features beyond the default (2) show `×` to remove
- `[+]` adds another feature of that type

**Default:** 2 of each type (12 total features)

**Stored as:**
```typescript
interface ButtplugFeatureConfig {
  position: number;      // count, e.g., 2
  positionWithDuration: number;
  vibrate: number;
  oscillate: number;
  rotate: number;
  constrict: number;
}
```

### Feature Indexing

Features are indexed sequentially by type:

| Feature Index | Type | Name |
|---------------|------|------|
| 0 | Position | Position 1 |
| 1 | Position | Position 2 |
| 2 | PositionWithDuration | PosDur 1 |
| 3 | PositionWithDuration | PosDur 2 |
| 4 | Vibrate | Vibrate 1 |
| 5 | Vibrate | Vibrate 2 |
| ... | ... | ... |

---

## Feature Linking UI

### Linking Panel (Buttplug Mode)

When Buttplug is the input source, the linking panel shows a grid of toggle buttons grouped by feature type. Each row acts like a radio button set - selecting one deselects others in the same row. Press again to deselect.

```
┌─ Channel A Intensity ───────────────────────────┐
│  Source: ○ Static  ● Linked                     │
│                                                  │
│  [📍1] [📍2] [⏱️1] [⏱️2]         ← Position     │
│  [🔄1] [🔄2] [↔️1] [↔️2]         ← Motion       │
│  [📳1] [📳2]                     ← Vibrate      │
│  [⊘1] [⊘2]                      ← Constrict    │
│                                                  │
│  ─── Feature Config ───                         │
│  Vibrate 1:                                      │
│    Distance: [====●=====] 0.3                   │
│  Constrict 1:                                    │
│    Min Floor: [==●=======] 0.2                  │
│    Method: [Downsample ▼]                       │
│    Midpoint: [ ]                                │
│                                                  │
└──────────────────────────────────────────────────┘
```

**Behavior:**
- Buttons toggle on/off
- Within Position row: only one can be selected (📍 or ⏱️)
- Within Motion row: only one can be selected (🔄 or ↔️)
- Within Vibrate row: only one can be selected
- Within Constrict row: only one can be selected
- Config sliders appear for selected features

**Icons** (using Lucide icon set):
- 📍 Position → `MapPin` or `Target`
- ⏱️ PositionWithDuration → `Timer` or `Clock`
- 🔄 Rotate → `RotateCw` / `RotateCcw`
- ↔️ Oscillate → `ArrowLeftRight` or `MoveHorizontal`
- 📳 Vibrate → `Activity` or `Zap`
- ⊘ Constrict → `Minimize2` or `Shrink` (not ban icon)

**Note:** Same feature can link to multiple parameters (e.g., Position 1 → both Channel A and B intensity), just like current T-Code linking.

---

## Input Indicator

### Header Display

Shows current input source. Once connected, displays source name directly for conciseness:

```
┌─────────────────────────────────────────────────┐
│  T-Code ● 🔌                                    │  ← T-Code connected
│  Buttplug ● 🎮                                  │  ← Buttplug connected
│  Input ◌ ⟳                                      │  ← No input (spinner)
└─────────────────────────────────────────────────┘
```

**States:**
- `●` Green dot + icon when connected, label shows source name
- `◌` Empty dot + spinner when waiting, label shows "Input"

---

## Input Monitoring

### Renamed: "Input Monitor" (was "T-Code Monitor")

Flexible display of all input values regardless of source:

**T-Code Mode:**
```
┌─ Input Monitor ─────────────────────────────────┐
│  L0: ████████░░ 0.82    R0: ██░░░░░░░░ 0.21    │
│  L1: ░░░░░░░░░░ 0.00    R1: ░░░░░░░░░░ 0.00    │
│  L2: ░░░░░░░░░░ 0.00    R2: ░░░░░░░░░░ 0.00    │
│  V0: ███░░░░░░░ 0.35    V1: ░░░░░░░░░░ 0.00    │
└─────────────────────────────────────────────────┘
```

**Buttplug Mode:**
```
┌─ Input Monitor ─────────────────────────────────┐
│  📍1: ████████░░ 0.82   📍2: ░░░░░░░░░░ 0.00   │
│  ⏱️1: ████░░░░░░ 0.45   ⏱️2: ░░░░░░░░░░ 0.00   │
│  📳1: ██████░░░░ 0.65   📳2: ░░░░░░░░░░ 0.00   │
│  🔄1: ░░░░░░░░░░ 0.00   🔄2: ░░░░░░░░░░ 0.00   │
│  ↔️1: ░░░░░░░░░░ 0.00   ↔️2: ░░░░░░░░░░ 0.00   │
│  ⊘1: ███████░░░ 0.75   ⊘2: ░░░░░░░░░░ 0.00   │
└─────────────────────────────────────────────────┘
```

### Implementation

Backend provides generic input value map:

```rust
// Generic input value structure
struct InputValues {
    source: InputSource,  // TCode or Buttplug
    values: HashMap<String, f64>,  // "L0" → 0.82, "Vibrate1" → 0.65
    last_updated: Instant,  // Single timestamp for staleness detection
}
```

Frontend renders based on source type, using appropriate icons/labels.

---

## Output Options / Presets

### Preset Structure

Unified structure that works for both T-Code and Buttplug inputs:

```typescript
interface ChannelPreset {
  name: string;
  inputType: 'tcode' | 'buttplug';
  intensity: ParameterSource;
  frequency: ParameterSource;
  freqBalance: ParameterSource;
  intBalance: ParameterSource;
}

interface ParameterSource {
  type: 'static' | 'linked';
  staticValue?: number;
  linkedFeature?: FeatureLink;  // Single feature per stage (enforced by UI)
}

interface FeatureLink {
  featureType: FeatureType;  // 'L' | 'R' | 'V' | 'Position' | 'Vibrate' | etc.
  featureIndex: number;      // e.g., 0 for "L0" or 1 for "Vibrate 1"
  config?: FeatureTypeConfig;
}

type FeatureType =
  | 'L' | 'R' | 'V'  // T-Code axes
  | 'Position' | 'PositionWithDuration'
  | 'Vibrate' | 'Rotate' | 'Oscillate' | 'Constrict';  // Buttplug

interface FeatureTypeConfig {
  // Vibrate
  distance?: number;
  // Rotate
  scale?: number;
  maxSpeed?: number;
  // Oscillate
  scale?: number;
  maxSpeed?: number;
  // Constrict
  minFloor?: number;
  useMidpoint?: boolean;
  method?: 'downsample' | 'clamp';
  // T-Code (L, R)
  curve?: CurveType;
  // ... other T-Code specific config
}
```

### Output Options UI

Simplified compact layout. Input source is auto-determined by the active connection:

```
┌─────────────────────────────────────────────────┐
│  Preset              Engine (T-Code Only)       │
│  [ None ▼] [+]      [ V2 Smooth ▼] ⓘ           │
└─────────────────────────────────────────────────┘
```

**Notes:**
- No "Output Options" label needed - saves space
- Input source not selectable (auto-determined by input stream)
- Engine dropdown only visible when T-Code is active
- Info icons (ⓘ) retained for tooltips

---

## Buttplug Processing Pipeline (Detailed)

### Per-Channel Processing State

```rust
struct ButtplugChannelState {
    // Current base position (computed from Position or PositionWithDuration)
    base_position: f64,

    // PositionWithDuration interpolation
    pos_dur_state: Option<PositionDurationState>,

    // Vibrate phase tracking
    vibrate_phase: f64,

    // Oscillate phase tracking
    oscillate_phase: f64,

    // Rotate phase tracking
    rotate_phase: f64,

    // Final output after pipeline
    output: f64,
}

struct PositionDurationState {
    start_time: Instant,
    start_position: f64,
    target_position: f64,
    duration_ms: u32,
}
```

### Pipeline Execution (per 10Hz tick)

```rust
fn process_buttplug_pipeline(
    state: &mut ButtplugChannelState,
    features: &ButtplugFeatureValues,
    config: &ButtplugLinkConfig,
    now: Instant,
    dt_ms: u32,
) -> f64 {
    let mut value: f64;

    // 1. POSITION - Set base value
    if let Some(pos) = features.get_position(config.position_feature) {
        state.base_position = pos;
    }

    // 1b. POSITION WITH DURATION - Smooth interpolation (overwrites position)
    if let Some((target, duration)) = features.get_new_position_with_duration(config.pos_dur_feature) {
        // New command received - start interpolation
        state.pos_dur_state = Some(PositionDurationState {
            start_time: now,
            start_position: state.base_position,
            target_position: target,
            duration_ms: duration,
        });
    }

    // Process ongoing interpolation
    if let Some(ref pds) = state.pos_dur_state {
        let elapsed_ms = now.duration_since(pds.start_time).as_millis() as f64;
        let progress = (elapsed_ms / pds.duration_ms as f64).min(1.0);
        state.base_position = lerp(pds.start_position, pds.target_position, progress);

        if progress >= 1.0 {
            state.pos_dur_state = None;
        }
    }

    value = state.base_position;

    // 2. MOTION - Oscillate OR Rotate (mutually exclusive)
    // Only one can be linked at a time per pipeline rules
    if let Some(speed) = features.get_oscillate(config.oscillate_feature) {
        let scale = config.oscillate_scale.unwrap_or(0.5);
        let max_speed = config.oscillate_max_speed.unwrap_or(5.0);
        let freq_hz = speed * max_speed;

        state.oscillate_phase += freq_hz * (dt_ms as f64 / 1000.0);
        let phase_norm = state.oscillate_phase % 1.0;
        let triangle = 1.0 - (2.0 * phase_norm - 1.0).abs();
        let offset = (triangle - 0.5) * 2.0 * scale;
        value += offset;
    } else if let Some((speed, clockwise)) = features.get_rotate(config.rotate_feature) {
        let scale = config.rotate_scale.unwrap_or(0.5);
        let max_speed = config.rotate_max_speed.unwrap_or(5.0);
        let freq_hz = speed * max_speed;

        state.rotate_phase += freq_hz * (dt_ms as f64 / 1000.0);
        let sawtooth = state.rotate_phase % 1.0;
        let direction = if clockwise { 1.0 } else { -1.0 };
        let offset = sawtooth * scale * direction;
        value += offset;
    }

    // 3. VIBRATE - Add wobble
    if let Some(speed) = features.get_vibrate(config.vibrate_feature) {
        let distance = config.vibrate_distance.unwrap_or(0.2);
        let freq_hz = speed * 20.0;  // 0-20 Hz

        state.vibrate_phase += freq_hz * (dt_ms as f64 / 1000.0) * 2.0 * PI;
        let offset = state.vibrate_phase.sin() * distance;
        value += offset;
    }

    // 4. CONSTRICT - Downsample (or clamp) to range
    if let Some(constriction) = features.get_constrict(config.constrict_feature) {
        let min_floor = config.constrict_min_floor.unwrap_or(0.0);
        let use_midpoint = config.constrict_use_midpoint.unwrap_or(false);
        let method = config.constrict_method.unwrap_or(ConstrictionMethod::Downsample);

        let effective = lerp(min_floor, 1.0, constriction);
        let center = if use_midpoint { 0.5 } else { state.base_position };
        let half_range = effective * 0.5;
        let min_bound = (center - half_range).max(0.0);
        let max_bound = (center + half_range).min(1.0);

        value = match method {
            ConstrictionMethod::Downsample => {
                // Remap 0.0-1.0 to constrained range
                min_bound + (value.clamp(0.0, 1.0) * (max_bound - min_bound))
            }
            ConstrictionMethod::Clamp => {
                value.clamp(min_bound, max_bound)
            }
        };
    }

    state.output = value.clamp(0.0, 1.0);
    state.output
}

enum ConstrictionMethod {
    Downsample,  // Default - remap to range
    Clamp,       // Cut off at bounds
}
```

---

## Example Scenarios

### Scenario 1: Basic Funscript Playback

**Config:**
- PositionWithDuration 1 → Channel A Intensity
- PositionWithDuration 1 → Channel B Intensity (same feature, both channels)

**Behavior:** Both channels follow the funscript position smoothly.

### Scenario 2: Funscript + Vibration Enhancement

**Config:**
- PositionWithDuration 1 → Channel A Intensity
- Vibrate 1 → Channel A Intensity (distance: 0.15)

**Behavior:** Channel A follows funscript with a subtle 0-20Hz wobble on top.

### Scenario 3: Independent Channel Control

**Config:**
- Position 1 → Channel A Intensity
- Position 2 → Channel B Intensity
- Constrict 1 → Channel A Intensity (min_floor: 0.3)
- Constrict 2 → Channel B Intensity (min_floor: 0.1)

**Behavior:** Each channel independently controlled with different range constraints.

### Scenario 4: Oscillation Pattern

**Config:**
- Position 1 → Channel A Intensity (base at 0.5)
- Oscillate 1 → Channel A Intensity (scale: 0.4)
- Constrict 1 → Channel A Intensity

**Behavior:** Channel A oscillates around 50% with dynamic range control.

---

## Implementation Phases

### Phase 1: Backend Foundation
- [ ] Define Buttplug feature types and state structures
- [ ] Implement processing pipeline in Rust
- [ ] Add feature value storage per connected client
- [ ] Integrate with existing 10Hz device loop

### Phase 2: Buttplug Protocol
- [ ] Implement Buttplug server endpoint (`/buttplug`)
- [ ] Handle handshake (RequestServerInfo/ServerInfo)
- [ ] Implement device enumeration (configurable features)
- [ ] Handle all output commands (LinearCmd, VibrateCmd, etc.)
- [ ] Implement Battery sensor

### Phase 3: Settings & Configuration
- [ ] Add Buttplug settings section
- [ ] Feature count configuration per type
- [ ] Persist configuration

### Phase 4: Linking UI
- [ ] Update linking panel for Buttplug mode
- [ ] Toggle grid grouped by feature type
- [ ] Type-specific config sliders
- [ ] Select Lucide icons for each type

### Phase 5: Input Monitoring
- [ ] Rename T-Code Monitor → Input Monitor
- [ ] Generic input value display
- [ ] Input source indicator (simplified)

### Phase 6: Presets
- [ ] Unified preset structure
- [ ] Save/load linking configs

---

## Design Decisions (Resolved)

| Question | Decision |
|----------|----------|
| **Constrict centering** | User choice via toggle. When Position is linked, show "Constrict Midpoint" toggle. Off = use position value, On = use 0.5 |
| **Constrict method** | Configurable: Downsample (default) or Clamp. Downsample remaps to range, Clamp cuts off at bounds |
| **Vibrate centering** | Automatic. Uses position value if Position feature is linked, otherwise uses midpoint (0.5) |
| **Motion pattern stacking** | Mutually exclusive. Selecting Rotate disables Oscillate and vice versa (like same-type feature restriction) |
| **LED feature scope** | Hidden global feature. Not user-configurable. Targets: channel sliders, logo/bolt, navbar pills |
| **Pipeline order** | Fixed: Position → Motion → Vibrate → Constrict |
| **PositionWithDuration state** | Uses start_time + duration, computes current position on the fly |
| **Linking UI style** | Toggle grid grouped by type (Position, Motion, Vibrate, Constrict), rows act as radio sets |
| **Input indicator** | Shows source name once connected ("T-Code ●" not "Input ● T-Code") |
| **Preset structure** | Unified structure with inputType field, works for both T-Code and Buttplug |

---

*This is a working plan. Update as implementation progresses.*
