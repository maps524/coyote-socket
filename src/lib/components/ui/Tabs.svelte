<script lang="ts">
  import { createEventDispatcher } from 'svelte';

  interface Props {
    value?: string;
    tabs?: Array<{ value: string; label: string }>;
    children?: import('svelte').Snippet;
  }

  let { value = $bindable(''), tabs = [], children }: Props = $props();

  const dispatch = createEventDispatcher();

  function selectTab(tabValue: string) {
    value = tabValue;
    dispatch('change', tabValue);
  }
</script>

<div class="w-full">
  <div class="inline-flex h-10 items-center justify-center rounded-md bg-card border border-border p-1 text-muted-foreground" role="tablist">
    {#each tabs as tab}
      <button
        role="tab"
        aria-selected={value === tab.value}
        aria-controls="panel-{tab.value}"
        data-state={value === tab.value ? 'active' : 'inactive'}
        class="inline-flex items-center justify-center whitespace-nowrap rounded-sm px-3 py-1.5 text-sm font-medium ring-offset-background transition-all focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50 {value === tab.value ? 'bg-background text-primary shadow-xs border border-border' : 'border border-transparent'}"
        onclick={() => selectTab(tab.value)}
      >
        {tab.label}
      </button>
    {/each}
  </div>

  <div class="mt-2" role="tabpanel" id="panel-{value}">
    {@render children?.()}
  </div>
</div>