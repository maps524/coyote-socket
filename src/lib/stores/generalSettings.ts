import { writable } from 'svelte/store';
import type { NoInputBehavior } from '$lib/types/modulation';
import type { ProcessingEngine, PeakFillStrategy } from './connection';

/**
 * General application settings
 * Controls debouncing, processing, and behavior
 */
export interface GeneralSettings {
  noInputBehavior: NoInputBehavior;
  noInputDecayMs: number;       // Decay time for 'decay' behavior (100-2000ms)
  updateRateMs: number;         // Backend state update rate (10-100ms, default: 50ms)
  saveRateMs: number;           // File persistence rate (100-2000ms, default: 500ms)
  showTCodeMonitor: boolean;    // Toggle T-Code monitor visibility
  processingEngine: ProcessingEngine;  // Moved from outputOptions for centralization
  peakFill: PeakFillStrategy;   // V2 Detailed variant: legacy cascade vs forward-fill
}

/**
 * Default general settings
 */
export const defaultGeneralSettings: GeneralSettings = {
  noInputBehavior: 'hold',
  noInputDecayMs: 1000,
  updateRateMs: 50,
  saveRateMs: 500,
  showTCodeMonitor: false,
  processingEngine: 'v2-smooth',
  peakFill: 'forward'
};

/**
 * Writable store for general settings
 */
export const generalSettings = writable<GeneralSettings>(defaultGeneralSettings);
