<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  
  interface Props {
    checked?: boolean;
    disabled?: boolean;
  }

  let { checked = $bindable(false), disabled = false }: Props = $props();
  
  const dispatch = createEventDispatcher();
  
  function toggle() {
    if (!disabled) {
      checked = !checked;
      dispatch('change', checked);
    }
  }
</script>

<button
  type="button"
  role="switch"
  aria-checked={checked}
  onclick={toggle}
  {disabled}
  class="relative inline-flex h-5 w-9 items-center rounded-full transition-colors duration-200 ease-in-out focus:outline-hidden focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:cursor-not-allowed disabled:opacity-50 {checked ? 'bg-primary' : 'bg-muted'}"
>
  <span
    class="inline-block h-4 w-4 transform rounded-full bg-background shadow-lg ring-0 transition duration-200 ease-in-out {checked ? 'translate-x-4' : 'translate-x-0.5'}"
></span>
</button>