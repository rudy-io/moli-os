<script>
  import {t,locale} from '../../lib/i18n.svelte.js';
  import {ago} from '../../lib/format.js';
  import Icon from './Icon.svelte';
  let {sync,now=Date.now(),detailed=false,driver=null,compact=false}=$props();
  // A receipt can arrive between UI clock ticks. Reuse the validated zero
  // age; null ages from genuine clock skew must retain their warning.
  const receiptNow=$derived(sync.age===0?Math.max(now,sync.received):now);
  const at=(value)=>Number.isFinite(value)?new Date(value).toLocaleString(locale()):t('airzone_ui.unknown');
</script>
<div class="sync-status" class:compact class:warning={sync.state!=='current'} data-sync-state={sync.state}>
  <p class="summary"><Icon name={sync.state==='current'?'check':'clock'} size={16}/>{#if !compact||sync.state!=='current'}<strong>{t(`airzone_live.state.${sync.state}`)}</strong>{/if}{#if sync.received!=null}<span>{t('airzone_live.received_ago',{time:ago(sync.received,receiptNow)})}</span>{/if}{#if sync.inProgress}<span>{t('airzone_live.in_progress')}</span>{/if}</p>
  {#if detailed}
    <dl>
      <div><dt>{t('airzone_live.attempt')}</dt><dd>{at(sync.attempted)}</dd></div>
      <div><dt>{t('airzone_live.response')}</dt><dd>{at(sync.responded)}</dd></div>
      <div><dt>{t('airzone_live.success')}</dt><dd>{at(sync.succeeded)}</dd></div>
      <div><dt>{t('airzone_live.entity_received')}</dt><dd>{at(sync.received)}</dd></div>
      <div><dt>{t('airzone_live.browser_received')}</dt><dd>{at(sync.browserReceivedAt)}</dd></div>
      <div><dt>{t('airzone_live.interval')}</dt><dd>{sync.interval==null?t('airzone_ui.unknown'):t('airzone_live.seconds',{count:sync.interval})}</dd></div>
      <div><dt>{t('airzone_live.error')}</dt><dd>{sync.error||t('airzone_live.no_reported_error')}</dd></div>
      {#if driver?.status?.retry_in_ms!=null}<div><dt>{t('airzone_live.retry')}</dt><dd>{t('airzone_live.seconds',{count:Math.ceil(driver.status.retry_in_ms/1000)})}</dd></div>{/if}
    </dl>
    <p class="help">{t('airzone_live.freshness_help')}</p>
  {/if}
</div>
<style>
.sync-status.compact {padding:0;border:0;background:transparent;font-size:.75rem;flex:1}.compact .summary {gap:4px 7px}.compact .summary strong {font-weight:500;color:var(--ink-2)}
.sync-status {min-width:0;padding:12px 16px;border:1px solid var(--line);border-radius:var(--r-sm);background:var(--surface);font-size:.875rem;color:var(--ink-2)}
.summary {display:flex;align-items:center;flex-wrap:wrap;gap:6px 10px;line-height:1.5}.summary strong {color:var(--ink);font-weight:600}.warning {background:var(--surface-2)}
dl {display:grid;gap:0;margin:12px 0 0}dl>div {display:flex;justify-content:space-between;flex-wrap:wrap;gap:6px 20px;padding:9px 0;border-top:1px solid var(--line)}dd {margin:0;color:var(--ink);overflow-wrap:anywhere}.help {margin-top:12px;line-height:1.5}
</style>
