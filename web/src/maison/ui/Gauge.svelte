<script>
  import { onMount } from 'svelte';

  /** A half-ring gauge (look only): the arc fills from `min` to `value`
   *  in the given colours, a bead rides its tip; `text` is the figure as
   *  the page writes it, `label` the word under it. */
  let { value = null, min = 0, max = 100, label = '', text = '', colors = ['#3fae7c', '#e8b931', '#e0705a'] } = $props();
  const uid = $props.id();
  let shown = $state(false);

  const p = $derived(value == null ? 0 : Math.max(0, Math.min(1, (value - min) / (max - min || 1))));
  const display = $derived(value == null ? '—' : text);

  onMount(() => {
    let id = requestAnimationFrame(() => (id = requestAnimationFrame(() => (shown = true))));
    return () => cancelAnimationFrame(id);
  });
</script>

<svg viewBox="0 0 120 70" role="img" aria-label={`${label} ${display}`}>
  <defs>
    <linearGradient id={`g-${uid}`} x1="0" y1="0" x2="120" y2="0" gradientUnits="userSpaceOnUse">
      {#each colors as c, i (i)}
        <stop offset={`${(i / Math.max(1, colors.length - 1)) * 100}%`} stop-color={c} />
      {/each}
    </linearGradient>
  </defs>

  <path d="M 10 60 A 50 50 0 0 1 110 60" fill="none" stroke="var(--surface-3)" stroke-width="10" stroke-linecap="round" />

  {#if value != null}
    <path
      class="arc"
      d="M 10 60 A 50 50 0 0 1 110 60"
      fill="none"
      stroke={`url(#g-${uid})`}
      stroke-width="10"
      stroke-linecap="round"
      pathLength="100"
      stroke-dasharray="{shown ? p * 100 : 0} 100"
    />
    <g class="dot" style:transform="rotate({shown ? p * 180 : 0}deg)">
      <circle cx="10" cy="60" r="6" fill="white" stroke="var(--ink)" stroke-width="3" />
    </g>
  {/if}

  <text x="60" y="54" text-anchor="middle" class="big">{display}</text>
  {#if label}<text x="60" y="68" text-anchor="middle" class="small">{label}</text>{/if}
</svg>

<style>
  svg {
    display: block;
    width: 100%;
    max-width: 160px;
    overflow: visible;
  }

  .arc {
    transition: stroke-dasharray 1s cubic-bezier(0.22, 1, 0.36, 1);
  }

  .dot {
    transform-origin: 60px 60px;
    transform-box: view-box;
    transition: transform 1s cubic-bezier(0.22, 1, 0.36, 1);
  }

  .big {
    font-size: 20px;
    font-weight: 750;
    fill: var(--ink);
    font-variant-numeric: tabular-nums;
  }

  .small {
    font-size: 9px;
    font-weight: 600;
    fill: var(--ink-3);
  }

  @media (prefers-reduced-motion: reduce) {
    .arc,
    .dot {
      transition: none;
    }
  }
</style>
