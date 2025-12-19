# Future Ideas - CoyoteSocket

Ideas and features for future consideration. These are not actively planned but may be implemented later.

---

## Parameter Modulation

### Multi-Axis Combinations
Combine multiple T-Code axes to control a single parameter.

**Examples:**
- `Frequency = L0 * 0.5 + L1 * 0.5` (average of two axes)
- `Intensity = max(L0, R2)` (whichever is higher)
- `FreqBalance = L0 * R1` (multiply for complex modulation)

**Operations to support:**
- Average
- Maximum
- Minimum
- Add
- Multiply
- Subtract

**UI concept:**
```
┌─ Source Configuration ──────────────────┐
│ [L0 ▼] [×0.5] [+ ▼] [L1 ▼] [×0.5]      │
│                                         │
│ Preview: (L0 × 0.5) + (L1 × 0.5)       │
└─────────────────────────────────────────┘
```

---

### Custom Curve Editor
Visual editor for creating custom response curves.

**Features:**
- Drag control points on a graph
- Add/remove points
- Curve smoothing options
- Save/load curve presets
- Import/export as JSON

**UI concept:**
```
┌─ Curve Editor ──────────────────────────┐
│   100% ┤        ●───────●              │
│        │       ╱                        │
│    50% ┤    ●─╯                         │
│        │   ╱                            │
│     0% ┼──●─────────────────────────   │
│        0%      50%      100%            │
│                                         │
│ [Add Point] [Reset] [Save] [Load]       │
└─────────────────────────────────────────┘
```

---

### LFO/Oscillator Sources
Built-in oscillators as parameter sources for rhythmic effects.

**Waveforms:**
- Sine
- Triangle
- Square
- Sawtooth
- Random/Noise

**Parameters:**
- Frequency (0.1 - 10 Hz)
- Amplitude (0-100%)
- Offset (center point)
- Phase offset

**Use cases:**
- Rhythmic frequency sweeps
- Pulsing intensity modulation
- Random variation

---

## Channel Features

### Parameter Presets
Save and quickly switch between complete channel configurations.

**Features:**
- Name presets
- Save all parameters (freq, balances, intensity config, curves)
- Quick-switch buttons or dropdown
- Import/export presets

---

### Recording/Playback
Record parameter changes over time and play them back.

**Features:**
- Record all parameter changes with timestamps
- Loop playback
- Trim/edit recordings
- Export as automation data

---

## UI Enhancements

### Theme Support
Light and dark mode themes.

**Implementation:**
- CSS variables for theme colors
- System preference detection
- Manual override in settings

---

### Real-time Value Display
Show live resolved values next to parameter controls.

**Example:**
```
Frequency: [L0 ▼] [50]──[200] Hz  → Current: 127 Hz
```

---

### Visual Feedback on Curves
Animate a dot moving along the curve based on current input.

---

## Advanced Processing

### Conditional Logic
Parameter behavior based on conditions.

**Examples:**
- If L0 > 0.5: use Curve A, else use Curve B
- If R2 < 0.1: disable Channel B
- Threshold-based mode switching

---

### Smart Smoothing
Intelligent smoothing that adapts to input patterns.

- Fast response to quick changes
- Smooth response to gradual changes
- Adjustable sensitivity

---

### Beat Detection
Detect rhythmic patterns in input and sync effects.

- Auto-detect BPM from input patterns
- Sync LFOs to detected beat
- Quantize output to beat grid

---

## Integration

### MIDI Support
Accept MIDI CC messages as parameter sources.

- Map MIDI channels/CCs to parameters
- Support MIDI learn (detect incoming CC)
- Multiple MIDI device support

---

### OSC Support
Open Sound Control protocol for integration with audio software.

- Receive OSC messages
- Map OSC addresses to parameters
- Bidirectional (send current state)

---

### Plugin System
Allow third-party extensions.

- Custom curve functions
- Custom input sources
- Custom processing modes

---

## Performance

### GPU-Accelerated Visualization
Use WebGL for smooth visualizations.

- Real-time waveform display
- Curve rendering
- Input visualization

---

### Benchmark Mode
Test and display processing performance.

- Measure latency
- Show update rate
- Identify bottlenecks

---

## Notes

- Ideas in this file are not committed features
- Priority and feasibility not yet assessed
- Some may never be implemented
- Feel free to add new ideas with date

---

*Last updated: 2025-01-15*
