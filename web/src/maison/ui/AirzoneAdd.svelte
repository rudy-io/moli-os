<script>
  import { t } from '../../lib/i18n.svelte.js';
  import { gatewayProposal } from '../lib/airzone-onboarding.js';
  import Icon from './Icon.svelte';
  let { drivers = [] } = $props();
  let id = $state('');
  let host = $state('');
  let port = $state('3000');
  let submitted = $state(false);
  const proposal = $derived(gatewayProposal({id,host,port}, drivers));
</script>

<div class="add-stack">
  <section>
    <span class="step"><Icon name="air-conditioner" size={22} /></span>
    <h2>{t('airzone_add.zone_title')}</h2>
    <p>{t('airzone_add.zone_help')}</p>
    <ol><li>{t('airzone_add.zone_step1')}</li><li>{t('airzone_add.zone_step2')}</li><li>{t('airzone_add.zone_step3')}</li></ol>
    <a href="#/airzone">{t('airzone_add.see_zones')}<Icon name="chevron-right" size={18} /></a>
  </section>
  <section>
    <h2>{t('airzone_add.restore_title')}</h2>
    <p>{t('airzone_add.restore_help')}</p>
    <a href="#/airzone/devices">{t('airzone_add.manage')}<Icon name="chevron-right" size={18} /></a>
  </section>
  <section>
    <h2>{t('airzone_add.gateway_title')}</h2>
    <p>{t('airzone_add.gateway_help')}</p>
    <details><summary>{t('airzone_add.prepare')}</summary>
      <form onsubmit={(event) => {event.preventDefault(); submitted = true;}}>
        <label>{t('airzone_add.id')}<input bind:value={id} autocomplete="off" maxlength="32" required placeholder="airzone-2" /></label>
        <label>{t('airzone_add.host')}<input bind:value={host} autocomplete="off" maxlength="253" required placeholder="controller.example" /></label>
        <label>{t('airzone_add.port')}<input bind:value={port} inputmode="numeric" maxlength="5" required /></label>
        <button type="submit">{t('airzone_add.preview')}</button>
      </form>
      {#if submitted}
        {#if proposal.error}<p class="error" role="alert">{t(`airzone_add.error.${proposal.error}`)}</p>
        {:else}<h3>{t('airzone_add.proposal')}</h3><pre><code>{proposal.config}</code></pre><p class="explanation">{t('airzone_add.apply_help')}</p>{/if}
      {/if}
    </details>
  </section>
</div>

<style>
  .add-stack {display:grid; gap:18px; max-width:850px;}
  section {background:var(--surface); border-radius:var(--r-lg); padding:26px; box-shadow:var(--shadow);}
  h2 {font-size:21px; margin:0 0 12px; letter-spacing:-.025em;}
  h3 {font-size:16px;}
  p, li {font-size:15px; line-height:1.65; color:var(--ink-2); max-width:65ch;}
  ol {padding-left:22px; display:grid; gap:9px;}
  a {display:inline-flex; align-items:center; gap:8px; color:var(--cool); min-height:44px; font-weight:600;}
  .step {display:inline-flex; margin-bottom:12px; color:var(--cool);}
  summary {cursor:pointer; min-height:44px; padding:12px 0; font-weight:600;}
  form {display:grid; grid-template-columns:1fr 1fr 100px; gap:14px; margin:12px 0;}
  label {display:grid; gap:8px; font-size:13px;}
  input {min-width:0; width:100%; background:var(--surface-2); color:var(--ink); border:1px solid var(--line); border-radius:var(--r-sm); padding:12px; font:inherit;}
  button {grid-column:1/-1; justify-self:start; min-height:44px; padding:10px 18px; border:1px solid var(--line); border-radius:var(--r-sm); background:var(--surface-2); color:var(--ink); font:inherit; cursor:pointer;}
  pre {padding:16px; background:var(--surface-2); border-radius:var(--r-sm); overflow:auto; font-size:13px;}
  .explanation {font-size:13px;}
  .error {color:var(--alert);}
  :is(a,button,input,summary):focus-visible {outline:3px solid var(--cool); outline-offset:3px;}
  @media(max-width:700px) {section {padding:20px;} form {grid-template-columns:1fr;}}
</style>
