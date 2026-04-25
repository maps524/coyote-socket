import { writable } from 'svelte/store';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

export interface ControllerInfo {
  id: string;
  name: string;
  selected: boolean;
}

export interface GamepadStatus {
  connected: boolean;
  count: number;
  engine: 'off' | 'gilrs' | 'xinput' | string;
  controllers: ControllerInfo[];
  selected_id: string | null;
}

export const gamepadStatus = writable<GamepadStatus>({
  connected: false,
  count: 0,
  engine: 'off',
  controllers: [],
  selected_id: null,
});

let unlisten: UnlistenFn | null = null;

export async function startGamepadStatusSync(): Promise<void> {
  if (unlisten) return;
  try {
    unlisten = await listen<GamepadStatus>('gamepad-status', (event) => {
      gamepadStatus.set(event.payload);
    });
    // Seed with the current snapshot so the pill is correct before the first
    // change event arrives (also restores state after frontend HMR).
    try {
      const snapshot = await invoke<GamepadStatus>('get_gamepad_status');
      gamepadStatus.set(snapshot);
    } catch (e) {
      console.warn('[gamepadStatus] initial fetch failed:', e);
    }
  } catch (e) {
    console.error('[gamepadStatus] failed to start listener:', e);
  }
}

export function stopGamepadStatusSync(): void {
  if (unlisten) {
    unlisten();
    unlisten = null;
  }
}
