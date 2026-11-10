<script>
  import { onMount } from 'svelte';

  /** A small ring: `on` out of `total`, drawn in when shown (look only). */
  let { on = 0, total = 1, label = '', color = 'var(--warm)' } = $props();
  let drawn = $state(false);
  const r = 15;
  const c = 2 * Math.PI * r;
  const pct = $derived(total > 0 ? Math.min(1, Math.max(0, on / total)) : 0);

  onMount(() => {
    let id = requestAnimationFrame(() => (id = requestAnimationFrame(() => (drawn = true))));
    return () => cancelAnimationFrame(id);
  });
</script>

<span class="ring" role="img" aria-label={label}>
  <svg viewBox="0 0 36 36" width="36" height="36" aria-hidden="true">
    <circle cx="18" cy="18" {r} fill="none" stroke="var(--surface-3)" stroke-width="3" />
    <circle
      cx="18"
      cy="18"
      {r}
      fill="none"
      stroke={color}
      stroke-width="3"
      stroke-linecap="round"
      transform="rotate(-90 18 18)"
      stroke-dasharray={c}
      stroke-dashoffset={drawn ? c * (1 - pct) : c}
    />
  </svg>
  <span class="n">{on}</span>
</span>

<style>
  .ring {
    position: relative;
    width: 36px;
    height: 36px;
    display: inline-grid;
    place-items: center;
    flex: none;
  }

  svg {
    position: absolute;
    inset: 0;
  }

  circle {
    transition: stroke-dashoffset 0.8s cubic-bezier(0.22, 1, 0.36, 1);
  }

  .n {
    font-size: 12px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }

  @media (prefers-reduced-motion: reduce) {
    circle {
      transition: none;
    }
  }
</style>
