<script>
  import { onMount } from 'svelte';
  import { energy, kwh, euros } from '../lib/energy-live.svelte.js';
  import { deviceSeries, offPeakShare } from '../lib/energy-series.js';
  import { tariffSplit } from '../lib/tariff-split.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** Off-peak against peak, in kWh and in euros side by side: the whole
   *  house, or the sum of the parts picked in the list (one radiator, or
   *  several). Each hour is split as the contract split it (the report's
   *  `by_period`), else by the off-peak windows (`windows`). */
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
  const contract = $derived(!!report?.buckets.some((b) => Object.keys(b.by_period ?? {}).length));

  // Each part's off-peak and peak, kWh and euros, over the span.
  const split = $derived.by(() => {
    const period = s?.[span];
    if (!report || !period) return null;
    const buckets = report.buckets.filter((b) => b.start >= period.from && b.start < period.to);
    return tariffSplit(parts, buckets, (b) => offPeakShare(b.start, windows));
  });

  // What is shown: nothing picked is the whole house.
  let picked = $state([]);
  const pick = (id) => (picked = picked.includes(id) ? picked.filter((x) => x !== id) : [...picked, id]);
  const shown = $derived.by(() => {
    if (!split) return null;
    const rows = picked.length ? split.filter((r) => picked.includes(r.id)) : split;
    const sum = (key) => rows.reduce((t, r) => t + r[key], 0);
    const priced = rows.some((r) => r.priced);
    return { hc: sum('hc'), hp: sum('hp'), hcCost: priced ? sum('hcCost') : null, hpCost: priced ? sum('hpCost') : null };
  });
  const label = $derived(
    !picked.length ? t('energie.tarif.total') : picked.length === 1 ? (parts.find((p) => p.id === picked[0])?.name ?? '') : t('energie.tarif.choisis', { n: picked.length }),
  );

  // The list opens under its button and closes on a tap elsewhere or Escape.
  let open = $state(false);
  let menu;
  const outside = (e) => open && menu && !menu.contains(e.target) && (open = false);

  // ---- the two rings ---------------------------------------------------------------

  const C = 80;
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
  /** A ring of two: off-peak from the top, clockwise, then peak. */
  function ring(hc, hp) {
    const all = hc + hp;
    if (!(all > 0)) return null;
    const end = (hc / all) * Math.PI * 2;
    return {
      all,
      slices: [
        { id: 'hc', value: hc, pct: Math.round((hc / all) * 100), d: arc(52, 74, 0, end) },
        { id: 'hp', value: hp, pct: Math.round((hp / all) * 100), d: arc(52, 74, end, Math.PI * 2) },
      ].filter((x) => x.value > 0.0005),
    };
  }
  const pies = $derived(
    shown
      ? [
          { id: 'kwh', title: t('energie.tarif.en_kwh'), ring: ring(shown.hc, shown.hp), text: (v) => `${kwh(v)} kWh` },
          shown.hcCost != null ? { id: 'eur', title: t('energie.tarif.en_euros'), ring: ring(shown.hcCost, shown.hpCost), text: (v) => euros(v, cur) } : null,
        ].filter(Boolean)
      : [],
  );
  let hover = $state(null);
</script>

<svelte:window onpointerdown={outside} onkeydown={(e) => e.key === 'Escape' && (open = false)} />

