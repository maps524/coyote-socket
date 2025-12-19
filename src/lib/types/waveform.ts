/**
 * Waveform Data Types
 *
 * TypeScript definitions for waveform visualization data structures.
 * These match the Rust structs in src-tauri/src/waveform.rs
 */

/**
 * A single waveform sample capturing channel state at a specific moment
 * Samples are collected at 10Hz by the device loop
 */
export interface WaveformSample {
  /** Unix timestamp in milliseconds */
  timestamp: number;
  /** Channel A intensity (0-200) after range scaling */
  channel_a_intensity: number;
  /** Channel B intensity (0-200) after range scaling */
  channel_b_intensity: number;
  /** Channel A frequency in Hz (1-200) */
  channel_a_frequency: number;
  /** Channel B frequency in Hz (1-200) */
  channel_b_frequency: number;
}

/**
 * Full state snapshot for a single channel
 */
export interface ChannelState {
  /** Normalized intensity (0.0-1.0) */
  intensity: number;
  /** Frequency in Hz */
  frequency: number;
  /** Frequency balance (0-255) */
  frequency_balance: number;
  /** Intensity balance (0-255) */
  intensity_balance: number;
  /** Raw input value (0.0-1.0) before processing */
  input_value: number;
  /** Input source identifier (e.g., "L0", "R2", "static") */
  input_source: string;
}

/**
 * Waveform data response from backend
 */
export interface WaveformDataResponse {
  samples: WaveformSample[];
}

/**
 * Channel state response from backend
 */
export interface ChannelStateResponse {
  channel_a: ChannelState;
  channel_b: ChannelState;
}
