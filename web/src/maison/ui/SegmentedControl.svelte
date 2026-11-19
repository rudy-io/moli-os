<script>
  import Icon from './Icon.svelte';

  /**
   * Segmented control with animated sliding indicator.
   * The label always names the segment: compact shows the icon alone and keeps
   * the text as its accessible name and tooltip.
   * @template T
   * @typedef {Object} Segment
   * @property {T} value - Value when selected
   * @property {string} label - Display label (accessible name when compact)
   * @property {boolean} [disabled] - Disable this segment
   * @property {string} [icon] - Optional icon path (MDI), drawn by Icon
   */

  /** @type {{ segments: Segment<any>[], value?: any, onchange?: (value: any) => void, label?: string, compact?: boolean }} */
  let { segments, value = $bindable(undefined), onchange, label, compact = false } = $props();

  let buttons = $state([]);
  let group = $state();
  let indicator = $state({ left: 0, width: 0, ready: false });

  function measure() {
    const selected = buttons[segments.findIndex((segment) => segment.value === value)];
    if (!selected) return;
    indicator = { left: selected.offsetLeft, width: selected.offsetWidth, ready: true };
  }

  // The pill follows the selected label, including after the text is re-laid
  // out: language, viewport or the compact icons switch.
  $effect(() => {
    void compact;
    measure();
  });
  $effect(() => {
    if (!group) return;
    const observer = new ResizeObserver(measure);
    observer.observe(group);
    return () => observer.disconnect();
  });

  function select(segment) {
    if (segment.disabled) return;
    // The parent owns the value: a choice only becomes real once the parent
    // accepts it. A confirmation can be cancelled, and the pill must then
    // stay where the house really is.
    onchange?.(segment.value);
  }
</script>

<div class="segmented-control" class:compact role="group" aria-label={label} bind:this={group}>
  <div class="segments" class:ready={indicator.ready}>
    <span
      class="indicator"
      style:left="{indicator.left}px"
      style:width="{indicator.width}px"></span>
    {#each segments as segment, i (segment.value)}
      <button
        bind:this={buttons[i]}
        type="button"
        class="segment"
        aria-pressed={value === segment.value}
        aria-label={compact ? segment.label : undefined}
        title={compact ? segment.label : undefined}
        disabled={segment.disabled}
        onclick={() => select(segment)}
      >
        {#if segment.icon}
          <Icon path={segment.icon} size={20} />
        {/if}
        {#if !compact}
          <span>{segment.label}</span>
        {/if}
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
    opacity: 0;
    transition: left 200ms cubic-bezier(0.2, 0.8, 0.2, 1), width 200ms cubic-bezier(0.2, 0.8, 0.2, 1), opacity 150ms ease;
  }

  .segments.ready .indicator {
    opacity: 1;
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

  /* Readable even before the pill is measured (or if it never is). */
  .segment[aria-pressed="true"] {
    color: var(--warm-deep);
    background: var(--warm);
  }

  .segments.ready .segment[aria-pressed="true"] {
    background: transparent;
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
    .indicator,
    .segment {
      transition: none;
    }

    .segment:not(:disabled):active {
      transform: none;
    }
  }
</style>
