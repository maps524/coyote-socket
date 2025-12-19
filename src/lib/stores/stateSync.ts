/**
 * State Synchronization Store
 *
 * Handles event-driven state updates from the Rust backend.
 * This enables a stateless frontend that survives HMR reloads.
 *
 * The backend is the single source of truth for:
 * - Connection status (WebSocket, Bluetooth)
 * - Channel parameters
 * - Output options
 *
 * The frontend listens to events and updates its display accordingly.
 */

import { writable, get } from 'svelte/store';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

// ============================================================================
// Types
// ============================================================================

export interface BluetoothDevice {
  address: string;
  name: string | null;
  rssi: number | null;
}

export interface ConnectionStatus {
  websocket_running: boolean;
  detected_input_protocol: 'none' | 'tcode' | 'buttplug';
  bluetooth_connected: boolean;
  bluetooth_device_address: string | null;
  battery_level: number | null;
  discovered_devices: BluetoothDevice[];
}

export interface ChannelStateSnapshot {
  frequency: number;
  freq_balance: number;
  int_balance: number;
  range_min: number;
  range_max: number;
  current_intensity: number;
}

export interface OutputOptionsSnapshot {
  processing_engine: string;
  channel_interplay: string;
  chase_delay_ms: number;
}

export interface FullAppState {
  connection: ConnectionStatus;
  channel_a: ChannelStateSnapshot;
  channel_b: ChannelStateSnapshot;
  output_options: OutputOptionsSnapshot;
  timestamp: number;
}

interface ConnectionChangedPayload {
  connection_type: 'websocket' | 'bluetooth';
  connected: boolean;
  device_address: string | null;
  timestamp: number;
}

// ============================================================================
// Connection State Store
// ============================================================================

export const connectionState = writable<ConnectionStatus>({
  websocket_running: false,
  detected_input_protocol: 'none',
  bluetooth_connected: false,
  bluetooth_device_address: null,
  battery_level: null,
  discovered_devices: [],
});

// ============================================================================
// Event Listeners
// ============================================================================

let connectionChangedUnlisten: UnlistenFn | null = null;
let isStateSyncActive = false;

/**
 * Handle connection changed events from backend
 */
function handleConnectionChanged(payload: ConnectionChangedPayload) {
  connectionState.update(state => {
    if (payload.connection_type === 'websocket') {
      return {
        ...state,
        websocket_running: payload.connected,
      };
    } else if (payload.connection_type === 'bluetooth') {
      return {
        ...state,
        bluetooth_connected: payload.connected,
        bluetooth_device_address: payload.device_address,
        // Clear battery when disconnected
        battery_level: payload.connected ? state.battery_level : null,
      };
    }
    return state;
  });
}

/**
 * Start listening for state sync events from backend
 * Call this in App.svelte onMount
 */
export async function startStateSync(): Promise<void> {
  if (isStateSyncActive) return;
  isStateSyncActive = true;

  try {
    // Listen for connection changes
    connectionChangedUnlisten = await listen<ConnectionChangedPayload>(
      'connection-changed',
      (event) => {
        console.log('[StateSync] Connection changed:', event.payload);
        handleConnectionChanged(event.payload);
      }
    );

    console.log('[StateSync] Event listeners started');
  } catch (e) {
    console.error('[StateSync] Failed to start event listeners:', e);
    isStateSyncActive = false;
  }
}

/**
 * Stop listening for state sync events
 * Call this in App.svelte onDestroy
 */
export function stopStateSync(): void {
  isStateSyncActive = false;

  if (connectionChangedUnlisten) {
    connectionChangedUnlisten();
    connectionChangedUnlisten = null;
  }

  console.log('[StateSync] Event listeners stopped');
}

/**
 * Query current connection status from backend
 * Use this to restore state after HMR
 */
export async function refreshConnectionStatus(): Promise<ConnectionStatus> {
  try {
    const status = await invoke<ConnectionStatus>('get_connection_status');
    connectionState.set(status);
    return status;
  } catch (e) {
    console.error('[StateSync] Failed to refresh connection status:', e);
    throw e;
  }
}

/**
 * Query full application state from backend
 * Use this for complete state recovery after HMR
 */
export async function getFullState(): Promise<FullAppState> {
  try {
    const state = await invoke<FullAppState>('get_full_state');
    // Update connection state store
    connectionState.set(state.connection);
    return state;
  } catch (e) {
    console.error('[StateSync] Failed to get full state:', e);
    throw e;
  }
}

/**
 * Update battery level in connection state
 * Called when battery poll succeeds
 */
export function updateBatteryLevel(level: number | null): void {
  connectionState.update(state => ({
    ...state,
    battery_level: level,
  }));
}

/**
 * Check if state sync is currently active
 */
export function isStateSyncRunning(): boolean {
  return isStateSyncActive;
}
