# CLAUDE.md - CoyoteSocket Project Context

## Project Overview

**CoyoteSocket** is a Svelte 5 + Tauri desktop application for controlling DG-LAB Coyote e-stim devices. It provides real-time parameter control via T-Code input over WebSocket connections with advanced parameter linking and curve transformations.

### Tech Stack
- **Frontend**: Svelte 5, TypeScript, Tailwind CSS, shadcn-ui
- **Backend**: Rust with Tauri 2.0
- **Protocols**: DG-LAB Coyote (0xB0/0xBF commands), T-Code v0.3, Buttplug (WIP)
- **Communication**: WebSockets, Bluetooth LE

## Project Structure

```
src/                        # Frontend (Svelte)
├── App.svelte              # Main component
├── main.ts                 # Vite entry point
└── lib/
    ├── components/         # UI components
    │   ├── ui/             # Reusable UI (Button, Slider, etc.)
    │   └── settings/       # Settings tab components
    ├── services/           # CoyoteService, connection handling
    ├── stores/             # Svelte stores for state
    │   ├── channels.ts     # Channel parameters & ParameterSource configs
    │   ├── connection.ts   # WebSocket/Bluetooth connection state
    │   ├── inputPosition.ts # Real-time T-Code axis values
    │   ├── presetSelection.ts # Selected preset per ecosystem
    │   └── buttplugSettings.ts # Buttplug feature counts
    ├── utils/              # Protocol implementations (tcode.ts, protocol.ts)
    └── types/              # TypeScript definitions
        ├── modulation.ts   # ParameterSource, CurveType definitions
        └── buttplug.ts     # Buttplug feature types
src-tauri/                  # Backend (Rust)
└── src/
    ├── main.rs             # Tauri commands & event emission
    ├── processing.rs       # Signal processing engine (multi-algorithm)
    ├── modulation.rs       # Curve transformations & parameter sources
    ├── websocket.rs        # WebSocket server & T-Code parsing
    ├── device.rs           # 10Hz device update loop
    ├── bluetooth.rs        # btleplug Bluetooth handling
    ├── protocol.rs         # DG-LAB 0xB0/0xBF command generation
    ├── waveform.rs         # Waveform synthesis
    ├── settings.rs         # Persistent configuration & presets
    ├── logging.rs          # Ring buffer logging
    └── buttplug/           # Buttplug protocol integration (WIP)
        ├── handler.rs
        ├── messages.rs
        ├── pipeline.rs
        ├── state.rs
        └── types.rs
```

## Development Commands

```bash
npm install          # Install dependencies
npm run tauri:dev    # Run development build (Vite + Tauri)
npm run tauri:build  # Production build (auto-bumps patch version)
npm run dev          # Frontend only (http://localhost:1421)
```

## Key Concepts

### Parameter Linking (ParameterSource)
Any channel parameter can be static or linked to a T-Code axis:
```typescript
interface ParameterSource {
  type: 'static' | 'linked'
  staticValue?: number
  sourceAxis?: 'L0' | 'L1' | 'L2' | 'R0' | 'R1' | 'R2'
  rangeMin: number
  rangeMax: number
  curve: 'linear' | 'exponential' | 'logarithmic' | 's-curve' | 'inverse'
  curveStrength?: number  // 0.1-3.0
  midpoint?: boolean      // Use distance from center
}
```

### Presets System
- Presets store complete channel configurations (all 4 parameters per channel)
- Ecosystem-scoped: separate selections for T-Code vs Buttplug input modes
- Stored in `%APPDATA%/com.coyotesocket.app/presets.json`

### Processing Engines
- **v1**: Queue-based ramping (original)
- **v2-Smooth**: Smoothest transitions
- **v2-Balanced**: Balance of smoothness and response
- **v2-Detailed**: Most responsive to input changes

## Critical Architecture

### Protocol Implementation
- **T-Code Parser** (`src/lib/utils/tcode.ts`): Parses L0-L2, R0-R2 axes
- **DG-LAB Protocol** (`src-tauri/src/protocol.rs`): Generates 0xB0/0xBF commands

### Real-time Processing
- Rust backend processes at configurable tick rate
- Curve transformations applied in `modulation.rs`
- Device communication at 10Hz in `device.rs`
- Frontend receives state updates via Tauri events

### State Management
- Svelte stores manage channel parameters, connection state, settings
- Dual debouncing: 50ms for UI responsiveness, 500ms for file I/O
- HMR resilience via `hmrReloading` flag

## Safety Considerations

This application controls e-stim hardware. All changes must:
- Respect intensity limits and safety bounds
- Handle connection failures gracefully
- Never allow uncontrolled output increases

## Debugging & Logs

The Rust backend writes logs to a ring buffer file (last 1000 lines).

**Log file location**: `%APPDATA%/com.coyotesocket.app/coyote-socket.log`

Log format:
```
[timestamp_ms] [LEVEL] message
```

Use `log_info!`, `log_warn!`, `log_error!`, `log_debug!` macros in Rust code.

## Related Documentation

- `docs/plans/` - Feature implementation plans
- `README.md` - User documentation
