<script>
  import { home, value, num } from '../lib/home.svelte.js';
  import { weatherOf } from '../lib/icons.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  const w = $derived(home.config?.outdoor?.weather);
  const day = $derived(value(w, 'daylight') !== false);
  const sky = $derived(weatherOf(value(w, 'weather_code'), day));
  const hhmm = (iso) => (iso ? String(iso).slice(11, 16) : '—');
</script>

<a class="card weather" href="#/dehors" class:night={!day}>
  <svg viewBox="0 0 24 24" aria-hidden="true"><path d={sky.icon} /></svg>
  <div class="main">
    <strong class="num">{num(value(w, 'temperature'))}°</strong>
    <span>{sky.label}</span>
    <small class="muted">{t('moli.meteo.extremes', { max: num(value(w, 'today_max')), min: num(value(w, 'today_min')) })}</small>
  </div>
  <ul>
    <li><Icon name="water" size={16} />{t('moli.meteo.pluie', { chance: num(value(w, 'today_rain_chance')) })}</li>
    <li><Icon name="wind" size={16} />{num(value(w, 'wind_speed'))} km/h</li>
    <li><Icon name={day ? 'sunset' : 'sunrise'} size={16} />{day ? hhmm(value(w, 'sunset')) : hhmm(value(w, 'sunrise'))}</li>
  </ul>
</a>

<style>
  .weather {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 16px;
    align-items: center;
    color: inherit;
    text-decoration: none;
    background:
      radial-gradient(120% 140% at 100% 0%, color-mix(in srgb, var(--sun) 20%, transparent), transparent 60%),
      var(--surface);
  }

  .night {
    background:
      radial-gradient(120% 140% at 100% 0%, color-mix(in srgb, var(--cool) 20%, transparent), transparent 60%),
      var(--surface);
  }

  svg {
    width: 64px;
    height: 64px;
    fill: var(--sun);
  }

  .night svg {
    fill: var(--cool);
  }

  .main {
    display: grid;
  }

  strong {
    font-size: 40px;
    font-weight: 300;
    letter-spacing: -0.03em;
    line-height: 1;
  }

  .main span {
    font-weight: 650;
  }

  ul {
    grid-column: 1 / -1;
    list-style: none;
    margin: 10px 0 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
    font-size: 14px;
    color: var(--ink-2);
  }

  li {
    display: flex;
    align-items: center;
    gap: 6px;
  }
</style>
