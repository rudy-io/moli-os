<script>
  import { onMount } from 'svelte';
  import { t } from '../../../lib/i18n.svelte.js';

  /** The last hours of a point, as a small filled line (from the history). */
  let { point, hours = 6, max = null, label = '', unit = '' } = $props();

  let series = $state([]);
  async function load() {
    try {
      const res = await fetch(`/api/history?point=${encodeURIComponent(point)}&hours=${hours}&points=90`);
      if (!res.ok) return;
      const h = await res.json();
      series = h.buckets?.length
        ? h.buckets.map((b) => [b.ts, b.avg])
        : (h.raw ?? []).filter(([, v]) => typeof v === 'number');
    } catch {
      /* next round */
    }
  }
  onMount(() => {
    load();
    const timer = setInterval(load, 60_000);
    return () => clearInterval(timer);
  });

  const W = 300;
  const H = 54;
  const path = $derived.by(() => {
    if (series.length < 2) return null;
    const t0 = series[0][0];
    const t1 = series.at(-1)[0];
    const top = max ?? Math.max(...series.map(([, v]) => v), 1);
    const x = (ts) => ((ts - t0) / Math.max(1, t1 - t0)) * W;
    const y = (v) => H - 2 - (Math.min(v, top) / top) * (H - 6);
    const line = series.map(([ts, v], i) => `${i ? 'L' : 'M'}${x(ts).toFixed(1)} ${y(v).toFixed(1)}`).join(' ');
    return { line, area: `${line} L${W} ${H} L0 ${H} Z` };
  });
  const peak = $derived(series.length ? Math.max(...series.map(([, v]) => v)) : null);
</script>

<figure class="spark">
  <figcaption>
    <span>{label}, {hours} h</span>
    {#if peak != null}<span class="muted">{t('systeme.infra.spark.pointe', { value: `${Math.round(peak)}${unit}` })}</span>{/if}
  </figcaption>
  {#if path}
    <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" aria-hidden="true">
      <path d={path.area} class="area" />
      <path d={path.line} class="line" />
    </svg>
  {:else}
    <div class="empty muted">{t('systeme.infra.spark.vide')}</div>
  {/if}
</figure>

<style>
  .spark {
    margin: 0;
    display: grid;
    gap: 4px;
  }

  figcaption {
    display: flex;
    justify-content: space-between;
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-2);
  }

  svg {
    width: 100%;
    height: 54px;
    display: block;
  }

  .area {
    fill: var(--cool-soft);
  }

  .line {
    fill: none;
    stroke: var(--cool);
    stroke-width: 1.6;
    vector-effect: non-scaling-stroke;
  }

  .empty {
    height: 54px;
    display: grid;
    place-items: center;
    font-size: 12px;
  }
</style>
