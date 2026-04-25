<script lang="ts">
  import type { GamepadBinding } from '$lib/types/settings';

  // Standard browser-Gamepad-API button index → Xbox SVG asset
  // Backend gilrs maps to the same indices in src-tauri/src/gamepad.rs.
  import buttonA from '$lib/assets/gamepad/xbox/button_a.svg';
  import buttonB from '$lib/assets/gamepad/xbox/button_b.svg';
  import buttonX from '$lib/assets/gamepad/xbox/button_x.svg';
  import buttonY from '$lib/assets/gamepad/xbox/button_y.svg';
  import lb from '$lib/assets/gamepad/xbox/lb.svg';
  import rb from '$lib/assets/gamepad/xbox/rb.svg';
  import lt from '$lib/assets/gamepad/xbox/lt.svg';
  import rt from '$lib/assets/gamepad/xbox/rt.svg';
  import view from '$lib/assets/gamepad/xbox/view.svg';
  import menu from '$lib/assets/gamepad/xbox/menu.svg';
  import stickLPress from '$lib/assets/gamepad/xbox/stick_l_press.svg';
  import stickRPress from '$lib/assets/gamepad/xbox/stick_r_press.svg';
  import dpadUp from '$lib/assets/gamepad/xbox/dpad_up.svg';
  import dpadDown from '$lib/assets/gamepad/xbox/dpad_down.svg';
  import dpadLeft from '$lib/assets/gamepad/xbox/dpad_left.svg';
  import dpadRight from '$lib/assets/gamepad/xbox/dpad_right.svg';
  import stickLUp from '$lib/assets/gamepad/xbox/stick_l_up.svg';
  import stickLDown from '$lib/assets/gamepad/xbox/stick_l_down.svg';
  import stickLLeft from '$lib/assets/gamepad/xbox/stick_l_left.svg';
  import stickLRight from '$lib/assets/gamepad/xbox/stick_l_right.svg';
  import stickRUp from '$lib/assets/gamepad/xbox/stick_r_up.svg';
  import stickRDown from '$lib/assets/gamepad/xbox/stick_r_down.svg';
  import stickRLeft from '$lib/assets/gamepad/xbox/stick_r_left.svg';
  import stickRRight from '$lib/assets/gamepad/xbox/stick_r_right.svg';

  import type { ChordPart } from '$lib/types/settings';

  export let binding: GamepadBinding | undefined = undefined;
  export let size: number = 20;

  const BUTTON_LABELS: Record<number, string> = {
    0: 'A', 1: 'B', 2: 'X', 3: 'Y',
    4: 'LB', 5: 'RB', 6: 'LT', 7: 'RT',
    8: 'View', 9: 'Menu', 10: 'L3', 11: 'R3',
    12: 'D↑', 13: 'D↓', 14: 'D←', 15: 'D→',
    16: 'Guide'
  };

  function buttonSrc(index: number): string {
    switch (index) {
      case 0: return buttonA;
      case 1: return buttonB;
      case 2: return buttonX;
      case 3: return buttonY;
      case 4: return lb;
      case 5: return rb;
      case 6: return lt;
      case 7: return rt;
      case 8: return view;
      case 9: return menu;
      case 10: return stickLPress;
      case 11: return stickRPress;
      case 12: return dpadUp;
      case 13: return dpadDown;
      case 14: return dpadLeft;
      case 15: return dpadRight;
      default: return '';
    }
  }

  function axisSrc(index: number, dir: 'pos' | 'neg'): string {
    // 0 = LX, 1 = LY, 2 = RX, 3 = RY, 4 = LT analog, 5 = RT analog,
    // 6 = DPad X (digital-as-axis), 7 = DPad Y
    switch (index) {
      case 0: return dir === 'pos' ? stickLRight : stickLLeft;
      case 1: return dir === 'pos' ? stickLDown  : stickLUp;
      case 2: return dir === 'pos' ? stickRRight : stickRLeft;
      case 3: return dir === 'pos' ? stickRDown  : stickRUp;
      case 4: return lt;
      case 5: return rt;
      case 6: return dir === 'pos' ? dpadRight : dpadLeft;
      case 7: return dir === 'pos' ? dpadDown  : dpadUp;
      default: return '';
    }
  }

  function partSrc(p: ChordPart): string {
    return p.kind === 'button' ? buttonSrc(p.index) : axisSrc(p.index, p.dir);
  }

  function partLabel(p: ChordPart): string {
    return p.kind === 'button'
      ? (BUTTON_LABELS[p.index] ?? `Btn${p.index}`)
      : `Ax${p.index}${p.dir === 'pos' ? '+' : '-'}`;
  }
</script>

{#if binding}
  {#if binding.kind === 'combo'}
    <span class="inline-flex items-center gap-0.5 align-middle">
      {#each binding.parts as part, i}
        {#if i > 0}
          <span class="text-xs text-muted-foreground">+</span>
        {/if}
        {@const src = partSrc(part)}
        {@const label = partLabel(part)}
        {#if src}
          <img {src} alt={label} width={size} height={size} class="inline-block" title={label} />
        {:else}
          <span class="text-xs font-mono text-muted-foreground">{label}</span>
        {/if}
      {/each}
    </span>
  {:else}
    {@const src = binding.kind === 'button' ? buttonSrc(binding.index) : axisSrc(binding.index, binding.dir)}
    {@const label = binding.kind === 'button'
      ? (BUTTON_LABELS[binding.index] ?? `Btn${binding.index}`)
      : `Ax${binding.index}${binding.dir === 'pos' ? '+' : '-'}`}
    <span class="inline-flex items-center gap-1 align-middle" title={label}>
      {#if src}
        <img {src} alt={label} width={size} height={size} class="inline-block" />
      {:else}
        <span class="text-xs font-mono text-muted-foreground">{label}</span>
      {/if}
    </span>
  {/if}
{/if}
