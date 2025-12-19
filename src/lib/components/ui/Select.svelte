<script lang="ts">
  import { cn } from '$lib/utils/cn.js';
  import type { HTMLSelectAttributes } from 'svelte/elements';

  interface $$Props extends HTMLSelectAttributes {
    class?: string;
    value?: string;
  }

  let className: $$Props['class'] = undefined;
  export { className as class };
  export let value: string = '';
</script>

<select
  class={cn(
    'flex h-10 w-full rounded-md border border-input bg-background text-foreground pl-3 pr-10 py-2 text-sm ring-offset-background appearance-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50',
    className
  )}
  bind:value
  {...$$restProps}
  on:change
>
  <slot />
</select>

<style>
  select {
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12'%3E%3Cpath fill='%23888' d='M10.293 3.293L6 7.586 1.707 3.293A1 1 0 00.293 4.707l5 5a1 1 0 001.414 0l5-5a1 1 0 10-1.414-1.414z'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 0.7rem center;
    background-size: 12px;
  }
  
  :global(.dark) select {
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12'%3E%3Cpath fill='%23aaa' d='M10.293 3.293L6 7.586 1.707 3.293A1 1 0 00.293 4.707l5 5a1 1 0 001.414 0l5-5a1 1 0 10-1.414-1.414z'/%3E%3C/svg%3E");
  }
  
  select option {
    background-color: hsl(var(--background));
    color: hsl(var(--foreground));
    padding: 0.5rem;
  }
  
  select option:hover,
  select option:focus,
  select option:checked {
    background-color: hsl(var(--muted));
  }
</style>