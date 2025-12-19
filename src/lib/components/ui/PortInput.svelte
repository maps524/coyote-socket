<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { cn } from '$lib/utils/cn.js';

  export let port: number = 12346;
  export let disabled: boolean = false;
  let className: string = '';
  export { className as class };

  const dispatch = createEventDispatcher<{ change: number }>();

  // Handle input validation - only allow numbers
  function handleInput(event: Event) {
    const input = event.target as HTMLInputElement;
    const value = input.value.replace(/[^0-9]/g, '');
    const numValue = parseInt(value, 10);

    if (!isNaN(numValue) && numValue >= 1 && numValue <= 65535) {
      port = numValue;
      dispatch('change', port);
    } else if (value === '') {
      port = 12346;
      dispatch('change', port);
    }
    input.value = port.toString();
  }
</script>

<div
  class={cn(
    'flex items-center h-10 w-full rounded-md border border-input bg-background text-sm font-mono ring-offset-background',
    'focus-within:ring-2 focus-within:ring-ring focus-within:ring-offset-2',
    disabled && 'cursor-not-allowed opacity-50',
    className
  )}
>
  <!-- Protocol and IP prefix (non-editable, inside the input) -->
  <span class="pl-3 text-muted-foreground select-none pointer-events-none">
    ws://127.0.0.1:
  </span>

  <!-- Port input (editable) -->
  <input
    type="text"
    inputmode="numeric"
    pattern="[0-9]*"
    value={port}
    on:input={handleInput}
    {disabled}
    class="flex-1 h-full bg-transparent py-2 pr-3 text-sm font-mono outline-none disabled:cursor-not-allowed"
  />
</div>
