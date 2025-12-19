# CoyoteSocket Processing Pipeline Architecture

Complete data flow documentation from WebSocket input to Bluetooth device output.

---

## Overview Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              INPUT LAYER                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│  WebSocket Server (websocket.rs)                                            │
│  └── Auto-detects protocol on first message                                 │
│       ├── T-Code: "L0500", "R2750I1000"                                     │
│       └── Buttplug: JSON arrays [{...}]                                     │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           PROCESSING LAYER                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│  ProcessingState (processing.rs)                                            │
│  ├── axis_values: HashMap<String, AxisState>    ◄─── T-Code values         │
│  ├── buttplug_features: HashMap<String, f64>    ◄─── Buttplug values       │
│  ├── V1/V2/V3 Channel States                    ◄─── Processing engines    │
│  └── channel_a/b_config                         ◄─── Parameter configs     │
│                                                                              │
│  Modulation (modulation.rs)                                                 │
│  └── ParameterSource: Static OR Linked to axis                              │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼ (Every 100ms / 10Hz)
┌─────────────────────────────────────────────────────────────────────────────┐
│                           OUTPUT GENERATION                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│  get_next_waveform_data()                                                   │
│  ├── Extract 4 intensity values (0-200 device units)                        │
│  ├── Apply channel interplay (Mirror/Chase/Alternating)                     │
│  └── Generate WaveformData { intensity, waveform_intensity[4] }             │
│                                                                              │
│  send_device_update()                                                       │
│  ├── Resolve frequency, balance from sources                                │
│  ├── Scale intensity by range (if linked)                                   │
│  └── Build B0 command bytes                                                 │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           BLUETOOTH LAYER                                    │
├─────────────────────────────────────────────────────────────────────────────┤
│  BluetoothManager (bluetooth.rs)                                            │
│  └── write_command(&b0_command) → DG-LAB Coyote Device                     │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 1. WebSocket Input Layer

**File**: `src-tauri/src/websocket.rs`

### Server Setup

- Binds to `127.0.0.1:{port}` (default 12346)
- Spawns async TCP listener accepting WebSocket connections
- Auto-detects input protocol on first message

### Protocol Detection (`detect_protocol()`)

```rust
fn detect_protocol(message: &str) -> InputProtocol {
    if message.starts_with('[') {
        InputProtocol::Buttplug    // JSON arrays
    } else if message.starts_with(['L', 'R', 'V', 'A', 'D']) {
        InputProtocol::TCode       // T-Code commands
    } else {
        InputProtocol::TCode       // Default fallback
    }
}
```

### T-Code Input Handler

Parses commands like:
- `L0500` → Channel A at 50%
- `R2750I1000` → Ramp to 75% over 1000ms
- `D0`, `D1`, `D2` → Device info queries
- `DSTOP` → Stop command

**Output Data Structure**:
```rust
pub struct TCodeCommand {
    pub axis: String,           // "L0", "R2", "V1", etc.
    pub value: f64,             // 0.0-1.0 normalized
    pub interval_ms: Option<u32>, // Ramp duration (optional)
    pub received_at: u64,       // Timestamp
}
```

### Buttplug Input Handler

- JSON parsing of Buttplug v2 protocol messages
- Routes to `buttplug::handler::handle_buttplug_message()`
- Processes client commands, returns server responses

---

## 2. Processing Layer

**Files**: `src-tauri/src/processing.rs`, `src-tauri/src/modulation.rs`

### Central State (`ProcessingState`)

Global `Arc<RwLock<ProcessingState>>` containing:

```rust
pub struct ProcessingState {
    // Input tracking
    pub axis_values: HashMap<String, AxisState>,      // All T-Code axis values
    pub buttplug_features: HashMap<String, f64>,      // Buttplug feature values

    // Channel configurations
    pub channel_a_config: ChannelConfig,
    pub channel_b_config: ChannelConfig,

    // Buttplug routing
    pub buttplug_link_config_a: ButtplugLinkConfig,
    pub buttplug_link_config_b: ButtplugLinkConfig,

    // Processing engines (one of each per channel)
    pub v1_channel_a: V1ChannelState,
    pub v1_channel_b: V1ChannelState,
    pub v2_channel_a: V2ChannelState,
    pub v2_channel_b: V2ChannelState,
    pub v3_channel_a: V3ChannelState,
    pub v3_channel_b: V3ChannelState,

    // Interplay history
    pub a_history: VecDeque<u8>,  // 24 slots for chase modes
}
```

