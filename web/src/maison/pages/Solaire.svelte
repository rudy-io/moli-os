<script>
  import { onMount, tick } from 'svelte';
  import { home, hub, value, note } from '../lib/home.svelte.js';
  import { energy, useEnergy } from '../lib/energy-live.svelte.js';
  import { prices as pricesOf } from '../lib/energy-advice.js';
  import {
    DEFAULTS,
    buildData,
    pvHours,
    simulate,
    summarize,
    byDay,
    lastDays,
    loadEffects,
    proposals,
    flowNow,
    clearSkyPower,
    hourKt,
    ktFromCover,
    skyOf,
  } from '../lib/solar-sim.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import { ico } from '../ui/solar/solar-icons.js';
  import { pct, pts, eur, kwhs, perKwh, dayName, skyName, skyKind, ideaWords } from '../ui/solar/format.js';
  import SolarFlow from '../ui/solar/SolarFlow.svelte';
  import SolarDay from '../ui/solar/SolarDay.svelte';
  import SolarMonth from '../ui/solar/SolarMonth.svelte';
  import SolarAutopilot from '../ui/solar/SolarAutopilot.svelte';
  import SolarIdeas from '../ui/solar/SolarIdeas.svelte';
  import SolarSizes from '../ui/solar/SolarSizes.svelte';
  import SolarSettings from '../ui/solar/SolarSettings.svelte';

  // Solaire (démo): fictitious panels on the house's real consumption.
  // Nothing here touches a device: the switches only change the simulation.

  const HOUR = 3_600_000;
  onMount(useEnergy);

  // ---- the house's last 31 days, hour by hour ------------------------------------

  let report = $state(null);
  let loadedAt = $state(Date.now());
  let error = $state('');
  async function load() {
    try {
      const r = await fetch('/api/energy/series?step=hour&count=744');
      if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error ?? `HTTP ${r.status}`);
      report = await r.json();
      loadedAt = Date.now();
      error = '';
    } catch (e) {
      if (!report) error = e.message;
    }
  }
  onMount(() => {
    load();
    const timer = setInterval(load, 10 * 60_000);
    return () => clearInterval(timer);
  });

  // ---- the hypotheses (kept on this device) --------------------------------------

  const KEY = 'maison-solaire';
  function readSaved() {
    try {
      return JSON.parse(localStorage.getItem(KEY) ?? '{}') ?? {};
    } catch {
      return {};
    }
  }
  const saved = readSaved();
  let kwc = $state(saved.kwc ?? DEFAULTS.kwc);
  let battery = $state(saved.battery ?? DEFAULTS.battery);
  let sellPrice = $state(saved.sellPrice ?? DEFAULTS.sellPrice);
  let batteryCost = $state(saved.batteryCost ?? DEFAULTS.batteryCost);
  let tilt = $state(saved.tilt ?? DEFAULTS.tilt);
  let azimuth = $state(saved.azimuth ?? DEFAULTS.azimuth);
  let losses = $state(saved.losses ?? DEFAULTS.losses);
  let on = $state(saved.on ?? {});
  let shares = $state(saved.shares ?? {});

  $effect(() => {
    const snap = $state.snapshot({ kwc, battery, sellPrice, batteryCost, tilt, azimuth, losses, on, shares });
    try {
      localStorage.setItem(KEY, JSON.stringify(snap));
    } catch {
      /* private mode: kept for this visit */
    }
  });

  function reset() {
    kwc = DEFAULTS.kwc;
    battery = DEFAULTS.battery;
    sellPrice = DEFAULTS.sellPrice;
    batteryCost = DEFAULTS.batteryCost;
    tilt = DEFAULTS.tilt;
    azimuth = DEFAULTS.azimuth;
    losses = DEFAULTS.losses;
    on = {};
    shares = {};
  }

  // ---- the simulation ------------------------------------------------------------

  const s = $derived(energy.summary);
  const weather = $derived(home.config?.outdoor?.weather);
  const cloud = $derived.by(() => {
    const v = weather ? value(weather, 'cloud_cover') : null;
    return typeof v === 'number' ? v : null;
  });
  const priceInfo = $derived(pricesOf(s));

  const data = $derived(report ? buildData(report, { now: loadedAt }) : null);
  const geo = $derived({ lat: DEFAULTS.lat, lon: DEFAULTS.lon, albedo: DEFAULTS.albedo, tilt, azimuth, losses });
  const pv1 = $derived(data ? pvHours(data.hours, { geo, cloud, now: loadedAt }) : []);
  const pilot = $derived(Object.fromEntries((data?.loads ?? []).map((l) => [l.id, on[l.id] ? (shares[l.id] ?? l.flex) : 0])));
  const opts = $derived({ ...geo, kwc, battery, sellPrice, batteryCost, pilot, shares: { ...shares } });

  const rows = $derived(data ? simulate(data, pv1, opts) : []);
  const recent = $derived(lastDays(rows, 30));
  const totals = $derived(summarize(recent.rows, sellPrice));
  const per = $derived(totals.days ? 30 / totals.days : 1);
  const days = $derived(byDay(recent.rows, sellPrice));
  const plain = $derived(data ? summarize(simulate(data, pv1, { ...opts, pilot: {} }).filter((r) => r.start >= recent.from), sellPrice) : null);
  const effects = $derived(data ? loadEffects(data, pv1, opts, recent.from) : {});
  const ideas = $derived(data ? proposals(data, pv1, opts, recent.from) : []);
  const sizes = $derived(
    data ? [3, 6, 9].map((k) => ({ kwc: k, ...summarize(simulate(data, pv1, { ...opts, kwc: k }).filter((r) => r.start >= recent.from), sellPrice) })) : [],
  );
  const anyOn = $derived((data?.loads ?? []).some((l) => on[l.id]));
  const enough = $derived(data && data.hours.length >= 24);

  // ---- right now -----------------------------------------------------------------

  const livePower = (point, fallback) => {
    if (!point) return fallback ?? null;
    const i = point.indexOf('/');
    const v = hub.devices[point.slice(0, i)]?.state?.[point.slice(i + 1)]?.value;
    return typeof v === 'number' ? v : (fallback ?? null);
  };
  const whole = $derived(s?.meters.find((m) => m.role === 'total'));
  const liveW = $derived(whole ? livePower(whole.power, whole.power_w) : null);
  const last = $derived(rows.at(-1) ?? null);
  const lastDur = $derived(last ? (last.end - last.start) / HOUR : 0);
  // What Moli moved into (or out of) this hour, on top of the live draw.
  const shiftedKw = $derived(last && lastDur > 0.05 ? (last.load - last.load0) / lastDur : 0);
  const pvNow = $derived(clearSkyPower(home.now, geo) * (cloud != null ? ktFromCover(cloud) : hourKt(home.now, DEFAULTS.seed)) * kwc);
  const loadNow = $derived(liveW != null ? Math.max(0, liveW / 1000 + shiftedKw) : last && lastDur > 0.05 ? last.load / lastDur : 0);
  const flow = $derived(flowNow(pvNow, loadNow, battery > 0 ? { cap: battery, soc: last?.soc ?? 0, power: battery * DEFAULTS.batteryPower } : null));
  const skyNow = $derived(skyOf(home.now, { cloud, now: home.now }));

  // ---- the day shown -------------------------------------------------------------

  let picked = $state(null);
  let dayCard = $state(null);
  const fallbackDay = $derived.by(() => {
    if (!days.length) return null;
    const lastDay = days.at(-1);
    // Before mid-afternoon, yesterday tells more (a whole day of sun).
    return (lastDay.rows.at(-1)?.hour ?? 0) >= 14 || days.length < 2 ? lastDay.day : days.at(-2).day;
  });
  const dayKey = $derived(days.some((d) => d.day === picked) ? picked : fallbackDay);
  const dayIdx = $derived(days.findIndex((d) => d.day === dayKey));
  const day = $derived(days[dayIdx] ?? null);
  const isToday = $derived(day != null && dayIdx === days.length - 1);
  const daySky = $derived(day ? skyOf(day.rows.find((r) => r.hour === 12)?.start ?? day.start + 12 * HOUR, { cloud, now: loadedAt }) : null);
  const forecast = $derived.by(() => {
    if (!isToday || !day) return [];
    const lastRow = day.rows.at(-1);
    const ahead = [];
    for (let k = 1; lastRow.hour + k <= 23; k++) {
      const start = lastRow.start + k * HOUR;
      ahead.push({ start, end: start + HOUR, hour: lastRow.hour + k });
    }
    const pv = pvHours(ahead, { geo, cloud, now: loadedAt });
    return ahead.map((h, i) => ({ hour: h.hour, kw: pv[i] * kwc }));
  });
  const nowHour = $derived(isToday && day ? day.rows.at(-1).hour + Math.min(1, (home.now - day.rows.at(-1).start) / HOUR) : null);

  async function pickDay(key, scroll = false) {
    picked = key;
    if (scroll) {
      await tick();
      dayCard?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }
  }

  // ---- acting on the simulation (never on a device) ------------------------------

  function toggle(id, next) {
    on = { ...on, [id]: next };
  }
  function setShare(id, share) {
    shares = { ...shares, [id]: share };
  }
  function pilotAll() {
    on = Object.fromEntries((data?.loads ?? []).map((l) => [l.id, true]));
    note(t('energie.solaire.note_pilote_tout'));
  }
  function apply(idea) {
    if (idea.type === 'load') {
      shares = { ...shares, [idea.load]: idea.flex };
      on = { ...on, [idea.load]: true };
      const title = ideaWords(idea).title;
      note(t('energie.solaire.note_idee', { idea: `${title.charAt(0).toLowerCase()}${title.slice(1)}` }));
    } else if (idea.type === 'battery') {
      battery = idea.battery;
      note(t('energie.solaire.note_batterie', { n: idea.battery }));
    }
  }

  const ORIENTATION = {
    90: 'energie.solaire.orient_phrase.90',
    135: 'energie.solaire.orient_phrase.135',
    180: 'energie.solaire.orient_phrase.180',
    225: 'energie.solaire.orient_phrase.225',
    270: 'energie.solaire.orient_phrase.270',
  };
  const deltaChip = (now, before) => (before == null || now == null ? null : (now - before) * 100);
