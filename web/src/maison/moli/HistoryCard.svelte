<script>
  import { device, nameOf, num, clock, relative, home } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** How one value moved over the last hours: a curve for numbers, the
   *  last changes for the rest. */
  let { point, hours = 24, title = null } = $props();

  const [id, key] = $derived([point.slice(0, point.indexOf('/')), point.slice(point.indexOf('/') + 1)]);
  const spec = $derived(device(id)?.points.find((p) => p.key === key));
  let series = $state(null);
  let failed = $state(false);

  $effect(() => {
    failed = false;
    fetch(`/api/history?point=${encodeURIComponent(point)}&hours=${hours}&points=120`)
      .then((r) => (r.ok ? r.json() : Promise.reject()))
      .then((s) => (series = s))
      .catch(() => (failed = true));
  });

  // Numbers: averages per bucket when bucketed, else the raw changes.
  const pts = $derived.by(() => {
    if (!series) return [];
    if (series.buckets?.length) return series.buckets.map((b) => [b.ts, b.avg]);
    const raw = series.raw.filter(([, v]) => typeof v === 'number');
    if (series.before && typeof series.before[1] === 'number') raw.unshift([series.from, series.before[1]]);
    if (raw.length) raw.push([series.to, raw.at(-1)[1]]);
    return raw;
  });
  const numeric = $derived(pts.length >= 2);
  const W = 320;
  const H = 90;
  const stats = $derived.by(() => {
    if (!numeric) return null;
    const vs = pts.map((p) => p[1]);
    return { min: Math.min(...vs), max: Math.max(...vs), last: vs.at(-1) };
  });
  const path = $derived.by(() => {
    if (!numeric) return '';
    const { min, max } = stats;
    const span = max - min || 1;
    const x = (t) => ((t - series.from) / (series.to - series.from)) * W;
    const y = (v) => H - 6 - ((v - min) / span) * (H - 12);
    return pts.map(([t, v], i) => `${i ? 'L' : 'M'}${x(t).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
  });
  const unit = $derived(spec?.unit ? ` ${spec.unit}` : '');
  const changes = $derived(numeric ? [] : (series?.raw ?? []).slice(-6).reverse());
  const say = (v) => (v === true ? t('moli.historique.oui') : v === false ? t('moli.historique.non') : String(v));
</script>

<section class="card hist">
  <div class="card-head">
    <h2><Icon name="tune" size={18} />{title ?? `${nameOf(id)} · ${spec?.label ?? key}`}</h2>
    <span class="muted">{hours} h</span>
  </div>
  {#if failed}
    <p class="muted">{t('moli.historique.indisponible')}</p>
  {:else if !series}
    <p class="muted">…</p>
  {:else if numeric}
    <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" aria-hidden="true">
      <path d="{path} L{W},{H} L0,{H} Z" class="area" />
      <path d={path} class="line" />
    </svg>
    <div class="stats">
      <span><small>{t('moli.historique.maintenant')}</small><b class="num">{num(stats.last, 1)}{unit}</b></span>
      <span><small>{t('moli.historique.min')}</small><b class="num">{num(stats.min, 1)}{unit}</b></span>
      <span><small>{t('moli.historique.max')}</small><b class="num">{num(stats.max, 1)}{unit}</b></span>
    </div>
  {:else if changes.length}
    <ul>
      {#each changes as [ts, v], i (i)}
        <li><b>{say(v)}</b><span class="muted">{clock(ts)} · {relative(ts, home.now)}</span></li>
      {/each}
    </ul>
  {:else}
    <p class="muted">{t('moli.historique.aucun')}</p>
  {/if}
</section>

<style>
  svg {
    width: 100%;
    height: 90px;
    display: block;
  }

  .area {
    fill: color-mix(in srgb, var(--cool) 14%, transparent);
  }

  .line {
    fill: none;
    stroke: var(--cool);
    stroke-width: 2;
    vector-effect: non-scaling-stroke;
  }

  .stats {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    margin-top: 10px;
  }

  .stats span {
    display: grid;
  }

  small {
    font-size: 11px;
    font-weight: 650;
    color: var(--ink-3);
  }

  b {
    font-weight: 650;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }

  li {
    display: flex;
    justify-content: space-between;
    gap: 12px;
  }
</style>
