<script>
  import { onMount } from 'svelte';
  import { home, hub, value, isOn, nameOf, deviceKind, doorOpen, act } from '../lib/home.svelte.js';
  import { energy, useEnergy, euros, kwh, watts, power, liveLook, billOf } from '../lib/energy-live.svelte.js';
  import { offPeak, nextChange, clockText, minutesText, windowText, baseload, typicalDay, todayByHour, prices, advise } from '../lib/energy-advice.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import EnergyChart from '../ui/EnergyChart.svelte';
  import TariffPie from '../ui/TariffPie.svelte';
  import DayProfile from '../ui/DayProfile.svelte';

  onMount(useEnergy);

  const s = $derived(energy.summary);

  // The monthly budget: a helper (« Budget élec mensuel ») set from here.
  const budgetAt = $derived.by(() => {
    const point = s?.budget_point;
    if (!point) return null;
    const i = point.indexOf('/');
    return { point, device: point.slice(0, i), key: point.slice(i + 1) };
  });
  const budget = $derived(budgetAt ? value(budgetAt.device, budgetAt.key) : null);
  const budgetKind = $derived(hub.devices[budgetAt?.device]?.points.find((p) => p.key === budgetAt?.key)?.kind ?? {});
  const bill = $derived(billOf(s));
  function stepBudget(dir) {
    if (typeof budget !== 'number') return;
    const step = budgetKind.step ?? 10;
    const next = Math.min(budgetKind.max ?? Infinity, Math.max(budgetKind.min ?? 0, budget + dir * step));
    if (next !== budget) act(budgetAt.point, next, t('energie.budget.libelle'));
  }
  const cur = $derived(s?.currency ?? '€');

  const livePower = (point, fallback) => {
    if (!point) return fallback ?? null;
    const i = point.indexOf('/');
    const v = hub.devices[point.slice(0, i)]?.state?.[point.slice(i + 1)]?.value;
    return typeof v === 'number' ? v : (fallback ?? null);
  };

  const total = $derived(s?.meters.find((m) => m.role === 'total'));
  const now = $derived(total ? livePower(total.power, total.power_w) : null);
  const meterNow = $derived(s?.live ? livePower(s.live.point, s.live.value) : null);
  const look = $derived(liveLook(meterNow, s?.live?.warn));

  // Appliances measured at their plug: shown inside their circuit, or
  // inside what no circuit explains (never added on top).
  const appliances = $derived((s?.meters ?? []).filter((m) => m.role === 'appliance'));
  // The smart lights (estimated) outside any measured circuit: a line of
  // their own, taken out of « le reste » (so the lines add up to the total).
  const lights = $derived(appliances.filter((a) => a.estimated && !a.within));
  const inside = (row) =>
    row === '__lights' ? lights : appliances.filter((a) => (a.within ?? '__rest') === row && !(a.estimated && !a.within));

  // Who draws power right now: each circuit, then what none explains.
  const draws = $derived.by(() => {
    if (!s) return [];
    const list = s.meters
      .filter((m) => m.role === 'circuit' && m.power)
      .map((m) => ({ id: m.id, name: m.name, w: Math.max(0, livePower(m.power, m.power_w) ?? 0) }));
    const lit = lights.reduce((t, a) => t + Math.max(0, livePower(a.power, a.power_w) ?? 0), 0);
    if (lights.length) list.push({ id: '__lights', name: t('energie.lumieres'), w: lit, estimated: true });
    if (now != null) {
      const rest = Math.max(0, now - list.reduce((t, c) => t + c.w, 0));
      list.push({ id: '__rest', name: t('energie.reste'), w: rest, rest: true });
    }
    return list
      .filter((c) => c.w >= 1)
      .sort((a, b) => b.w - a.w)
      .map((c) => ({
        ...c,
        subs: inside(c.id)
          .map((a) => ({ id: a.id, name: a.name, estimated: a.estimated, w: Math.max(0, livePower(a.power, a.power_w) ?? 0) }))
          .filter((a) => a.w >= 1)
          .sort((a, b) => b.w - a.w),
      }));
  });
  const drawMax = $derived(Math.max(1, ...draws.map((d) => d.w)));

  const isHc = $derived(/hc/i.test(s?.period ?? ''));
  const vsYesterday = $derived.by(() => {
    const t = s?.today.total.kwh;
    const y = s?.yesterday.total.kwh;
    if (t == null || !y) return null;
    // Same hour yesterday is unknown here: compare to the share of the day gone by.
    const dayFrac = Math.min(1, Math.max(0.05, (home.now - s.today.from) / (s.today.to - s.today.from)));
    return Math.round(((t / dayFrac - y) / y) * 100);
  });

  const monthDelta = $derived.by(() => {
    const p = s?.month_projection?.kwh;
    const l = s?.last_month.total.kwh;
    if (p == null || !l) return null;
    return Math.round(((p - l) / l) * 100);
  });

  // Where it went, circuit by circuit: today, yesterday or this month.
  let span = $state('today');
  const SPANS = [
    ['today', 'energie.span.today'],
    ['yesterday', 'energie.span.yesterday'],
    ['month', 'energie.span.month'],
  ];
  const parts = $derived.by(() => {
    const period = s?.[span];
    if (!period) return [];
    const list = s.meters
      .filter((m) => m.role === 'circuit')
      .map((m) => ({ id: m.id, name: m.name, ...(period.meters[m.id] ?? { kwh: 0 }) }));
    const of = (a) => period.meters[a.id] ?? { kwh: 0 };
    const lit = lights.reduce((t, a) => t + (of(a).kwh ?? 0), 0);
    const litCost = lights.some((a) => of(a).cost != null) ? lights.reduce((t, a) => t + (of(a).cost ?? 0), 0) : null;
    if (lights.length) list.push({ id: '__lights', name: t('energie.lumieres'), kwh: lit, cost: litCost, estimated: true });
    if (period.unmeasured) {
      const r = period.unmeasured;
      list.push({
        id: '__rest',
        name: t('energie.reste'),
        kwh: Math.max(0, r.kwh - lit),
        cost: r.cost != null ? Math.max(0, r.cost - (litCost ?? 0)) : null,
        rest: true,
      });
    }
    return list
      .filter((p) => p.kwh > 0.01)
      .sort((a, b) => b.kwh - a.kwh)
      .map((p) => ({
        ...p,
        subs: inside(p.id)
          .map((a) => ({ id: a.id, name: a.name, estimated: a.estimated, ...(period.meters[a.id] ?? { kwh: 0 }) }))
          .filter((a) => a.kwh > 0.005)
          .sort((a, b) => b.kwh - a.kwh),
      }));
  });
  const partsMax = $derived(Math.max(0.01, ...parts.map((p) => p.kwh)));
  const partsTotal = $derived(parts.reduce((t, p) => t + p.kwh, 0));
  // This month, the share billed off-peak.
  const offShare = $derived.by(() => {
    const grid = (s?.meters ?? []).filter((m) => m.role === 'grid');
    const of = (re) => grid.filter((m) => re.test(m.id + m.name)).reduce((t, m) => t + (s.month.meters[m.id]?.kwh ?? 0), 0);
    const off = of(/hc|creuse/i);
    const all = grid.reduce((t, m) => t + (s.month.meters[m.id]?.kwh ?? 0), 0);
    return all > 1 ? Math.round((off / all) * 100) : null;
  });

  // Learned from the last week: off-peak hours, typical day, night floor.
  let tariff = $state(null);
  let week = $state(null);
  async function learn() {
    try {
      const point = energy.summary?.period_point;
      const [t, w] = await Promise.all([
        point ? fetch(`/api/history?point=${encodeURIComponent(point)}&hours=168&points=2000`).then((r) => (r.ok ? r.json() : null)) : null,
        fetch('/api/energy/series?step=hour&count=168').then((r) => (r.ok ? r.json() : null)),
      ]);
      tariff = t;
      week = w;
    } catch {
      /* next round */
    }
  }
  onMount(() => {
    // Every 10 minutes: today's hours fill in as they go.
    const timer = setInterval(learn, 10 * 60_000);
    return () => clearInterval(timer);
  });
  let asked = false;
  $effect(() => {
    if (s && !asked) {
      asked = true;
      learn();
    }
  });

  const windows = $derived(offPeak(tariff, home.now));
  const change = $derived(nextChange(windows, home.now));
  const profile = $derived(typicalDay(week));
  const todayHours = $derived(todayByHour(week, home.now));
  const base = $derived(baseload(week));
  const price = $derived(prices(s));

  const climates = $derived(
    Object.values(hub.devices)
      .filter((d) => deviceKind(d) === 'climate')
      .map((d) => ({
        name: nameOf(d.id),
        on: value(d.id, 'on') === true,
        mode: value(d.id, 'mode'),
        temp: value(d.id, 'temperature'),
        target: value(d.id, 'target_temperature'),
        outdoor: value(d.id, 'outdoor_temperature'),
      })),
  );
  const openDoors = $derived(
    (home.config?.doors ?? [])
      .filter((d) => doorOpen(d.id) === true)
      .map((d) => d.name),
  );
  const poolId = $derived(home.config?.outdoor?.pool_pump);
  const tips = $derived(
    s
      ? advise({
          off: s.period ? isHc : (change?.off ?? null),
          change,
          live: meterNow,
          warn: s.live?.warn,
          draws,
          climates,
          openDoors,
          pool: poolId ? { on: isOn(poolId) } : null,
          price,
          base,
          avgPrice: s.month.total.kwh ? (s.month.total.cost ?? 0) / s.month.total.kwh : null,
        })
      : [],
  );

  let range = $state('hour');
  let by = $state('devices');
  const RANGES = [
    ['hour', 'energie.range.hour'],
    ['day', 'energie.range.day'],
    ['month', 'energie.range.month'],
  ];