### Processing Engines

#### V1: Queue-Based (Original)
```rust
struct V1ChannelState {
    ramp_queue: VecDeque<u8>,  // Pre-computed values at 25ms intervals
}
```
- Generates queue entries when ramping
- Dequeues 4 values per 100ms output window

#### V2: Interpolation-Based
```rust
struct V2ChannelState {
    current_value: f64,
    target_value: f64,
    ramp_start: Instant,
    ramp_duration: Duration,
    downsampler: Downsampler,
}
```
- Linear interpolation between current and target
- Four downsampling variants:
  - **V2Smooth**: Averaging
  - **V2Balanced**: Linear interpolation
  - **V2Detailed**: Peak-preserving
  - **V2Dynamic**: Oscillation-preserving

#### V3: Predictive/Lookahead
```rust
struct V3ChannelState {
    command_buffer: VecDeque<BufferedCommand>,
    lookahead_ms: u32,  // Default 1000ms
}
```
- Delays commands (treats them as "effective 1s from now")
- Identifies critical points (peaks/valleys) for smooth ramps
- Best for MFP-style instant position updates

### Command Processing Flow

```
TCodeCommand arrives
    │
    ▼
process_command()
    │
    ├── Track axis value: axis_values["L0"] = 0.5
    │
    ├── Route to channel (if intensity linked to this axis)
    │   │
    │   ├── Apply midpoint transformation (if enabled)
    │   │   └── value = if value < 0.5 { 0 } else { (value - 0.5) * 2 }
    │   │
    │   ├── Apply curve transformation
    │   │   └── Linear, Exponential, Logarithmic, S-Curve, Inverse
    │   │
    │   └── Convert to device units: (0.0-1.0) → (0-200)
    │
    ├── Apply to V1 state (queue)
    ├── Apply to V2 state (interpolation)
    └── Apply to V3 state (lookahead buffer)
```

### Parameter Modulation System

**File**: `src-tauri/src/modulation.rs`

```rust
pub enum ParameterSourceType {
    Static,     // Fixed value
    Linked,     // Linked to T-Code axis
}

pub struct ParameterSource {
    pub source_type: ParameterSourceType,
    pub static_value: f64,
    pub source_axis: String,
    pub range_min: f64,
    pub range_max: f64,
    pub curve: CurveType,
    pub curve_strength: f64,
}
```

**Curve Types**:
- Linear (no transformation)
- Exponential (more responsive at high values)
- Logarithmic (more responsive at low values)
- S-Curve (smooth acceleration/deceleration)
- Inverse (flip the curve)

**No-Input Behavior** (when axis stale >1s):
- `Hold` - Keep last value
- `Default` - Return to default value
- `Decay` - Gradually decay to zero
- `Zero` - Immediately zero

---

## 3. Buttplug Integration

**Files**: `src-tauri/src/buttplug/*.rs`

### Feature Link Configuration

```rust
pub struct ButtplugLinkConfig {
    pub position_feature: Option<usize>,
    pub pos_dur_feature: Option<usize>,
    pub vibrate_feature: Option<usize>,
    pub rotate_feature: Option<usize>,
    pub oscillate_feature: Option<usize>,
    pub constrict_feature: Option<usize>,
    // Each with config: scale, distance, max_speed, etc.
}
```

### Feature Value Storage

```rust
pub struct ButtplugFeatureValues {
    pub position: Vec<f64>,                      // 0.0-1.0
    pub position_with_duration: Vec<(f64, u32)>, // (position, duration_ms)
    pub vibrate: Vec<f64>,
    pub rotate: Vec<(f64, bool)>,                // (speed, clockwise)
    pub oscillate: Vec<f64>,
    pub constrict: Vec<f64>,
}
```

### Pipeline Processing (`process_buttplug_pipeline()`)

Processing order is fixed:

```
Stage 1: Position
    └── Sets base_value from Position or PositionWithDuration

Stage 2: Motion (Rotate OR Oscillate)
    └── Adds oscillation/rotation offset to base_value

Stage 3: Vibrate
    └── Adds sine-wave wobble: offset += sin(t) * distance

Stage 4: Constrict
    └── Downsamples output to constrained range

Output: Single f64 value (0.0-1.0)
```

---

## 4. Output Generation

**Triggered**: Every 100ms (10Hz) by device loop

### Waveform Data Extraction

