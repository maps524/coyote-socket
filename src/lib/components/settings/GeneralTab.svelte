<script lang="ts">
  import { generalSettings } from '$lib/stores/generalSettings';
  import Select from '$lib/components/ui/Select.svelte';
  import Toggle from '$lib/components/ui/Toggle.svelte';
  import Tooltip from '$lib/components/ui/Tooltip.svelte';
  import Slider from '$lib/components/ui/Slider.svelte';
  import { Info } from 'lucide-svelte';

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

  <div class="pt-3 border-t border-border">
    <p class="text-xs text-muted-foreground">
      Settings are automatically saved to disk. The Update Rate controls real-time responsiveness,
      while the Save Rate controls how often changes are persisted to prevent excessive disk I/O.
    </p>
  </div>
</div>
