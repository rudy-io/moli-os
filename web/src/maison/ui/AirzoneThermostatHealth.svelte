<script>
  import {t} from '../../lib/i18n.svelte.js';
  import {batterySummary,readingOf} from '../lib/airzone.js';
  import Icon from './Icon.svelte';
  import {mdiWifi} from '@mdi/js';
  let {zone,live=true}=$props();
  const battery=$derived(batterySummary(zone));
  const coverage=$derived(readingOf(zone,'coverage'));
  const validCoverage=$derived(coverage?.unit==='%'&&Number.isInteger(coverage.value)&&coverage.value>=0&&coverage.value<=100);
  const model=$derived(readingOf(zone,'thermos_type')?.value);
  const modelKeys={'1':'blueface','2':'blueface_zero','3':'lite','4':'think'};
  const modelKey=$derived(Object.hasOwn(modelKeys,model)?modelKeys[model]:null);
  const firmware=$derived(readingOf(zone,'thermos_firmware')?.value);
  const hasLevels=$derived(battery.status==='level'||validCoverage);
  const hasRadio=$derived(battery.radio===true||hasLevels);
</script>

<section class="health-card" aria-label={t('airzone_detail.thermostat')}>
  <header><span class="icon"><Icon name={battery.radio===false?'lan':hasRadio?'battery':'info'} size={22}/></span><div><h2>{t('airzone_detail.thermostat')}</h2><p>{battery.radio===true?t('airzone_health.radio'):battery.radio===false?t('airzone_health.wired'):t('airzone_health.thermostat_unknown')}</p></div></header>
  {#if modelKey||firmware}
    <dl class="identity">
      {#if modelKey}<div><dt>{t('airzone_health.model')}</dt><dd>{t(`airzone_health.model.${modelKey}`)}</dd></div>{/if}
      {#if typeof firmware==='string'&&firmware.trim()}<div><dt>{t('airzone_health.firmware')}</dt><dd>{firmware}</dd></div>{/if}
    </dl>
  {/if}
  {#if hasRadio}
    <dl class="levels">
      <div><dt><Icon name="battery" size={18}/>{t('airzone_health.battery')}</dt><dd class="num">{battery.status==='level'?`${battery.percent} %`:t('airzone_health.battery_missing')}</dd></div>
      <div><dt><Icon path={mdiWifi} size={18}/>{t('airzone_health.signal')}</dt><dd class="num">{validCoverage?`${coverage.value} %`:t('airzone_ui.unknown')}</dd></div>
    </dl>
    <p class="help">{t('airzone_health.battery_help')}</p>
  {:else if battery.radio===false}
    <p class="help">{t('airzone_detail.no_battery')}. {t('airzone_health.wired_help')}</p>
  {:else}
    <p class="unreported">{t('airzone_health.metadata_missing')}</p>
    <p class="help">{t('airzone_health.metadata_help')}</p>
  {/if}
  {#if !live}<p class="latest">{t('airzone_ui.latest')}</p>{/if}
  {#if battery.radio!==false&&!hasLevels}
    <a href="#/airzone/installation"><span>{t('airzone_health.inspect_gateway')}</span><Icon name="chevron-right" size={18}/></a>
  {/if}
</section>

<style>
  .health-card {min-width:0;padding:20px;border:1px solid var(--line);border-radius:var(--r-lg);background:var(--surface)}
  header {display:flex;align-items:flex-start;gap:12px;margin-bottom:18px}header>div {min-width:0}
  .icon {display:flex;align-items:center;justify-content:center;flex:none;width:40px;height:40px;border-radius:13px;background:var(--surface-2);color:var(--ink-2)}
  h2 {font-size:1.0625rem;font-weight:650;letter-spacing:-.02em;line-height:1.4}
  header p {margin-top:3px;color:var(--ink-2);font-size:1rem;line-height:1.4;overflow-wrap:anywhere}
  dl {margin:0}dl>div {display:flex;justify-content:space-between;align-items:baseline;gap:18px;padding:11px 0;font-size:1rem;line-height:1.4}
  dl>div+div,.identity+.levels {border-top:1px solid var(--line)}
  dt {display:flex;align-items:center;gap:8px;color:var(--ink-2)}dd {margin:0;font-weight:650;text-align:right;max-width:60%;overflow-wrap:anywhere}
  .help {margin-top:12px;color:var(--ink-2);font-size:.875rem;line-height:1.55}
  .unreported {font-weight:600;line-height:1.45}.latest {margin-top:12px;font-size:.875rem;font-weight:600;color:var(--ink-2);line-height:1.5}
  a {display:flex;align-items:center;justify-content:space-between;gap:8px;min-height:44px;margin-top:12px;color:var(--ink-2);font-weight:600;font-size:1rem;text-decoration:none;border-radius:var(--r-sm)}
  a:focus-visible {outline:3px solid var(--cool);outline-offset:4px}a:hover {color:var(--ink)}
  @media(max-width:440px) {.help {font-size:1rem}}
  @media(max-width:320px) {.health-card {padding:16px}dl>div {flex-wrap:wrap;gap:6px}dd {max-width:100%;text-align:left}}
  @media(prefers-contrast:more) {.health-card {border-color:var(--ink-2)}.help,header p,dt,a {color:var(--ink)}}
</style>
