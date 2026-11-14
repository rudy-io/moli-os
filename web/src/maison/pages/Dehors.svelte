<script>
  import { home, value, num, isOn, reachable, device, since, nameOf, relative } from '../lib/home.svelte.js';
  import { OUTSIDE, idsOf, pick, originOf, measures, compass } from '../lib/outside.js';
  import { weatherOf } from '../lib/icons.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import LightTile from '../ui/LightTile.svelte';
  import CameraTile from '../ui/CameraTile.svelte';

  const out = $derived(home.config?.outdoor ?? {});
  const w = $derived(out.weather);
  const air = $derived(out.air);
  const pool = $derived(out.pool);
  const v = (id, key) => value(id, key);

  // « Dehors » (#210): with `[outdoor]`, Moli builds `dehors:meteo`, fed by the
  // stations' measures while they are fresh, else by the forecast. Without it,
  // the page reads the forecast as before.
  const fused = $derived(device(OUTSIDE) != null);
  const stations = $derived(idsOf(v(OUTSIDE, 'stations')));
  const forecasts = $derived(idsOf(v(OUTSIDE, 'forecast')));
  /** The outside device's value for a meaning, else the forecast's own point. */
  const val = (key, dev, devKey = key) => (fused && v(OUTSIDE, key) != null ? v(OUTSIDE, key) : v(dev, devKey));
  let open = $state(null);

  /** Where a value comes from, how old it is, and whether the fallback plays:
   *  a station has the meaning but the forecast gave it. */
  function origin(key) {
    const source = v(OUTSIDE, `${key}_source`);
    const kind = originOf(source, stations);
    let quiet = null;
    if (kind === 'forecast') {
      const semantic = device(OUTSIDE)?.points.find((p) => p.key === key)?.semantic;
      for (const id of stations) {
        const p = pick(device(id)?.points, semantic, key);
        if (!p) continue;
        const ts = since(id, p.key);
        quiet = ts ? t('maison.dehors.station_muette', { age: relative(ts, home.now) }) : t('maison.dehors.station_sans_mesure');
        break;
      }
    }
    return { kind, name: source ? nameOf(source) : '', age: relative(since(OUTSIDE, key), home.now), quiet };
  }

  const windFrom = $derived(compass(val('wind_direction', w)));
  const show = (id, p) => {
    const x = v(id, p.key);
    if (x == null) return '—';
    if (p.kind?.type === 'binary') return x ? t('commun.oui') : t('commun.non');
    const n = Number(x);
    return `${num(n, Number.isInteger(n) ? 0 : 1)}${p.unit ? ` ${p.unit}` : ''}`;
  };

  const day = $derived(v(w, 'daylight') !== false);
  const sky = $derived(weatherOf(v(w, 'weather_code'), day));
  const hhmm = (iso) => (iso ? String(iso).slice(11, 16) : '—');

  const aqi = $derived(v(air, 'aqi'));
  const aqiWord = $derived(
    aqi == null
      ? '—'
      : aqi <= 20
        ? t('maison.air.tres_bon')
        : aqi <= 40
          ? t('maison.air.bon')
          : aqi <= 60
            ? t('maison.air.moyen')
            : aqi <= 80
              ? t('maison.air.mediocre')
              : t('maison.air.mauvais'),
  );
  const aqiTone = $derived(aqi == null ? '' : aqi <= 40 ? 'good' : aqi <= 60 ? 'warm' : 'alert');
  const uv = $derived(val('uv_index', air));
  const uvWord = $derived(
    uv == null ? '' : uv < 3 ? t('maison.dehors.uv_faible') : uv < 6 ? t('maison.dehors.uv_modere') : uv < 8 ? t('maison.dehors.uv_eleve') : t('maison.dehors.uv_tres_eleve'),
  );

  // Pool chemistry: in range or not, in words.
  const ph = $derived(v(pool, 'ph'));
  const orp = $derived(v(pool, 'orp'));
  const phWord = $derived(
    ph == null ? null : ph < 7.0 ? t('maison.dehors.ph_acide') : ph > 7.6 ? t('maison.dehors.ph_basique') : t('maison.dehors.ph_equilibre'),
  );
  const orpWord = $derived(
    orp == null ? null : orp < 650 ? t('maison.dehors.orp_faible') : orp > 800 ? t('maison.dehors.orp_forte') : t('maison.dehors.orp_saine'),
  );
  const pumpId = $derived(out.pool_pump);
  const pumpKnown = $derived(pumpId && reachable(pumpId) && v(pumpId, 'switch_1') != null);

  const outsideCams = $derived((home.config?.favorites?.cameras ?? []).filter((c) => !/sonnette|doorbell/i.test(c.name)));

  const sunPos = $derived.by(() => {
    const rise = Date.parse(v(w, 'sunrise'));
    const set = Date.parse(v(w, 'sunset'));
    if (!Number.isFinite(rise) || !Number.isFinite(set)) return null;
    return Math.min(1, Math.max(0, (home.now - rise) / (set - rise)));
  });
