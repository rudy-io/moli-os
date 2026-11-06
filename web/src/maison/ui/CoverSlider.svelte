<script>
  import {
    mdiChevronUp,
    mdiChevronDoubleUp,
    mdiChevronDown,
    mdiChevronDoubleDown,
  } from '@mdi/js';
  import Icon from './Icon.svelte';

  /**
   * A cover position slider showing actual position and target on click.
   * - writable: click to set target, shows button with directional icon
   * - read-only: shows only actual position marker, no interaction
   */
  let { min = 0, max = 100, step = 1, value, label, writable = false, oncommit } = $props();

  let container = $state();
  let clickState = $state(null); // { target, mouseAboveTrack }

  const actualPercent = $derived(max > min ? ((value - min) / (max - min)) * 100 : 50);

  // Determine icon based on target vs actual and mouse position
  const icon = $derived.by(() => {
    if (!clickState) return null;
    const goingUp = clickState.target > value;
    const fast = clickState.mouseAboveTrack;
    if (goingUp && fast) return mdiChevronDoubleUp;
    if (goingUp && !fast) return mdiChevronUp;
    if (!goingUp && fast) return mdiChevronDoubleDown;
    return mdiChevronDown;
  });

  function handlePointerDown(e) {
    if (!writable || !container) return;

    const rect = container.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const percent = Math.max(0, Math.min(100, (x / rect.width) * 100));
    const target = Math.round(min + (percent / 100) * (max - min));

    // Check if mouse is above or below the track centerline
    const trackCenterY = rect.top + rect.height / 2;
    const mouseAboveTrack = e.clientY < trackCenterY;

    clickState = { target, mouseAboveTrack };
  }

  function handlePointerUp() {
    if (!writable || !clickState) return;
    oncommit?.(clickState.target);
    clickState = null;
  }

  function handlePointerCancel() {
    clickState = null;
  }
</script>

{#if writable}
  <div
    bind:this={container}
    class="cover-slider"
    role="slider"
    tabindex="0"
    aria-label={label}
    aria-valuemin={min}
    aria-valuemax={max}
    aria-valuenow={value}
    onpointerdown={handlePointerDown}
    onpointerup={handlePointerUp}
    onpointercancel={handlePointerCancel}
    style="--actual: {actualPercent}%">
    <div class="track"></div>
    <div class="actual-marker"></div>
    {#if clickState}
      <div class="target-button" style="left: {((clickState.target - min) / (max - min)) * 100}%">
        <Icon path={icon} size={20} />
      </div>
    {/if}
  </div>
{:else}
  <div class="cover-slider read-only" style="--actual: {actualPercent}%" aria-label={label} aria-valuenow={value} role="progressbar">
    <div class="track"></div>
    <div class="marker"></div>
  </div>
{/if}

<style>
  .cover-slider {
    position: relative;
    flex: 1;
    min-width: 0;
    width: 100%;
    height: 6px;
    margin: 14px 0;
    border-radius: 999px;
    cursor: pointer;
    touch-action: pan-y;
  }

  .cover-slider:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 6px;
  }

  .track {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    border-radius: 999px;
    background: var(--surface-3);
    box-shadow: inset 0 1px 2px rgb(0 0 0 / 12%);
  }

  /* Read-only: no interaction */
  .cover-slider.read-only {
    cursor: default;
    pointer-events: none;
  }

  .cover-slider.read-only .marker {
    position: absolute;
    top: 50%;
    left: var(--actual, 50%);
    transform: translate(-50%, -50%);
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--warm);
    box-shadow: 0 1px 3px rgb(0 0 0 / 25%);
  }

  /* Writable: show actual position marker */
  .actual-marker {
    position: absolute;
    top: 50%;
    left: var(--actual, 50%);
    transform: translate(-50%, -50%);
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--warm);
    opacity: 0.7;
    pointer-events: none;
    z-index: 1;
  }

  /* Target button: shown on click with directional icon */
  .target-button {
    position: absolute;
    top: 50%;
    transform: translate(-50%, -50%);
    width: 36px;
    height: 36px;
    border-radius: 50%;
    background: var(--cool);
    color: #fff;
    display: grid;
    place-items: center;
    box-shadow: 0 2px 8px rgb(0 0 0 / 30%);
    pointer-events: none;
    z-index: 2;
    animation: appear 0.15s var(--ease);
  }

  @keyframes appear {
    from {
      opacity: 0;
      transform: translate(-50%, -50%) scale(0.8);
    }
    to {
      opacity: 1;
      transform: translate(-50%, -50%) scale(1);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .target-button {
      animation: none;
    }
  }
</style>
