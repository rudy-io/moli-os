<script>
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  let { report, busy = false, connected = true, onchoose = () => {} } = $props();
  const model = (candidate) => candidate.gateway_type ? t(`airzone_add.model.${candidate.gateway_type}`) : t('airzone_wizard.gateway');
  const actionLabel = (action, candidate) => t('airzone_wizard.action_identity', {action: t(action), gateway: model(candidate), address: candidate.host});
</script>

<div class="results" aria-busy={busy}>
  {#if report?.incomplete}<p class="hint" role="status">{t('airzone_wizard.incomplete')}</p>{/if}
  {#if report?.gateways.length}
    <ul>
      {#each report.gateways as candidate (candidate.mac)}
        <li>
          <div class="identity"><span class="device-icon"><Icon name="air-conditioner" size={26} /></span><div><h3>{model(candidate)}</h3><p>{t('airzone_wizard.zone_count',{count:candidate.zones})}</p></div></div>
          {#if candidate.existing_instance}
            <span class="known"><Icon name={candidate.existing_enabled ? 'ok' : 'pause'} size={16} />{t(candidate.existing_enabled ? 'airzone_wizard.already_added' : 'airzone_wizard.already_paused')}</span>
          {/if}
          <details><summary>{t('airzone_wizard.identify')}</summary><dl><div><dt>{t('airzone_add.host')}</dt><dd>{candidate.host}:{candidate.port}</dd></div><div><dt>{t('airzone_wizard.hardware_id')}</dt><dd>{candidate.mac.match(/.{2}/g).join(':')}</dd></div>{#if candidate.existing_instance}<div><dt>{t('airzone_add.id')}</dt><dd>{candidate.existing_instance}</dd></div>{/if}</dl></details>
          {#if candidate.existing_instance}
            <a href={candidate.existing_enabled ? '#/airzone' : '#/airzone/installation'} aria-label={actionLabel(candidate.existing_enabled ? 'airzone_wizard.see_zones' : 'airzone_wizard.manage_paused', candidate)}>{t(candidate.existing_enabled ? 'airzone_wizard.see_zones' : 'airzone_wizard.manage_paused')}<Icon name="chevron-right" size={18} /></a>
          {:else}
            <button type="button" aria-label={actionLabel('airzone_wizard.choose_button', candidate)} disabled={busy || !connected} onclick={() => onchoose(candidate)}>{t('airzone_wizard.choose_button')}<Icon name="chevron-right" size={18} /></button>
          {/if}
        </li>
      {/each}
    </ul>
  {:else}
    <div class="empty" role="status"><Icon name="search" size={28} /><h3>{t('airzone_wizard.empty_title')}</h3><p>{t('airzone_wizard.empty_help')}</p></div>
  {/if}
</div>

<style>
  ul {list-style:none;margin:0;padding:0;display:grid;gap:12px;}
  li {min-width:0;padding:20px;border:1px solid var(--line);border-radius:var(--r-md);background:var(--surface-2);}
  .identity {display:flex;align-items:center;gap:14px;}.device-icon {color:var(--cool);display:flex;}
  h3 {margin:0;font-size:1.0625rem;letter-spacing:-.015em;overflow-wrap:anywhere;}p {margin:6px 0 0;color:var(--ink-2);font-size:.875rem;line-height:1.5;}
  .known {display:inline-flex;align-items:center;gap:6px;font-size:.8125rem;color:var(--ink-2);margin-top:14px;}
  summary {cursor:pointer;min-height:44px;padding:12px 0;font-size:.8125rem;color:var(--ink-2);}
  details {margin-top:4px;}dl {margin:0 0 12px;display:grid;gap:10px;}dt {color:var(--ink-2);font-size:.75rem;}dd {margin:4px 0 0;font-size:.875rem;overflow-wrap:anywhere;}
  button,a {display:flex;align-items:center;justify-content:center;gap:8px;min-height:46px;padding:10px 14px;width:100%;background:var(--surface);color:var(--ink);border:1px solid var(--line);border-radius:var(--r-sm);font:inherit;font-size:.875rem;font-weight:600;text-decoration:none;cursor:pointer;}
  button:disabled {opacity:.55;cursor:not-allowed;}:is(button,a,summary):focus-visible {outline:3px solid var(--cool);outline-offset:3px;}
  button:active:not(:disabled) {background:var(--surface-3);}.empty {text-align:center;padding:16px 12px;color:var(--ink-2);}.empty h3 {color:var(--ink);margin:14px 0 8px;}.empty p {max-width:42ch;margin:auto;}
  .hint {font-size:.875rem;line-height:1.5;padding:14px;border-radius:var(--r-sm);background:var(--surface-2);margin:0 0 16px;}
  @media(max-width:600px) {li {padding:16px;}}
</style>