```rust
fn get_next_waveform_data(channel: Channel) -> WaveformData {
    // Extract 4 values based on selected processing engine
    let values = match processing_engine {
        V1 => dequeue_4_values(&v1_state.ramp_queue),
        V2Smooth => v2_state.interpolate_smooth(4),
        V2Balanced => v2_state.interpolate_balanced(4),
        V2Detailed => v2_state.interpolate_detailed(4),
        V2Dynamic => v2_state.interpolate_dynamic(4),
        V3 => v3_state.extract_lookahead(4),
    };

    WaveformData {
        intensity: values.iter().max(),  // Peak for this window
        waveform_intensity: values,       // 4 relative intensities (0-100)
    }
}
```

### Channel Interplay

Applied after waveform extraction:

| Mode | Behavior |
|------|----------|
| `None` | Channels independent |
| `Mirror` | B copies A |
| `MirrorInverted` | B = inverse of A |
| `Chase` | B follows A with delay |
| `ChaseInverted` | B follows inverted A with delay |
| `Alternating` | Ping-pong: A on slots 0,2; B on slots 1,3 |

### WaveformData Structure

```rust
pub struct WaveformData {
    pub intensity: u8,              // Max intensity 0-200
    pub waveform_intensity: [u8; 4], // Relative intensity per slot 0-100
}
```

---

## 5. Device Command Generation

### Range Scaling

Applied ONLY to linked intensity (not static):

```rust
fn scale_intensity(intensity: u8, range_min: u8, range_max: u8) -> u8 {
    // Maps 0-200 input to range [min, max]
    range_min + (intensity * (range_max - range_min) / 200)
}
```

### B0 Command Format

```
Byte 0:    0xB0 (header)
Byte 1:    interpretation_byte
           └── serial (4 bits) + interp_a (2 bits) + interp_b (2 bits)
Byte 2:    intensity_a (0-200)
Byte 3:    intensity_b (0-200)
Bytes 4-7: waveform_a_frequency[4] (period values)
Bytes 8-11: waveform_a_intensity[4] (0-100 relative)
Bytes 12-15: waveform_b_frequency[4]
Bytes 16-19: waveform_b_intensity[4]
```

### Frequency Conversion

```rust
fn frequency_to_period(f_hz: f64) -> u8 {
    let period_ms = 1000.0 / f_hz;
    convert_period(period_ms)
}

fn convert_period(period: f64) -> u8 {
    if period <= 100.0 {
        period as u8
    } else if period <= 600.0 {
        ((period - 100.0) / 5.0 + 100.0) as u8
    } else if period <= 1000.0 {
        ((period - 600.0) / 10.0 + 200.0) as u8
    } else {
        240
    }
}
```

---

## 6. Bluetooth Output Layer

**File**: `src-tauri/src/bluetooth.rs`

### Connection Flow

1. `scan_bluetooth_devices(adapter_index)` - Discover devices
2. `connect_bluetooth_device(address)` - Establish connection
3. `read_battery()` - Get battery level
4. `write_command(&command_data)` - Send B0 commands
5. `disconnect_device()` - Clean disconnect

### Real-time Sending

- Device loop calls `manager.write_command(&b0_command)`
- Runs at 10Hz (100ms intervals)
- When paused, sends zero-intensity command

---

## 7. State Management

### Frontend Stores (Svelte)

| Store | Purpose |
|-------|---------|
| `connection.ts` | WebSocket/Bluetooth connection status |
| `channels.ts` | Channel parameter values |
| `inputSource.ts` | Parameter source types and linked axes |
| `buttplugSettings.ts` | Buttplug feature link configurations |
| `stateSync.ts` | HMR-resilient state recovery |

### Backend → Frontend Events

| Event | Trigger | Data |
|-------|---------|------|
| `axis-update` | T-Code input received | `{ axis, value }` |
| `connection-changed` | WS/BT status change | Connection state |
| `waveform-sample` | Every 100ms | Current output values |
| `buttplug-features` | Buttplug update | Feature values |
| `output-pause-changed` | Pause toggled | Pause state |

### State Query Commands

For HMR recovery and initial sync:
- `get_connection_status()` - Current connection state
- `get_full_state()` - All configs, parameters, status
- `get_channel_intensities()` - Current intensity values
- `get_axis_values()` - Current axis values with stale handling

---

## 8. Settings Persistence

**File**: `src-tauri/src/settings.rs`

### Settings Structure

