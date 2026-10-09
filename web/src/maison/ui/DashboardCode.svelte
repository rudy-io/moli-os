<script>
  import { hub, home } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The house's code: a new one, typed twice in the code sheet, freely by
   *  an owner (Cloudflare Access proves the e-mail), after the current code
   *  for anyone else. Moli keeps it encrypted; no agent can change it. */
  let done = $state(false);

  function choose() {
    done = false;
    home.held = {
      label: t('systeme.code.titre'),
      reason: 'code',
      newCode: true,
      custom: async () => {
        done = true;
      },
      onCancel: () => {},
    };
  }
</script>

{#if hub.session?.setup}
  <section class="card code">
    <span class="icon"><Icon name="lock" size={20} /></span>
    <div class="what">
      <b>{t('systeme.code.titre')}</b>
      <span class="muted">{t('systeme.code.pas_de_code')}</span>
    </div>
    <button class="primary" onclick={() => (home.setupLater = false)}>{t('systeme.code.terminer')}</button>
  </section>
{:else if hub.session?.can_change}
  <section class="card code">
    <span class="icon"><Icon name="lock" size={20} /></span>
    <div class="what">
      <b>{t('systeme.code.titre')}</b>
      <span class="muted">{hub.session?.owner ? t('systeme.code.demande_proprietaire') : t('systeme.code.demande_autre')}</span>
      {#if done}<span class="good">{t('systeme.code.enregistre')}</span>{/if}
    </div>
    <button class="primary" onclick={choose}>{hub.session?.pin_configured ? t('systeme.code.changer') : t('systeme.code.choisir')}</button>
  </section>
{/if}

<style>
  .code {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px 16px;
  }

  .icon {
    display: grid;
    place-items: center;
    width: 42px;
    height: 42px;
    border-radius: 50%;
    background: var(--cool-soft);
    color: var(--cool);
  }

  .what {
    display: grid;
    flex: 1;
    min-width: 220px;
    gap: 2px;
    font-size: 14px;
  }

  .good {
    color: var(--good);
    font-weight: 650;
  }

  .primary {
    min-height: 42px;
    padding: 0 18px;
    border: none;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }
</style>
