<script lang="ts">
  // Presentational control for the three states of a gamepad binding:
  // unbound, actively-capturing, and bound. Emits intent events; the parent
  // owns the global capture state (rebindCapture) and the raw-event wiring.
  //
  // Two layouts via `compact`:
  //   compact=false (default) — settings editor: labelled Bind/Rebind buttons.
  //   compact=true            — inline lists (e.g. preset reorder rows): the
  //                             combo is the rebind target, icon-only when unbound.
  import { createEventDispatcher } from 'svelte';
  import { X, Gamepad2 } from 'lucide-svelte';
  import Button from './Button.svelte';
  import GamepadIcon from './GamepadIcon.svelte';
  import type { ChordPart, GamepadBinding } from '../../types/settings';

  
  interface Props {
    binding?: GamepadBinding | undefined;
    capturing?: boolean;
    captureParts?: ChordPart[];
    compact?: boolean;
    // Label used in the icon-button titles (compact mode), e.g. the preset name.
    label?: string;
  }

  let {
    binding = undefined,
    capturing = false,
    captureParts = [],
    compact = false,
    label = ''
  }: Props = $props();

  const dispatch = createEventDispatcher<{
    start: void; save: void; cancel: void; clear: void;
  }>();

  function captureToBinding(parts: ChordPart[]): GamepadBinding {
    if (parts.length === 1) return parts[0] as GamepadBinding;
    return { kind: 'combo', parts };
  }

  let bindTitle = $derived(label ? `Bind a gamepad combo to ${label}` : 'Bind a gamepad combo');
</script>

{#if compact}
  <div class="flex items-center gap-1">
    {#if capturing}
      {#if captureParts.length === 0}
        <span class="text-[11px] text-amber-500 whitespace-nowrap px-1">Press buttons…</span>
      {:else}
        <GamepadIcon binding={captureToBinding(captureParts)} />
      {/if}
      <Button variant="default" size="sm" class="h-6 px-2 text-[11px]" on:click={() => dispatch('save')}>Save</Button>
      <Button variant="ghost" size="sm" class="h-6 px-2 text-[11px]" on:click={() => dispatch('cancel')}>Cancel</Button>
    {:else if binding}
      <button
        type="button"
        class="flex items-center rounded px-1 py-0.5 hover:bg-background/50 cursor-pointer"
        title="Rebind gamepad combo"
        onclick={() => dispatch('start')}
      >
        <GamepadIcon {binding} />
      </button>
      <button
        type="button"
        class="w-6 h-6 flex items-center justify-center rounded text-muted-foreground hover:text-destructive hover:bg-destructive/10 cursor-pointer"
        title="Clear gamepad combo"
        onclick={() => dispatch('clear')}
      >
        <X class="h-3.5 w-3.5" />
      </button>
    {:else}
      <button
        type="button"
        class="w-7 h-7 flex items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-background/50 cursor-pointer"
        title={bindTitle}
        onclick={() => dispatch('start')}
      >
        <Gamepad2 class="h-4 w-4" />
      </button>
    {/if}
  </div>
{:else}
  <span class="flex-1 flex justify-end items-center min-w-0 overflow-hidden">
    {#if capturing}
      {#if captureParts.length === 0}
        <span class="text-xs text-amber-500">Press buttons…</span>
      {:else}
        <GamepadIcon binding={captureToBinding(captureParts)} />
      {/if}
    {:else}
      <GamepadIcon {binding} />
    {/if}
  </span>
  {#if capturing}
    <Button variant="default" size="sm" on:click={() => dispatch('save')}>Save</Button>
    <Button variant="ghost" size="sm" on:click={() => dispatch('cancel')}>Cancel</Button>
  {:else}
    <Button variant="outline" size="sm" on:click={() => dispatch('start')}>
      {binding ? 'Rebind' : 'Bind'}
    </Button>
    {#if binding}
      <Button variant="ghost" size="sm" on:click={() => dispatch('clear')}>✕</Button>
    {/if}
  {/if}
{/if}
