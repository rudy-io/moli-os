<script>
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  let { gateway, connected = true, busy = false, onchoose = () => {} } = $props();
</script>

<article class="gateway-row" aria-label={gateway.instance}>
  <div class="identity"><span class="gateway-icon"><Icon name="air-conditioner" size={26} /></span><div><h3>{t('airzone_wizard.gateway_name')}</h3><p class="status"><Icon name={gateway.enabled ? 'lan' : 'pause'} size={16} />{t(!gateway.enabled ? 'airzone_wizard.connection_paused' : !connected ? 'airzone_gateways.unknown' : gateway.running ? 'airzone_wizard.connection_active' : 'airzone_gateways.stopped')}</p></div></div>
  <div class="mode"><span class="mode-label" class:authorized={!gateway.read_only}><Icon name={gateway.read_only ? 'lock' : 'tune'} size={15} />{t(gateway.read_only ? 'airzone_add.read_only_short' : 'airzone_gateways.control_allowed')}</span><p>{t(gateway.read_only ? 'airzone_gateways.read_only_summary' : 'airzone_gateways.control_summary')}</p></div>
  <button class="mode-action" type="button" aria-label={t('airzone_wizard.action_identity',{action:t(gateway.read_only ? 'airzone_gateways.allow_control' : 'airzone_gateways.restore_read_only'),gateway:t('airzone_wizard.gateway_name'),address:gateway.host})} disabled={busy || !connected || (gateway.read_only && !gateway.enabled)} onclick={() => onchoose('mode')}><Icon name={gateway.read_only ? 'tune' : 'lock'} size={18} />{t(gateway.read_only ? 'airzone_gateways.allow_control' : 'airzone_gateways.restore_read_only')}</button>
  {#if gateway.read_only && !gateway.enabled}<p class="explanation">{t('airzone_gateways.resume_before_control')}</p>{/if}
  {#if !gateway.read_only && gateway.enabled}<a class="control-link" href="#/pieces">{t('airzone_gateways.open_controls')}<Icon name="chevron-right" size={17} /></a>{/if}
  <button class="manage" type="button" aria-label={t('airzone_wizard.action_identity',{action:t(gateway.enabled ? 'airzone_wizard.manage_button' : 'airzone_gateways.resume'),gateway:t('airzone_wizard.gateway_name'),address:gateway.host})} disabled={busy || !connected} onclick={() => onchoose('edit')}>{t(gateway.enabled ? 'airzone_wizard.manage_button' : 'airzone_gateways.resume')}<Icon name="chevron-right" size={18} /></button>
  <details><summary>{t('airzone_wizard.gateway_settings')}</summary>
    <dl><div><dt>{t('airzone_add.id')}</dt><dd>{gateway.instance}</dd></div><div><dt>{t('airzone_add.host')}</dt><dd>{gateway.host}:{gateway.port}</dd></div></dl>
    <p class="source">{t(gateway.source === 'moli' ? 'airzone_gateways.source_moli' : 'airzone_gateways.source_configuration')}</p>
    {#if !gateway.enabled}<p class="explanation">{t('airzone_gateways.paused_help')}</p>{:else if gateway.running}<p class="explanation">{t('airzone_gateways.running_help')}</p>{/if}
    <div class="actions">{#if gateway.enabled}<button type="button" disabled={busy || !connected} onclick={() => onchoose('withdraw')}>{t('airzone_gateways.withdraw')}</button>{/if}<button type="button" disabled={busy || !connected} onclick={() => onchoose('replace')}>{t('airzone_gateways.replace')}</button></div>
  </details>
</article>

<style>
  .gateway-row {padding:22px;border:1px solid var(--line);border-radius:var(--r-md);min-width:0;}
  .identity {display:flex;align-items:center;gap:14px;}.identity>div {min-width:0;}.gateway-icon {display:grid;place-items:center;width:46px;height:46px;border-radius:14px;background:var(--surface-3);color:var(--cool);}
  h3 {font-size:1.125rem;margin:0;letter-spacing:-.02em;overflow-wrap:anywhere;}.status {display:flex;align-items:center;gap:6px;margin:7px 0 0;color:var(--ink-2);font-size:.8125rem;}
  .mode {color:var(--ink-2);font-size:.8125rem;margin:16px 0;}.mode-label {display:inline-flex;align-items:center;gap:7px;font-weight:650;}.mode-label.authorized {color:var(--cool);}.mode p {margin:8px 0 0;line-height:1.55;max-width:65ch;}
  button {min-height:46px;padding:11px 14px;border:1px solid var(--line);border-radius:var(--r-sm);background:var(--surface-2);color:var(--ink);font:inherit;font-size:.875rem;font-weight:600;cursor:pointer;transition:background 150ms;}
  .manage {display:flex;align-items:center;justify-content:space-between;gap:12px;width:100%;margin-top:10px;}.actions {display:flex;gap:8px;flex-wrap:wrap;margin-top:16px;}.mode-action {display:flex;align-items:center;justify-content:center;gap:9px;width:100%;background:var(--cool);border-color:transparent;color:var(--paper);}.control-link {min-height:44px;display:flex;align-items:center;justify-content:center;gap:6px;color:var(--cool);font-size:.875rem;font-weight:600;text-decoration:none;margin-top:6px;}
  summary {cursor:pointer;min-height:44px;padding:12px 0;font-size:.8125rem;color:var(--ink-2);margin-top:8px;}
  dl {display:grid;gap:12px;margin:8px 0;}dt,.source {font-size:.75rem;color:var(--ink-2);}dd {margin:4px 0 0;font-size:.875rem;overflow-wrap:anywhere;}
  .explanation {font-size:.8125rem;line-height:1.55;color:var(--ink-2);max-width:65ch;}.source {margin:16px 0 0;}
  button:disabled {opacity:.55;cursor:not-allowed;}:is(button,summary,a):focus-visible {outline:3px solid var(--cool);outline-offset:3px;}
  @media(hover:hover) {button:not(.mode-action):hover:not(:disabled) {background:var(--surface-3);}.mode-action:hover:not(:disabled) {filter:brightness(1.08);}.control-link:hover {text-decoration:underline;}}
  @media(max-width:600px) {.gateway-row {padding:18px;}.actions button {width:100%;}}
  @media(prefers-reduced-motion:reduce) {button {transition:none;}}
</style>
