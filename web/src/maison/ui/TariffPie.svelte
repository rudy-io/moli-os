<script>
  import { onMount } from 'svelte';
  import { energy, kwh, euros } from '../lib/energy-live.svelte.js';
  import { deviceSeries, offPeakShare } from '../lib/energy-series.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** Off-peak against peak, and inside each who drew what: an inner ring
   *  (the two tariffs), an outer ring (the parts of the house in each), a
   *  legend with the volumes. The tariff of each hour comes from the
   *  off-peak windows learnt off the meter (`windows`). */
  let { windows = [] } = $props();

  const SPANS = [
    ['today', 'energie.span.today'],
    ['yesterday', 'energie.span.yesterday'],
    ['month', 'energie.span.month'],
  ];
  let span = $state('month');
  let report = $state(null);

  async function load() {
    try {
      const r = await fetch('/api/energy/series?step=hour&count=744');
      if (r.ok) report = await r.json();
    } catch {
      /* next round */
    }
  }
  onMount(() => {
    load();
    const timer = setInterval(load, 10 * 60_000);
    return () => clearInterval(timer);
  });

  const s = $derived(energy.summary);
  const cur = $derived(s?.currency ?? '€');
  const parts = $derived(report ? deviceSeries(report.meters) : []);

  const split = $derived.by(() => {
    const period = s?.[span];
    if (!report || !period) return null;
    const acc = parts.map((p) => ({ p, hc: 0, hp: 0, hcCost: 0, hpCost: 0, priced: false }));
    for (const b of report.buckets) {
      if (b.start < period.from || b.start >= period.to) continue;
      const share = offPeakShare(b.start, windows);
      for (const a of acc) {
        const k = a.p.kwh(b);
        const c = a.p.cost(b);
        a.hc += k * share;
        a.hp += k * (1 - share);
        if (c != null) {
          a.priced = true;
          a.hcCost += c * share;
          a.hpCost += c * (1 - share);
        }
      }
    }
    const sum = (key) => acc.reduce((t, a) => t + a[key], 0);
    const priced = acc.some((a) => a.priced);
    return { acc, hc: sum('hc'), hp: sum('hp'), hcCost: priced ? sum('hcCost') : null, hpCost: priced ? sum('hpCost') : null };
  });
  const total = $derived(split ? split.hc + split.hp : 0);

  // ---- the rings ---------------------------------------------------------------------

  const C = 110;
  const arc = (r0, r1, a0, a1) => {
    if (a1 - a0 >= Math.PI * 2 - 1e-6) a1 = a0 + Math.PI * 2 - 1e-4;
    const p = (r, a) => [C + r * Math.sin(a), C - r * Math.cos(a)];
    const large = a1 - a0 > Math.PI ? 1 : 0;
    const [x0, y0] = p(r1, a0);
    const [x1, y1] = p(r1, a1);
    const [x2, y2] = p(r0, a1);
    const [x3, y3] = p(r0, a0);
    return `M${x0} ${y0} A${r1} ${r1} 0 ${large} 1 ${x1} ${y1} L${x2} ${y2} A${r0} ${r0} 0 ${large} 0 ${x3} ${y3} Z`;
  };

  const rings = $derived.by(() => {
    if (!split || total <= 0) return null;
    const turn = Math.PI * 2;
    const hcEnd = (split.hc / total) * turn;
    const inner = [
      { id: 'hc', name: t('energie.hc'), kwh: split.hc, d: arc(54, 74, 0, hcEnd), color: 'var(--cool)' },
      { id: 'hp', name: t('energie.hp'), kwh: split.hp, d: arc(54, 74, hcEnd, turn), color: 'var(--warm)' },
    ].filter((r) => r.kwh > 0.001);
    const outer = [];
    let a = 0;
    for (const tariff of ['hc', 'hp']) {
      for (const x of [...split.acc].sort((p, q) => q[tariff] - p[tariff])) {
        const v = x[tariff];
        if (v <= 0.0005) continue;
        const b = a + (v / total) * turn;
        outer.push({ id: `${tariff}-${x.p.id}`, tariff, name: x.p.name, kwh: v, color: x.p.color, d: arc(78, 104, a, b) });
        a = b;
      }
    }
    return { inner, outer };
  });

  let hover = $state(null);
  const focus = $derived(rings && hover ? [...rings.inner, ...rings.outer].find((r) => r.id === hover) : null);

  const rows = (tariff) =>
    (split?.acc ?? [])
      .map((a) => ({ id: a.p.id, name: a.p.name, color: a.p.color, estimated: a.p.estimated, kwh: a[tariff], cost: a.priced ? a[`${tariff}Cost`] : null }))
      .filter((r) => r.kwh > 0.0005)
      .sort((x, y) => y.kwh - x.kwh);
  const pct = (v) => (total > 0 ? Math.round((v / total) * 100) : 0);
