import { writable } from 'svelte/store';
import type { ButtplugFeatureConfig } from '$lib/types/buttplug';
import { defaultButtplugFeatureConfig } from '$lib/types/buttplug';

/**
 * Writable store for Buttplug feature configuration
 * Controls how many of each feature type to advertise to Buttplug clients
 */
export const buttplugSettings = writable<ButtplugFeatureConfig>(defaultButtplugFeatureConfig);

/**
 * Reset Buttplug settings to defaults
 */
export function resetButtplugSettings() {
  buttplugSettings.set(defaultButtplugFeatureConfig);
}