</script>

<div class="energie">
  <header class="head">
    <div>
      <h1 class="page-title">{t('energie.titre')}</h1>
      <p class="page-sub">{t('energie.sous_titre')}</p>
    </div>
    <a class="demo" href="#/energie/solaire"><Icon name="sun" size={16} />{t('energie.demo_solaire')}</a>
  </header>

  {#if !s}
    <p class="muted">{energy.missing ? t('energie.aucun_compteur') : t('energie.chargement')}</p>
  {:else}
    <div class="heads">
      <section class="card now" class:hued={look.hue != null} style:--h={look.hue}>
        <span class="eyebrow">{t('energie.now.titre')}</span>
        {#if s.live}
          <strong class="big num">{power(meterNow, s.live.unit)}</strong>
          <div class="meter" title={t('energie.abonnement', { power: power(s.live.max, s.live.unit) })}>
            <i style:width="{Math.min(100, ((meterNow ?? 0) / s.live.max) * 100)}%"></i>
            <b style:left="{(s.live.warn / s.live.max) * 100}%"></b>
          </div>
          <span class="muted small">{t(now != null ? 'energie.now.detail_fine' : 'energie.now.detail', { level: look.word, plan: power(s.live.max, s.live.unit), fine: watts(now) })}</span>
        {:else}
          <strong class="big num">{watts(now)}</strong>
        {/if}
        <div class="row">
          {#if s.period}<span class="chip {isHc ? 'cool' : 'warm'}">{isHc ? t('energie.hc') : t('energie.hp')}</span>{/if}
          {#if change}<span class="muted small">{t(change.off ? 'energie.now.pleines' : 'energie.now.creuses', { at: clockText(change.at), delay: minutesText(change.minutes) })}</span>{/if}
        </div>
      </section>

      <section class="card">
        <span class="eyebrow">{t('energie.span.today')}</span>
        <strong class="big num">{kwh(s.today.total.kwh)} <small>kWh</small></strong>
        <div class="row">
          <b class="num">{euros(s.today.total.cost, cur)}</b>
          {#if vsYesterday != null}
            <span class="chip {vsYesterday > 10 ? 'warm' : vsYesterday < -10 ? 'good' : ''}">
              {t('energie.vs_hier', { n: `${vsYesterday > 0 ? '+' : ''}${vsYesterday}` })}
            </span>
          {/if}
        </div>
      </section>

      <section class="card">
        <span class="eyebrow">{t('energie.mois.titre')}</span>
        <strong class="big num">{euros(s.month.total.cost, cur)}</strong>
        <div class="row">
          {#if s.month_projection}
            <span class="muted num small">
              {#if s.monthly_fee != null && s.month_projection.cost != null}
                {t('energie.mois.facture', { bill: euros(s.month_projection.cost + s.monthly_fee, cur) })}
              {:else}
                {t('energie.mois.fin', { cost: euros(s.month_projection.cost, cur) })}
              {/if}
            </span>
          {/if}
          {#if monthDelta != null}<span class="chip {monthDelta > 5 ? 'warm' : monthDelta < -5 ? 'good' : ''}">{t('energie.mois.vs', { n: `${monthDelta > 0 ? '+' : ''}${monthDelta}` })}</span>{/if}
          {#if offShare != null}<span class="muted small">{t('energie.mois.creuses', { n: offShare })}</span>{/if}
        </div>
        {#if typeof budget === 'number'}
          <div class="budget" class:over={bill != null && bill > budget}>
            <div class="budget-line">
              <span class="small">{t('energie.budget.titre')}</span>
              <span class="stepper">
                <button onclick={() => stepBudget(-1)} aria-label={t('energie.budget.baisser')}>−</button>
                <b class="num">{Math.round(budget)} {cur}</b>
                <button onclick={() => stepBudget(1)} aria-label={t('energie.budget.monter')}>+</button>
              </span>
            </div>
            {#if bill != null}
              <div class="bar" aria-hidden="true"><span style:width="{Math.min(100, (bill / budget) * 100)}%"></span></div>
              <span class="muted small num">
                {bill > budget ? t('energie.budget.au_dessus', { n: Math.round(bill - budget), cur }) : t('energie.budget.marge', { n: Math.round(budget - bill), cur })}
              </span>
            {/if}
          </div>
        {/if}
      </section>

    </div>

    {#if tips.length}
      <section class="card tips">
        <div class="card-head"><h2><Icon name="sparkles" size={18} />{t('energie.conseils.titre')}</h2></div>
        <ul>
          {#each tips as tip (tip.title)}
            <li class="tip {tip.tone}">
              <span class="tip-icon"><Icon name={tip.icon} size={18} /></span>
              <div>
                <b>{tip.title}</b>
                <p>{tip.text}</p>
              </div>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <div class="split">
      <section class="card">
        <div class="card-head"><h2><Icon name="flash" size={18} />{t('energie.direct.titre')}</h2></div>
        {#if draws.length}
          <ul class="bars">
            {#each draws as d (d.id)}
              <li class:rest={d.rest}>
                <span class="name">{d.name}{#if d.estimated} <small class="muted" title={t('energie.estime_ampoules')}>{t('energie.estime')}</small>{/if}</span>
                <span class="track"><i style="width:{(d.w / drawMax) * 100}%"></i></span>
                <b class="num">{watts(d.w)}</b>
              </li>
              {#each d.subs as a (a.id)}
                <li class="sub">
                  <span class="name">{t('energie.dont', { name: a.name })}{#if a.estimated} <small class="muted" title={t('energie.estime_ampoule')}>{t('energie.estime')}</small>{/if}</span>
                  <span class="track"><i style="width:{(a.w / drawMax) * 100}%"></i></span>
                  <b class="num">{watts(a.w)}</b>
                </li>
              {/each}
            {/each}
          </ul>
        {:else}
          <p class="muted">{t('energie.direct.vide')}</p>
        {/if}
      </section>

      <section class="card">
        <div class="card-head">
          <h2><Icon name="bolt" size={18} />{t('energie.repartition.titre')}</h2>
          <div class="seg" role="group" aria-label={t('energie.periode')}>
            {#each SPANS as [key, label] (key)}
              <button class:on={span === key} onclick={() => (span = key)}>{t(label)}</button>
            {/each}
          </div>
        </div>
        {#if parts.length}
          <ul class="bars">
            {#each parts as p (p.id)}
              <li class:rest={p.rest}>
                <span class="name">{p.name} <small class="muted">{t('energie.pct', { n: Math.round((p.kwh / partsTotal) * 100) })}{p.estimated ? ' ≈' : ''}</small></span>
                <span class="track"><i style="width:{(p.kwh / partsMax) * 100}%"></i></span>
                <b class="num">{kwh(p.kwh)} kWh</b>
                {#if p.cost != null}<small class="muted num cost">{euros(p.cost, cur)}</small>{/if}
              </li>
              {#each p.subs as a (a.id)}
                <li class="sub">
                  <span class="name">{t('energie.dont', { name: a.name })}{#if a.estimated} <small class="muted" title={t('energie.estime_ampoule')}>{t('energie.estime')}</small>{/if}</span>
                  <span class="track"><i style="width:{(a.kwh / partsMax) * 100}%"></i></span>
                  <b class="num">{kwh(a.kwh)} kWh</b>
                  {#if a.cost != null}<small class="muted num cost">{euros(a.cost, cur)}</small>{/if}
                </li>
              {/each}
            {/each}
          </ul>
        {:else}
          <p class="muted">{t('energie.repartition.vide')}</p>
        {/if}
      </section>
    </div>

    <TariffPie {windows} />

    {#if profile}
      <section class="card">
        <div class="card-head"><h2><Icon name="clock" size={18} />{t('energie.journee.titre')}</h2></div>
        <DayProfile {profile} {windows} {base} today={todayHours} now={home.now} />
        {#if windows.length}
          <p class="foot muted">{t('energie.journee.creuses', { windows: windows.map(windowText).join(t('energie.et')) })}</p>
        {/if}
      </section>
    {/if}

    <section class="card">
      <div class="card-head">
        <h2><Icon name="tune" size={18} />{t('energie.histo.titre')}</h2>
        <div class="seg" role="group" aria-label={t('energie.histo.empile')}>
          <button class:on={by === 'devices'} onclick={() => (by = 'devices')}>{t('energie.histo.appareils')}</button>
          <button class:on={by === 'tariff'} onclick={() => (by = 'tariff')}>{t('energie.histo.tarif')}</button>
        </div>
        <div class="seg" role="group" aria-label={t('energie.periode')}>
          {#each RANGES as [key, label] (key)}
            <button class:on={range === key} onclick={() => (range = key)}>{t(label)}</button>
          {/each}
        </div>
      </div>
      <EnergyChart {range} {by} />
      <p class="foot muted">
        {by === 'devices' ? t('energie.histo.note_appareils') : t('energie.histo.note_tarif')}
        {t('energie.histo.avant_moli')}
      </p>
    </section>
  {/if}
</div>

<style>
  .energie {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    justify-content: space-between;
    gap: 10px 16px;
  }

  /* The way to the solar demo: discreet, out of the real figures' way. */
  .demo {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 7px 14px 7px 11px;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow);
    color: var(--ink-2);
    font-size: 13px;
    font-weight: 650;
    text-decoration: none;
  }

  .heads {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 220px), 1fr));
    gap: 20px;
  }

  .eyebrow {
    display: block;
    font-size: 12px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-3);
    font-weight: 700;
  }

  .big {
    display: block;
    font-size: 40px;
    font-weight: 300;
    letter-spacing: -0.03em;
    margin: 8px 0 10px;
    line-height: 1;
  }

  .big small {
    font-size: 18px;
    font-weight: 500;
    color: var(--ink-3);
  }

  .now .big {
    font-weight: 600;
  }

  .now.hued .big {
    color: hsl(var(--h) 70% 38%);
  }

  :global(.maison[data-theme='night']) .now.hued .big {
    color: hsl(var(--h) 70% 62%);
  }

  /* The draw against the contract, with a tick where it gets « soutenue ». */
  .meter {
    position: relative;
    height: 8px;
    border-radius: 999px;
    background: var(--surface-2);
    margin-bottom: 8px;
  }

  .meter i {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--good);
    transition: width 0.6s var(--ease);
  }

  .now.hued .meter i {
    background: hsl(var(--h) 72% 46%);
  }

  .meter b {
    position: absolute;
    top: -3px;
    bottom: -3px;
    width: 2px;
    background: var(--ink-3);
    border-radius: 2px;
  }

  .small {
    font-size: 13px;
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
    font-size: 14px;
    margin-top: 10px;
  }

  .budget {
    display: grid;
    gap: 8px;
    margin-top: 16px;
    padding-top: 14px;
    border-top: 1px solid var(--line);
  }

  .budget-line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 6px 12px;
    font-weight: 650;
  }

  .stepper {
    display: inline-flex;
    align-items: center;
    gap: 10px;
  }

  .stepper button {
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 18px;
    font-weight: 650;
    cursor: pointer;
  }

  .bar {
    height: 8px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }

  .bar span {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: var(--good);
  }

  .budget.over .bar span {
    background: var(--alert);
  }

  .budget.over .small.num {
    color: var(--alert);
  }


  .tips ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 300px), 1fr));
    gap: 12px;
  }

  .tip {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 12px;
    padding: 14px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .tip-icon {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-2);
  }

  .tip.good .tip-icon {
    background: var(--good-soft);
    color: var(--good);
  }

  .tip.warm .tip-icon {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .tip.alert .tip-icon {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .tip.info .tip-icon {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .tip b {
    font-size: 14px;
    font-weight: 700;
  }

  .tip p {
    margin: 4px 0 0;
    font-size: 13px;
    line-height: 1.45;
    color: var(--ink-2);
  }

  .split {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 20px;
  }

  .bars {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 14px;
  }

  .bars li {
    display: grid;
    grid-template-columns: minmax(120px, 1fr) 2fr auto;
    align-items: center;
    gap: 4px 14px;
  }

  .bars .cost {
    grid-column: 3;
    justify-self: end;
    font-size: 12px;
    margin-top: -4px;
  }

  .name {
    font-weight: 600;
    font-size: 14px;
  }

  .name small {
    font-weight: 600;
    font-size: 12px;
  }

  .track {
    height: 10px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }

  .track i {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--warm);
    transition: width 0.6s var(--ease);
  }

  .rest .track i {
    background: var(--ink-3);
  }

  .rest .name {
    color: var(--ink-3);
  }

  /* An appliance inside the row above: indented, finer, cool. */
  .bars li.sub {
    margin-top: -6px;
  }

  .sub .name {
    padding-left: 14px;
    font-weight: 550;
    font-size: 13px;
    color: var(--ink-2);
  }

  .sub .track {
    height: 6px;
  }

  .sub .track i {
    background: var(--cool);
  }

  .sub b {
    font-size: 13px;
    font-weight: 600;
  }

  .bars b {
    font-weight: 650;
    font-size: 14px;
    min-width: 72px;
    text-align: right;
  }

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

  .foot {
    font-size: 13px;
    margin-top: 14px;
  }

  @media (max-width: 900px) {
    .split {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  @media (max-width: 560px) {
    .energie,
    .heads,
    .split {
      gap: 14px;
    }

    /* Two by two: the figures stay readable without a long scroll. */
    .heads {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }

    .heads .now {
      grid-column: 1 / -1;
    }

    .big {
      font-size: 28px;
    }

    .big small {
      font-size: 14px;
    }

    /* Narrow cards: a chip wraps instead of spilling out. */
    .heads .chip {
      white-space: normal;
      font-size: 12px;
      line-height: 1.3;
    }

    .card-head {
      flex-wrap: wrap;
      gap: 10px;
    }

    .seg button {
      padding: 6px 10px;
      font-size: 12px;
    }

    .bars li {
      grid-template-columns: minmax(0, 1fr) auto;
    }

    .bars .track {
      grid-column: 1 / -1;
      grid-row: 2;
    }

    .bars .cost {
      grid-column: 2;
      grid-row: 1;
      display: none;
    }
  }
</style>
