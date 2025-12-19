<script lang="ts">
  import { createEventDispatcher, tick, onMount, onDestroy } from 'svelte';
  import { scale } from 'svelte/transition';

  export let open = false;
  export let align: 'start' | 'center' | 'end' = 'start';
  export let sideOffset = 8;
  export let contentClass = ''; // Additional classes for content area
  export let compact = false; // Use smaller padding

  const dispatch = createEventDispatcher();

  // Unique ID for this popover instance
  const popoverId = `popover-${Math.random().toString(36).substr(2, 9)}`;

  let triggerEl: HTMLElement | null = null;
  let contentEl: HTMLElement | null = null;
  let portalContainer: HTMLElement | null = null;
  let popoverStyle = '';
  let mounted = false;

  // Create a portal container at the body level to escape stacking contexts
  onMount(() => {
    portalContainer = document.createElement('div');
    portalContainer.className = 'popover-portal';
    portalContainer.style.cssText = 'position: fixed; top: 0; left: 0; z-index: 9999; pointer-events: none;';
    document.body.appendChild(portalContainer);
    mounted = true;

    // Listen for other popovers opening
    window.addEventListener('popover-open', handleOtherPopoverOpen as EventListener);
  });

  onDestroy(() => {
    if (portalContainer && document.body.contains(portalContainer)) {
      document.body.removeChild(portalContainer);
    }
    window.removeEventListener('popover-open', handleOtherPopoverOpen as EventListener);
  });

  // Close this popover when another one opens
  function handleOtherPopoverOpen(event: CustomEvent<string>) {
    if (event.detail !== popoverId && open) {
      open = false;
      dispatch('close');
    }
  }

  // Notify other popovers when this one opens
  function notifyPopoverOpen() {
    window.dispatchEvent(new CustomEvent('popover-open', { detail: popoverId }));
  }

  // Action to portal element to body
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
    if (!triggerEl || !open) return;

    await tick();

    // Wait for next frame to ensure layout is complete
    await new Promise(resolve => requestAnimationFrame(resolve));

    const rect = triggerEl.getBoundingClientRect();
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;

    let top = rect.bottom + sideOffset;
    let left = rect.left;

    if (align === 'center') {
      left = rect.left + rect.width / 2;
    } else if (align === 'end') {
      left = rect.right;
    }

    // Use fixed width since we set w-[320px] on the popover
    // Measuring during transition can give incorrect values
    const contentWidth = 320;
    const contentHeight = contentEl?.offsetHeight || 400;

    // Horizontal boundary check - keep within viewport
    if (left + contentWidth > viewportWidth - 16) {
      left = viewportWidth - contentWidth - 16;
    }
    if (left < 16) {
      left = 16;
    }

    // Vertical boundary check
    if (top + contentHeight > viewportHeight - 16) {
      // Position above the trigger instead
      top = rect.top - contentHeight - sideOffset;
      // If still off-screen (above viewport), clamp to top
      if (top < 16) {
        top = 16;
      }
    }

    popoverStyle = `top: ${top}px; left: ${left}px;`;
  }

  $: if (open) {
    updatePosition();
  }

  function handleBackdropClick(event: MouseEvent) {
    // Only close if clicking the backdrop itself, not bubbled events
    if (event.target === event.currentTarget) {
      open = false;
      dispatch('close');
    }
  }

  function handleContentClick(event: MouseEvent) {
    // Prevent clicks inside content from closing the popover
    event.stopPropagation();
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && open) {
      event.preventDefault();
      open = false;
      dispatch('close');
    }
  }

  function handleTriggerClick() {
    open = !open;
    if (open) {
      notifyPopoverOpen();
      updatePosition();
    }
  }

  function handleTriggerKeydown(event: KeyboardEvent) {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      handleTriggerClick();
    }
  }

  // Handle clicks outside when open
  function handleDocumentClick(event: MouseEvent) {
    if (!open) return;

    const target = event.target as HTMLElement;

    // Check if click is inside trigger or content
    if (triggerEl?.contains(target)) return;
    if (contentEl?.contains(target)) return;

    // Click was outside, close the popover
    open = false;
    dispatch('close');
  }
</script>

<svelte:window on:keydown={handleKeydown} />
<svelte:document on:click={handleDocumentClick} />

<div class="relative inline-block">
  <!-- Trigger -->
  <!-- svelte-ignore a11y-no-static-element-interactions -->
  <div
    bind:this={triggerEl}
    class="popover-trigger"
    on:click|stopPropagation={handleTriggerClick}
    on:keydown={handleTriggerKeydown}
    role="button"
    tabindex="0"
  >
    <slot name="trigger" />
  </div>
</div>

<!-- Popover Content (portaled to body to escape stacking contexts) -->
{#if open && mounted && portalContainer}
  <div
    use:portal
    bind:this={contentEl}
    class="popover-content fixed rounded-lg border border-border bg-popover text-popover-foreground shadow-xl outline-none pointer-events-auto overflow-hidden w-[320px] max-w-[calc(100vw-32px)]
           {align === 'center' ? '-translate-x-1/2' : align === 'end' ? '-translate-x-full' : ''}
           {contentClass}"
    style="{popoverStyle}"
    transition:scale={{ duration: 150, start: 0.95, opacity: 0 }}
    on:click={handleContentClick}
    role="dialog"
    aria-modal="true"
  >
    <div class={compact ? 'p-2' : 'p-4'}>
      <slot />
    </div>
  </div>
{/if}
