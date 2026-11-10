<script>
  import { t } from '../../lib/i18n.svelte.js';
  import { deviceName, formatValue } from '../../lib/format.js';
  import { zoneSummary, batterySummary } from '../lib/airzone.js';
  import { climateMode } from '../lib/climate.js';
  import Icon from './Icon.svelte';

  let { device, href } = $props();

  const zone = $derived(zoneSummary(device));
  const battery = $derived(batterySummary(device));
  const name = $derived(deviceName(device) || t('airzone_ui.unknown'));
  const titleId = $derived(`airzone-zone-${encodeURIComponent(device.id)}`);
  const room = $derived(device.label?.room || device.native_room);
  const mode = $derived(climateMode(zone.mode));
  const modeLabel = $derived(mode ? t(mode.label) : (zone.mode ?? t('airzone_ui.unknown')));
  const tone = $derived(zone.online && zone.on !== false && zone.status !== 'unknown' ? (mode?.tone ?? 'neutral') : 'neutral');
  const targets = $derived(zone.target
    ? [{ reading: zone.target, label: 'airzone_ui.target' }]
    : [
        { reading: zone.heatingTarget, label: 'airzone_ui.heating_target' },
        { reading: zone.coolingTarget, label: 'airzone_ui.cooling_target' },
      ].filter(({ reading }) => reading));
  const shown = (reading) => reading ? formatValue(reading.point, reading.value) : '—';
</script>

