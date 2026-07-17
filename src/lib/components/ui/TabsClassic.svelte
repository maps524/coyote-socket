<script lang="ts">
  
  interface Props {
    value?: string;
    tabs?: Array<{ value: string; label: string }>;
    children?: import('svelte').Snippet;
  }

  let { value = $bindable(''), tabs = [], children }: Props = $props();
  
  
  function selectTab(tabValue: string) {
    value = tabValue;
  }
</script>

<div class="w-full flex-1 min-h-0 flex flex-col">
  <div class="flex border-b border-border shrink-0">
    {#each tabs as tab}
      <button
        class="px-4 py-2 text-sm font-medium transition-colors hover:text-primary relative {value === tab.value ? 'text-primary' : 'text-muted-foreground'}"
        onclick={() => selectTab(tab.value)}
      >
        {tab.label}
        {#if value === tab.value}
          <div class="absolute bottom-0 left-0 right-0 h-0.5 bg-primary"></div>
        {/if}
      </button>
    {/each}
  </div>

  <div class="py-4 flex-1 min-h-0 overflow-y-auto scrollbar-thin">
    {@render children?.()}
  </div>
</div>

<style>
  .scrollbar-thin {
    scrollbar-width: thin;
    scrollbar-color: hsl(var(--muted-foreground) / 0.3) transparent;
  }

  .scrollbar-thin::-webkit-scrollbar {
    width: 6px;
  }

  .scrollbar-thin::-webkit-scrollbar-track {
    background: transparent;
  }

  .scrollbar-thin::-webkit-scrollbar-thumb {
    background-color: hsl(var(--muted-foreground) / 0.3);
    border-radius: 3px;
  }

  .scrollbar-thin::-webkit-scrollbar-thumb:hover {
    background-color: hsl(var(--muted-foreground) / 0.5);
  }
</style>