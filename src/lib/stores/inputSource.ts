import { writable, derived } from 'svelte/store';
import { connectionState } from './stateSync';

export type InputSource = 'none' | 'tcode' | 'buttplug' | 'lovense';

export interface ButtplugFeatureValue {
  featureType: string;  // 'Position', 'Vibrate', etc.
  featureIndex: number; // 0, 1, 2...
  value: number;        // 0.0-1.0
  label: string;        // Display label e.g., "Position 1", "Vibrate 2"
}

export interface InputSourceState {
  source: InputSource;
  buttplugFeatures: ButtplugFeatureValue[];
}

// Store for input source state
export const inputSourceState = writable<InputSourceState>({
  source: 'none',
  buttplugFeatures: []
});

// Derived store that auto-determines source from connection state
export const currentInputSource = derived(
  [connectionState, inputSourceState],
  ([$connectionState, $inputSourceState]) => {
    // Use the detected protocol from the backend (auto-detected from first message)
    const detected = $connectionState.detected_input_protocol;
    if (detected === 'tcode' || detected === 'buttplug' || detected === 'lovense') {
      return detected as InputSource;
    }

    // Fallback: check if we have any Buttplug feature values from local state
    if ($inputSourceState.buttplugFeatures.length > 0) {
      return 'buttplug' as InputSource;
    }

    return 'none' as InputSource;
  }
);

// Update Buttplug feature values (called from backend events)
export function updateButtplugFeatures(features: ButtplugFeatureValue[]) {
  inputSourceState.update(state => ({
    ...state,
    buttplugFeatures: features
  }));
}

// Clear all input values (on disconnect)
export function clearInputSource() {
  inputSourceState.set({
    source: 'none',
    buttplugFeatures: []
  });
}
