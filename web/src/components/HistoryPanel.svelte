<script>
  import { hub } from '../lib/hub.svelte.js';
  import { deviceName, formatValue, unitOf } from '../lib/format.js';
  import { t, locale } from '../lib/i18n.svelte.js';

  let { point: pointId, onclose } = $props();

  const RANGES = [
    { label: 'systeme.historique.plage_6h', hours: 6 },
    { label: 'systeme.historique.plage_24h', hours: 24 },
    { label: 'systeme.historique.plage_7j', hours: 168 },
  ];
  let hours = $state(24);
  let series = $state(null);
  let error = $state('');

  const W = 640;
  const H = 220;
  const PAD = { l: 44, r: 12, t: 12, b: 26 };

  const deviceId = $derived(pointId.slice(0, pointId.indexOf('/')));
  const key = $derived(pointId.slice(pointId.indexOf('/') + 1));
  const device = $derived(hub.devices[deviceId]);
  const spec = $derived(device?.points.find((p) => p.key === key));

  $effect(() => {
    const url = `/api/history?point=${encodeURIComponent(pointId)}&hours=${hours}&points=300`;
    series = null;
    error = '';
    fetch(url)
      .then((r) => (r.ok ? r.json() : r.json().then((e) => Promise.reject(new Error(e.error)))))
      .then((s) => (series = s))
      .catch((e) => (error = e.message));
  });

  const toNum = (v) => (typeof v === 'boolean' ? Number(v) : typeof v === 'number' ? v : null);

  // Points to draw: [ts, value, min, max].
  const data = $derived.by(() => {
    if (!series) return [];
    const points = series.buckets.length
      ? series.buckets.map((b) => [b.ts, b.avg, b.min, b.max])
      : series.raw.map(([ts, v]) => [ts, toNum(v), toNum(v), toNum(v)]);
    // The value held when the window opens: the line starts at `from`.
    if (series.before) {
      const v = toNum(series.before[1]);
      points.unshift([series.from, v, v, v]);
    }
    return points.filter((d) => d[1] !== null);
  });

  const binary = $derived(spec?.kind.type === 'binary');
  const textual = $derived(series && data.length === 0 && series.raw.length > 0);

  const scale = $derived.by(() => {
    if (!series || !data.length) return null;
    const lo = binary ? 0 : Math.min(...data.map((d) => d[2]));
    const hi = binary ? 1 : Math.max(...data.map((d) => d[3]));
    const span = hi - lo || Math.abs(hi) || 1;
    const y0 = binary ? 0 : lo - span * 0.08;
    const y1 = binary ? 1 : hi + span * 0.08;
    const x = (ts) => PAD.l + ((ts - series.from) / (series.to - series.from)) * (W - PAD.l - PAD.r);
    const y = (v) => PAD.t + (1 - (v - y0) / (y1 - y0)) * (H - PAD.t - PAD.b);
    return { x, y, y0, y1 };
  });

  // Step path: a value holds until the next change, then to "now".
  const line = $derived.by(() => {
    if (!scale) return '';
    let d = '';
    data.forEach(([ts, v], i) => {
      const px = scale.x(ts);
      const py = scale.y(v);
      d += i === 0 ? `M${px},${py}` : `H${px}V${py}`;
    });
    return d + `H${scale.x(series.to)}`;
  });

  const band = $derived.by(() => {
    if (!scale || !series.buckets.length) return '';
    const top = data.map(([ts, , , max]) => `${scale.x(ts)},${scale.y(max)}`);
    const bottom = data.map(([ts, , min]) => `${scale.x(ts)},${scale.y(min)}`).reverse();
    return `M${top.join('L')}L${bottom.join('L')}Z`;
  });

  const ticks = $derived.by(() => {
    if (!scale) return [];
    if (binary) return [0, 1];
    const n = 4;
    return Array.from({ length: n + 1 }, (_, i) => scale.y0 + ((scale.y1 - scale.y0) * i) / n);
  });

  const nf = $derived(new Intl.NumberFormat(locale(), { maximumFractionDigits: 1 }));
  const timeFmt = $derived(
    new Intl.DateTimeFormat(locale(), hours > 24 ? { weekday: 'short', day: 'numeric' } : { hour: '2-digit', minute: '2-digit' }),
  );
  const xTicks = $derived(
    series ? Array.from({ length: 5 }, (_, i) => series.from + ((series.to - series.from) * i) / 4) : [],
  );

  function onkey(e) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="backdrop" onclick={onclose} role="presentation"></div>
