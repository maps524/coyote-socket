<script lang="ts">
  import { buttplugSettings, resetButtplugSettings } from '$lib/stores/buttplugSettings';
  import { getTotalFeatureCount } from '$lib/types/buttplug';
  import Button from '$lib/components/ui/Button.svelte';
  import { MapPin, Timer, Zap, RotateCw, MoveHorizontal, Minimize2, Plus, X } from 'lucide-svelte';

  // Feature type configuration with title-cased display names
  const featureTypes = [
    { name: 'Position', key: 'position' as const, icon: MapPin, default: 2 },
    { name: 'Position With Duration', key: 'positionWithDuration' as const, icon: Timer, default: 2 },
    { name: 'Vibrate', key: 'vibrate' as const, icon: Zap, default: 2 },
    { name: 'Rotate', key: 'rotate' as const, icon: RotateCw, default: 2 },
    { name: 'Oscillate', key: 'oscillate' as const, icon: MoveHorizontal, default: 2 },
    { name: 'Constrict', key: 'constrict' as const, icon: Minimize2, default: 2 }
  ] as const;

  type FeatureKey = typeof featureTypes[number]['key'];

  // Calculate total features directly from store
  $: totalFeatures = getTotalFeatureCount($buttplugSettings);

  function addFeature(key: FeatureKey) {
    buttplugSettings.update(s => ({
      ...s,
      [key]: s[key] + 1
    }));
  }

  function removeFeature(key: FeatureKey) {
    buttplugSettings.update(s => ({
      ...s,
      [key]: Math.max(0, s[key] - 1)
    }));
  }
</script>

<div class="space-y-4">
  <div class="space-y-3">
    <h3 class="text-sm font-medium text-foreground">Buttplug Features</h3>
    <p class="text-xs text-muted-foreground">
      Configure which features to advertise to Buttplug clients. Each feature can be linked to channel parameters.
    </p>

    <!-- Feature Count Configuration -->
    <div class="space-y-2">
      {#each featureTypes as type}
        <div class="flex items-center justify-between py-2 border-b border-border last:border-0">
          <div class="flex items-center gap-2">
            <svelte:component this={type.icon} class="h-4 w-4 text-primary" />
            <span class="text-sm text-foreground min-w-[120px]">{type.name}</span>
          </div>

          <div class="flex items-center gap-1">
            <!-- Feature count buttons -->
            {#each Array($buttplugSettings[type.key]) as _, index}
              <button
                on:click={() => removeFeature(type.key)}
                class="flex items-center justify-center h-6 w-6 rounded border border-border bg-muted hover:bg-muted/70 transition-colors text-xs font-medium text-foreground relative group"
                title="Remove {type.name} {index + 1}"
              >
                {index + 1}
                {#if index >= type.default}
                  <div class="absolute inset-0 flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity bg-destructive/90 rounded">
                    <X class="h-3 w-3 text-destructive-foreground" />
                  </div>
                {/if}
              </button>
            {/each}

            <!-- Add button -->
            <button
              on:click={() => addFeature(type.key)}
              class="flex items-center justify-center h-6 w-6 rounded border border-border bg-primary/10 hover:bg-primary/20 transition-colors"
              title="Add {type.name}"
            >
              <Plus class="h-3 w-3 text-primary" />
            </button>
          </div>
        </div>
      {/each}
    </div>

    <!-- Total Features Counter -->
    <div class="flex items-center justify-between pt-3 border-t border-border">
      <span class="text-sm font-medium text-muted-foreground">Total Features</span>
      <span class="text-sm font-bold text-primary">{totalFeatures}</span>
    </div>

    <!-- Reset Button -->
    <div class="flex justify-end pt-2">
      <Button variant="outline" size="sm" on:click={resetButtplugSettings}>
        Reset to Defaults
      </Button>
    </div>
  </div>

  <div class="pt-3 border-t border-border">
    <p class="text-xs text-muted-foreground">
      Features can be linked to channel parameters in the linking panel. Default configuration provides 2 of each type (12 total features). Click [+] to add more features of a type, or hover over feature numbers beyond the default to remove them.
    </p>
  </div>
</div>
