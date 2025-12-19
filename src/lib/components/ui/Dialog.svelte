<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { fade, scale } from 'svelte/transition';
  import { X } from 'lucide-svelte';
  
  export let open = false;
  export let title = '';
  
  const dispatch = createEventDispatcher();
  
  function handleClose() {
    open = false;
    dispatch('close');
  }
  
  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      handleClose();
    }
  }
</script>

{#if open}
  <div
    class="fixed inset-0 z-50 bg-black/80 backdrop-blur-sm"
    transition:fade={{ duration: 150 }}
    on:click={handleClose}
    on:keydown={handleKeydown}
    role="button"
    tabindex="-1"
  />
  <div
    class="fixed left-[50%] top-[50%] z-50 flex flex-col w-full max-w-lg max-h-[90vh] translate-x-[-50%] translate-y-[-50%] border bg-background p-6 shadow-lg sm:rounded-lg"
    transition:scale={{ duration: 150 }}
    on:click|stopPropagation
    on:keydown={handleKeydown}
    role="dialog"
    tabindex="-1"
  >
    {#if title}
      <div class="flex items-center justify-between flex-shrink-0 mb-4">
        <h2 class="text-lg font-semibold">{title}</h2>
        <button
          on:click={handleClose}
          class="rounded-sm opacity-70 ring-offset-background transition-opacity hover:opacity-100 focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2"
        >
          <X class="h-4 w-4" />
          <span class="sr-only">Close</span>
        </button>
      </div>
    {/if}
    <div class="text-sm flex-1 min-h-0 flex flex-col">
      <slot />
    </div>
  </div>
{/if}

<svelte:window on:keydown={handleKeydown} />