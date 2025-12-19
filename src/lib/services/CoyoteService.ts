/**
 * CoyoteService - Utility service for DG-LAB Coyote communication
 *
 * NOTE: The main 10Hz update loop has been moved to the Rust backend (device.rs)
 * for HMR resilience. This service now only provides utility methods that the
 * frontend may need to call directly.
 *
 * The backend handles:
 * - 10Hz device update loop
 * - T-Code parsing and processing
 * - B0/BF command generation and sending
 * - Axis value tracking and event emission
 */

import { invoke } from '@tauri-apps/api/core';
import { generateB0Command, type CoyoteB0Command } from '../utils/protocol.js';

export class CoyoteService {
  // No longer maintains its own update loop - backend handles this

  constructor() {
    // Backend handles the 10Hz loop, nothing to start here
    console.log('[CoyoteService] Initialized (frontend utility mode - backend handles 10Hz loop)');
  }

  /**
   * Stop all stimulation immediately
   * This sends a zero-intensity command to the device
   */
  async stop(): Promise<void> {
    const stopCommand: CoyoteB0Command = {
      interpretationA: 0,
      interpretationB: 0,
      intensityA: 0,
      intensityB: 0,
      waveformAfrequency: [0, 0, 0, 0],
      waveformAintensity: [0, 0, 0, 0],
      waveformBfrequency: [0, 0, 0, 0],
      waveformBintensity: [0, 0, 0, 0]
    };

    try {
      const commandBytes = Array.from(generateB0Command(stopCommand));
      await invoke('send_coyote_command', { commandData: commandBytes });
      console.log('[CoyoteService] Stop command sent');
    } catch (error) {
      console.error('[CoyoteService] Failed to stop device:', error);
    }
  }

  /**
   * Clean up resources
   * No-op now that backend handles the update loop
   */
  destroy(): void {
    // Nothing to clean up - backend manages its own loop
    console.log('[CoyoteService] Destroyed');
  }
}

// Global service instance
export const coyoteService = new CoyoteService();
