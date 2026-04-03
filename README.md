# CoyoteSocket

A modern desktop application for controlling DG-LAB Coyote e-stim devices with T-Code or Buttplug input.

**[Download Now](https://github.com/maps524/coyote-socket/releases/latest)**

## Features

### Connection Management
- **WebSocket Server**: Built-in WebSocket server (default port 12346) for T-Code input
- **Protocol Detection**: Auto-detects T-Code vs Buttplug protocol
- **Real-time Status**: Live connection status and error reporting

### Bluetooth Integration
- **Device Discovery**: Scan for DG-LAB Coyote devices (identifier: "47L121000")
- **Adapter Selection**: Support for multiple Bluetooth adapters
- **Battery Monitoring**: Real-time battery level display
- **Automatic Reconnection**: Robust connection handling with auto-connect option

### Dual Channel Control
- **Independent Channels**: Separate control for channels A and B
- **Frequency Control**: 1-200 Hz with precision period calculation
- **Frequency Balance**: 0-255 range for waveform feel adjustment
- **Intensity Balance**: 0-255 range for pulse width control
- **Real-time Updates**: Live parameter synchronization with device

### Parameter Linking (T-Code to Channel Mapping)
Any channel parameter can be dynamically linked to any T-Code axis:
- **6 T-Code Axes**: L0-L2 (Stroke, Twist, Roll) and R0-R2 (independent axes)
- **4 Parameters per Channel**: Frequency, Frequency Balance, Intensity Balance, Intensity
- **Curve Transformations**: Linear, Exponential, Logarithmic, S-Curve, Inverse
- **Range Mapping**: Configure min/max output values for each axis
- **Midpoint Mode**: Use distance from center position instead of absolute value
- **Curve Strength**: Adjustable 0.1-3.0 strength for exp/log curves

### Presets System
- **Save/Load Presets**: Store complete channel configurations
- **Ecosystem-Scoped**: Separate presets for T-Code and Buttplug input modes
- **Dirty Tracking**: Visual indicator when preset has unsaved changes
- **Quick Selection**: Inline preset selector in header

### Buttplug Integration (Work in Progress)
Experimental support for Buttplug.io protocol:
- **Feature Types**: Position, Vibrate, Rotate, Oscillate, Constrict
- **Feature Pipeline**: Position → Motion → Vibrate → Constrict
- **Configurable Counts**: Set number of each feature type

### Advanced Features
- **Processing Engines**: Multiple algorithms (v1, v2-Smooth, v2-Balanced, v2-Detailed, v3-predictive)
- **Keyboard Shortcuts**: Fully configurable hotkeys
- **Output Pause**: Spacebar to pause all stimulation

### Modern UI
- Dark Theme
- **Responsive Design**: Adaptive layout for different screen sizes
- **Real-time Feedback**: Live parameter display, waveform visualization
- **Range Sliders with Indicators**: Visual feedback showing linked parameter values

## Installation

Download the latest release from the [GitHub Releases](https://github.com/maps524/coyote-socket/releases) page.

## Usage

### Initial Setup

1. **Open WebSocket Server**
   - The WebSocket server starts automatically (configurable)
   - Default port: 12346
   - Connect your T-Code source (e.g., Funscript player, custom scripts)

2. **Scan for Bluetooth Devices**
   - Click "Scan for Devices" to discover DG-LAB Coyote devices
   - Select your device from the dropdown
   - Click "Connect Coyote" to establish Bluetooth connection

### Channel Configuration

#### Parameter Linking
Each parameter (Frequency, Frequency Balance, Intensity Balance, Intensity) can be:
- **Static**: Manual slider control
- **Linked**: Dynamically controlled by a T-Code axis with curve transformation

Example: Link Channel A Intensity to L0 (stroke axis), and Channel B Intensity to L0 with Inverse alignment

#### Range Limits
- **Percentage Range**: Adjustable min/max intensity scaling
- **Visual Feedback**: Real-time percentage display with input position indicator
- **Independent Channels**: Separate limits for each channel

### Presets

1. Configure channels as desired
2. Click the **+** button to save as new preset
3. Select presets from the dropdown to load
4. Click the save icon to update an existing preset

### T-Code Protocol

Supported axes: `L0`, `L1`, `L2`, `R0`, `R1`, `R2`

- `L0<position>` - Set axis position (0-9999)
- `L0<position>I<interval>` - Ramped movement over time (ms)

#### Examples
```
L0500      # Set L0 (stroke) to 50%
R2750I1000 # Ramp R2 to 75% over 1 second
L09999     # Set L0 to maximum
L00        # Set L0 to minimum
DSTOP      # Emergency stop all channels
```

## Safety

**Important Safety Information**
- This device outputs electrical stimulation
- Use only as intended by the manufacturer
- Start with low intensities and increase gradually
- Stop immediately if you experience any discomfort
- Not for use by persons with heart conditions or pacemakers
- Adult use only - keep away from children

## Troubleshooting

### Connection Issues
- **Bluetooth**: Check Windows Bluetooth is enabled
- **WebSocket**: Ensure websocket port you've selected is not in use by another application

### Device Not Found
- Ensure DG-LAB Coyote is in pairing mode
- Check device identifier is "47L121000"
- Try rescanning after turning device off/on

### Performance Issues
- Close other Bluetooth applications
- Use a dedicated USB Bluetooth adapter

## Development

### Prerequisites
- **Node.js** 18+ and npm
- **Rust** (latest stable)
- **Git**

### Setup

1. **Clone and install dependencies**
```bash
npm install
```

2. **Run in development mode**
```bash
npm run tauri:dev
```

3. **Build for production**
```bash
npm run tauri:build
```

### Architecture
- **Frontend**: Svelte 5 with TypeScript and Tailwind CSS
- **Backend**: Rust with Tauri 2.0 for native desktop integration
- **Communication**: btleplug for Bluetooth LE, tokio-tungstenite for WebSocket
- **Protocol**: DG-LAB Coyote 0xB0/0xBF command generation

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

Based on the original DG-LAB open source Bluetooth protocol documentation.

## Acknowledgments

- DG-LAB for providing open source protocol documentation
- Original PyQt5 implementation authors
- Svelte and Tauri communities
- btleplug and tokio-tungstenite library authors
- [MultiFunPlayer](https://github.com/Yoooi0/MultiFunPlayer) for being such a great tool to plug into this
