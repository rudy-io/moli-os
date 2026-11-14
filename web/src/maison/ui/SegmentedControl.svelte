<script>
  import { spring } from 'svelte/motion';

  /**
   * Segmented control with animated sliding indicator.
   * @template T
   * @typedef {Object} Segment
   * @property {T} value - Value when selected
   * @property {string} label - Display label
   * @property {boolean} [disabled] - Disable this segment
   * @property {string} [icon] - Optional icon component/path
   */

  /** @type {{ segments: Segment<any>[], value?: any, onchange?: (value: any) => void, label?: string, compact?: boolean }} */
  let { segments, value = $bindable(undefined), onchange, label, compact = false } = $props();

  let buttons = $state([]);
  let indicator = spring({ left: 0, width: 0 }, { stiffness: 0.2, damping: 0.3 });

  $effect(() => {
    const selectedIndex = segments.findIndex(s => s.value === value);
    if (selectedIndex >= 0 && buttons[selectedIndex]) {
      const btn = buttons[selectedIndex];
      indicator.set({ left: btn.offsetLeft, width: btn.offsetWidth });
    }
  });

  function select(segment) {
    if (segment.disabled) return;
    value = segment.value;
    onchange?.(segment.value);
  }
</script>

<div class="segmented-control" class:compact role="group" aria-label={label}>
  <div class="segments">
    <span class="indicator" style:left="{$indicator.left}px" style:width="{$indicator.width}px"></span>
    {#each segments as segment, i (segment.value)}
      <button
        bind:this={buttons[i]}
        type="button"
        class="segment"
        aria-pressed={value === segment.value}
        disabled={segment.disabled}
        onclick={() => select(segment)}
      >
        {#if segment.icon}
          {@html segment.icon}
        {/if}
        <span>{segment.label}</span>
      </button>
    {/each}
  </div>
</div>

<style>
  .segmented-control {
    display: inline-flex;
    flex-direction: column;
    gap: 4px;
  }

  .segments {
    position: relative;
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 999px;
    background: linear-gradient(135deg, var(--warm-soft) 0%, color-mix(in srgb, var(--warm-soft) 70%, var(--surface-3)) 100%);
    isolation: isolate;
  }

  .indicator {
    position: absolute;
    top: 2px;
    bottom: 2px;
    z-index: 0;
    background: var(--warm);
    border-radius: 999px;
    box-shadow: 0 1px 3px rgb(0 0 0 / 12%), 0 2px 8px color-mix(in srgb, var(--warm) 30%, transparent);
    pointer-events: none;
    transition: opacity 200ms ease;
  }

  .segment {
    position: relative;
    z-index: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-height: 44px;
    min-width: 60px;
    padding: 8px 16px;
    border: none;
    background: transparent;
    color: color-mix(in srgb, var(--warm) 85%, var(--ink));
    font-size: 14px;
    font-weight: 600;
    line-height: 1.2;
    border-radius: 999px;
    cursor: pointer;
    touch-action: manipulation;
    transition: color 200ms ease;
  }

  .segment[aria-pressed="true"] {
    color: #fff;
  }

  .segment:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .segment:not(:disabled):active {
    transform: scale(0.98);
  }

  .segment:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 2px;
  }

  @media (hover: hover) {
    .segment:not(:disabled):not([aria-pressed="true"]):hover {
      color: var(--ink);
    }
  }

  .compact .segment {
    min-width: 44px;
    padding: 8px 12px;
    font-size: 13px;
  }

  @media (prefers-reduced-motion: reduce) {
    .segment:not(:disabled):active {
      transform: none;
    }
  }
</style>
