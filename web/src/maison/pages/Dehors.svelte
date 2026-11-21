<script>
  import { home, value, num, isOn, reachable, device, since, nameOf, relative, roomList } from '../lib/home.svelte.js';
  import { OUTSIDE, SUN, idsOf, measures, compass, readOutside, facadesOf, dayFraction, onArc } from '../lib/outside.js';
  import { outdoorView } from '../lib/outdoor.js';
  import { weatherOf } from '../lib/icons.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import LightTile from '../ui/LightTile.svelte';
  import CameraTile from '../ui/CameraTile.svelte';
  import RoomCard from '../ui/RoomCard.svelte';
  import SourceBadge from '../ui/SourceBadge.svelte';

  const out = $derived(home.config?.outdoor ?? {});
  const w = $derived(out.weather);
  const air = $derived(out.air);
  const outsideCams = $derived((home.config?.favorites?.cameras ?? []).filter((c) => !/sonnette|doorbell/i.test(c.name)));
  // The rooms outside, as `/api/home` says (#217): what they hold shows here too.
  const view = $derived(
    outdoorView({
      rooms: roomList(),
      layoutRooms: home.config?.rooms,
      pool: out.pool ?? null,
      pump: out.pool_pump ?? null,
      garden: out.garden_lights ?? [],
      cams: outsideCams,
      on: isOn,
      // A station shows on « Données réelles » (#210): not again in its room.
      shown: idsOf(value(OUTSIDE, 'stations')),
    }),
  );
  const pool = $derived(view.pool);
  const v = (id, key) => value(id, key);

  // « Dehors » (#210): with `[outdoor]`, Moli builds `dehors:meteo`, fed by the
  // stations' measures while they are fresh, else by the forecast. Without it,
  // the page reads the forecast as before.
  const stations = $derived(idsOf(v(OUTSIDE, 'stations')));
  const forecasts = $derived(idsOf(v(OUTSIDE, 'forecast')));
  /** The outside device's value for a meaning, else the forecast's own point. */
  const val = (key, dev, devKey = key) => readOutside(value, key, dev, devKey);

  const windFrom = $derived(compass(val('wind_direction', w)));
  /** A station's point by its meaning, in the house's language (Zigbee2MQTT
   *  labels are English); its own label otherwise. */
  const measureName = (p) =>
    ({
      temperature: t('maison.dehors.sens_temperature'),
      humidity: t('maison.dehors.humidite'),
      pressure: t('maison.dehors.sens_pression'),
      illuminance: t('maison.dehors.sens_luminosite'),
      battery: t('maison.dehors.sens_batterie'),
      wind_speed: t('maison.dehors.vent'),
      wind_gust: t('maison.dehors.sens_rafales'),
      wind_direction: t('maison.dehors.sens_direction'),
      rain_rate: t('maison.dehors.sens_intensite'),
      rain_today: t('maison.dehors.pluie_jour'),
      uv_index: 'UV',
    })[p.semantic] ?? p.label;

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

  // The sun (#258): where it is in the sky, computed in the house
  // (`dehors:soleil`), and the façades it is on.
  const elevation = $derived(v(SUN, 'elevation'));
  const sunFrom = $derived(compass(v(SUN, 'azimuth')));
  const facades = $derived(facadesOf(v(SUN, 'facades')));
  const hasDay = $derived(Number.isFinite(Date.parse(v(w, 'sunrise'))) && Number.isFinite(Date.parse(v(w, 'sunset'))));
  // On the arc between sunrise and sunset; none at night.
  const ARC = { w: 1000, h: 46 };
  const sunAt = $derived.by(() => {
    const f = dayFraction(v(w, 'sunrise'), v(w, 'sunset'), home.now);
    return f == null ? null : onArc(f, ARC.w, ARC.h);
  });
</script>

