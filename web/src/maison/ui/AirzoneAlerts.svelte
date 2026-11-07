<script>
  import { t } from '../../lib/i18n.svelte.js';
  import { deviceName } from '../../lib/format.js';
  import Icon from './Icon.svelte';
  let { report, devices, hidden = [] } = $props();
  const byId = $derived(new Map(devices.map((device) => [device.id,device])));
  const target = (item) => {
    const device = byId.get(item.deviceId);
    if (!device) return '#/airzone/installation';
    if (hidden.includes(device.id) || device.model !== 'Local API') return `#/airzone/devices/${encodeURIComponent(device.id)}`;
    return `#/airzone/${encodeURIComponent(device.id)}`;
  };
</script>

<section class="alerts" aria-label={t('airzone_ui.alerts')}>
  <p class="intro">{t('airzone_health.help')}</p>
  {#if report.items.length}
    <ul>
      {#each report.items as item (item.id)}
        <li data-severity={item.severity}>
          <Icon name={item.kind === 'offline' || item.kind === 'connection' ? 'offline' : 'alert'} size={23} />
          <div>
            <h2>{t(`airzone_health.kind.${item.kind}`)}</h2>
            <p>{item.deviceId && byId.has(item.deviceId) ? deviceName(byId.get(item.deviceId)) : item.instance ?? t('airzone_ui.title')}{#if item.scope === 'system'} · {t('airzone_health.shared')}{/if}</p>
            {#if item.kind === 'device_error'}<span class="code">{t('airzone_health.code',{code:item.code})}</span>{/if}
            {#if item.kind === 'device_warning'}<span class="code">{t(`airzone_health.warning.${item.code}`)}</span>{/if}
            {#if item.affectedIds?.length > 1}<span class="code">{t('airzone_health.affected',{count:item.affectedIds.length})}</span>{/if}
            {#if hidden.includes(item.deviceId)}<span class="code">{t('airzone_health.hidden')}</span>{/if}
            <p class="hint">{t(`airzone_health.help.${item.kind}`)}</p>
          </div>
          <a href={target(item)}>{t('airzone_health.inspect')}<Icon name="chevron-right" size={18} /></a>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="empty">{t(report.diagnostics.reported ? 'airzone_health.none_reported' : 'airzone_health.not_reported')}</p>
  {/if}
  {#if report.diagnostics.unknown > 0}<p class="coverage">{t('airzone_health.coverage',{count:report.diagnostics.unknown})}</p>{/if}
</section>

<style>
  .intro {max-width:65ch; font-size:15px; line-height:1.65; color:var(--ink-2); margin:0 0 20px;}
  ul {list-style:none; padding:0; margin:0; display:grid; gap:14px;}
  li {display:grid; grid-template-columns:24px minmax(0,1fr) auto; gap:16px; padding:22px; background:var(--surface); border:1px solid var(--line); border-left:4px solid var(--warm); border-radius:var(--r-md); align-items:start;}
  li[data-severity='error'] {border-left-color:var(--alert);}
  h2 {font-size:18px; margin:0 0 6px; letter-spacing:-.02em;}
  p {margin:0; line-height:1.5; overflow-wrap:anywhere;}
  .hint {font-size:13px; color:var(--ink-2); margin-top:8px; max-width:65ch;}
  .code {display:inline-block; font-size:12px; color:var(--ink-2); background:var(--surface-2); padding:4px 8px; border-radius:var(--r-sm); margin:8px 8px 0 0;}
  a {display:inline-flex; align-items:center; gap:4px; min-height:44px; color:var(--cool); font-size:13px; font-weight:650;}
  a:focus-visible {outline:3px solid var(--cool); outline-offset:3px;}
  .empty {padding:24px; background:var(--surface); border-radius:var(--r-md);}
  .coverage {font-size:13px; color:var(--ink-2); margin-top:18px; max-width:65ch;}
  @media(max-width:700px) {li {grid-template-columns:24px minmax(0,1fr); padding:18px;} a {grid-column:2;}}
</style>
