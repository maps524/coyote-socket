/**
 * Preset Selection Store
 *
 * Tracks which preset is selected for each input ecosystem (TCode vs Buttplug).
 * This allows switching between input modes while preserving the selected preset
 * for each mode independently.
 */

import { writable } from 'svelte/store';
import type { PresetEcosystem } from '$lib/types/settings';

interface PresetSelectionState {
  tcode: string;      // Selected preset name for TCode ecosystem (empty string = none)
  buttplug: string;   // Selected preset name for Buttplug ecosystem (empty string = none)
}

// Initialize from sessionStorage to survive HMR and page reloads
function loadInitialState(): PresetSelectionState {
  if (typeof sessionStorage !== 'undefined') {
    const tcodePreset = sessionStorage.getItem('selectedPreset:tcode') || '';
    const buttplugPreset = sessionStorage.getItem('selectedPreset:buttplug') || '';
    return { tcode: tcodePreset, buttplug: buttplugPreset };
  }
  return { tcode: '', buttplug: '' };
}

// Create the store
const presetSelection = writable<PresetSelectionState>(loadInitialState());

// Subscribe to persist to sessionStorage
presetSelection.subscribe(state => {
  if (typeof sessionStorage !== 'undefined') {
    if (state.tcode) {
      sessionStorage.setItem('selectedPreset:tcode', state.tcode);
    } else {
      sessionStorage.removeItem('selectedPreset:tcode');
    }

    if (state.buttplug) {
      sessionStorage.setItem('selectedPreset:buttplug', state.buttplug);
    } else {
      sessionStorage.removeItem('selectedPreset:buttplug');
    }
  }
});

export const presetSelectionStore = presetSelection;

/**
 * Set the selected preset for a specific ecosystem
 */
export function setSelectedPreset(ecosystem: PresetEcosystem, presetName: string) {
  presetSelection.update(state => ({
    ...state,
    [ecosystem]: presetName
  }));
}

/**
 * Get the selected preset name for a specific ecosystem
 */
export function getSelectedPreset(ecosystem: PresetEcosystem): string {
  let currentState: PresetSelectionState = { tcode: '', buttplug: '' };
  const unsubscribe = presetSelection.subscribe(state => {
    currentState = state;
  });
  unsubscribe();
  return currentState[ecosystem];
}

/**
 * Clear the selected preset for a specific ecosystem
 */
export function clearSelectedPreset(ecosystem: PresetEcosystem) {
  presetSelection.update(state => ({
    ...state,
    [ecosystem]: ''
  }));
}
