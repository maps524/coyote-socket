import { writable, type Writable } from 'svelte/store';
import type { ParameterSource } from '$lib/types/modulation.js';

export interface ChannelParams {
  frequency: number;
  frequencyBalance: number;
  intensityBalance: number;
  period: number;
  rangeMin: number;
  rangeMax: number;

  // Parameter source configurations
  frequencySource?: ParameterSource;
  frequencyBalanceSource?: ParameterSource;
  intensityBalanceSource?: ParameterSource;
  intensitySource?: ParameterSource;  // For intensity range (linked to T-Code axis)
}

export interface RangeParams {
  min: number;
  max: number;
  range: number;
  maximum: number;
}

export type ChannelLetter = 'A' | 'B';

// Default axis per channel: A → L0 (Stroke), B → R2 (Pitch)
export function defaultIntensityAxisFor(letter: ChannelLetter): 'L0' | 'R2' {
  return letter === 'A' ? 'L0' : 'R2';
}

export function createChannelStore(letter: ChannelLetter): Writable<ChannelParams> {
  const intensityAxis = defaultIntensityAxisFor(letter);
  return writable<ChannelParams>({
    frequency: 100,         // 100Hz — balanced, distinct pulses
    frequencyBalance: 128,  // Neutral
    intensityBalance: 128,  // Neutral
    period: 10,
    rangeMin: 10,
    rangeMax: 20,

    frequencySource: {
      type: 'static',
      staticValue: 100,
      rangeMin: 1,
      rangeMax: 200,
      curve: 'linear'
    },
    frequencyBalanceSource: {
      type: 'static',
      staticValue: 128,
      rangeMin: 0,
      rangeMax: 255,
      curve: 'linear'
    },
    intensityBalanceSource: {
      type: 'static',
      staticValue: 128,
      rangeMin: 0,
      rangeMax: 255,
      curve: 'linear'
    },
    intensitySource: {
      type: 'linked',
      sourceAxis: intensityAxis,
      rangeMin: 10,
      rangeMax: 20,
      curve: 'linear'
    }
  });
}

export const channelA = createChannelStore('A');
export const channelB = createChannelStore('B');

export const rangeA = writable<RangeParams>({
  min: 10,
  max: 20,
  range: 5.0,
  maximum: 10.0
});

export const rangeB = writable<RangeParams>({
  min: 10,
  max: 20,
  range: 5.0,
  maximum: 10.0
});