```rust
Settings {
    connection: ConnectionSettings {
        websocket_port: u16,
        auto_open_websocket: bool,
        show_tcode_monitor: bool,
    },
    bluetooth: BluetoothSettings {
        interface: String,
        auto_scan: bool,
        auto_connect: bool,
        saved_devices: Vec<SavedDevice>,
    },
    channel_a: ChannelSettings { ... },
    channel_b: ChannelSettings { ... },
    output: OutputSettings {
        channel_interplay: String,
        processing_engine: String,
        chase_delay_ms: u32,
    },
    general: GeneralSettings {
        no_input_behavior: String,
        decay_time_ms: u32,
    },
}
```

### Persistence

- Location: JSON files alongside executable
- Files: `settings.json`, `presets.json`
- Auto-migration for legacy formats

---

## 9. Complete Data Flow Examples

### T-Code Flow

```
WebSocket: "L0500 R2750I1000"
    │
    ▼
detect_protocol() → TCode
    │
    ▼
parse_tcode() → [
    TCodeCommand { axis: "L0", value: 0.5, interval: None },
    TCodeCommand { axis: "R2", value: 0.75, interval: Some(1000) }
]
    │
    ▼
process_command("L0", 0.5, None)
    ├── axis_values["L0"] = AxisState { value: 0.5, timestamp }
    ├── If intensity_a linked to "L0":
    │   ├── midpoint? → (0.5 - 0.5) * 2 = 0.0 (or skip)
    │   ├── curve(0.5, Exponential) → 0.25
    │   ├── to_device_units(0.25) → 50
    │   ├── v1_a.queue_value(50)
    │   ├── v2_a.set_target(50)
    │   └── v3_a.buffer_command(50)
    └── emit("axis-update", { axis: "L0", value: 0.5 })
    │
    ▼ (100ms later)
get_next_waveform_data(ChannelA)
    ├── Extract [50, 50, 50, 50] from selected engine
    ├── apply_interplay() if needed
    └── WaveformData { intensity: 50, waveform_intensity: [50, 50, 50, 50] }
    │
    ▼
send_device_update()
    ├── Resolve frequency from source
    ├── scale_intensity(50, range_min, range_max)
    ├── Build B0 command bytes
    └── bluetooth.write_command(b0_bytes)
    │
    ▼
DG-LAB Coyote outputs stimulation
```

### Buttplug Flow

```
WebSocket: [{"LinearCmd": {"Id": 1, "DeviceIndex": 0, "Vectors": [{"Index": 0, "Duration": 500, "Position": 0.75}]}}]
    │
    ▼
detect_protocol() → Buttplug
    │
    ▼
handle_buttplug_message()
    ├── Parse LinearCmd
    └── Update position_with_duration[0] = (0.75, 500)
    │
    ▼ (100ms later)
process_buttplug_pipeline(channel_a)
    │
    ├── Stage 1 (PosDur): Interpolate to 0.75 over 500ms
    │   └── base_value = 0.6 (current interpolated)
    │
    ├── Stage 2 (Motion): Skip (no rotate/oscillate linked)
    │
    ├── Stage 3 (Vibrate): Skip (no vibrate linked)
    │
    └── Stage 4 (Constrict): Skip (no constrict linked)
    │
    ▼
output = 0.6
    │
    ▼
to_device_units(0.6) → 120
    │
    ▼
WaveformData { intensity: 120, waveform_intensity: [...] }
    │
    ▼
B0 command → Bluetooth → Device
```

---

## 10. Timing Summary

| Operation | Timing |
|-----------|--------|
| WebSocket input | Event-driven (on message) |
| Axis emission to frontend | Immediate on input |
| Processing engine update | Immediate on input |
| Waveform generation | Every 100ms (10Hz) |
| Device command send | Every 100ms (tied to waveform) |
| Stale axis timeout | 1000ms |

---

## 11. Key Architectural Decisions

1. **Axis Tracking**: All T-Code axes stored centrally, enabling any parameter to link to any axis

2. **State Separation**:
   - Frontend: Settings, UI state, connection status
   - Backend: Processing engine, axis values, Buttplug features
   - Device: Final command generation

3. **Range Scaling**: Applied only to linked intensity to avoid double-application

4. **Thread Safety**: `Arc<RwLock<>>` for global state with single-level locking

5. **HMR Resilience**: 10Hz loop in Rust backend; state queryable for recovery

6. **Fixed Output Rate**: 10Hz regardless of input rate for predictable device behavior
