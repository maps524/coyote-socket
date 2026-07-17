<script lang="ts">
  import { run, stopPropagation } from 'svelte/legacy';

  import { createEventDispatcher, tick, onMount, onDestroy } from 'svelte';
  import { scale } from 'svelte/transition';

  interface Props {
    open?: boolean;
    align?: 'start' | 'center' | 'end';
    sideOffset?: number;
    contentClass?: string; // Additional classes for content area
    compact?: boolean; // Use smaller padding
    trigger?: import('svelte').Snippet;
    children?: import('svelte').Snippet;
  }

  let {
    open = $bindable(false),
    align = 'start',
    sideOffset = 8,
    contentClass = '',
    compact = false,
    trigger,
    children
  }: Props = $props();

  const dispatch = createEventDispatcher();

  // Unique ID for this popover instance
  const popoverId = `popover-${Math.random().toString(36).substr(2, 9)}`;

  let triggerEl: HTMLElement | null = $state(null);
  let contentEl: HTMLElement | null = $state(null);
  let portalContainer: HTMLElement | null = $state(null);
  let popoverStyle = $state('');
  let mounted = $state(false);

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

    // Measure the real rendered size. A `scale` transition doesn't change the
    // layout box, so offsetWidth/Height are stable here (we've already awaited
    // tick + a frame). Falls back to sane defaults if not yet laid out.
    const contentWidth = contentEl?.offsetWidth || 320;
    const contentHeight = contentEl?.offsetHeight || 400;

    // Horizontal boundary check. `left` is the CSS anchor, but the element is
    // shifted by a translate for center/end alignment, so clamp the *visual*
    // box [visualLeft, visualLeft+contentWidth] into the viewport, then convert
    // back to the anchor. Without this, an end-aligned popover gets clamped as
    // if it grew rightward and drifts off its trigger in narrow windows.
    const margin = 16;
    const anchorToVisual = align === 'center' ? contentWidth / 2 : align === 'end' ? contentWidth : 0;
    let visualLeft = left - anchorToVisual;
    if (visualLeft + contentWidth > viewportWidth - margin) {
      visualLeft = viewportWidth - margin - contentWidth;
    }
    if (visualLeft < margin) {
      visualLeft = margin;
    }
    left = visualLeft + anchorToVisual;

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

  run(() => {
    if (open) {
      updatePosition();
    }
  });

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

<svelte:window onkeydown={handleKeydown} />
<svelte:document onclick={handleDocumentClick} />

<div class="relative inline-block">
  <!-- Trigger -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    bind:this={triggerEl}
    class="popover-trigger"
    onclick={stopPropagation(handleTriggerClick)}
    onkeydown={handleTriggerKeydown}
    role="button"
    tabindex="0"
  >
    {@render trigger?.()}
  </div>
</div>

<!-- Popover Content (portaled to body to escape stacking contexts) -->
{#if open && mounted && portalContainer}
  <div
    use:portal
    bind:this={contentEl}
    class="popover-content fixed rounded-lg border border-border bg-popover text-popover-foreground shadow-xl outline-hidden pointer-events-auto overflow-hidden w-[320px] max-w-[calc(100vw-32px)]
           {align === 'center' ? '-translate-x-1/2' : align === 'end' ? '-translate-x-full' : ''}
           {contentClass}"
    style="{popoverStyle}"
    transition:scale={{ duration: 150, start: 0.95, opacity: 0 }}
    onclick={handleContentClick}
    role="dialog"
    aria-modal="true"
  >
    <div class={compact ? 'p-2' : 'p-4'}>
      {@render children?.()}
    </div>
  </div>
{/if}
