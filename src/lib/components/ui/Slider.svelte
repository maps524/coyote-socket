<script lang="ts">
  import { cn } from '$lib/utils/cn.js';
  import { createEventDispatcher } from 'svelte';

  interface $$Props {
    id?: string;
    value?: number;
    min?: number;
    max?: number;
    step?: number;
    disabled?: boolean;
    class?: string;
    variant?: 'primary' | 'secondary';
    wheelStep?: (currentValue: number, direction: 'up' | 'down') => number;
  }

  export let id: $$Props['id'] = undefined;
  export let value: number = 0;
  export let min: number = 0;
  export let max: number = 100;
  export let step: number = 1;
  export let disabled: boolean = false;
  export let variant: 'primary' | 'secondary' = 'primary';
  export let wheelStep: $$Props['wheelStep'] = undefined;
  let className: $$Props['class'] = undefined;
  export { className as class };

  $: percentage = ((value - min) / (max - min)) * 100;

  const dispatch = createEventDispatcher<{ change: number }>();

  function handleInput(event: Event) {
    const target = event.target as HTMLInputElement;
    value = Number(target.value);
    dispatch('change', value);
  }

  function handleWheel(event: WheelEvent) {
    if (disabled) return;
    event.preventDefault();

    const direction = event.deltaY < 0 ? 'up' : 'down';
    let newValue: number;

    if (wheelStep) {
      // Use custom step function
      newValue = wheelStep(value, direction);
    } else {
      // Default behavior
      const delta = direction === 'up' ? step : -step;
      newValue = value + delta;
    }

    newValue = Math.max(min, Math.min(max, newValue));

    if (newValue !== value) {
      value = newValue;
      dispatch('change', value);
    }
  }
</script>

<div 
  class={cn('relative flex w-full touch-none select-none items-center', className)} 
  on:wheel={handleWheel}
  style="--slider-color: hsl(var(--{variant})); --slider-shadow-1: hsl(var(--{variant}) / 0.2); --slider-shadow-2: hsl(var(--{variant}) / 0.6); --slider-shadow-3: hsl(var(--{variant}) / 0.8)"
>
  <input
    type="range"
    {id}
    {min}
    {max}
    {step}
    {value}
    {disabled}
    on:input={handleInput}
    class="slider-enhanced relative h-3 w-full cursor-pointer appearance-none rounded-full outline-none disabled:cursor-not-allowed disabled:opacity-50"
    style="background: linear-gradient(to right, hsl(var(--{variant})) 0%, hsl(var(--{variant})) {percentage}%, hsl(var(--muted)) {percentage}%, hsl(var(--muted)) 100%)"
  />
</div>

<style>
  .slider-enhanced::-webkit-slider-thumb {
    appearance: none;
    width: 24px;
    height: 24px;
    background: var(--slider-color, hsl(var(--primary)));
    border: 3px solid hsl(var(--background));
    border-radius: 50%;
    cursor: pointer;
    box-shadow: 0 0 0 1px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 15px var(--slider-shadow-2, hsl(var(--primary) / 0.6));
    transition: all 0.2s ease;
  }
  
  .slider-enhanced::-webkit-slider-thumb:hover {
    box-shadow: 0 0 0 6px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 25px var(--slider-shadow-3, hsl(var(--primary) / 0.8));
    transform: scale(1.1);
  }
  
  .slider-enhanced::-webkit-slider-thumb:active {
    transform: scale(0.95);
  }
  
  .slider-enhanced::-moz-range-thumb {
    width: 24px;
    height: 24px;
    background: var(--slider-color, hsl(var(--primary)));
    border: 3px solid hsl(var(--background));
    border-radius: 50%;
    cursor: pointer;
    box-shadow: 0 0 0 1px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 15px var(--slider-shadow-2, hsl(var(--primary) / 0.6));
    transition: all 0.2s ease;
  }
  
  .slider-enhanced::-moz-range-thumb:hover {
    box-shadow: 0 0 0 6px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 25px var(--slider-shadow-3, hsl(var(--primary) / 0.8));
    transform: scale(1.1);
  }
  
  .slider-enhanced::-moz-range-thumb:active {
    transform: scale(0.95);
  }
  
  .slider-enhanced:focus {
    outline: none;
  }
  
  .slider-enhanced:focus-visible::-webkit-slider-thumb {
    box-shadow: 0 0 0 3px hsl(var(--background)), 0 0 0 5px hsl(var(--ring));
  }
  
  .slider-enhanced:focus-visible::-moz-range-thumb {
    box-shadow: 0 0 0 3px hsl(var(--background)), 0 0 0 5px hsl(var(--ring));
  }
</style>