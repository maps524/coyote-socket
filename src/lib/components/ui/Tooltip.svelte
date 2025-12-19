<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { fade } from 'svelte/transition';

  export let content = '';
  export let side: 'top' | 'bottom' | 'left' | 'right' = 'top';
  export let placement: 'top' | 'bottom' | 'left' | 'right' | undefined = undefined; // Alias for backwards compat
  export let sideOffset = 6;
  export let delayDuration = 400;
  export let skipDelayDuration = 300;

  // Use placement as fallback for side (backwards compatibility)
  $: effectiveSide = placement || side;

  let triggerEl: HTMLElement;
  let tooltipEl: HTMLElement;
  let portalContainer: HTMLElement | null = null;
  let isVisible = false;
  let isOpen = false;
  let openTimeout: ReturnType<typeof setTimeout> | null = null;
  let closeTimeout: ReturnType<typeof setTimeout> | null = null;
  let tooltipStyle = '';

  // Track if user recently closed a tooltip (for skip delay behavior)
  let lastCloseTime = 0;

  onMount(() => {
    // Create portal container
    portalContainer = document.createElement('div');
    portalContainer.className = 'tooltip-portal';
    portalContainer.style.cssText = 'position: fixed; top: 0; left: 0; z-index: 9999; pointer-events: none;';
    document.body.appendChild(portalContainer);
  });

  onDestroy(() => {
    if (openTimeout) clearTimeout(openTimeout);
    if (closeTimeout) clearTimeout(closeTimeout);
    if (portalContainer && document.body.contains(portalContainer)) {
      document.body.removeChild(portalContainer);
    }
  });

  // Portal action
  function portal(node: HTMLElement) {
    if (portalContainer) {
      portalContainer.appendChild(node);
    }
    return {
      destroy() {
        if (node.parentNode) {
          node.parentNode.removeChild(node);
        }
      }
    };
  }

  async function updatePosition() {
    if (!triggerEl || !isOpen) return;

    await tick();

    const triggerRect = triggerEl.getBoundingClientRect();
    const tooltipWidth = tooltipEl?.offsetWidth || 0;
    const tooltipHeight = tooltipEl?.offsetHeight || 0;

    let top = 0;
    let left = 0;

    switch (effectiveSide) {
      case 'top':
        top = triggerRect.top - tooltipHeight - sideOffset;
        left = triggerRect.left + (triggerRect.width - tooltipWidth) / 2;
        break;
      case 'bottom':
        top = triggerRect.bottom + sideOffset;
        left = triggerRect.left + (triggerRect.width - tooltipWidth) / 2;
        break;
      case 'left':
        top = triggerRect.top + (triggerRect.height - tooltipHeight) / 2;
        left = triggerRect.left - tooltipWidth - sideOffset;
        break;
      case 'right':
        top = triggerRect.top + (triggerRect.height - tooltipHeight) / 2;
        left = triggerRect.right + sideOffset;
        break;
    }

    // Keep tooltip within viewport
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;

    if (left < 8) left = 8;
    if (left + tooltipWidth > viewportWidth - 8) left = viewportWidth - tooltipWidth - 8;
    if (top < 8) top = 8;
    if (top + tooltipHeight > viewportHeight - 8) top = viewportHeight - tooltipHeight - 8;

    tooltipStyle = `top: ${top}px; left: ${left}px;`;
  }

  function handleMouseEnter() {
    if (closeTimeout) {
      clearTimeout(closeTimeout);
      closeTimeout = null;
    }

    // Skip delay if user recently interacted with another tooltip
    const timeSinceLastClose = Date.now() - lastCloseTime;
    const delay = timeSinceLastClose < skipDelayDuration ? 0 : delayDuration;

    openTimeout = setTimeout(() => {
      isOpen = true;
      isVisible = true;
      updatePosition();
    }, delay);
  }

  function handleMouseLeave() {
    if (openTimeout) {
      clearTimeout(openTimeout);
      openTimeout = null;
    }

    closeTimeout = setTimeout(() => {
      isOpen = false;
      isVisible = false;
      lastCloseTime = Date.now();
    }, 100);
  }

  function handleFocus() {
    handleMouseEnter();
  }

  function handleBlur() {
    handleMouseLeave();
  }

  $: if (isOpen) {
    updatePosition();
  }
</script>

<!-- svelte-ignore a11y-no-static-element-interactions -->
<div
  bind:this={triggerEl}
  class="inline-flex"
  on:mouseenter={handleMouseEnter}
  on:mouseleave={handleMouseLeave}
  on:focus={handleFocus}
  on:blur={handleBlur}
>
  <slot />
</div>

{#if isVisible && content && portalContainer}
  <div
    use:portal
    bind:this={tooltipEl}
    class="tooltip-content fixed px-3 py-1.5 text-xs font-medium text-popover-foreground bg-popover border border-border rounded-md shadow-lg pointer-events-none max-w-[280px]"
    style={tooltipStyle}
    role="tooltip"
    transition:fade={{ duration: 150 }}
  >
    {@html content}
  </div>
{/if}

<style>
  .tooltip-content {
    animation: tooltipIn 0.15s ease-out;
  }

  .tooltip-content :global(code) {
    font-family: ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, monospace;
    font-size: 0.85em;
    padding: 0.1em 0.3em;
    margin-left: 0.25em;
    background: hsl(var(--muted));
    border-radius: 3px;
  }

  @keyframes tooltipIn {
    from {
      opacity: 0;
      transform: scale(0.96);
    }
    to {
      opacity: 1;
      transform: scale(1);
    }
  }
</style>