<div class="dehors">
  <header>
    <h1 class="page-title">{t('maison.dehors.titre')}</h1>
    <p class="page-sub">{t('maison.dehors.sous_titre')}</p>
  </header>

  <section class="card sky" class:night={!day}>
    <div class="now">
      <svg class="sky-icon" viewBox="0 0 24 24" aria-hidden="true"><path d={sky.icon} /></svg>
      <div>
        <!-- The badge sits by the temperature it qualifies, not by the sky (a forecast). -->
        <span class="temp-line"><strong class="temp num">{num(val('temperature', w), 0)}°</strong><SourceBadge key="temperature" /></span>
        <span class="label">{sky.label}</span>
        <span class="muted">{t('maison.dehors.ressenti', { feels: num(v(w, 'feels_like'), 0), max: num(v(w, 'today_max'), 0), min: num(v(w, 'today_min'), 0) })}</span>
        <!-- What the stations measured since midnight (#246): never mixed with the forecast above. -->
        {#if v(OUTSIDE, 'measured_max') != null && v(OUTSIDE, 'measured_min') != null}
          <span class="muted">{t('maison.dehors.mesure_jour', { max: num(v(OUTSIDE, 'measured_max'), 1), min: num(v(OUTSIDE, 'measured_min'), 1) })}</span>
        {/if}
      </div>
    </div>
    <div class="facts">
      <div><Icon name="humidity" size={20} /><small>{t('maison.dehors.humidite')} <SourceBadge key="humidity" /></small><b class="num">{num(val('humidity', w))} %</b></div>
      <div>
        <Icon name="wind" size={20} /><small>{t('maison.dehors.vent')} <SourceBadge key="wind_speed" /></small><b class="num">{num(val('wind_speed', w))} km/h</b>
        <em class="muted num">{t('maison.dehors.rafales', { speed: num(val('wind_gust', w, 'wind_gusts')) })}{#if windFrom != null}{' · '}{t('maison.dehors.cap').split(',')[windFrom]}{/if}</em>
      </div>
      <div><Icon name="water" size={20} /><small>{t('maison.dehors.pluie_jour')} <SourceBadge key="rain_today" /></small><b class="num">{num(val('rain_today', w, 'today_rain'), 1)} mm</b><em class="muted num">{t('maison.dehors.risque', { pct: num(v(w, 'today_rain_chance')) })}</em></div>
      <div>
        <Icon name="sun-angle" size={20} /><small>{t('maison.dehors.soleil')}</small><b class="num">{hhmm(v(w, 'sunrise'))} → {hhmm(v(w, 'sunset'))}</b>
        {#if elevation != null}
          <em class="muted num">{t('maison.dehors.soleil_position', { elevation: num(elevation, 0) })}{#if sunFrom != null}{' · '}{t('maison.dehors.cap').split(',')[sunFrom]}{/if}</em>
        {/if}
      </div>
    </div>
    {#if hasDay}
      <!-- The sun's arc, sunrise on the left, sunset on the right (#258). -->
      <div class="sunarc">
        <svg viewBox="-12 -12 {ARC.w + 24} {ARC.h + 16}" preserveAspectRatio="none" aria-hidden="true">
          <path d="M0 {ARC.h} Q{ARC.w / 2} {-ARC.h} {ARC.w} {ARC.h}" vector-effect="non-scaling-stroke" />
          <line x1="-12" y1={ARC.h} x2={ARC.w + 12} y2={ARC.h} vector-effect="non-scaling-stroke" />
        </svg>
        {#if sunAt}
          <i style="left:{((sunAt.x + 12) / (ARC.w + 24)) * 100}%;top:{((sunAt.y + 12) / (ARC.h + 16)) * 100}%"></i>
        {/if}
      </div>
    {/if}
    {#if facades.length}
      <div class="facades">
        <small>{t('maison.dehors.soleil_sur')}</small>
        {#each facades as f (f.id)}
          {@const on = v(SUN, `${f.id}_sun`) === true}
          {@const inc = v(SUN, `${f.id}_incidence`)}
          <span class="chip" class:warm={on} title={inc != null ? t('maison.dehors.incidence', { deg: num(inc, 0) }) : undefined}>
            {#if on}<Icon name="sun-angle" size={14} />{/if}{f.name}
          </span>
        {/each}
      </div>
    {/if}
  </section>

  <div class="grid">
    <section class="card">
      <div class="card-head"><h2><Icon name="leaf" size={18} />{t('maison.dehors.air')}</h2><span class="chip {aqiTone}">{aqiWord}</span></div>
      <div class="pairs">
        <div><small>{t('maison.dehors.indice')}</small><b class="num">{num(aqi)}</b></div>
        <div><small>UV <SourceBadge key="uv_index" /></small><b class="num">{num(uv, 0)}</b><em class="muted">{uvWord}</em></div>
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
                  <div><small>{measureName(p)}</small><b class="num">{show(id, p)}</b><em class="muted">{relative(since(id, p.key), home.now)}</em></div>
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

    {#if view.garden.length}
      <section class="card">
        <div class="card-head"><h2><Icon name="flower-outline" size={18} />{t('maison.dehors.jardin')}</h2>
          <span class="muted">{view.garden.filter((l) => isOn(l.id)).length ? t('maison.dehors.eclaire') : t('maison.dehors.noir')}</span>
        </div>
        <div class="lights">
          {#each view.garden as l (l.id)}
            <LightTile id={l.id} name={l.name} icon="light" compact />
          {/each}
        </div>
      </section>
    {/if}

    <!-- The rooms outside (#217), among the other cards. -->
    {#each view.rooms as room (room.name)}
      <div class="slot"><RoomCard {room} /></div>
    {/each}
  </div>

  {#if outsideCams.length || view.cams.length}
    <section class="cams">
      {#each outsideCams as cam (cam.id)}
        <CameraTile id={cam.id} name={cam.name} refresh={cam.refresh} battery={cam.battery} onopen={() => (location.hash = '#/cameras')} />
      {/each}
      {#each view.cams as cam (cam.id)}
        <CameraTile id={cam.id} name={nameOf(cam.id)} onopen={() => (location.hash = '#/cameras')} />
      {/each}
    </section>
  {/if}

  {#if w || air || forecasts.length || stations.length}
    <!-- CC BY 4.0: the weather and air data credit their source. -->
    <p class="credit">
      {#if stations.length}
        {#if w || air || forecasts.length}{t('maison.dehors.credit_previsions')} <a href="https://open-meteo.com/" target="_blank" rel="noopener">Open-Meteo.com</a>{' · '}{/if}{t('maison.dehors.credit_mesures', { names: stations.map((id) => nameOf(id)).join(', ') })}
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

  .temp-line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    column-gap: 4px;
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

  .sunarc {
    position: relative;
    height: 58px;
    margin-top: 18px;
  }

  .sunarc svg {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
  }

  .sunarc path {
    fill: none;
    stroke: color-mix(in srgb, var(--sun) 60%, transparent);
    stroke-width: 2;
    stroke-dasharray: 2 6;
    stroke-linecap: round;
  }

  .sunarc line {
    stroke: var(--surface-3);
    stroke-width: 1;
  }

  .sunarc i {
    position: absolute;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--sun);
    box-shadow: 0 0 0 5px color-mix(in srgb, var(--sun) 30%, transparent);
    transform: translate(-50%, -50%);
  }

  .facades {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
  }

  .facades small {
    margin-right: 4px;
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

  .slot {
    min-width: 0;
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
