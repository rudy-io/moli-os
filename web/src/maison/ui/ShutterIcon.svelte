<script>
  import { coverPosition } from '../lib/home.svelte.js';
  import Icon from './Icon.svelte';

  /** Shutter icon showing open/closed/semi-open state */
  let { id, size = 46, compact = false } = $props();

  const pct = $derived(coverPosition(id));
  const isOpen = $derived(pct != null && pct > 0);

  // Any opening uses the open shutter; the alert icon reads as a fault.
  const iconName = $derived(pct != null && pct > 0 ? 'shutter-open' : 'shutter');
</script>

<div class="shutter-icon" class:compact class:open={isOpen} style:--size="{size}px" style:--glow="var(--warm-glow)">
  <Icon name={iconName} size={compact ? 18 : 26} />
</div>

<style>
  .shutter-icon {
    position: relative;
    width: var(--size, 46px);
    height: var(--size, 46px);
    border-radius: 50%;
    display: grid;
    place-items: center;
    flex: none;
    background: var(--surface);
    color: var(--ink-3);
    transition: all 0.35s var(--ease);
  }

  .shutter-icon :global(svg) {
    display: block;
    margin: 0;
  }

  .shutter-icon.open {
    background: var(--warm);
    color: #fff;
    box-shadow: 0 0 0 6px var(--glow, var(--warm-glow)),
                0 6px 22px var(--glow, var(--warm-glow));
  }
</style>