</script>

<section class="card pie">
  <div class="card-head">
    <h2>{t('energie.tarif.titre')}</h2>
    <div class="seg" role="group" aria-label={t('energie.periode')}>
      {#each SPANS as [key, label] (key)}
        <button class:on={span === key} onclick={() => (span = key)}>{t(label)}</button>
      {/each}
    </div>
  </div>
  {#if !windows.length}
    <p class="muted">{t('energie.tarif.pas_apprises')}</p>
  {:else if !rings}
    <p class="muted">{report ? t('energie.repartition.vide') : t('energie.chargement')}</p>
  {:else}
    <div class="body">
      <svg viewBox="0 0 220 220" role="img" aria-label={t('energie.tarif.repartition')} onpointerleave={() => (hover = null)}>
        {#each rings.inner as r (r.id)}
          <path d={r.d} fill={r.color} class:dim={hover && hover !== r.id} onpointerenter={() => (hover = r.id)} role="presentation" />
        {/each}
        {#each rings.outer as r (r.id)}
          <path d={r.d} fill={r.color} class:dim={hover && hover !== r.id} onpointerenter={() => (hover = r.id)} role="presentation" />
        {/each}
        <text x={C} y={C - 6} class="center" text-anchor="middle">{focus ? `${kwh(focus.kwh)} kWh` : `${kwh(total)} kWh`}</text>
        <text x={C} y={C + 14} class="center-sub" text-anchor="middle">{focus ? (focus.tariff ? t(focus.tariff === 'hc' ? 'energie.tarif.focus_hc' : 'energie.tarif.focus_hp', { name: focus.name }) : focus.name) : t('energie.tarif.en_tout')}</text>
      </svg>
      <div class="cols">
        {#each [['hc', 'energie.hc', 'cool', split.hcCost], ['hp', 'energie.hp', 'warm', split.hpCost]] as [tariff, name, tone, cost] (tariff)}
          <div class="col">
            <h3><i class={tone}></i>{t(name)} <span class="muted">{t('energie.pct', { n: pct(split[tariff]) })}</span></h3>
            <p class="sum"><b class="num">{kwh(split[tariff])} kWh</b>{#if cost != null}<span class="muted num">{euros(cost, cur)}</span>{/if}</p>
            <ul>
              {#each rows(tariff) as r (r.id)}
                <li class:on={hover === `${tariff}-${r.id}`}>
                  <i style="background:{r.color}"></i>
                  <span>{r.name}{#if r.estimated} <small class="muted">≈</small>{/if}</span>
                  <b class="num">{kwh(r.kwh)}</b>
                </li>
              {/each}
            </ul>
          </div>
        {/each}
      </div>
    </div>
  {/if}
</section>

<style>
  .seg {
    display: inline-flex;
    padding: 4px;
    background: var(--surface-2);
    border-radius: 999px;
    gap: 2px;
  }

  .seg button {
    border: 0;
    background: none;
    border-radius: 999px;
    padding: 7px 12px;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-3);
    cursor: pointer;
  }

  .seg button.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .body {
    display: grid;
    grid-template-columns: minmax(180px, 240px) minmax(0, 1fr);
    gap: 20px;
    align-items: center;
  }

  @media (max-width: 640px) {
    .body {
      grid-template-columns: minmax(0, 1fr);
      justify-items: center;
    }
  }

  svg {
    width: 100%;
    max-width: 240px;
    display: block;
  }

  path {
    stroke: var(--surface);
    stroke-width: 1.5;
    transition: opacity 0.2s;
  }

  path.dim {
    opacity: 0.35;
  }

  .center {
    font-size: 18px;
    font-weight: 700;
    fill: var(--ink);
  }

  .center-sub {
    font-size: 11px;
    font-weight: 600;
    fill: var(--ink-3);
  }

  .cols {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 16px 24px;
    width: 100%;
  }

  h3 {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
    font-weight: 700;
  }

  h3 i {
    width: 12px;
    height: 12px;
    border-radius: 4px;
  }

  h3 i.cool {
    background: var(--cool);
  }

  h3 i.warm {
    background: var(--warm);
  }

  .sum {
    display: flex;
    gap: 10px;
    align-items: baseline;
    margin: 4px 0 8px;
  }

  .sum b {
    font-size: 18px;
    font-weight: 700;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 5px;
  }

  li {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    border-radius: 6px;
  }

  li.on {
    background: var(--surface-2);
  }

  li i {
    width: 9px;
    height: 9px;
    border-radius: 3px;
  }

  li b {
    font-weight: 650;
  }
</style>