</script>

<div class="solaire">
  <header class="top">
    <a class="back" href="#/energie"><Icon name="arrow-left" size={18} />{t('energie.titre')}</a>
    <div class="title">
      <h1 class="page-title">{t('energie.solaire.titre')}</h1>
      <span class="chip demo"><Icon path={ico('flask')} size={15} />{t('energie.solaire.demo')}</span>
    </div>
    <p class="page-sub">{t('energie.solaire.sous_titre')}</p>
  </header>

  <section class="banner" aria-label={t('energie.solaire.simulation')}>
    <span class="banner-icon"><Icon path={ico('flask')} size={22} /></span>
    <p>
      <b>{t('energie.solaire.banner.simulation')}</b>
      {t('energie.solaire.banner.debut', {
        kwc,
        orientation: ORIENTATION[azimuth] ? t(ORIENTATION[azimuth]) : '',
        battery: battery ? ` ${t('energie.solaire.banner.avec_batterie', { n: battery })}` : '',
      })}
      <b>{t('energie.solaire.banner.vraie')}</b>
      {t('energie.solaire.banner.fin', { days: totals.days ? ` ${t('energie.solaire.banner.sur_jours', { n: totals.days })}` : '' })}
    </p>
  </section>

  {#if error}
    <p class="card muted">{t('energie.solaire.indisponible', { error })}</p>
  {:else if !data}
    <p class="card muted">{t('energie.solaire.chargement')}</p>
  {:else if !enough}
    <p class="card muted">{t('energie.solaire.pas_assez')}</p>
  {:else}
    <div class="hero">
      <section class="card flow-card">
        <div class="card-head">
          <h2><Icon path={ico('solar')} size={18} />{t('energie.solaire.maintenant')}</h2>
          <span class="chip" title={skyNow.real ? t('energie.solaire.ciel_releve') : t('energie.solaire.ciel_tire')}>
            <Icon path={ico(skyNow.kind)} size={15} />{skyNow.real ? t('energie.solaire.nuages', { n: Math.round(skyNow.cover) }) : skyName(skyNow.kind)}
          </span>
        </div>
        <SolarFlow {flow} battery={battery > 0 ? { cap: battery, soc: last?.soc ?? 0 } : null} shifted={Math.max(0, shiftedKw)} />
        <p class="muted small center">
          {flow.pv < 0.02 ? t(battery > 0 ? 'energie.solaire.dorment_batterie' : 'energie.solaire.dorment') : t('energie.solaire.mesure')}
        </p>
      </section>

      <div class="kpis">
        <section class="card kpi">
          <span class="eyebrow">{t('energie.solaire.kpi.autoconsommation')}</span>
          <strong class="big">{pct(totals.autoconsumption)}</strong>
          <span class="meter" aria-hidden="true"><i class="self" style:width="{(totals.autoconsumption ?? 0) * 100}%"></i></span>
          <span class="sub">{t('energie.solaire.kpi.autoconsommation_sous')}</span>
          {#if anyOn && deltaChip(totals.autoconsumption, plain?.autoconsumption) > 0.5}
            <span class="chip good">{t('energie.solaire.kpi.grace', { pts: pts(deltaChip(totals.autoconsumption, plain?.autoconsumption)) })}</span>
          {/if}
        </section>
        <section class="card kpi">
          <span class="eyebrow">{t('energie.solaire.kpi.autoproduction')}</span>
          <strong class="big">{pct(totals.autoproduction)}</strong>
          <span class="meter" aria-hidden="true"><i class="pv" style:width="{(totals.autoproduction ?? 0) * 100}%"></i></span>
          <span class="sub">{t('energie.solaire.kpi.autoproduction_sous')}</span>
          {#if anyOn && deltaChip(totals.autoproduction, plain?.autoproduction) > 0.5}
            <span class="chip good">{t('energie.solaire.kpi.grace', { pts: pts(deltaChip(totals.autoproduction, plain?.autoproduction)) })}</span>
          {/if}
        </section>
        <section class="card kpi">
          <span class="eyebrow">{t('energie.solaire.kpi.economies')}</span>
          <strong class="big">{eur(totals.savings * per)}</strong>
          <span class="sub">{t('energie.solaire.kpi.economies_sous')}</span>
          <span class="muted tiny">{t('energie.solaire.kpi.sur_place', { kwh: kwhs(totals.selfUsed * per) })}</span>
        </section>
        <section class="card kpi">
          <span class="eyebrow">{t('energie.solaire.kpi.surplus')}</span>
          <strong class="big">{eur(totals.revenue * per)}</strong>
          <span class="sub">{t('energie.solaire.kpi.surplus_sous', { kwh: kwhs(totals.export * per), price: perKwh(sellPrice) })}</span>
          <span class="muted tiny">{t('energie.solaire.kpi.rachat')}</span>
        </section>
      </div>
    </div>

    <section class="card gain-line">
      <span class="gain-icon"><Icon path={ico('piggy')} size={22} /></span>
      <p>
        {t('energie.solaire.gain.total')} <b>{t('energie.solaire.gain.par_mois', { eur: eur(totals.gain * per) })}</b> {t('energie.solaire.gain.rythme', { days: totals.days, kwh: kwhs(totals.pv * per) })}
        {#if anyOn && plain}{t('energie.solaire.gain.pilotage')} <b>{eur((totals.gain - plain.gain) * per)}</b>.{:else if ideas.some((i) => i.type === 'load')}{t('energie.solaire.gain.mieux')}{/if}
      </p>
    </section>

    {#if day}
      <section class="card" bind:this={dayCard}>
        <div class="card-head day-head">
          <h2><Icon path={ico('curve')} size={18} />{t('energie.solaire.jour.titre')}</h2>
          <div class="pager">
            <button class="more" onclick={() => pickDay(days[dayIdx - 1]?.day)} disabled={dayIdx <= 0} aria-label={t('energie.solaire.jour.precedent')}><Icon name="chevron-left" size={20} /></button>
            <span class="day-name">{isToday ? t('energie.span.today') : dayName(day.start, data.timezone)}</span>
            <button class="more" onclick={() => pickDay(days[dayIdx + 1]?.day)} disabled={dayIdx >= days.length - 1} aria-label={t('energie.solaire.jour.suivant')}><Icon name="chevron-right" size={20} /></button>
          </div>
          {#if daySky}
            <span class="chip" title={daySky.real ? t('energie.solaire.ciel_releve') : t('energie.solaire.ciel_tire')}>
              <Icon path={ico(daySky.kind)} size={15} />{daySky.real ? t('energie.solaire.jour.meteo', { n: Math.round(daySky.cover) }) : t('energie.solaire.jour.ciel_simule', { kind: skyKind(daySky.kind) })}
            </span>
          {/if}
        </div>
        <SolarDay rows={day.rows} {forecast} {nowHour} loads={data.loads} />
      </section>
    {/if}

    <section class="card">
      <div class="card-head">
        <h2><Icon path={ico('month')} size={18} />{t('energie.solaire.mois.titre', { n: days.length })}</h2>
        <span class="muted small">{t('energie.solaire.mois.aide')}</span>
      </div>
      <SolarMonth {days} selected={dayKey} timezone={data.timezone} onselect={(key) => pickDay(key, true)} />
    </section>

    <section class="card">
      <div class="card-head">
        <h2><Icon path={ico('robot')} size={18} />{t('energie.solaire.auto.titre')}</h2>
        {#if data.loads.length && !data.loads.every((l) => on[l.id])}
          <button class="all" onclick={pilotAll}>{t('energie.solaire.auto.tout')}</button>
        {/if}
      </div>
      <p class="intro">
        {t('energie.solaire.auto.intro', { price: perKwh(sellPrice) })} <span class="muted">{t('energie.solaire.auto.intro_simulation')}</span>
      </p>
      <SolarAutopilot loads={data.loads} {on} {shares} {effects} {plain} {totals} ontoggle={toggle} onshare={setShare} />
    </section>

    <section class="card">
      <div class="card-head">
        <h2><Icon path={ico('idea')} size={18} />{t('energie.solaire.idees.titre')}</h2>
        <span class="muted small">{t('energie.solaire.idees.aide', { n: totals.days })}</span>
      </div>
      <SolarIdeas {ideas} piloted={anyOn} onapply={apply} />
    </section>

    <section class="card">
      <div class="card-head">
        <h2><Icon path={ico('solar')} size={18} />{t('energie.solaire.tailles.titre')}</h2>
        <span class="muted small">{t(battery ? 'energie.solaire.tailles.meme_batterie' : 'energie.solaire.tailles.meme')}</span>
      </div>
      <SolarSizes {sizes} current={kwc} onpick={(k) => (kwc = k)} />
      <p class="foot muted">{t('energie.solaire.tailles.note', { price: perKwh(sellPrice) })}</p>
    </section>

    <SolarSettings
      bind:kwc
      bind:battery
      bind:sellPrice
      bind:batteryCost
      bind:tilt
      bind:azimuth
      bind:losses
      prices={priceInfo}
      priced={data.priced}
      buyPrice={DEFAULTS.buyPrice}
      {cloud}
      lat={DEFAULTS.lat}
      lon={DEFAULTS.lon}
      onreset={reset}
    />

    <p class="disclaimer muted">
      {t('energie.solaire.avertissement')}
    </p>
  {/if}
</div>

<style>
  /* The demo's own colours, on top of the dashboard's tokens: amber for the
     panels, green for what the sun covers, blue for the grid. */
  .solaire {
    --pv: #c98b0c;
    --pv-ink: #7a4f00;
    --self: var(--good);
    --grid: var(--cool);
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  :global(.maison[data-theme='night']) .solaire {
    --pv: #d4981f;
    --pv-ink: #f3c46b;
  }

  .top {
    display: grid;
    gap: 4px;
  }

  .back {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    width: fit-content;
    margin-bottom: 6px;
    padding: 6px 12px 6px 8px;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow);
    color: var(--ink-2);
    font-size: 13.5px;
    font-weight: 650;
    text-decoration: none;
  }

  .title {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .chip.demo {
    background: color-mix(in srgb, var(--pv) 16%, var(--surface));
    color: var(--pv-ink);
    font-weight: 700;
  }

  .banner {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 14px;
    align-items: start;
    padding: 16px 20px;
    border-radius: var(--r-lg);
    background: linear-gradient(120deg, color-mix(in srgb, var(--sun) 20%, var(--surface)), var(--surface) 70%);
    box-shadow: var(--shadow), inset 0 0 0 1px color-mix(in srgb, var(--pv) 30%, transparent);
    font-size: 14.5px;
    line-height: 1.5;
    color: var(--ink-2);
  }

  .banner b {
    color: var(--ink);
  }

  .banner-icon {
    width: 40px;
    height: 40px;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--pv-ink);
  }

  .hero {
    display: grid;
    grid-template-columns: minmax(0, 1.05fr) minmax(0, 1fr);
    gap: 20px;
  }

  .flow-card {
    display: grid;
    align-content: start;
    gap: 6px;
  }

  .center {
    text-align: center;
  }

  .small {
    font-size: 13px;
  }

  .kpis {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 20px;
  }

  .kpi {
    display: grid;
    align-content: start;
    gap: 8px;
  }

  .eyebrow {
    font-size: 12px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-3);
    font-weight: 700;
  }

  .big {
    font-size: 46px;
    font-weight: 300;
    letter-spacing: -0.03em;
    line-height: 1;
    margin-top: 2px;
  }

  .meter {
    height: 8px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }

  .meter i {
    display: block;
    height: 100%;
    border-radius: 999px;
    transition: width 0.6s var(--ease);
  }

  .meter i.self {
    background: var(--self);
  }

  .meter i.pv {
    background: var(--pv);
  }

  .sub {
    font-size: 13.5px;
    color: var(--ink-2);
    line-height: 1.35;
  }

  .tiny {
    font-size: 12px;
  }

  .kpi .chip {
    justify-self: start;
  }

  .gain-line {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 16px 20px;
    font-size: 15px;
    line-height: 1.45;
    color: var(--ink-2);
  }

  .gain-line b {
    color: var(--ink);
  }

  .gain-icon {
    width: 40px;
    height: 40px;
    flex: none;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: var(--good-soft);
    color: var(--self);
  }

  .day-head {
    flex-wrap: wrap;
  }

  .pager {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    margin-left: auto;
  }

  .pager button:disabled {
    opacity: 0.35;
  }

  .day-name {
    min-width: 150px;
    text-align: center;
    font-weight: 650;
    font-size: 14.5px;
  }

  .intro {
    font-size: 14px;
    line-height: 1.5;
    color: var(--ink-2);
    margin: -4px 0 16px;
    max-width: 75ch;
  }

  .all {
    border: 0;
    border-radius: 999px;
    padding: 8px 14px;
    font-weight: 700;
    font-size: 13px;
    background: var(--self);
    color: #fff;
  }

  .foot {
    font-size: 13px;
    margin-top: 12px;
  }

  .disclaimer {
    font-size: 12.5px;
    text-align: center;
    max-width: 70ch;
    margin: 0 auto;
    line-height: 1.5;
  }

  @media (max-width: 1000px) {
    .hero {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  @media (max-width: 560px) {
    .solaire,
    .hero,
    .kpis {
      gap: 14px;
    }

    .big {
      font-size: 34px;
    }

    .kpi {
      padding: 16px;
    }

    .eyebrow {
      font-size: 11px;
      letter-spacing: 0.06em;
    }

    .banner {
      padding: 14px 16px;
      font-size: 13.5px;
    }

    .banner-icon {
      display: none;
    }

    .banner {
      grid-template-columns: minmax(0, 1fr);
    }

    .card-head {
      flex-wrap: wrap;
      gap: 10px;
    }

    .pager {
      margin-left: 0;
    }

    .day-name {
      min-width: 0;
    }
  }
</style>
