<script>
  import { mainWindow } from '../../lib/solar-sim.js';
  import { kw, kwhs, pct, hours, loadName } from './format.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** One day, hour by hour (average kW of each hour): what the panels
   *  give (amber line), what the house draws (dark line), the part of it
   *  the sun covers (green zone), the surplus (amber wash) and what is
   *  still bought (blue wash). Today: the panels' forecast dashed after
   *  now. `rows`: the day's simulated hours; `forecast`: `[{ hour, kw }]`. */
  let { rows = [], forecast = [], nowHour = null, loads = [] } = $props();

  const HOUR = 3_600_000;
  const H = 230;
  const TOP = 14;
  const BOTTOM = 26;
  const LEFT = 4;
  let W = $state(640);

  // The day's hours as average power (a partial hour under way counts once
  // a quarter of it has passed).
  const points = $derived.by(() => {
    const out = [];
    for (const r of rows) {
      const dur = (r.end - r.start) / HOUR;
      if (dur < 0.25) continue;
      const k = (v) => v / dur;
      out.push({
        hour: r.hour,
        pv: k(r.pv),
        load: k(r.load),
        self: k(r.direct + r.battOut),
        batt: k(r.battOut),
        charge: k(r.battIn),
        imp: k(r.import),
        exp: k(r.export),
        moved: k(Object.values(r.moved).reduce((t, v) => t + v, 0)),
        r,
      });
    }
    return out.sort((a, b) => a.hour - b.hour);
  });

  const max = $derived.by(() => {
    const top = Math.max(1, ...points.map((p) => Math.max(p.pv, p.load)), ...forecast.map((f) => f.kw));
    const step = top <= 2 ? 0.5 : top <= 5 ? 1 : 2;
    return Math.ceil(top / step) * step;
  });
  const ticks = $derived([max / 2, max]);
  const x = (h) => LEFT + ((h + 0.5) / 24) * (W - LEFT * 2);
  const y = (v) => TOP + (H - TOP - BOTTOM) * (1 - Math.min(v, max) / max);

  /** Monotone cubic through the points (Fritsch-Carlson): smooth, never
   *  overshooting a value that is not there. `lead`: 'M' to start a path,
   *  'L' to carry on one. */
  function curve(pts, lead = 'M') {
    const n = pts.length;
    if (!n) return '';
    if (n === 1) return `${lead}${pts[0][0]},${pts[0][1]}`;
    const dx = [];
    const m = [];
    for (let i = 0; i < n - 1; i++) {
      dx.push(pts[i + 1][0] - pts[i][0]);
      m.push((pts[i + 1][1] - pts[i][1]) / (dx[i] || 1));
    }
    const t = pts.map((_, i) => (i === 0 ? m[0] : i === n - 1 ? m[n - 2] : m[i - 1] * m[i] <= 0 ? 0 : (m[i - 1] + m[i]) / 2));
    for (let i = 0; i < n - 1; i++) {
      if (m[i] === 0) {
        t[i] = 0;
        t[i + 1] = 0;
        continue;
      }
      const a = t[i] / m[i];
      const b = t[i + 1] / m[i];
      const s = a * a + b * b;
      if (s > 9) {
        const k = 3 / Math.sqrt(s);
        t[i] = k * a * m[i];
        t[i + 1] = k * b * m[i];
      }
    }
    let d = `${lead}${pts[0][0].toFixed(1)},${pts[0][1].toFixed(1)}`;
    for (let i = 0; i < n - 1; i++) {
      const c1x = pts[i][0] + dx[i] / 3;
      const c1y = pts[i][1] + (t[i] * dx[i]) / 3;
      const c2x = pts[i + 1][0] - dx[i] / 3;
      const c2y = pts[i + 1][1] - (t[i + 1] * dx[i]) / 3;
      d += ` C${c1x.toFixed(1)},${c1y.toFixed(1)} ${c2x.toFixed(1)},${c2y.toFixed(1)} ${pts[i + 1][0].toFixed(1)},${pts[i + 1][1].toFixed(1)}`;
    }
    return d;
  }

  const line = (key) => curve(points.map((p) => [x(p.hour), y(p[key])]));
  const band = (top, bottom) => {
    if (!points.length) return '';
    const up = points.map((p) => [x(p.hour), y(top(p))]);
    const down = points.map((p) => [x(p.hour), y(bottom(p))]).reverse();
    return `${curve(up)} ${curve(down, 'L')} Z`;
  };

  const selfBand = $derived(band((p) => p.self, () => 0));
  const buyBand = $derived(band((p) => Math.max(p.load, p.self), (p) => p.self));
  const surplusBand = $derived(band((p) => p.pv, (p) => Math.min(p.pv, p.load)));
  const pvLine = $derived(line('pv'));
  const loadLine = $derived(line('load'));
  const ahead = $derived.by(() => {
    if (!forecast.length) return '';
    const last = points.at(-1);
    const pts = [...(last ? [[x(last.hour), y(last.pv)]] : []), ...forecast.map((f) => [x(f.hour), y(f.kw)])];
    return curve(pts);
  });

  // The day in figures, and the hour under the finger.
  const totals = $derived.by(() => {
    const t = { pv: 0, load: 0, self: 0, exp: 0, imp: 0 };
    for (const r of rows) {
      t.pv += r.pv;
      t.load += r.load;
      t.self += r.direct + r.battOut;
      t.exp += r.export;
      t.imp += r.import;
    }
    return t;
  });
  const aheadKwh = $derived(forecast.reduce((t, f) => t + f.kw, 0));

  let hover = $state(null);
  const shown = $derived(hover == null ? null : points.find((p) => p.hour === hover) ?? null);
  function pick(e) {
    const box = e.currentTarget.getBoundingClientRect();
    const h = Math.floor(((e.clientX - box.left) / box.width) * 24);
    hover = Math.max(0, Math.min(23, h));
  }

  // What Moli would have moved onto the sun that day.
  const moved = $derived(
    loads
      .map((l) => {
        const hist = new Array(24).fill(0);
        for (const r of rows) hist[r.hour] += r.moved[l.id] ?? 0;
        return { id: l.id, name: loadName(l), kwh: hist.reduce((t, v) => t + v, 0), window: mainWindow(hist) };
      })
      .filter((m) => m.kwh > 0.05),
  );

  const nowX = $derived(nowHour == null ? null : LEFT + (nowHour / 24) * (W - LEFT * 2));