<section class="card pie">
  <div class="card-head">
    <h2>{t('energie.tarif.titre')}</h2>
    <div class="seg" role="group" aria-label={t('energie.periode')}>
      {#each SPANS as [key, name] (key)}
        <button class:on={span === key} onclick={() => (span = key)}>{t(name)}</button>
      {/each}
    </div>
  </div>
  {#if !windows.length && !contract}
    <p class="muted">{t('energie.tarif.pas_apprises')}</p>
  {:else if !split}
    <p class="muted">{t('energie.chargement')}</p>
  {:else}
    <div class="pick" bind:this={menu}>
      <button class="select" onclick={() => (open = !open)} aria-expanded={open} aria-haspopup="true">
        <span class="muted">{t('energie.tarif.afficher')}</span>
        <b>{label}</b>
        <svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true"><path d="M2 4l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.6" /></svg>
      </button>
      {#if open}
        <div class="menu" role="group" aria-label={t('energie.tarif.afficher')}>
          <!-- The whole house, or each part ticked in (its own off-peak / peak). -->
          <button class="opt" role="checkbox" aria-checked={!picked.length} onclick={() => ((picked = []), (open = false))}>
            <i class="box" class:on={!picked.length}></i>
            <span>{t('energie.tarif.total')}</span>
            <small class="muted num">{kwh(split.reduce((a, r) => a + r.hc + r.hp, 0))} kWh</small>
          </button>
          {#each split as r (r.id)}
            <button class="opt" role="checkbox" aria-checked={picked.includes(r.id)} onclick={() => pick(r.id)}>
              <i class="box" class:on={picked.includes(r.id)}></i>
              <span>{r.name}{#if r.estimated} <small class="muted">≈</small>{/if}</span>
              <small class="muted num">{kwh(r.hc + r.hp)} kWh</small>
            </button>
          {/each}
        </div>
      {/if}
      {#if picked.length > 1}
        <div class="chips">
          {#each picked as id (id)}
            <button class="chip" onclick={() => pick(id)} aria-label={t('energie.tarif.retirer', { name: parts.find((p) => p.id === id)?.name ?? id })}>
              {parts.find((p) => p.id === id)?.name ?? id} <span aria-hidden="true">×</span>
            </button>
          {/each}
        </div>
      {/if}
    </div>

    {#if !pies[0]?.ring}
      <p class="muted">{t('energie.repartition.vide')}</p>
    {:else}
      <div class="pies">
        {#each pies as pie (pie.id)}
          <figure>
            <figcaption>{pie.title}</figcaption>
            {#if pie.ring}
              {@const focus = pie.ring.slices.find((x) => x.id === hover)}
              <svg viewBox="0 0 160 160" role="img" aria-label="{pie.title} : {pie.ring.slices.map((x) => `${t(`energie.${x.id}`)} ${pie.text(x.value)} (${x.pct} %)`).join(', ')}" onpointerleave={() => (hover = null)}>
                {#each pie.ring.slices as x (x.id)}
                  <path d={x.d} class={x.id} class:dim={hover && hover !== x.id} onpointerenter={() => (hover = x.id)} role="presentation" />
                {/each}
                <text x={C} y={C - 2} class="center" text-anchor="middle">{pie.text(focus ? focus.value : pie.ring.all)}</text>
                <text x={C} y={C + 16} class="center-sub" text-anchor="middle">{focus ? t('energie.pct', { n: focus.pct }) : t('energie.tarif.en_tout')}</text>
              </svg>
              <ul>
                {#each pie.ring.slices as x (x.id)}
                  <li class:on={hover === x.id} onpointerenter={() => (hover = x.id)} onpointerleave={() => (hover = null)}>
                    <i class={x.id}></i>
                    <span>{t(`energie.tarif.court_${x.id}`)}</span>
                    <b class="num">{pie.text(x.value)}</b>
                    <small class="muted num">{t('energie.pct', { n: x.pct })}</small>
                  </li>
                {/each}
              </ul>
            {/if}
          </figure>
        {/each}
      </div>
    {/if}
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

  .pick {
    position: relative;
    display: grid;
    gap: 8px;
    margin-bottom: 14px;
  }

  .select {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    justify-self: start;
    max-width: 100%;
    padding: 9px 14px;
    border: 0;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 14px;
    cursor: pointer;
  }

  .select b {
    font-weight: 650;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .menu {
    position: absolute;
    top: 46px;
    left: 0;
    z-index: 5;
    display: grid;
    width: min(360px, 100%);
    max-height: 60vh;
    overflow-y: auto;
    padding: 6px;
    border-radius: var(--r-md);
    background: var(--surface);
    box-shadow: 0 12px 40px rgb(0 0 0 / 25%);
  }

  .opt {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 10px;
    padding: 10px;
    border: 0;
    border-radius: 8px;
    background: none;
    color: var(--ink);
    font: inherit;
    font-size: 14px;
    text-align: left;
    cursor: pointer;
  }

  .opt:hover {
    background: var(--surface-2);
  }

  .box {
    width: 16px;
    height: 16px;
    border: 2px solid var(--ink-3);
    border-radius: 4px;
  }

  .box.on {
    border-color: var(--cool);
    background: var(--cool) url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 12 12'%3E%3Cpath d='M2.5 6.2l2.3 2.3 4.7-5' fill='none' stroke='white' stroke-width='1.8'/%3E%3C/svg%3E") center / 12px no-repeat;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  button.chip {
    border: 0;
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
  }

  .pies {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 16px;
  }

  figure {
    display: grid;
    justify-items: center;
    gap: 8px;
    margin: 0;
  }

  figcaption {
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-3);
  }

  svg[role='img'] {
    width: 100%;
    max-width: 200px;
    display: block;
  }

  path {
    stroke: var(--surface);
    stroke-width: 2;
    transition: opacity 0.2s;
  }

  path.hc,
  i.hc {
    fill: var(--cool);
    background: var(--cool);
  }

  path.hp,
  i.hp {
    fill: var(--warm);
    background: var(--warm);
  }

  path.dim {
    opacity: 0.35;
  }

  .center {
    font-size: 17px;
    font-weight: 700;
    fill: var(--ink);
  }

  .center-sub {
    font-size: 11px;
    font-weight: 600;
    fill: var(--ink-3);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
    width: 100%;
    max-width: 220px;
  }

  li {
    display: grid;
    grid-template-columns: auto 1fr auto auto;
    align-items: center;
    gap: 8px;
    padding: 2px 4px;
    font-size: 13px;
    border-radius: 6px;
  }

  li.on {
    background: var(--surface-2);
  }

  li i {
    width: 10px;
    height: 10px;
    border-radius: 3px;
  }

  li b {
    font-weight: 650;
  }

  @media (max-width: 420px) {
    .pies {
      gap: 8px;
    }

    li {
      gap: 5px;
      font-size: 12px;
    }
  }
</style>
