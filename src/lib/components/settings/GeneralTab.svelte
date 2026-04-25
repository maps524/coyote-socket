<script lang="ts">
  import { generalSettings } from '$lib/stores/generalSettings';
  import { channelA, channelB } from '$lib/stores/channels';
  import Select from '$lib/components/ui/Select.svelte';
  import Toggle from '$lib/components/ui/Toggle.svelte';
  import Tooltip from '$lib/components/ui/Tooltip.svelte';
  import Slider from '$lib/components/ui/Slider.svelte';
  import { Info } from 'lucide-svelte';
  import { invoke } from '@tauri-apps/api/core';

  // Local state bound to the store
  let noInputBehavior = $generalSettings.noInputBehavior;
  let noInputDecayMs = $generalSettings.noInputDecayMs;
  let updateRateMs = $generalSettings.updateRateMs;
  let saveRateMs = $generalSettings.saveRateMs;
  let showTCodeMonitor = $generalSettings.showTCodeMonitor;

  // Update store when local state changes (processingEngine is controlled from main UI)
  $: {
    generalSettings.update(s => ({
      ...s,
      noInputBehavior,
      noInputDecayMs,
      updateRateMs,
      saveRateMs,
      showTCodeMonitor
    }));
  }

  // Per-channel device intensity cap ("soft mode"). Push to backend + clamp the
  // channel's modulation range to fit inside the new cap, so stored rangeMax
  // values can't render past the slider's visible end.
  async function applyMaxIntensity(channel: 'A' | 'B', value: number) {
    const clamped = Math.max(0, Math.min(200, Math.round(value)));
    generalSettings.update(s => ({
      ...s,
      channelAMaxIntensity: channel === 'A' ? clamped : s.channelAMaxIntensity,
      channelBMaxIntensity: channel === 'B' ? clamped : s.channelBMaxIntensity
    }));

    try {
      await invoke('set_channel_max_intensity', { channel, value: clamped });
    } catch (e) {
      console.error(`[GeneralTab] set_channel_max_intensity failed:`, e);
    }

    const store = channel === 'A' ? channelA : channelB;
    const params = channel === 'A' ? $channelA : $channelB;
    const src = params.intensitySource;
    if (src) {
      const newMin = Math.min(src.rangeMin, clamped);
      const newMax = Math.min(src.rangeMax, clamped);
      if (newMin !== src.rangeMin || newMax !== src.rangeMax) {
        const updated = { ...src, rangeMin: newMin, rangeMax: newMax };
        store.update(s => ({ ...s, intensitySource: updated, rangeMin: newMin, rangeMax: newMax }));
        try {
          await invoke('update_parameter_source', {
            channel,
            parameter: 'intensity',
            source: updated
          });
        } catch (e) {
          console.error(`[GeneralTab] clamp intensity range failed:`, e);
        }
      }
    }
  }

  // "Soft mode" preset (legacy app: fixed cap toggle). 100 = 50% of device max.
  function applySoftMode() {
    applyMaxIntensity('A', 100);
    applyMaxIntensity('B', 100);
  }
  function clearCaps() {
    applyMaxIntensity('A', 200);
    applyMaxIntensity('B', 200);
  }
</script>

