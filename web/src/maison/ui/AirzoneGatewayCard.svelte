<script>
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  let { gateway, connected = true, busy = false, onchoose = () => {} } = $props();
</script>

<article class="gateway-row" aria-label={gateway.instance}>
  <div class="identity"><div><h3>{gateway.instance}</h3><p class="address">{gateway.host}:{gateway.port}</p></div><span class="status"><Icon name={gateway.enabled ? 'lan' : 'pause'} size={17} />{t(!gateway.enabled ? 'airzone_gateways.paused' : !connected ? 'airzone_gateways.unknown' : gateway.running ? 'airzone_gateways.running' : 'airzone_gateways.stopped')}</span></div>
  <div class="facts"><span><Icon name={gateway.read_only ? 'lock' : 'tune'} size={15} />{t(gateway.read_only ? 'airzone_gateways.current_read_only' : 'airzone_gateways.current_writable')}</span><span>{t(gateway.source === 'moli' ? 'airzone_gateways.source_moli' : 'airzone_gateways.source_configuration')}</span></div>
  {#if !gateway.enabled}<p class="explanation">{t('airzone_gateways.paused_help')}</p>{:else if gateway.running}<p class="explanation">{t('airzone_gateways.running_help')}</p>{/if}
  <div class="actions"><button type="button" disabled={busy || !connected} onclick={() => onchoose('edit')}>{t(gateway.enabled ? 'airzone_gateways.edit' : 'airzone_gateways.resume')}</button>{#if gateway.enabled}<button type="button" disabled={busy || !connected} onclick={() => onchoose('withdraw')}>{t('airzone_gateways.withdraw')}</button>{/if}<button type="button" disabled={busy || !connected} onclick={() => onchoose('replace')}>{t('airzone_gateways.replace')}</button></div>
</article>

<style>
  .gateway-row {padding:20px;border:1px solid var(--line);border-radius:var(--r-md);min-width:0;}
  .identity {display:flex;align-items:flex-start;justify-content:space-between;gap:16px;}
  .identity>div {min-width:0;}
  h3 {font-size:18px;margin:0;letter-spacing:-.02em;overflow-wrap:anywhere;}
  .address {margin:5px 0 0;font-size:14px;color:var(--ink-2);overflow-wrap:anywhere;}
  .status {display:inline-flex;align-items:center;gap:6px;flex:none;font-size:12px;color:var(--ink-2);padding:6px 9px;background:var(--surface-2);border-radius:var(--r-sm);}
  .facts {display:flex;flex-wrap:wrap;gap:8px 16px;margin-top:16px;color:var(--ink-2);font-size:12px;}
  .facts span {display:inline-flex;gap:6px;align-items:center;}
  .explanation {font-size:13px;line-height:1.5;color:var(--ink-2);max-width:65ch;margin:12px 0 0;}
  .actions {display:flex;gap:8px;flex-wrap:wrap;margin-top:16px;}
  button {min-height:44px;padding:10px 12px;border:1px solid var(--line);border-radius:var(--r-sm);background:var(--surface-2);color:var(--ink);font:inherit;font-size:13px;font-weight:600;cursor:pointer;transition:background 150ms;}
  button:disabled {opacity:.55;cursor:not-allowed;}
  button:focus-visible {outline:3px solid var(--cool);outline-offset:3px;}
  @media(hover:hover) {button:hover:not(:disabled) {background:var(--surface-3);}}
  @media(max-width:600px) {.gateway-row {padding:16px;}.identity {flex-direction:column;gap:10px;}.actions button {flex:1 1 auto;}}
  @media(prefers-reduced-motion:reduce) {button {transition:none;}}
</style>
