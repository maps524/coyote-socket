<script lang="ts">
  import RangeSliderWithIndicator from './ui/RangeSliderWithIndicator.svelte';
  import { Zap } from 'lucide-svelte';
  import { channelA, channelB } from '$lib/stores/channels.js';
  import { generalSettings } from '$lib/stores/generalSettings.js';
  import { resolvedChannelA, resolvedChannelB, indicatorOf } from '$lib/stores/resolvedState.js';
  import { currentInputSource } from '$lib/stores/inputSource.js';
  import { type ParameterSource } from '$lib/types/modulation.js';

  // Translate the live input source into the simpler tri-state
  // ('tcode' | 'buttplug' | 'none') that RangeSliderWithIndicator and
  // ButtplugLinkPanel understand:
  //  - lovense  → buttplug (shares the buttplug feature pipeline)
  //  - anything else (tcode, none, gamepad-only) → tcode
  // Falling through to 'tcode' keeps the T-Code-style link UI visible even
  // when no network input source is detected, so a parameter bound to a
  // gamepad axis (GP_*) is still configurable.
  $: effectiveInputMode = ($currentInputSource === 'buttplug' || $currentInputSource === 'lovense')
    ? 'buttplug' as const
    : 'tcode' as const;

  export let channel: 'A' | 'B';
  export let compact = false;
  export let shortcuts: {
    freqUp: string;
    freqDown: string;
    intUp: string;
    intDown: string;
    freqBalUp: string;
    freqBalDown: string;
    intBalUp: string;
    intBalDown: string;
  } | undefined = undefined;

  // Get reactive store for this channel
  $: store = channel === 'A' ? channelA : channelB;
  // Resolver-side snapshot for this channel — feeds the per-parameter
  // position indicators with post-curve, post-transforms values from the
  // backend `resolved-update` event (10Hz, RAF-smoothed).
  $: resolved = channel === 'A' ? resolvedChannelA : resolvedChannelB;

  // Channel parameters matching the original Python implementation
  $: frequency = $store.frequency;
  $: frequencyBalance = $store.frequencyBalance;
  $: intensityBalance = $store.intensityBalance;

  // Parameter sources - sync staticValue with store value for hotkey support
  $: frequencySource = (() => {
    const stored = $store.frequencySource;
    if (stored) {
      // If stored source is static, sync staticValue with store.frequency
      if (stored.type === 'static') {
        return { ...stored, staticValue: frequency };
      }
      return stored;
    }
    return {
      type: 'static' as const,
      staticValue: frequency,
      rangeMin: 1,
      rangeMax: 200,
      curve: 'linear' as const
    };
  })();

  $: frequencyBalanceSource = (() => {
    const stored = $store.frequencyBalanceSource;
    if (stored) {
      if (stored.type === 'static') {
        return { ...stored, staticValue: frequencyBalance };
      }
      return stored;
    }
    return {
      type: 'static' as const,
      staticValue: frequencyBalance,
      rangeMin: 0,
      rangeMax: 255,
      curve: 'linear' as const
    };
  })();

  $: intensityBalanceSource = (() => {
    const stored = $store.intensityBalanceSource;
    if (stored) {
      if (stored.type === 'static') {
        return { ...stored, staticValue: intensityBalance };
      }
      return stored;
    }
    return {
      type: 'static' as const,
      staticValue: intensityBalance,
      rangeMin: 0,
      rangeMax: 255,
      curve: 'linear' as const
    };
  })();

  $: intensitySource = $store.intensitySource ?? {
    type: 'linked' as const,
    sourceAxis: channel === 'A' ? 'L0' : 'R2',
    rangeMin: $store.rangeMin,
    rangeMax: $store.rangeMax,
    curve: 'linear' as const
  };

  // Indicator values come straight from the resolver. `indicatorOf`
  // returns `normalized_pre_range` (0..1, post-delay+curve+transforms)
  // for Linked samples and 0 for Static — RangeSliderWithIndicator
  // only renders the dot when the value is > 0, and a Static
  // parameter's `normalized_pre_range` can carry its raw static byte
  // (e.g. 100 for frequency) which would push the dot off-track.
  //
  // Caveat (sub G open issue from sub E): transforms attached to
  // non-`bp:` Linked intensity links are silently inert because the
  // V2/V3 engine path runs instead of the resolver. Backend
  // synthesizes a coherent `ResolvedSample` from engine state so the
  // dot still moves, which means the user gets a *plausible* indicator
  // even when their attached transforms are doing nothing. Closing
  // this UX hazard is a sub G follow-up: either route engine-path
  // intensity through a transform tail or surface a "transforms
  // inactive on this engine" hint here.
  $: freqIndicator = indicatorOf($resolved.frequency);
  $: freqBalIndicator = indicatorOf($resolved.frequency_balance);
  $: intBalIndicator = indicatorOf($resolved.intensity_balance);
  $: intensityIndicator = indicatorOf($resolved.intensity);

  // Build tooltip strings
  $: freqTooltip = `Controls the pulse frequency (1-200 Hz)${shortcuts ? ` <code>${shortcuts.freqDown}/${shortcuts.freqUp}</code>` : ''}`;
  $: freqBalTooltip = `Controls waveform pulse width (0-255)${shortcuts ? ` <code>${shortcuts.freqBalDown}/${shortcuts.freqBalUp}</code>` : ''}`;
  $: intBalTooltip = `Adjusts high/low frequency feeling (0-255)${shortcuts ? ` <code>${shortcuts.intBalDown}/${shortcuts.intBalUp}</code>` : ''}`;
  $: intensityTooltip = `Min/max output levels${shortcuts ? ` <code>${shortcuts.intDown}/${shortcuts.intUp}</code>` : ''}`;

  // Snap frequency to valid period-based value
  function snapFrequency(value: number): number {
    const period = Math.round(1000 / value);
    const clampedPeriod = Math.max(5, Math.min(1000, period)); // 5ms=200Hz to 1000ms=1Hz
    return 1000 / clampedPeriod;
  }

  // Period-based frequency stepping (for scroll wheel)
  function frequencyWheelStep(currentValue: number, direction: 'up' | 'down'): number {
    const currentPeriod = Math.round(1000 / currentValue);
    // Decrease period = increase frequency, increase period = decrease frequency
    const newPeriod = direction === 'up' ? currentPeriod - 1 : currentPeriod + 1;
    // Clamp period to valid range (5ms = 200Hz, 1000ms = 1Hz)
    const clampedPeriod = Math.max(5, Math.min(1000, newPeriod));
    return 1000 / clampedPeriod;
  }

  // Source-change handlers all share the same shape: update the channel
  // store and let App.svelte's reactive watcher push the full channel
  // through apply_channel_config (debounced, with persist=false on the fast
  // path and persist=true on the trailing edge). Per-parameter Tauri
  // commands are gone; the store is the only thing the UI writes to.
  function handleFrequencySourceChange(event: CustomEvent<ParameterSource>) {
    const newSource = event.detail;
    const snappedFreq = newSource.type === 'static'
      ? snapFrequency(newSource.staticValue ?? 100)
      : undefined;
    const finalSource = snappedFreq !== undefined
      ? { ...newSource, staticValue: snappedFreq }
      : newSource;
    store.update(s => ({
      ...s,
      frequencySource: finalSource,
      frequency: snappedFreq ?? s.frequency
    }));
  }

  function handleFrequencyBalanceSourceChange(event: CustomEvent<ParameterSource>) {
    const newSource = event.detail;
    store.update(s => ({
      ...s,
      frequencyBalanceSource: newSource,
      frequencyBalance: newSource.type === 'static' ? (newSource.staticValue ?? 128) : s.frequencyBalance
    }));
  }

  function handleIntensityBalanceSourceChange(event: CustomEvent<ParameterSource>) {
    const newSource = event.detail;
    store.update(s => ({
      ...s,
      intensityBalanceSource: newSource,
      intensityBalance: newSource.type === 'static' ? (newSource.staticValue ?? 128) : s.intensityBalance
    }));
  }

  function handleIntensitySourceChange(event: CustomEvent<ParameterSource>) {
    const newSource = event.detail;
    store.update(s => ({
      ...s,
      intensitySource: newSource,
      rangeMin: newSource.rangeMin,
      rangeMax: newSource.rangeMax
    }));
  }
