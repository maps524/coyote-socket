<script lang="ts">
  // Small dot + label used to surface a live activity state (e.g. Bluetooth
  // scanning). The dot pulses while active so the user can see something is
  
  interface Props {
    // happening even when the result text is steady.
    label?: string;
    state?: 'scanning' | 'active' | 'idle';
  }

  let { label = '', state = 'idle' }: Props = $props();

  const dotClass: Record<typeof state, string> = {
    scanning: 'bg-yellow-400 animate-pulse',
    active: 'bg-green-400',
    idle: 'bg-muted-foreground/40'
  };
</script>

<span class="flex items-center gap-1.5 text-xs text-muted-foreground">
  <span class="w-1.5 h-1.5 rounded-full {dotClass[state]}"></span>
  {label}
</span>