<a class="zone-card" {href} aria-labelledby={titleId} data-tone={tone} data-status={zone.status} class:offline={zone.availability === 'offline'}>
  <header>
    <span class="climate-icon"><Icon name={zone.on === false ? 'power' : (mode?.icon ?? 'air-conditioner')} size={21} /></span>
    <div class="identity">
      <h3 id={titleId}>{name}</h3>
      {#if room && room !== name}<p class="room">{room}</p>{/if}
    </div>
  </header>

  <div class="status-line">
    <span class="status"><i aria-hidden="true"></i>{t(`airzone_ui.status.${zone.status}`)}</span>
    {#if zone.online && zone.problem === true}
      <span class="problem"><Icon name="alert" size={15} />{t('airzone_ui.problem')}</span>
    {/if}
  </div>

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
    {#if battery.status === 'level'}<span class="battery"><Icon name="battery" size={16} /><span>{t('airzone_health.battery')} <span class="num">{battery.percent} %</span></span></span>{/if}
  </div>

  <footer>
    {#if zone.availability !== 'online'}<span class="freshness"><Icon name="clock" size={14} />{t('airzone_ui.latest')}</span>{/if}
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
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow);
    color: var(--ink);
    text-decoration: none;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
    transition: box-shadow 160ms var(--ease), transform 120ms var(--ease), background-color 120ms var(--ease);
  }
  @media (hover: hover) {
    .zone-card:hover { box-shadow: var(--shadow-lift); }
  }
  .zone-card:active { transform: scale(0.985); background: var(--surface-2); }
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
  .battery,
  .freshness,
  footer,
  .open-zone {
    display: flex;
    align-items: center;
  }
  header { gap: 12px; }
  .identity { min-width: 0; }
  .climate-icon {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    flex: none;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink-2);
  }
  [data-tone='warm'] .climate-icon { color: var(--warm-ink); background: var(--warm-soft); }
  [data-tone='cool'] .climate-icon { color: var(--cool); background: var(--cool-soft); }
  [data-tone='good'] .climate-icon { color: var(--good); background: var(--good-soft); }
  .status-line { min-width: 0; flex-wrap: wrap; gap: 7px 12px; margin-top: 16px; }
  .status,
  .problem { gap: 7px; font-size: 13px; line-height: 1.4; font-weight: 600; color: var(--ink-2); }
  .status i { width: 6px; height: 6px; flex: none; border-radius: 50%; background: var(--ink-3); }
  [data-status='demand'][data-tone='warm'] .status i { background: var(--warm-ink); }
  [data-status='demand'][data-tone='cool'] .status i { background: var(--cool); }
  [data-status='demand'][data-tone='good'] .status i { background: var(--good); }
  [data-status='demand'][data-tone='warm'] .status { color: var(--warm-ink); }
  .problem :global(svg) { color: var(--alert); }
  .zone-card h3 { margin: 0; font-size: 20px; font-weight: 650; line-height: 1.2; letter-spacing: -0.025em; overflow-wrap: anywhere; }
  .room { margin: 4px 0 0; color: var(--ink-2); font-size: 13px; line-height: 1.4; overflow-wrap: anywhere; }
  .temperatures { display: flex; flex-direction: column; align-items: stretch; gap: 18px; margin: 22px 0 0; }
  .temperatures > dl { min-width: 0; margin: 0; }
  dt { margin: 0 0 7px; color: var(--ink-2); font-size: 13px; line-height: 1.4; overflow-wrap: anywhere; }
  dd { margin: 0; font-weight: 550; line-height: 1.05; overflow-wrap: anywhere; }
  .current dd { display: flex; align-items: baseline; white-space: nowrap; font-size: clamp(44px, 4vw, 56px); letter-spacing: -0.055em; }
  small { margin-left: 3px; font-size: 0.43em; font-weight: 500; letter-spacing: 0; color: var(--ink-2); }
  .targets { display: grid; gap: 12px; padding: 12px 14px; border-radius: var(--r-sm); background: var(--surface-2); }
  .target { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
  .target dt { margin: 0; }
  .target dd { flex: none; font-size: 24px; letter-spacing: -0.035em; white-space: nowrap; }
  .target small { font-size: 0.6em; }
  .observations { min-width: 0; flex-wrap: wrap; gap: 10px 16px; margin-top: 18px; font-size: 13px; line-height: 1.4; color: var(--ink-2); }
  .mode,
  .humidity,
  .battery { min-width: 0; gap: 6px; }
  .mode span,
  .humidity span,
  .battery span { min-width: 0; overflow-wrap: anywhere; }
  footer { justify-content: flex-end; flex-wrap: wrap; gap: 8px 12px; margin-top: auto; padding-top: 18px; font-size: 12px; line-height: 1.4; }
  .freshness { min-width: 0; margin-right: auto; gap: 5px; color: var(--ink-2); overflow-wrap: anywhere; }
  .open-zone { margin-left: auto; justify-content: flex-end; gap: 4px; color: var(--ink-2); font-weight: 650; text-align: right; }
  .offline .current dd { color: var(--ink-2); }
  .sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
  @media (max-width: 700px) {
    .zone-card { padding: 17px; }
    header { align-items: flex-start; gap: 9px; }
    .zone-card h3 { font-size: 18px; }
    .climate-icon { width: 30px; height: 30px; border-radius: 10px; }
    .status-line { margin-top: 14px; }
    .temperatures { margin-top: 18px; gap: 14px; }
    .current dd { font-size: 44px; }
    .targets { padding: 10px; }
    .target { flex-wrap: wrap; gap: 4px 8px; }
    .target dd { font-size: 23px; }
    .observations { gap: 8px 12px; }
    footer { padding-top: 16px; }
  }
  @media (max-width: 430px) {
    .zone-card { padding: 20px; }
    header { align-items: center; gap: 12px; }
    .climate-icon { width: 38px; height: 38px; border-radius: 12px; }
    .zone-card h3 { font-size: 20px; }
    .temperatures { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); align-items: center; gap: 16px; }
    .current dd { font-size: clamp(38px, 12vw, 48px); }
    .target { display: block; }
    .target dt { margin-bottom: 5px; }
    .targets { padding: 12px; }
  }
  @media (prefers-contrast: more) {
    .zone-card { border-color: var(--ink-2); box-shadow: none; }
  }
  @media (prefers-reduced-motion: reduce) {
    .zone-card { transition: none; }
    .zone-card:active { transform: none; }
  }
</style>
