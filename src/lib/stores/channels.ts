import { writable } from 'svelte/store';
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

export const channelA = writable<ChannelParams>({
  frequency: 100,         // 100Hz (10ms period) - balanced, distinct pulses
  frequencyBalance: 128,  // Neutral - balanced high/low frequency feeling
  intensityBalance: 128,  // Neutral - balanced pulse width
  period: 10,
  rangeMin: 10,
  rangeMax: 20,

  // Default to static sources for balance/frequency
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
  // Intensity defaults to linked to L0 (Stroke) for Channel A
  intensitySource: {
    type: 'linked',
    sourceAxis: 'L0',
    rangeMin: 10,
    rangeMax: 20,
    curve: 'linear'
  }
});

export const channelB = writable<ChannelParams>({
  frequency: 100,         // 100Hz (10ms period) - balanced, distinct pulses
  frequencyBalance: 128,  // Neutral - balanced high/low frequency feeling
  intensityBalance: 128,  // Neutral - balanced pulse width
  period: 10,
  rangeMin: 10,
  rangeMax: 20,

  // Default to static sources for balance/frequency
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
  // Intensity defaults to linked to R2 (Pitch) for Channel B
  intensitySource: {
    type: 'linked',
    sourceAxis: 'R2',
    rangeMin: 10,
    rangeMax: 20,
    curve: 'linear'
  }
});

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