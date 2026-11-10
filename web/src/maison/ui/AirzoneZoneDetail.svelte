<script>
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { deviceName, formatValue } from '../../lib/format.js';
  import { zoneSummary, batterySummary, readingOf } from '../lib/airzone.js';
  import { climateMode } from '../lib/climate.js';
  import Icon from './Icon.svelte';
  import AirzoneReadings from './AirzoneReadings.svelte';

  let { zone, controls = [], onhistory } = $props();
  const summary = $derived(zoneSummary(zone));
  const battery = $derived(batterySummary(zone));
  const coverage = $derived(readingOf(zone, 'coverage'));
  const validCoverage = $derived(coverage?.unit === '%' && Number.isInteger(coverage.value)
    && coverage.value >= 0 && coverage.value <= 100);
  const mode = $derived(climateMode(summary.mode));
  const dualTargets = $derived(!summary.target && (summary.heatingTarget || summary.coolingTarget));
  const time = (ts) => new Date(ts).toLocaleString(locale(), { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
  const demandIcons = { heat_demand: 'fire', cold_demand: 'snowflake', air_demand: 'fan', floor_demand: 'waves' };
</script>

{#snippet metric(label, reading, prominent = false)}
  <button class="metric" class:prominent disabled={!reading}
    aria-label={reading ? t('airzone_ui.history_value', { label, value: `${formatValue(reading.point, reading.value)}${reading.unit ? ` ${reading.unit}` : ''}` }) : undefined}
    onclick={() => { if (reading) onhistory?.(reading.key); }}>
    <span class="metric-label">{label}</span>
    <span class="measurement num">{reading ? formatValue(reading.point, reading.value) : '—'}{#if reading?.unit}<small>{reading.unit}</small>{/if}</span>
    {#if reading}
      <span class="history-link"><Icon name="history" size={16} />{t('airzone_detail.view_history')}<Icon name="chevron-right" size={15} /></span>
      {#if reading.ts != null}<span class="sample">{t('airzone_detail.changed_at', { time: time(reading.ts) })}</span>{/if}
    {:else}
      <span class="sample">{t('airzone_ui.unknown')}</span>
    {/if}
  </button>
{/snippet}

<div class="zone-detail">
  {#if summary.availability !== 'online'}
    <p class="notice" role="status"><Icon name="offline" size={20} /><span>{summary.availability === 'offline' ? t('airzone_ui.offline_help') : t('airzone_ui.availability_unknown')}</span></p>
  {/if}
  {#if summary.online && summary.problem === true}
    <a class="notice problem" href="#/airzone/alerts"><Icon name="alert" size={20} /><span>{t('airzone_ui.problem')}</span><Icon name="chevron-right" size={18} /></a>
  {/if}

  <section class="comfort-card" class:stale={!summary.online} aria-label={t('airzone_ui.measurements')}>
    <header class="comfort-heading">
      <span class="section-label"><Icon name="thermometer" size={20} />{t('airzone_ui.measurements')}</span>
      <span class="state-chip" class:active={summary.status === 'demand'}>{t(`airzone_ui.status.${summary.status}`)}</span>
    </header>
    {#if !summary.online}<p class="latest">{t('airzone_ui.latest')}</p>{/if}
    <div class="climate-metrics">
      {@render metric(t('airzone_ui.current'), summary.temperature, true)}
      <div class="targets" class:dual={dualTargets}>
        {#if dualTargets}
          {@render metric(t('airzone_ui.heating_target'), summary.heatingTarget)}
          {@render metric(t('airzone_ui.cooling_target'), summary.coolingTarget)}
        {:else}
          {@render metric(t('airzone_ui.target'), summary.target)}
        {/if}
      </div>
    </div>
    <dl class="supporting">
      <div><dt><Icon name={mode?.icon ?? 'air-conditioner'} size={20} />{t('airzone_ui.mode')}</dt><dd>{mode ? t(mode.label) : summary.mode ?? t('airzone_ui.unknown')}</dd></div>
      {#if summary.humidity}<div><dt><Icon name="humidity" size={20} />{t('airzone_ui.humidity')}</dt><dd class="num">{formatValue(summary.humidity.point, summary.humidity.value)}{summary.humidity.unit ? ` ${summary.humidity.unit}` : ''}</dd></div>{/if}
    </dl>
  </section>

  <div class="status-grid">
    <section class="activity-card" aria-label={t('airzone_ui.activity')}>
      <header class="section-heading"><span class="section-icon"><Icon name="air-conditioner" size={22} /></span><div><h2>{t('airzone_ui.activity')}</h2><p class="activity-status">{!summary.online ? t('airzone_ui.latest') : t(summary.activity === 'demand' ? 'airzone_ui.status.demand' : summary.activity === 'no_reported_demand' ? 'airzone_ui.status.idle' : 'airzone_ui.unknown')}</p></div></header>
      <dl class="demands">
        {#each summary.demands as demand (demand.key)}
          {@const point = zone.points.find((point) => point.key === demand.key)}
          <div><dt><Icon name={demandIcons[demand.key]} size={18} />{t(`airzone_detail.demand.${demand.key}`)}</dt><dd class:reported={summary.online && demand.value}>{point ? formatValue(point, demand.value) : '—'}</dd></div>
        {/each}
      </dl>
      <p class="explanation">{t('airzone_ui.activity_help')}</p>
    </section>

    <section class="health-card" aria-label={t('airzone_detail.thermostat')}>
      <header class="section-heading"><span class="section-icon"><Icon name="battery" size={22} /></span><div><h2>{t('airzone_detail.thermostat')}</h2><p>{battery.radio === true ? t('airzone_health.radio') : battery.radio === false ? t('airzone_health.wired') : t('airzone_health.thermostat_unknown')}</p></div></header>
      <dl class="health-readings">
        <div><dt>{t('airzone_health.battery')}</dt><dd class="num">{battery.status === 'level' ? `${battery.percent} %` : battery.status === 'wired' ? t('airzone_detail.no_battery') : t('airzone_health.battery_missing')}</dd></div>
        {#if battery.radio !== false || validCoverage}<div><dt>{t('airzone_health.signal')}</dt><dd class="num">{validCoverage ? `${coverage.value} %` : t('airzone_ui.unknown')}</dd></div>{/if}
      </dl>
      {#if !summary.online}<p class="latest">{t('airzone_ui.latest')}</p>{/if}
      <p class="explanation">{t('airzone_health.battery_help')}</p>
    </section>
  </div>

  <a class="management-link" href={`#/airzone/devices/${encodeURIComponent(zone.id)}`}>
    <span class="section-icon"><Icon name="tune" size={22} /></span><span><strong>{t('airzone_detail.personalise')}</strong><small>{t('airzone_detail.personalise_help')}</small></span><Icon name="chevron-right" size={20} />
  </a>

  <div class="technical-stack">
    <details class="technical">
      <summary><span class="section-icon"><Icon name="info" size={22} /></span><span><strong>{t('airzone_ui.parameters')}</strong><small>{t('airzone_detail.parameters_help')}</small></span><span class="chevron"><Icon name="chevron-down" size={20} /></span></summary>
      <div class="technical-content"><AirzoneReadings device={zone} /></div>
    </details>
    {#if controls.length}
      <details class="technical">
        <summary><span class="section-icon"><Icon name="branch" size={22} /></span><span><strong>{t('airzone_ui.shared')}</strong><small>{t('airzone_detail.shared_hint')}</small></span><span class="chevron"><Icon name="chevron-down" size={20} /></span></summary>
        <div class="technical-content"><p class="explanation">{t('airzone_ui.shared_help')}</p>{#each controls as control (control.id)}<section class="shared-resource"><h3>{deviceName(control)}</h3><AirzoneReadings device={control} /></section>{/each}</div>
      </details>
    {/if}
  </div>
  <a class="installation-link" href="#/airzone/installation"><Icon name="lan" size={19} /><span>{t('airzone_ui.installation')}</span><Icon name="chevron-right" size={18} /></a>
</div>

<style>
  .zone-detail {display:grid;gap:16px;min-width:0}
  .notice {display:flex;align-items:center;gap:12px;padding:16px 20px;border:1px solid var(--line);border-radius:var(--r-sm);background:var(--surface-2);color:var(--ink-2);font-size:1rem;line-height:1.5}
  .notice span {flex:1;min-width:0}
  .problem {background:var(--alert-soft);color:var(--ink);text-decoration:none}
  .comfort-card,.activity-card,.health-card {min-width:0;border:1px solid var(--line);border-radius:var(--r-lg);background:var(--surface)}
  .comfort-card {padding:24px;box-shadow:var(--shadow)}
  .comfort-heading {display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:12px}
  .section-label {display:flex;align-items:center;gap:8px;font-size:1rem;font-weight:650;color:var(--ink-2)}
  .state-chip {padding:7px 11px;border-radius:999px;background:var(--surface-2);color:var(--ink-2);font-size:.875rem;font-weight:650;line-height:1.4}
  .state-chip.active {background:var(--warm-soft);color:var(--warm-ink)}
  .climate-metrics {display:grid;grid-template-columns:minmax(0,1.15fr) minmax(0,1fr);align-items:center;gap:24px;margin-top:24px}
  .targets {display:grid;min-width:0;gap:10px}
  .metric {display:flex;flex-direction:column;align-items:flex-start;gap:8px;min-width:0;width:100%;min-height:44px;padding:18px;text-align:left;color:var(--ink);border:1px solid transparent;border-radius:var(--r-md);background:var(--surface-2);touch-action:manipulation;transition:background .16s ease,border-color .16s ease;overflow-wrap:anywhere}
  .metric.prominent {padding:12px 0;background:transparent}
  .metric:disabled {cursor:default}
  .metric:not(:disabled):active {background:var(--surface-3);transition:none}
  .metric:focus-visible,summary:focus-visible,a:focus-visible {outline:3px solid var(--cool);outline-offset:4px}
  .metric-label {font-size:1rem;color:var(--ink-2);font-weight:550;line-height:1.35}
  .measurement {display:flex;align-items:baseline;gap:5px;flex-wrap:wrap;font-size:2.5rem;font-weight:600;letter-spacing:-.06em;line-height:1.08}
  .measurement small {font-size:.45em;font-weight:500;letter-spacing:-.02em}
  .prominent .measurement {font-size:clamp(3.5rem,8vw,5.75rem);font-weight:500}
  .history-link {display:flex;align-items:center;gap:6px;font-size:.875rem;font-weight:650;line-height:1.4;color:var(--ink-2)}
  .sample {font-size:.8125rem;line-height:1.4;color:var(--ink-2)}
  .supporting {display:flex;flex-wrap:wrap;gap:16px 28px;padding:20px 0 0;margin:24px 0 0;border-top:1px solid var(--line)}
  .supporting>div {display:flex;align-items:center;flex-wrap:wrap;gap:10px;font-size:1rem;min-width:0}
  dt {color:var(--ink-2)}
  dd {margin:0;font-weight:650;overflow-wrap:anywhere}
  .supporting dt {display:flex;align-items:center;gap:7px}
  .status-grid {display:grid;grid-template-columns:1fr 1fr;gap:16px;min-width:0}
  .activity-card,.health-card {padding:20px}
  .section-heading {display:flex;align-items:flex-start;gap:12px;margin-bottom:18px}
  .section-heading>div {min-width:0}
  .section-icon {display:flex;align-items:center;justify-content:center;flex:none;width:40px;height:40px;border-radius:13px;background:var(--surface-2);color:var(--ink-2)}
  h2 {font-size:1.0625rem;font-weight:650;letter-spacing:-.02em;line-height:1.4}
  .section-heading p {margin-top:3px;color:var(--ink-2);font-size:1rem;line-height:1.4;overflow-wrap:anywhere}
  .demands,.health-readings {margin:0;display:grid;gap:0}
  .demands>div,.health-readings>div {display:flex;justify-content:space-between;align-items:baseline;gap:18px;padding:11px 0;font-size:1rem;line-height:1.4}
  .demands>div+div,.health-readings>div+div {border-top:1px solid var(--line)}
  .demands dt {display:flex;align-items:center;gap:8px}
  .demands dd {color:var(--ink-2)}
  .demands dd.reported {color:var(--warm-ink)}
  .health-readings dd {text-align:right;max-width:60%}
  .explanation {margin-top:16px;color:var(--ink-2);font-size:.875rem;line-height:1.55}
  .latest {margin-top:12px;font-size:.875rem;font-weight:600;color:var(--ink-2);line-height:1.5}
  .management-link {display:flex;align-items:center;gap:14px;padding:17px 20px;min-height:44px;color:var(--ink);border:1px solid var(--line);border-radius:var(--r-md);background:var(--surface);text-decoration:none;touch-action:manipulation}
  .management-link>span:nth-child(2) {flex:1;min-width:0}
  .management-link strong,summary strong {font-size:1rem;font-weight:650;line-height:1.4;overflow-wrap:anywhere}
  .management-link small,summary small {display:block;margin-top:3px;font-size:.875rem;line-height:1.45;color:var(--ink-2);overflow-wrap:anywhere}
  .technical-stack {border:1px solid var(--line);border-radius:var(--r-md);background:var(--surface)}
  .technical+.technical {border-top:1px solid var(--line)}
  summary {display:flex;align-items:center;gap:14px;min-height:44px;padding:17px 20px;border-radius:var(--r-md);cursor:pointer;list-style:none;touch-action:manipulation}
  summary::-webkit-details-marker {display:none}
  summary>span:nth-child(2) {flex:1;min-width:0}
  .chevron {color:var(--ink-2);transition:transform .16s ease}
  .technical[open]>summary .chevron {transform:rotate(180deg)}
  .technical-content {padding:0 20px 20px}
  .technical-content>.explanation {margin:0 0 18px}
  .shared-resource+.shared-resource {margin-top:20px}
  h3 {font-size:1rem;font-weight:650;margin:0 0 12px;overflow-wrap:anywhere}
  .installation-link {display:flex;align-items:center;gap:8px;justify-self:start;min-height:44px;padding:8px 4px;color:var(--ink-2);font-size:1rem;font-weight:550;text-decoration:none}
  .installation-link span {min-width:0;overflow-wrap:anywhere}
  summary:active,.management-link:active {background:var(--surface-2)}
  @media(hover:hover) { .metric:not(:disabled):hover {border-color:var(--line);background:var(--surface-3)} summary:hover,.management-link:hover {background:var(--surface-2)} .installation-link:hover {color:var(--ink)} }
  @media(max-width:700px) { .status-grid {grid-template-columns:minmax(0,1fr)} .climate-metrics {gap:16px} .comfort-card {padding:20px} }
  @media(max-width:440px) { .climate-metrics {grid-template-columns:minmax(0,1fr);gap:16px;margin-top:20px} .metric.prominent {padding:0} .metric {padding:16px} .targets.dual {grid-template-columns:repeat(2,minmax(0,1fr))} .targets.dual .measurement {font-size:2rem} .targets.dual .history-link {flex-wrap:wrap} .supporting {margin-top:20px} .explanation,.management-link small,summary small {font-size:1rem} }
  @media(max-width:320px) { .targets.dual {grid-template-columns:minmax(0,1fr)} .comfort-card,.activity-card,.health-card {padding:16px} .demands>div,.health-readings>div {flex-wrap:wrap;gap:6px} .health-readings dd {max-width:100%;text-align:left} summary,.management-link {padding:14px 12px;gap:10px} .section-icon {width:32px;height:32px} }
  @media(prefers-reduced-motion:reduce) { .metric,.chevron {transition:none} }
  @media(prefers-contrast:more) { .comfort-card,.activity-card,.health-card,.technical-stack,.management-link {border-color:var(--ink-2)} .sample,.explanation,small {color:var(--ink)} }
</style>
