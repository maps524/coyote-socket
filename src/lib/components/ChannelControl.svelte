<script lang="ts">
  import RangeSliderWithIndicator from './ui/RangeSliderWithIndicator.svelte';
  import { Zap } from 'lucide-svelte';
  import { channelA, channelB } from '$lib/stores/channels.js';
  import { generalSettings } from '$lib/stores/generalSettings.js';
  import { allAxisValues } from '$lib/stores/inputPosition.js';
  import { currentInputSource, inputSourceState } from '$lib/stores/inputSource.js';
  import { type ParameterSource, type ButtplugLinks, applySourceTransform } from '$lib/types/modulation.js';

  // Lovense input feeds the same Buttplug feature pipeline, so the link UI
  // (RangeSliderWithIndicator → ButtplugLinkPanel) only knows about
  // 'tcode' | 'buttplug' | 'none'. Translate at the boundary.
  $: effectiveInputMode = ($currentInputSource === 'lovense' ? 'buttplug' : $currentInputSource) as 'tcode' | 'buttplug' | 'none';

  // Helper: Get Buttplug feature value from the current features based on linked config
  // Returns 0-1 normalized value or 0 if not found
  function getButtplugIndicatorValue(links: ButtplugLinks | undefined): number {
    if (!links) return 0;

    const features = $inputSourceState.buttplugFeatures;
    if (features.length === 0) return 0;

    // Check each pipeline stage in order of priority for the indicator
    // Position is the primary source, others modulate it
    if (links.position) {
      const feature = features.find(f =>
        f.featureType === links.position!.featureType &&
        f.featureIndex === links.position!.featureIndex
      );
      if (feature) return feature.value;
    }

    // Fall back to motion features if no position
    if (links.motion) {
      const feature = features.find(f =>
        f.featureType === links.motion!.featureType &&
        f.featureIndex === links.motion!.featureIndex
      );
      if (feature) return feature.value;
    }

    // Fall back to vibrate if no position/motion
    if (links.vibrate) {
      const feature = features.find(f =>
        f.featureType === 'Vibrate' &&
        f.featureIndex === links.vibrate!.featureIndex
      );
      if (feature) return feature.value;
    }

    // Fall back to constrict
    if (links.constrict) {
      const feature = features.find(f =>
        f.featureType === 'Constrict' &&
        f.featureIndex === links.constrict!.featureIndex
      );
      if (feature) return feature.value;
    }

    return 0;
  }

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

  // Get indicator values based on linked source axis or Buttplug features
  // In Buttplug mode, use Buttplug feature values; in T-Code mode, use axis values
  $: freqIndicator = (() => {
    if (($currentInputSource === 'buttplug' || $currentInputSource === 'lovense') && frequencySource.buttplugLinks) {
      return getButtplugIndicatorValue(frequencySource.buttplugLinks);
    }
    if (frequencySource.type === 'linked' && frequencySource.sourceAxis) {
      return applySourceTransform($allAxisValues[frequencySource.sourceAxis] ?? 0, frequencySource);
    }
    return 0;
  })();

  $: freqBalIndicator = (() => {
    if (($currentInputSource === 'buttplug' || $currentInputSource === 'lovense') && frequencyBalanceSource.buttplugLinks) {
      return getButtplugIndicatorValue(frequencyBalanceSource.buttplugLinks);
    }
    if (frequencyBalanceSource.type === 'linked' && frequencyBalanceSource.sourceAxis) {
      return applySourceTransform($allAxisValues[frequencyBalanceSource.sourceAxis] ?? 0, frequencyBalanceSource);
    }
    return 0;
  })();

  $: intBalIndicator = (() => {
    if (($currentInputSource === 'buttplug' || $currentInputSource === 'lovense') && intensityBalanceSource.buttplugLinks) {
      return getButtplugIndicatorValue(intensityBalanceSource.buttplugLinks);
    }
    if (intensityBalanceSource.type === 'linked' && intensityBalanceSource.sourceAxis) {
      return applySourceTransform($allAxisValues[intensityBalanceSource.sourceAxis] ?? 0, intensityBalanceSource);
    }
    return 0;
  })();

  $: intensityIndicator = (() => {
    if (($currentInputSource === 'buttplug' || $currentInputSource === 'lovense') && intensitySource.buttplugLinks) {
      return getButtplugIndicatorValue(intensitySource.buttplugLinks);
    }
    if (intensitySource.type === 'linked' && intensitySource.sourceAxis) {
      return applySourceTransform($allAxisValues[intensitySource.sourceAxis] ?? 0, intensitySource);
    }
    return 0;
  })();

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

  // Handle parameter source changes
  async function handleFrequencySourceChange(event: CustomEvent<ParameterSource>) {
    const newSource = event.detail;
    // Apply frequency snapping when in static mode
    const snappedFreq = newSource.type === 'static'
      ? snapFrequency(newSource.staticValue ?? 100)
      : undefined;

    const finalSource = snappedFreq !== undefined
      ? { ...newSource, staticValue: snappedFreq }
      : newSource;

    // Update backend
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke('update_parameter_source', {
        channel: channel,
        parameter: 'frequency',
        source: finalSource
      });
    } catch (error) {
      console.error(`Failed to update frequency source for channel ${channel}:`, error);
    }

    // Update store
    store.update(s => ({
      ...s,
      frequencySource: finalSource,
      frequency: snappedFreq ?? s.frequency
    }));
  }

  async function handleFrequencyBalanceSourceChange(event: CustomEvent<ParameterSource>) {
    const newSource = event.detail;

    // Update backend
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke('update_parameter_source', {
        channel: channel,
        parameter: 'frequency_balance',
        source: newSource
      });
    } catch (error) {
      console.error(`Failed to update frequency balance source for channel ${channel}:`, error);
    }

    // Update store
    store.update(s => ({
      ...s,
      frequencyBalanceSource: newSource,
      frequencyBalance: newSource.type === 'static' ? (newSource.staticValue ?? 128) : s.frequencyBalance
    }));
  }

  async function handleIntensityBalanceSourceChange(event: CustomEvent<ParameterSource>) {
    const newSource = event.detail;

    // Update backend
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke('update_parameter_source', {
        channel: channel,
        parameter: 'intensity_balance',
        source: newSource
      });
    } catch (error) {
      console.error(`Failed to update intensity balance source for channel ${channel}:`, error);
    }

    // Update store
    store.update(s => ({
      ...s,
      intensityBalanceSource: newSource,
      intensityBalance: newSource.type === 'static' ? (newSource.staticValue ?? 128) : s.intensityBalance
    }));
  }

  async function handleIntensitySourceChange(event: CustomEvent<ParameterSource>) {
    const newSource = event.detail;

    // Update backend
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke('update_parameter_source', {
        channel: channel,
        parameter: 'intensity',
        source: newSource
      });
    } catch (error) {
      console.error(`Failed to update intensity source for channel ${channel}:`, error);
    }

    // Update store
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
