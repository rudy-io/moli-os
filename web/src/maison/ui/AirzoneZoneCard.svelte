<script>
  import { t } from '../../lib/i18n.svelte.js';
  import { deviceName, formatValue } from '../../lib/format.js';
  import { zoneSummary, batterySummary } from '../lib/airzone.js';
  import {primarySetpoints} from '../lib/airzone-controls.js';
  import AirzoneSyncStatus from './AirzoneSyncStatus.svelte';
  import {zoneAppearance} from '../lib/airzone-appearance.js';
  import '../lib/airzone-appearance.css';
  import Icon from './Icon.svelte';

  let { device, href, sync = null, now = Date.now() } = $props();

  const zone = $derived(zoneSummary(device));
  const live=$derived(zone.online&&(!sync||sync.state==='current'));
  const battery = $derived(batterySummary(device));
  const name = $derived(deviceName(device) || t('airzone_ui.unknown'));
  const titleId = $derived(`airzone-zone-${encodeURIComponent(device.id)}`);
  const room = $derived(device.label?.room || device.native_room);
  const appearance=$derived(zoneAppearance(zone,sync));
  const mode=$derived(appearance.mode);
  const modeLabel = $derived(mode ? t(mode.label) : (zone.mode ?? t('airzone_ui.unknown')));
  const targets = $derived(primarySetpoints(device).filter(({reading})=>reading));
  const shown = (reading) => reading ? formatValue(reading.point, reading.value) : '—';
</script>

<a class="zone-card airzone-climate-surface" {href} aria-labelledby={titleId} data-tone={appearance.tone} data-demand={appearance.demand} data-status={live?zone.status:'unknown'} class:offline={zone.availability === 'offline'||(sync&&sync.state!=='current')}>
  <span class="mode-art" aria-hidden="true"><Icon name={appearance.icon} size={100}/></span>
  <header>
    <span class="climate-icon"><Icon name={appearance.icon} size={24} /></span>
    <div class="identity">
      <h3 id={titleId}>{name}</h3>
      {#if room && room !== name}<p class="room">{room}</p>{/if}
    </div>
  </header>

  <div class="status-line">
    <span class="status"><i aria-hidden="true"></i>{!live&&zone.online?t('airzone_ui.latest'):t(`airzone_ui.status.${zone.status}`)}</span>
    {#if live && zone.problem === true}
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
    <span class="mode"><Icon name={mode?.icon ?? 'air-conditioner'} size={16} /><span><span class="sr-only">{t('airzone_ui.mode')} </span>{modeLabel}</span></span>
    {#if zone.humidity}
      <span class="humidity"><Icon name="humidity" size={16} /><span><span class="sr-only">{t('airzone_ui.humidity')} </span><span class="num">{shown(zone.humidity)}</span>{#if zone.humidity.unit} {zone.humidity.unit}{/if}</span></span>
    {/if}
    {#if battery.status === 'level'}<span class="battery"><Icon name="battery" size={16} /><span>{t('airzone_health.battery')} <span class="num">{battery.percent} %</span></span></span>{/if}
  </div>

  <footer>
    {#if sync}<AirzoneSyncStatus {sync} {now} compact />{:else if zone.availability !== 'online'}<span class="freshness"><Icon name="clock" size={14} />{t('airzone_ui.latest')}</span>{/if}
    <span class="open-zone">{t('airzone_ui.details')}<Icon name="chevron-right" size={18} /></span>
  </footer>
</a>

<style>
  .zone-card {
    font-family:system-ui,-apple-system,BlinkMacSystemFont,sans-serif;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 44px;
    padding: 20px;
    position:relative;
    overflow:hidden;
    isolation:isolate;
    background:var(--climate-background);
    border:1px solid var(--climate-border);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow);
    color: var(--ink);
    text-decoration: none;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
    transition: box-shadow 160ms var(--ease), transform 120ms var(--ease), background-color 120ms var(--ease);
  }
  @media (hover: hover) {
    .zone-card:hover { box-shadow:var(--shadow-lift);transform:translateY(-2px); }
  }
  .zone-card:active { transform:scale(.985);box-shadow:var(--shadow); }
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
  .mode-art {position:absolute;top:8px;right:10px;color:var(--climate-accent);opacity:var(--climate-art-opacity);pointer-events:none;transform:rotate(-12deg);z-index:-1}
  header {gap:12px}
  .identity { min-width: 0; }
  .climate-icon {
    display: grid;
    place-items: center;
    width:44px;
    height:44px;
    flex: none;
    border-radius: var(--r-sm);
    background:var(--climate-soft);
    color:var(--climate-accent);
  }
  .status-line {min-width:0;flex-wrap:wrap;gap:7px 12px;margin-top:14px}
  .status,
  .problem { gap: 7px; font-size: 13px; line-height: 1.4; font-weight: 600; color: var(--ink-2); }
  .status i { width: 6px; height: 6px; flex: none; border-radius: 50%; background: var(--ink-3); }
  .status {min-height:28px;padding:5px 9px;border-radius:99px;background:var(--surface);font-size:.75rem}
  [data-demand='true'] .status {color:var(--climate-accent);background:var(--climate-soft)}
  [data-demand='true'] .status i {background:var(--climate-accent)}
  .problem :global(svg) { color: var(--alert); }
  .zone-card h3 { margin: 0; font-size: 20px; font-weight: 650; line-height: 1.2; letter-spacing: -0.025em; overflow-wrap: anywhere; }
  .room { margin: 4px 0 0; color: var(--ink-2); font-size: 13px; line-height: 1.4; overflow-wrap: anywhere; }
  .temperatures { display:grid; grid-template-columns:minmax(0,1fr) auto; align-items:center; gap:16px; margin:18px 0 0; }
  .temperatures > dl { min-width: 0; margin: 0; }
  dt { margin: 0 0 7px; color: var(--ink-2); font-size: 13px; line-height: 1.4; overflow-wrap: anywhere; }
  dd { margin: 0; font-weight: 550; line-height: 1.05; overflow-wrap: anywhere; }
  .current dd { display: flex; align-items: baseline; white-space: nowrap; font-size: clamp(40px, 4vw, 50px); letter-spacing: -0.055em; }
  small { margin-left: 3px; font-size: 0.43em; font-weight: 500; letter-spacing: 0; color: var(--ink-2); }
  .targets { display:grid; gap:8px; max-width:125px; }
  .target { display:grid; gap:4px; }
  .target dt { margin: 0; }
  .target dd { flex: none; font-size: 22px; letter-spacing: -0.035em; white-space: nowrap; }
  .target small { font-size: 0.6em; }
  .observations { min-width: 0; flex-wrap: wrap; gap: 8px 14px; margin-top: 14px; font-size: 13px; line-height: 1.4; color: var(--ink-2); }
  .mode,
  .humidity,
  .battery { min-width: 0; gap: 6px; }
  .mode {padding:5px 9px;border-radius:99px;background:var(--climate-soft);color:var(--climate-accent);font-weight:600}
  .mode span,
  .humidity span,
  .battery span { min-width: 0; overflow-wrap: anywhere; }
  footer { justify-content: flex-end; flex-wrap: wrap; gap: 8px 12px; margin-top: auto; padding-top: 14px; font-size: 12px; line-height: 1.4; }
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
    .targets { padding: 0; }
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
    .targets { padding: 0; }
  }
  @media (prefers-contrast: more) {
    .zone-card { border-color: var(--ink-2); box-shadow: none; }
  }
  @media (prefers-reduced-motion: reduce) {
    .zone-card { transition: none; }
    .zone-card:hover,.zone-card:active {transform:none}
  }
</style>