</script>

<div class="readout" aria-live="polite">
  {#if shown}
    <span class="when">{hours({ from: shown.hour, to: shown.hour + 1 })}</span>
    <span class="fig"><i class="pv"></i>{t('energie.solaire.courbe.panneaux')} <b class="num">{kw(shown.pv)}</b></span>
    <span class="fig"><i class="load"></i>{t('energie.solaire.courbe.maison')} <b class="num">{kw(shown.load)}</b></span>
    <span class="fig"><i class="self"></i>{t('energie.solaire.courbe.soleil')} <b class="num">{kw(shown.self)}</b></span>
    {#if shown.exp > 0.01}<span class="fig"><i class="surplus"></i>{t('energie.solaire.courbe.surplus')} <b class="num">{kw(shown.exp)}</b></span>{/if}
    {#if shown.charge > 0.01}<span class="fig"><i class="self"></i>{t('energie.solaire.courbe.vers_batterie')} <b class="num">{kw(shown.charge)}</b></span>{/if}
    {#if shown.imp > 0.01}<span class="fig"><i class="buy"></i>{t('energie.solaire.courbe.achete')} <b class="num">{kw(shown.imp)}</b></span>{/if}
    {#if shown.moved > 0.01}<span class="chip good">{t('energie.solaire.courbe.lances', { kw: kw(shown.moved) })}</span>{/if}
  {:else}
    <span class="when">{forecast.length ? t('energie.solaire.courbe.jusqua') : t('energie.solaire.courbe.journee')}</span>
    <span class="fig"><i class="pv"></i>{t('energie.solaire.courbe.produit')} <b class="num">{kwhs(totals.pv)}</b></span>
    <span class="fig"><i class="load"></i>{t('energie.solaire.courbe.consomme')} <b class="num">{kwhs(totals.load)}</b></span>
    <span class="fig"><i class="self"></i>{t('energie.solaire.courbe.soleil')} <b class="num">{kwhs(totals.self)}</b>{#if totals.load > 0.05}<small class="muted"> {t('energie.solaire.courbe.besoins', { pct: pct(totals.self / totals.load) })}</small>{/if}</span>
    {#if forecast.length && aheadKwh > 0.05}<span class="muted small">{t('energie.solaire.courbe.attendus', { kwh: kwhs(aheadKwh) })}</span>{/if}
  {/if}
</div>

<div class="plot" bind:clientWidth={W}>
  <svg
    viewBox="0 0 {W} {H}"
    height={H}
    role="img"
    aria-label={t('energie.solaire.courbe.aria', { pv: kwhs(totals.pv), load: kwhs(totals.load), self: kwhs(totals.self) })}
    onpointermove={pick}
    onpointerdown={pick}
    onpointerleave={() => (hover = null)}
  >
    {#each ticks as tick (tick)}
      <line x1="0" x2={W} y1={y(tick)} y2={y(tick)} class="rule" />
    {/each}
    <line x1="0" x2={W} y1={y(0)} y2={y(0)} class="base" />

    <path d={surplusBand} class="surplus" />
    <path d={buyBand} class="buy" />
    <path d={selfBand} class="self" />
    {#if ahead}<path d={ahead} class="line pv ahead" />{/if}
    <path d={pvLine} class="line pv" />
    <path d={loadLine} class="line load" />

    {#if nowX != null}
      <line x1={nowX} x2={nowX} y1={TOP - 6} y2={y(0)} class="now" />
      <text x={nowX > W - 140 ? nowX - 5 : nowX + 5} y={TOP + 4} class="axis halo" text-anchor={nowX > W - 140 ? 'end' : 'start'}>{t('energie.solaire.courbe.maintenant')}</text>
    {/if}

    {#if shown}
      <line x1={x(shown.hour)} x2={x(shown.hour)} y1={TOP} y2={y(0)} class="cross" />
      <circle cx={x(shown.hour)} cy={y(shown.pv)} r="4.5" class="dot pv" />
      <circle cx={x(shown.hour)} cy={y(shown.load)} r="4.5" class="dot load" />
    {/if}

    {#each [0, 6, 12, 18] as h (h)}
      <text x={LEFT + (h / 24) * (W - LEFT * 2)} y={H - 6} class="axis">{t('energie.heure.pile', { h })}</text>
    {/each}
    {#each ticks as tick (tick)}
      <text x={W - 2} y={y(tick) - 5} class="axis halo" text-anchor="end">{kw(tick)}</text>
    {/each}
  </svg>
</div>

<ul class="legend">
  <li><i class="sw line-pv"></i>{t('energie.solaire.courbe.legende_production')}</li>
  <li><i class="sw line-load"></i>{t('energie.solaire.courbe.legende_conso')}</li>
  <li><i class="sw self"></i>{t('energie.solaire.courbe.legende_couverte')}</li>
  <li><i class="sw surplus"></i>{t('energie.solaire.courbe.legende_surplus')}</li>
  <li><i class="sw buy"></i>{t('energie.solaire.courbe.legende_achete')}</li>
  {#if forecast.length}<li><i class="sw line-ahead"></i>{t('energie.solaire.courbe.legende_attendue')}</li>{/if}
</ul>

{#if moved.length}
  <p class="moved">
    <b>{t('energie.solaire.courbe.deplace')}</b>
    {#each moved as m, i (m.id)}{i ? ' · ' : ' '}{m.name} {kwhs(m.kwh)} ({hours(m.window)}){/each}.
  </p>
{/if}

<details class="table">
  <summary>{t('energie.solaire.courbe.chiffres')}</summary>
  <div class="scroll">
    <table>
      <thead>
        <tr><th scope="col">{t('energie.solaire.courbe.heure')}</th><th scope="col">{t('energie.solaire.courbe.panneaux')}</th><th scope="col">{t('energie.solaire.courbe.maison')}</th><th scope="col">{t('energie.solaire.courbe.soleil')}</th><th scope="col">{t('energie.solaire.courbe.surplus')}</th><th scope="col">{t('energie.solaire.courbe.achete')}</th></tr>
      </thead>
      <tbody>
        {#each rows as r (r.start)}
          <tr>
            <th scope="row">{t('energie.heure.pile', { h: r.hour })}</th>
            <td class="num">{kwhs(r.pv)}</td>
            <td class="num">{kwhs(r.load)}</td>
            <td class="num">{kwhs(r.direct + r.battOut)}</td>
            <td class="num">{kwhs(r.export)}</td>
            <td class="num">{kwhs(r.import)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</details>

<style>
  .readout {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 16px;
    min-height: 34px;
    margin-bottom: 8px;
    font-size: 13.5px;
  }

  .when {
    font-weight: 700;
    color: var(--ink-2);
  }

  .fig {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--ink-2);
  }

  .fig b {
    color: var(--ink);
    font-weight: 650;
  }

  .fig i,
  .sw {
    width: 10px;
    height: 10px;
    border-radius: 3px;
    flex: none;
  }

  .fig i.pv {
    background: var(--pv);
  }

  .fig i.load {
    background: var(--ink);
  }

  .fig i.self,
  .sw.self {
    background: color-mix(in srgb, var(--self) 55%, var(--surface));
  }

  .fig i.surplus,
  .sw.surplus {
    background: color-mix(in srgb, var(--pv) 30%, var(--surface));
  }

  .fig i.buy,
  .sw.buy {
    background: color-mix(in srgb, var(--grid) 26%, var(--surface));
  }

  .small {
    font-size: 12.5px;
  }

  .plot {
    width: 100%;
    min-width: 0;
    overflow: hidden;
  }

  svg {
    display: block;
    width: 100%;
    touch-action: pan-y;
    cursor: crosshair;
  }

  .rule {
    stroke: var(--line);
    stroke-width: 1;
  }

  .base {
    stroke: var(--surface-3);
    stroke-width: 1;
  }

  path.self {
    fill: var(--self);
    opacity: 0.38;
  }

  path.surplus {
    fill: var(--pv);
    opacity: 0.16;
  }

  path.buy {
    fill: var(--grid);
    opacity: 0.13;
  }

  .line {
    fill: none;
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }

  .line.pv {
    stroke: var(--pv);
  }

  .line.load {
    stroke: var(--ink);
  }

  .line.ahead {
    stroke-dasharray: 4 5;
    opacity: 0.8;
  }

  .now {
    stroke: var(--ink-3);
    stroke-width: 1;
  }

  .cross {
    stroke: var(--ink-3);
    stroke-width: 1;
  }

  .dot {
    stroke: var(--surface);
    stroke-width: 2;
  }

  .dot.pv {
    fill: var(--pv);
  }

  .dot.load {
    fill: var(--ink);
  }

  .axis {
    font-size: 11px;
    font-weight: 600;
    fill: var(--ink-3);
  }

  .halo {
    paint-order: stroke;
    stroke: var(--surface);
    stroke-width: 4px;
    stroke-linejoin: round;
  }

  .legend {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
    font-size: 12.5px;
    color: var(--ink-2);
  }

  .legend li {
    display: inline-flex;
    align-items: center;
    gap: 7px;
  }

  .sw.line-pv,
  .sw.line-load,
  .sw.line-ahead {
    height: 0;
    width: 16px;
    border-radius: 0;
    border-top: 2px solid var(--pv);
  }

  .sw.line-load {
    border-top-color: var(--ink);
  }

  .sw.line-ahead {
    border-top-style: dashed;
  }

  .moved {
    margin-top: 12px;
    font-size: 13px;
    color: var(--ink-2);
    line-height: 1.45;
  }

  .moved b {
    font-weight: 650;
    color: var(--ink);
  }

  .table {
    margin-top: 12px;
    font-size: 13px;
  }

  .table summary {
    cursor: pointer;
    color: var(--ink-3);
    font-weight: 600;
  }

  .scroll {
    overflow-x: auto;
    margin-top: 8px;
  }

  table {
    border-collapse: collapse;
    width: 100%;
    min-width: 420px;
  }

  th,
  td {
    padding: 4px 8px;
    text-align: right;
    border-bottom: 1px solid var(--line);
    font-variant-numeric: tabular-nums;
  }

  th[scope='row'],
  thead th:first-child {
    text-align: left;
  }

  thead th {
    color: var(--ink-3);
    font-weight: 650;
  }
</style>
