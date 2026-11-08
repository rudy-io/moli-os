<script>
  import { t } from '../../lib/i18n.svelte.js';
  import { deviceName, formatValue } from '../../lib/format.js';
  import { zoneSummary } from '../lib/airzone.js';
  import { climateMode } from '../lib/climate.js';
  import Icon from './Icon.svelte';

  let { device, href } = $props();

  const zone = $derived(zoneSummary(device));
  const name = $derived(deviceName(device) || t('airzone_ui.unknown'));
  const mode = $derived(climateMode(zone.mode));
  const modeLabel = $derived(mode ? t(mode.label) : (zone.mode ?? t('airzone_ui.unknown')));
  const tone = $derived(zone.online && zone.on !== false ? (mode?.tone ?? 'neutral') : 'neutral');
  const targets = $derived(zone.target
    ? [{ reading: zone.target, label: 'airzone_ui.target' }]
    : [
        { reading: zone.heatingTarget, label: 'airzone_ui.heating_target' },
        { reading: zone.coolingTarget, label: 'airzone_ui.cooling_target' },
      ].filter(({ reading }) => reading));
  const shown = (reading) => reading ? formatValue(reading.point, reading.value) : '—';
</script>

<a class="zone-card" {href} data-tone={tone} class:offline={zone.availability === 'offline'}>
  <header>
    <span class="climate-icon"><Icon name={zone.on === false ? 'power' : (mode?.icon ?? 'air-conditioner')} size={21} /></span>
    <div class="status-line">
      {#if zone.online && zone.problem === true}
        <span class="problem"><Icon name="alert" size={15} />{t('airzone_ui.problem')}</span>
      {/if}
      <span class="status"><i aria-hidden="true"></i>{t(`airzone_ui.status.${zone.status}`)}</span>
    </div>
  </header>

  <h3>{name}</h3>

  <div class="temperatures">
    <dl class="current">
      <dt>{t('airzone_ui.current')}</dt>
      <dd class="num">{shown(zone.temperature)}{#if zone.temperature?.unit}<small>{zone.temperature.unit}</small>{/if}</dd>
    </dl>
    <dl class="targets">
      {#if targets.length}
        {#each targets as { reading, label } (label)}
          <div class="target">
            <dt>{t(label)}</dt>
            <dd class="num">{shown(reading)}{#if reading.unit}<small>{reading.unit}</small>{/if}</dd>
          </div>
        {/each}
      {:else}
        <div class="target"><dt>{t('airzone_ui.target')}</dt><dd>—</dd></div>
      {/if}
    </dl>
  </div>

  <div class="observations">
    <span class="mode"><Icon name={mode?.icon ?? 'air-conditioner'} size={16} /><span>{modeLabel}</span></span>
    {#if zone.humidity}
      <span class="humidity"><Icon name="humidity" size={16} /><span><span class="sr-only">{t('airzone_ui.humidity')} </span><span class="num">{shown(zone.humidity)}</span>{#if zone.humidity.unit} {zone.humidity.unit}{/if}</span></span>
    {/if}
  </div>

  <footer>
    <span class="freshness">{zone.availability === 'online' ? '' : t('airzone_ui.latest')}</span>
    <span class="open-zone">{t('airzone_ui.details')}<Icon name="chevron-right" size={18} /></span>
  </footer>
</a>

<style>
  .zone-card {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 44px;
    padding: 22px;
    background: var(--surface);
    border: 1px solid transparent;
    border-radius: var(--r-lg);
    box-shadow: var(--shadow);
    color: var(--ink);
    text-decoration: none;
    transition: box-shadow 160ms var(--ease), border-color 160ms var(--ease);
  }
  .zone-card:hover {
    border-color: var(--line);
    box-shadow: var(--shadow-lift);
  }
  .zone-card:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 4px;
  }
  header,
  .status-line,
  .status,
  .problem,
  .observations,
  .mode,
  .humidity,
  footer,
  .open-zone {
    display: flex;
    align-items: center;
  }
  header,
  footer { justify-content: space-between; gap: 12px; }
  .climate-icon {
    display: grid;
    place-items: center;
    width: 42px;
    height: 42px;
    flex: none;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink-2);
  }
  [data-tone='warm'] .climate-icon { color: var(--warm-ink); background: var(--warm-soft); }
  [data-tone='cool'] .climate-icon { color: var(--cool); background: var(--cool-soft); }
  [data-tone='good'] .climate-icon { color: var(--good); background: var(--good-soft); }
  .status-line { min-width: 0; flex-wrap: wrap; justify-content: flex-end; gap: 6px 12px; }
  .status,
  .problem { gap: 6px; font-size: 12px; font-weight: 650; color: var(--ink-2); }
  .status i { width: 6px; height: 6px; flex: none; border-radius: 50%; background: var(--ink-3); }
  [data-tone='warm'] .status i { background: var(--warm); }
  [data-tone='cool'] .status i { background: var(--cool); }
  [data-tone='good'] .status i { background: var(--good); }
  .problem { color: var(--alert); }
  .zone-card h3 { margin: 18px 0 22px; font-size: 21px; font-weight: 650; letter-spacing: -0.025em; overflow-wrap: anywhere; }
  .temperatures { display: flex; flex-direction: column; align-items: stretch; gap: 16px; margin: 0; }
  .temperatures > dl { min-width: 0; margin: 0; }
  dt { margin: 0 0 7px; color: var(--ink-3); font-size: 12px; line-height: 1.4; overflow-wrap: anywhere; }
  dd { margin: 0; font-weight: 550; line-height: 1.05; overflow-wrap: anywhere; }
  .current dd { display: flex; align-items: baseline; white-space: nowrap; font-size: clamp(36px, 4vw, 48px); letter-spacing: -0.06em; }
  small { margin-left: 3px; font-size: 0.43em; font-weight: 500; letter-spacing: 0; color: var(--ink-2); }
  .targets { display: grid; gap: 10px; border-top: 1px solid var(--line); padding-top: 14px; }
  .target { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
  .target dd { font-size: 25px; letter-spacing: -0.035em; }
  .target small { font-size: 0.6em; }
  .observations { min-width: 0; flex-wrap: wrap; gap: 10px 18px; margin-top: 18px; font-size: 13px; color: var(--ink-2); }
  .mode,
  .humidity { min-width: 0; gap: 6px; }
  .mode span,
  .humidity span { min-width: 0; overflow-wrap: anywhere; }
  footer { margin-top: auto; padding-top: 20px; font-size: 12px; }
  .freshness { min-width: 0; color: var(--ink-3); overflow-wrap: anywhere; }
  .open-zone { justify-content: flex-end; gap: 4px; color: var(--ink-2); font-weight: 650; text-align: right; }
  .offline .climate-icon { color: var(--ink-3); }
  .sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
  @media (max-width: 700px) {
    .zone-card { padding: 14px; }
    .zone-card h3 { font-size: 18px; margin: 12px 0 18px; }
    .climate-icon { display: none; }
    .status-line { justify-content: flex-start; min-height: 30px; }
    .status, .problem { font-size: 10px; }
    .current dd { font-size: 36px; }
    .target { flex-wrap: wrap; gap: 2px 8px; }
    .target dd { font-size: 21px; }
    .observations { font-size: 11px; gap: 8px; }
    footer { font-size: 10px; padding-top: 16px; flex-wrap: wrap; }
    dt { font-size: 11px; }
  }
  @media (prefers-reduced-motion: reduce) {
    .zone-card { transition: none; }
  }
</style>
