<script lang="ts">
  import { cn } from '$lib/utils/cn.js';
  import type { HTMLInputAttributes } from 'svelte/elements';

  interface $$Props extends HTMLInputAttributes {
    class?: string;
    value?: string;
  }

  let className: $$Props['class'] = undefined;
  export { className as class };
  export let value: string = '';
</script>

<input
  class={cn(
    'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background file:border-0 file:bg-transparent file:text-sm file:font-medium placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50',
    className
  )}
  bind:value
  {...$$restProps}
  on:input
  on:change
  on:keydown
  on:keyup
  on:focus
  on:blur
/>