</script>

<div class="bg-card border rounded-lg overflow-hidden {channel === 'A' ? 'border-primary/30' : 'border-secondary/30'}">
  <!-- Header -->
  <div class="flex items-center justify-between px-3 py-1.5 border-b {channel === 'A' ? 'bg-primary/10 border-primary/30' : 'bg-secondary/10 border-secondary/30'}">
    <div class="flex items-center gap-1.5">
      <Zap class="h-3.5 w-3.5 {channel === 'A' ? 'text-primary' : 'text-secondary'}" />
      <span class="text-sm font-medium {channel === 'A' ? 'text-primary' : 'text-secondary'}">Channel {channel}</span>
    </div>
  </div>

  <div class="{compact ? 'p-3 space-y-3' : 'p-4 space-y-6'}">
    <!-- Frequency Control with Source Selection -->
    <RangeSliderWithIndicator
      {channel}
      parameterName="Frequency"
      source={frequencySource}
      indicatorValue={freqIndicator}
      min={1}
      max={200}
      step={1}
      compact={compact}
      showLabels={true}
      showWrapper={false}
      tooltip={freqTooltip}
      wheelStep={frequencyWheelStep}
      inputMode={effectiveInputMode}
      on:sourceChange={handleFrequencySourceChange}
    />

    <!-- Frequency Balance Control with Source Selection -->
    <RangeSliderWithIndicator
      {channel}
      parameterName="Freq Balance"
      source={frequencyBalanceSource}
      indicatorValue={freqBalIndicator}
      min={0}
      max={255}
      step={1}
      compact={compact}
      showLabels={true}
      showWrapper={false}
      tooltip={freqBalTooltip}
      inputMode={effectiveInputMode}
      on:sourceChange={handleFrequencyBalanceSourceChange}
    />

    <!-- Intensity Balance Control with Source Selection -->
    <RangeSliderWithIndicator
      {channel}
      parameterName="Int Balance"
      source={intensityBalanceSource}
      indicatorValue={intBalIndicator}
      min={0}
      max={255}
      step={1}
      compact={compact}
      showLabels={true}
      showWrapper={false}
      tooltip={intBalTooltip}
      inputMode={effectiveInputMode}
      on:sourceChange={handleIntensityBalanceSourceChange}
    />

    <!-- Intensity Limits with Source Selection (no divider).
         Slider max is downsampled to per-channel "soft mode" cap so the visible
         range matches the device's enforced ceiling. -->
    <RangeSliderWithIndicator
      {channel}
      parameterName="Intensity"
      source={intensitySource}
      indicatorValue={intensityIndicator}
      min={0}
      max={channel === 'A' ? $generalSettings.channelAMaxIntensity : $generalSettings.channelBMaxIntensity}
      step={2}
      compact={compact}
      showLabels={true}
      showWrapper={false}
      tooltip={intensityTooltip}
      isIntensity={true}
      inputMode={effectiveInputMode}
      on:sourceChange={handleIntensitySourceChange}
    />
  </div>
</div>
