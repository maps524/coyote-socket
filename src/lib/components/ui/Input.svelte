<script lang="ts">
  import { cn } from '$lib/utils/cn.js';
  import type { HTMLInputAttributes } from 'svelte/elements';

  

  
  interface Props {
    class?: string;
    value?: string;
    [key: string]: any
  }

  let { class: className = undefined, value = $bindable(''), ...rest }: Props = $props();
</script>

<input
  class={cn(
    'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background file:border-0 file:bg-transparent file:text-sm file:font-medium placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50',
    className
  )}
  bind:value
  {...rest}
/>