<div class="panel" role="dialog" aria-modal="true" aria-label={t('systeme.historique.aria')}>
  <header>
    <div>
      <h3>{spec?.label ?? key}</h3>
      <p>{device ? deviceName(device) : deviceId}</p>
    </div>
    <div class="ranges">
      {#each RANGES as r (r.hours)}
        <button class:active={hours === r.hours} onclick={() => (hours = r.hours)}>{t(r.label)}</button>
      {/each}
      <button class="close" onclick={onclose} aria-label={t('commun.fermer')}>×</button>
    </div>
  </header>

  {#if error}
    <p class="empty">{error}</p>
  {:else if !series}
    <p class="empty">{t('systeme.chargement')}</p>
  {:else if textual}
    <ol class="changes">
      {#each [...series.raw].reverse().slice(0, 30) as [ts, v] (ts)}
        <li><time class="num">{new Date(ts).toLocaleString(locale())}</time> {spec ? formatValue(spec, v) : v}</li>
      {/each}
    </ol>
  {:else if !data.length}
    <p class="empty">{t('systeme.historique.vide')}</p>
  {:else}
    <svg viewBox="0 0 {W} {H}" role="img" aria-label={t('systeme.historique.courbe', { name: spec?.label ?? key })}>
      {#each ticks as tick (tick)}
        <line x1={PAD.l} x2={W - PAD.r} y1={scale.y(tick)} y2={scale.y(tick)} class="grid" />
        <text x={PAD.l - 6} y={scale.y(tick) + 4} text-anchor="end" class="axis num">
          {binary ? (tick ? t('systeme.historique.oui') : t('systeme.historique.non')) : nf.format(tick)}
        </text>
      {/each}
      {#each xTicks as tick (tick)}
        <text x={scale.x(tick)} y={H - 6} text-anchor="middle" class="axis num">{timeFmt.format(tick)}</text>
      {/each}
      {#if band}<path d={band} class="band" />{/if}
      <path d={line} class="line" />
    </svg>
    <p class="meta num">
      {t('systeme.historique.changements', { count: series.total })}{#if series.buckets.length} · {t('systeme.historique.moyenne')}{/if}
      {#if unitOf(spec ?? {})} · {unitOf(spec)}{/if}
    </p>
  {/if}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--ink) 35%, transparent);
    z-index: 20;
  }
  .panel {
    position: fixed;
    z-index: 21;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    width: min(720px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: auto;
    background: var(--card);
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    padding: 16px 18px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 10px;
  }
  h3 {
    margin: 0;
    font-size: 17px;
  }
  header p {
    margin: 2px 0 0;
    color: var(--ink-3);
    font-size: 13px;
  }
  .ranges {
    display: flex;
    gap: 6px;
  }
  .ranges button {
    border: 1.5px solid var(--line);
    background: transparent;
    border-radius: 999px;
    padding: 3px 10px;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }
  .ranges .active {
    border-color: var(--ink);
    background: var(--sun);
    color: #161512;
  }
  .ranges .close {
    border: 0;
    font-size: 20px;
    line-height: 1;
    padding: 0 6px;
  }
  svg {
    width: 100%;
    height: auto;
    display: block;
  }
  .grid {
    stroke: var(--line);
    stroke-width: 1;
  }
  .axis {
    fill: var(--ink-3);
    font-size: 11px;
  }
  .line {
    fill: none;
    stroke: var(--copper);
    stroke-width: 2;
    stroke-linejoin: round;
  }
  .band {
    fill: color-mix(in srgb, var(--sun) 35%, transparent);
    stroke: none;
  }
  .meta,
  .empty {
    color: var(--ink-3);
    font-size: 13px;
  }
  .changes {
    list-style: none;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 4px;
    font-size: 14px;
  }
  .changes time {
    color: var(--ink-3);
    margin-right: 8px;
  }
  button:focus-visible {
    outline: 2px solid var(--copper);
    outline-offset: 2px;
  }
</style>