</script>

{#snippet badge(key)}
  {#if fused && v(OUTSIDE, key) != null}
    {@const o = origin(key)}
    {#if o.kind}
      <button type="button" class="src {o.kind}" aria-expanded={open === key} onclick={() => (open = open === key ? null : key)}>
        {o.kind === 'measured' ? t('maison.dehors.mesure') : o.quiet ? t('maison.dehors.prevu_repli') : t('maison.dehors.prevu')}
      </button>
      {#if open === key}
        <em class="muted why">{o.name} · {o.age}{#if o.quiet} · {o.quiet}{/if}</em>
      {/if}
    {/if}
  {/if}
{/snippet}

<div class="dehors">
  <header>
    <h1 class="page-title">{t('maison.dehors.titre')}</h1>
    <p class="page-sub">{t('maison.dehors.sous_titre')}</p>
  </header>

  <section class="card sky" class:night={!day}>
    <div class="now">
      <svg class="sky-icon" viewBox="0 0 24 24" aria-hidden="true"><path d={sky.icon} /></svg>
      <div>
        <strong class="temp num">{num(val('temperature', w), 0)}°</strong>
        <span class="label">{sky.label} {@render badge('temperature')}</span>
        <span class="muted">{t('maison.dehors.ressenti', { feels: num(v(w, 'feels_like'), 0), max: num(v(w, 'today_max'), 0), min: num(v(w, 'today_min'), 0) })}</span>
      </div>
    </div>
    <div class="facts">
      <div><Icon name="humidity" size={20} /><small>{t('maison.dehors.humidite')} {@render badge('humidity')}</small><b class="num">{num(val('humidity', w))} %</b></div>
      <div>
        <Icon name="wind" size={20} /><small>{t('maison.dehors.vent')} {@render badge('wind_speed')}</small><b class="num">{num(val('wind_speed', w))} km/h{#if windFrom != null}<span class="muted cap"> {t('maison.dehors.cap').split(',')[windFrom]}</span>{/if}</b>
        <em class="muted num">{t('maison.dehors.rafales', { speed: num(val('wind_gust', w, 'wind_gusts')) })}</em>
      </div>
      <div><Icon name="water" size={20} /><small>{t('maison.dehors.pluie_jour')} {@render badge('rain_today')}</small><b class="num">{num(val('rain_today', w, 'today_rain'), 1)} mm</b><em class="muted num">{t('maison.dehors.risque', { pct: num(v(w, 'today_rain_chance')) })}</em></div>
      <div><Icon name="sun-angle" size={20} /><small>{t('maison.dehors.soleil')}</small><b class="num">{hhmm(v(w, 'sunrise'))} → {hhmm(v(w, 'sunset'))}</b></div>
    </div>
    {#if sunPos != null}
      <div class="sunline" aria-hidden="true"><i style="left:{sunPos * 100}%"></i></div>
    {/if}
  </section>

  <div class="grid">
    <section class="card">
      <div class="card-head"><h2><Icon name="leaf" size={18} />{t('maison.dehors.air')}</h2><span class="chip {aqiTone}">{aqiWord}</span></div>
      <div class="pairs">
        <div><small>{t('maison.dehors.indice')}</small><b class="num">{num(aqi)}</b></div>
        <div><small>UV {@render badge('uv_index')}</small><b class="num">{num(uv, 0)}</b><em class="muted">{uvWord}</em></div>
        <div><small>{t('maison.dehors.particules')}</small><b class="num">{num(v(air, 'pm2_5'), 1)} µg/m³</b></div>
        <div><small>{t('maison.dehors.ozone')}</small><b class="num">{num(v(air, 'ozone'))} µg/m³</b></div>
      </div>
    </section>

    {#if stations.length}
      <section class="card">
        <div class="card-head"><h2><Icon name="thermometer" size={18} />{t('maison.dehors.donnees_reelles')}</h2></div>
        {#each stations as id (id)}
          {@const d = device(id)}
          <div class="station">
            <div class="station-head">
              <b>{nameOf(id)}</b>
              {#if d && !reachable(id)}<span class="chip alert">{t('maison.dehors.injoignable')}</span>{/if}
            </div>
            {#if d}
              <div class="pairs">
                {#each measures(d) as p (p.key)}
                  <div><small>{p.label}</small><b class="num">{show(id, p)}</b><em class="muted">{relative(since(id, p.key), home.now)}</em></div>
                {/each}
              </div>
            {:else}
              <p class="muted small">{t('maison.dehors.station_inconnue', { id })}</p>
            {/if}
          </div>
        {/each}
      </section>
    {/if}

    {#if pool}
      <section class="card">
        <div class="card-head">
          <h2><Icon name="pool" size={18} />{t('maison.dehors.piscine')}</h2>
          {#if v(pool, 'action_required') === true}<span class="chip cool">{t('maison.dehors.coup_oeil')}</span>{:else}<span class="chip good">{t('maison.dehors.tout_va_bien')}</span>{/if}
        </div>
        <div class="pairs">
          <div><small>{t('maison.dehors.eau')}</small><b class="num">{v(pool, 'temperature') != null ? `${num(v(pool, 'temperature'), 1)}°` : '—'}</b></div>
          <div><small>pH</small><b class="num">{num(ph, 1)}</b>{#if phWord}<em class="muted">{phWord}</em>{/if}</div>
          <div><small>{t('maison.dehors.chlore')}</small><b class="num">{orp != null ? `${num(orp)} mV` : '—'}</b>{#if orpWord}<em class="muted">{orpWord}</em>{/if}</div>
          <div><small>{t('maison.dehors.filtration')}</small><b class="num">{v(pool, 'filtration') != null ? t('maison.dehors.heures_jour', { hours: num(v(pool, 'filtration')) }) : '—'}</b></div>
        </div>
        {#if v(pool, 'temperature') == null && ph == null}
          <p class="muted small">{t('maison.dehors.sans_mesure')}</p>
        {/if}
        {#if pumpId}
          <div class="pump">
            {#if pumpKnown}
              <LightTile id={pumpId} name={t('maison.dehors.pompe')} icon="pool" compact />
            {:else}
              <p class="muted small"><Icon name="info" size={15} />{t('maison.dehors.pompe_inconnue')}</p>
            {/if}
          </div>
        {/if}
      </section>
    {/if}

    {#if out.garden_lights?.length}
      <section class="card">
        <div class="card-head"><h2><Icon name="flower-outline" size={18} />{t('maison.dehors.jardin')}</h2>
          <span class="muted">{out.garden_lights.filter((l) => isOn(l.id)).length ? t('maison.dehors.eclaire') : t('maison.dehors.noir')}</span>
        </div>
        <div class="lights">
          {#each out.garden_lights as l (l.id)}
            <LightTile id={l.id} name={l.name} icon="light" compact />
          {/each}
        </div>
      </section>
    {/if}
  </div>

  {#if outsideCams.length}
    <section class="cams">
      {#each outsideCams as cam (cam.id)}
        <CameraTile id={cam.id} name={cam.name} refresh={cam.refresh} battery={cam.battery} onopen={() => (location.hash = '#/cameras')} />
      {/each}
    </section>
  {/if}

  {#if w || air || forecasts.length || stations.length}
    <!-- CC BY 4.0: the weather and air data credit their source. -->
    <p class="credit">
      {#if stations.length}
        {#if w || air || forecasts.length}{t('maison.dehors.credit_previsions')} <a href="https://open-meteo.com/" target="_blank" rel="noopener">Open-Meteo.com</a> · {/if}{t('maison.dehors.credit_mesures', { names: stations.map((id) => nameOf(id)).join(', ') })}
      {:else}
        {t('maison.dehors.credit')} <a href="https://open-meteo.com/" target="_blank" rel="noopener">Open-Meteo.com</a>
      {/if}
    </p>
  {/if}
</div>

<style>
  .credit {
    color: var(--ink-3);
    font-size: 12px;
    text-align: center;
  }

  .credit a {
    color: inherit;
  }

  .dehors {
    display: grid;
    gap: 20px;
  }

  .sky {
    padding: 28px;
    overflow: hidden;
    background:
      radial-gradient(120% 140% at 100% 0%, color-mix(in srgb, var(--sun) 22%, transparent), transparent 55%),
      var(--surface);
  }

  .sky.night {
    background:
      radial-gradient(120% 140% at 100% 0%, color-mix(in srgb, var(--cool) 22%, transparent), transparent 55%),
      var(--surface);
  }

  .now {
    display: flex;
    align-items: center;
    gap: 22px;
  }

  .sky-icon {
    width: 96px;
    height: 96px;
    fill: var(--sun);
    flex: none;
  }

  .night .sky-icon {
    fill: var(--cool);
  }

  .now > div {
    display: grid;
    gap: 2px;
  }

  .temp {
    font-size: 64px;
    font-weight: 250;
    letter-spacing: -0.04em;
    line-height: 0.95;
  }

  .label {
    font-size: 20px;
    font-weight: 650;
  }

  .facts {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 16px;
    margin-top: 26px;
  }

  .facts > div,
  .pairs > div {
    display: grid;
    gap: 2px;
    align-content: start;
  }

  .facts :global(svg) {
    color: var(--ink-3);
    margin-bottom: 4px;
  }

  small {
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-3);
  }

  b {
    font-size: 17px;
    font-weight: 650;
  }

  em {
    font-style: normal;
    font-size: 13px;
  }

  .sunline {
    position: relative;
    height: 4px;
    border-radius: 4px;
    margin-top: 22px;
    background: linear-gradient(90deg, var(--surface-3), var(--sun), var(--surface-3));
  }

  .sunline i {
    position: absolute;
    top: 50%;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--sun);
    box-shadow: 0 0 0 5px color-mix(in srgb, var(--sun) 30%, transparent);
    transform: translate(-50%, -50%);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: 20px;
    align-items: start;
  }

  .pairs {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 18px 16px;
  }

  .small {
    font-size: 13px;
    margin-top: 12px;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .pump {
    margin-top: 16px;
  }

  /* Where a value comes from: seen without hovering (tablets have none). */
  .src {
    display: inline-flex;
    align-items: center;
    margin-left: 6px;
    padding: 1px 7px;
    border: 0;
    border-radius: 999px;
    font: inherit;
    font-size: 11px;
    font-weight: 650;
    vertical-align: middle;
    cursor: pointer;
  }

  .src.measured {
    color: var(--good);
    background: color-mix(in srgb, var(--good) 14%, transparent);
  }

  .src.forecast {
    color: var(--ink-3);
    background: var(--surface-3);
  }

  .why {
    display: block;
    font-size: 12px;
  }

  .cap {
    font-size: 13px;
    font-weight: 500;
  }

  .station + .station {
    margin-top: 16px;
  }

  .station-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
  }

  .lights {
    display: grid;
    gap: 10px;
  }

  .cams {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 380px), 1fr));
    gap: 20px;
  }

  @media (max-width: 760px) {
    .facts {
      grid-template-columns: 1fr 1fr;
    }

    .sky-icon {
      width: 64px;
      height: 64px;
    }

    .temp {
      font-size: 52px;
    }
  }
</style>
