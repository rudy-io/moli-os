<script>
  /**
   * A cover position slider showing both target (draggable) and actual position.
   * - writable: shows actual position marker + draggable thumb for target
   * - read-only: shows only actual position marker, no interaction
   */
  let { min = 0, max = 100, step = 1, value, targetValue = null, label, writable = false, oncommit } = $props();

  let el = $state();
  let held = $state(false);
  let start = null;

  // In writable mode, targetValue is what the user drags (defaults to value if null)
  const displayTarget = $derived(writable ? (targetValue ?? value) : value);
  const actualPercent = $derived(max > min ? ((value - min) / (max - min)) * 100 : 50);
  const targetPercent = $derived(max > min ? ((displayTarget - min) / (max - min)) * 100 : 50);

  const paint = () => {
    if (!el) return;
    el.style.setProperty('--actual', `${actualPercent}%`);
    el.style.setProperty('--target', `${targetPercent}%`);
  };

  $effect(() => {
    if (el && !held) {
      el.value = String(displayTarget);
      paint();
    }
  });

  function live() {
    if (!writable) return;
    paint();
  }

  function commit() {
    if (!writable) return;
    held = false;
    oncommit?.(Number(el.value));
  }

  function press() {
    if (!writable) return;
    held = true;
    start = el.value;
  }

  function release() {
    if (!writable) return;
    if (held && el.value === start) held = false;
  }
</script>

{#if writable}
  <input
    bind:this={el}
    class="cover-slider"
    type="range"
    {min}
    {max}
    {step}
    aria-label={label}
    onpointerdown={press}
    onpointerup={release}
    onlostpointercapture={release}
    onpointercancel={() => (held = false)}
    oninput={live}
    onchange={commit} />
{:else}
  <div class="cover-slider read-only" style="--actual: {actualPercent}%" aria-label={label} aria-valuenow={value} role="progressbar">
    <div class="track"></div>
    <div class="marker"></div>
  </div>
{/if}

<style>
  .cover-slider {
    appearance: none;
    -webkit-appearance: none;
    flex: 1;
    min-width: 0;
    width: 100%;
    height: 6px;
    margin: 14px 0;
    border-radius: 999px;
    position: relative;
    background: var(--surface-3);
    box-shadow: inset 0 1px 2px rgb(0 0 0 / 12%);
    outline: none;
    cursor: pointer;
    touch-action: pan-y;
  }

  /* Read-only: no interaction */
  .cover-slider.read-only {
    display: block;
    position: relative;
    cursor: default;
    pointer-events: none;
  }

  .cover-slider.read-only .track {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    border-radius: 999px;
    background: var(--surface-3);
    box-shadow: inset 0 1px 2px rgb(0 0 0 / 12%);
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

  /* Writable: show actual position + draggable thumb */
  .cover-slider::before {
    content: '';
    position: absolute;
    top: 50%;
    left: var(--actual, 50%);
    transform: translate(-50%, -50%);
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--warm);
    opacity: 0.6;
    pointer-events: none;
    z-index: 1;
  }

  .cover-slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 16px;
    height: 16px;
    border: 2px solid var(--cool);
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 4px rgb(0 0 0 / 35%);
    transition: transform 0.15s var(--ease);
    cursor: grab;
    position: relative;
    z-index: 2;
  }

  .cover-slider::-moz-range-thumb {
    width: 16px;
    height: 16px;
    border: 2px solid var(--cool);
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 4px rgb(0 0 0 / 35%);
    transition: transform 0.15s var(--ease);
    cursor: grab;
    position: relative;
    z-index: 2;
  }

  .cover-slider:hover::-webkit-slider-thumb,
  .cover-slider:focus-visible::-webkit-slider-thumb,
  .cover-slider:active::-webkit-slider-thumb {
    transform: scale(1.5);
    cursor: grabbing;
  }

  .cover-slider:hover::-moz-range-thumb,
  .cover-slider:focus-visible::-moz-range-thumb,
  .cover-slider:active::-moz-range-thumb {
    transform: scale(1.5);
    cursor: grabbing;
  }

  .cover-slider::-moz-range-track {
    background: transparent;
  }

  .cover-slider:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 6px;
  }

  @media (prefers-reduced-motion: reduce) {
    .cover-slider::-webkit-slider-thumb {
      transition: none;
    }
    .cover-slider::-moz-range-thumb {
      transition: none;
    }
  }
</style>