<div class="space-y-4">
  <div class="space-y-3">
    <h3 class="text-sm font-medium text-foreground">General Settings</h3>

    <!-- No Input Behavior -->
    <div class="space-y-2">
      <div class="flex items-center gap-2">
        <label for="no-input-behavior" class="text-xs text-muted-foreground">
          No Input Behavior
        </label>
        <Tooltip content="What happens when a linked parameter axis has no incoming T-Code data: Hold (keep last value), Default (use static value), Decay (gradually reduce to min), Zero (immediately go to min)">
          <Info class="h-3 w-3 text-muted-foreground cursor-help" />
        </Tooltip>
      </div>
      <Select
        id="no-input-behavior"
        class="h-9 text-sm"
        bind:value={noInputBehavior}
      >
        <option value="hold">Hold Last Value</option>
        <option value="default">Use Default Value</option>
        <option value="decay">Decay to Minimum</option>
        <option value="zero">Zero Immediately</option>
      </Select>
    </div>

    <!-- Decay Time (only shown when decay is selected) -->
    {#if noInputBehavior === 'decay'}
      <div class="space-y-2 pl-4 border-l-2 border-muted">
        <div class="flex items-center justify-between">
          <div class="flex items-center gap-2">
            <label for="decay-time" class="text-xs text-muted-foreground">
              Decay Time
            </label>
            <Tooltip content="How long it takes to decay from current value to minimum (100-2000ms)">
              <Info class="h-3 w-3 text-muted-foreground cursor-help" />
            </Tooltip>
          </div>
          <span class="text-xs font-mono text-foreground">{noInputDecayMs}ms</span>
        </div>
        <Slider
          id="decay-time"
          bind:value={noInputDecayMs}
          min={100}
          max={2000}
          step={100}
          variant="primary"
        />
      </div>
    {/if}

    <!-- Update Rate -->
    <div class="space-y-2">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-2">
          <label for="update-rate" class="text-xs text-muted-foreground">
            Update Rate
          </label>
          <Tooltip content="How quickly parameter changes reach the Rust backend for real-time processing. Lower = more responsive but more CPU usage (10-100ms)">
            <Info class="h-3 w-3 text-muted-foreground cursor-help" />
          </Tooltip>
        </div>
        <span class="text-xs font-mono text-foreground">{updateRateMs}ms</span>
      </div>
      <Slider
        id="update-rate"
        bind:value={updateRateMs}
        min={10}
        max={100}
        step={10}
        variant="primary"
      />
    </div>

    <!-- Save Rate -->
    <div class="space-y-2">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-2">
          <label for="save-rate" class="text-xs text-muted-foreground">
            Save Rate
          </label>
          <Tooltip content="How often settings are written to disk. Lower = more frequent saves but more disk I/O (100-2000ms)">
            <Info class="h-3 w-3 text-muted-foreground cursor-help" />
          </Tooltip>
        </div>
        <span class="text-xs font-mono text-foreground">{saveRateMs}ms</span>
      </div>
      <Slider
        id="save-rate"
        bind:value={saveRateMs}
        min={100}
        max={2000}
        step={100}
        variant="primary"
      />
    </div>

    <!-- Show Input Monitor -->
    <div class="flex items-center justify-between py-2">
      <div class="flex items-center gap-2">
        <label for="show-tcode-monitor" class="text-xs text-muted-foreground">
          Show Input Monitor
        </label>
        <Tooltip content="Display the Input Monitor panel showing incoming axis values in real-time">
          <Info class="h-3 w-3 text-muted-foreground cursor-help" />
        </Tooltip>
      </div>
      <Toggle bind:checked={showTCodeMonitor} />
    </div>
  </div>

  <!-- Safety Limits -->
  <div class="space-y-3 pt-3 border-t border-border">
    <div class="flex items-center justify-between">
      <div class="flex items-center gap-2">
        <h3 class="text-sm font-medium text-foreground">Safety Limits</h3>
        <Tooltip content="Per-channel cap on device intensity. Channel sliders downsample to fit inside the cap, so 100% on the channel slider matches the cap you set here. The device itself enforces the limit too.">
          <Info class="h-3 w-3 text-muted-foreground cursor-help" />
        </Tooltip>
      </div>
      <div class="flex items-center gap-1">
        <button
          type="button"
          class="text-xs px-2 py-1 rounded border border-border bg-muted hover:bg-muted/70 text-foreground"
          on:click={applySoftMode}
        >
          Soft Mode
        </button>
        <button
          type="button"
          class="text-xs px-2 py-1 rounded border border-border bg-background hover:bg-muted/40 text-muted-foreground"
          on:click={clearCaps}
        >
          Clear
        </button>
      </div>
    </div>

    <!-- Channel A max intensity -->
    <div class="space-y-2">
      <div class="flex items-center justify-between">
        <label for="ch-a-max" class="text-xs text-muted-foreground">
          Channel A Max Intensity
        </label>
        <span class="text-xs font-mono text-foreground">
          {Math.round($generalSettings.channelAMaxIntensity / 2)}%
        </span>
      </div>
      <Slider
        id="ch-a-max"
        value={$generalSettings.channelAMaxIntensity}
        min={0}
        max={200}
        step={2}
        variant="primary"
        on:change={(e) => applyMaxIntensity('A', e.detail)}
      />
    </div>

    <!-- Channel B max intensity -->
    <div class="space-y-2">
      <div class="flex items-center justify-between">
        <label for="ch-b-max" class="text-xs text-muted-foreground">
          Channel B Max Intensity
        </label>
        <span class="text-xs font-mono text-foreground">
          {Math.round($generalSettings.channelBMaxIntensity / 2)}%
        </span>
      </div>
      <Slider
        id="ch-b-max"
        value={$generalSettings.channelBMaxIntensity}
        min={0}
        max={200}
        step={2}
        variant="secondary"
        on:change={(e) => applyMaxIntensity('B', e.detail)}
      />
    </div>

    <p class="text-xs text-muted-foreground">
      Soft Mode caps both channels at 50%. Channel intensity sliders rescale so 100% always matches the cap.
    </p>
  </div>

  <div class="pt-3 border-t border-border">
    <p class="text-xs text-muted-foreground">
      Settings are automatically saved to disk. The Update Rate controls real-time responsiveness,
      while the Save Rate controls how often changes are persisted to prevent excessive disk I/O.
    </p>
  </div>
</div>
