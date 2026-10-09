<script>
  import { hub } from '../lib/hub.svelte.js';
  import { kwh, money, watts, livePoint, meterColor } from '../lib/energy.js';
  import { t } from '../lib/i18n.svelte.js';

  let { onopen } = $props();
  let summary = $state(null);
  let missing = $state(false);

  async function load() {
    try {
      const res = await fetch('/api/energy');
      if (res.status === 404) {
        missing = true;
        return;
      }
      if (res.ok) summary = await res.json();
    } catch {
      /* next tick */
    }
  }

  $effect(() => {
    load();
    const timer = setInterval(load, 30_000);
    return () => clearInterval(timer);
  });

  const total = $derived(summary?.meters.find((m) => m.role === 'total' && m.power_w != null));
  const power = $derived(total ? (livePoint(hub.devices, total.power) ?? total.power_w) : null);
  const circuits = $derived(
    summary
      ? summary.meters
          .map((m, i) => ({ ...m, color: meterColor(m, i), today: summary.today.meters[m.id] }))
          .filter((m) => m.role === 'circuit' && (m.today?.kwh ?? 0) > 0.0005)
      : [],
  );
  const whole = $derived(summary?.meters.find((m) => m.role === 'total'));
  const wholeToday = $derived(whole ? summary.today.meters[whole.id]?.kwh : null);
  const isHc = $derived(/hc/i.test(summary?.period ?? ''));

  const pct = (v) => (wholeToday ? Math.max(0, Math.min(100, (100 * v) / wholeToday)) : 0);
</script>

{#if summary && !missing}
  <section class="energy" aria-label={t('systeme.energie.bandeau.aria')}>
    <button class="open" onclick={onopen} title={t('systeme.energie.bandeau.titre')}>
      <div class="live">
        <span class="label">{t('systeme.energie.bandeau.maintenant')}</span>
        <span class="big num">{watts(power)}</span>
        {#if summary.period}
          <span class="tag" class:hc={isHc}>{isHc ? t('systeme.energie.bandeau.creuses') : t('systeme.energie.bandeau.pleines')}{#if summary.price_now} · {money(summary.price_now, summary.currency)}/kWh{/if}</span>
        {/if}
      </div>
      <div>
        <span class="label">{t('systeme.energie.bandeau.aujourdhui')}</span>
        <span class="mid num">{kwh(summary.today.total.kwh)}</span>
        <span class="cost num">{money(summary.today.total.cost, summary.currency)}</span>
      </div>
      <div>
        <span class="label">{t('systeme.energie.bandeau.hier')}</span>
        <span class="mid num">{kwh(summary.yesterday.total.kwh)}</span>
        <span class="cost num">{money(summary.yesterday.total.cost, summary.currency)}</span>
      </div>
      <div>
        <span class="label">{t('systeme.energie.bandeau.ce_mois')}</span>
        <span class="mid num">{kwh(summary.month.total.kwh)}</span>
        <span class="cost num">
          {money(summary.month.total.cost, summary.currency)}
          {#if summary.month_projection?.cost != null}<span class="proj">→ ~{money(summary.month_projection.cost, summary.currency)}</span>{/if}
        </span>
      </div>
    </button>

    {#if circuits.length && wholeToday}
      <div class="split" title={t('systeme.energie.bandeau.repartition')}>
        {#each circuits as c (c.id)}
          <span style="width:{pct(c.today.kwh)}%; background:{c.color}" title="{c.name} · {kwh(c.today.kwh)}"></span>
        {/each}
      </div>
      <ul class="legend">
        {#each circuits as c (c.id)}
          <li><i style="background:{c.color}"></i>{c.name} <b class="num">{kwh(c.today.kwh)}</b></li>
        {/each}
        {#if summary.today.unmeasured}
          <li><i class="rest"></i>{t('systeme.energie.autres')} <b class="num">{kwh(summary.today.unmeasured.kwh)}</b></li>
        {/if}
      </ul>
    {/if}
  </section>
{/if}

<style>
  .energy {
    background: var(--card);
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    padding: 12px 16px;
    margin-bottom: 22px;
  }
  .open {
    all: unset;
    cursor: pointer;
    display: grid;
    grid-template-columns: 1.3fr repeat(3, 1fr);
    gap: 18px;
    width: 100%;
    align-items: end;
  }
  .open:focus-visible {
    outline: 2px solid var(--sun);
    outline-offset: 4px;
  }
  .open > div {
    display: grid;
    gap: 2px;
  }
  .label {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-3);
  }
  .big {
    font-size: 34px;
    font-weight: 800;
    letter-spacing: -0.03em;
    line-height: 1;
  }
  .mid {
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .cost {
    font-size: 13px;
    color: var(--ink-2);
  }
  .proj {
    color: var(--ink-3);
  }
  .tag {
    justify-self: start;
    font-size: 12px;
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--sun-soft);
  }
  .tag.hc {
    background: var(--line);
  }
  .split {
    display: flex;
    height: 8px;
    margin-top: 14px;
    border-radius: 999px;
    overflow: hidden;
    background: var(--line);
  }
  .split span {
    display: block;
    height: 100%;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 16px;
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    font-size: 13px;
    color: var(--ink-2);
  }
  .legend i {
    display: inline-block;
    width: 9px;
    height: 9px;
    border-radius: 2px;
    margin-right: 6px;
  }
  .legend i.rest {
    background: var(--line);
  }
  .legend b {
    font-weight: 600;
    color: var(--ink);
  }
  @media (max-width: 640px) {
    .open {
      grid-template-columns: 1fr 1fr;
    }
  }
</style>
