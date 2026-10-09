<script>
  import { actionState, settle } from '../lib/moli.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** What Moli did during an answer, and the orders waiting for an adult. */
  let { actions = [] } = $props();

  // The guard words its reason in the house's language: « calmes » or « quiet ».
  const why = (a) => (/calme|quiet/i.test(a.reason ?? '') ? t('moli.action.calmes') : t('moli.action.protegee'));
  const say = (v) =>
    v === true ? t('moli.action.allume') : v === false ? t('moli.action.eteint') : typeof v === 'number' ? t('moli.action.regle', { value: v }) : t('moli.action.valeur', { value: v });
</script>

{#if actions.length}
  <ul class="actions">
    {#each actions as a, i (i)}
      {@const state = actionState(a)}
      {@const label = t('moli.action.libelle', { device: a.device, value: say(a.value) })}
      <li class={state}>
        {#if state === 'done' || state === 'approved'}
          <Icon name="power" size={15} /><span>{label}</span><b>{state === 'approved' ? t('moli.action.autorise') : t('moli.action.fait')}</b>
        {:else if state === 'held'}
          <Icon name="lock" size={15} />
          <span>{label}<small>{t('moli.action.accord', { reason: why(a) })}</small></span>
          <button class="yes" onclick={() => settle(a, true, label)}>{t('moli.action.autoriser')}</button>
          <button class="no" onclick={() => settle(a, false, label)} aria-label={t('moli.action.refuser')}><Icon name="close" size={16} /></button>
        {:else if state === 'denied'}
          <Icon name="close" size={15} /><span>{label}</span><b>{t('moli.action.refuse')}</b>
        {:else if state === 'settled'}
          <Icon name="info" size={15} /><span>{label}</span><b>{t('moli.action.plus_en_attente')}</b>
        {:else}
          <Icon name="alert" size={15} /><span>{label}<small>{a.error ?? t('moli.action.echec')}</small></span><b>{t('moli.action.rate')}</b>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .actions {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px 8px 12px;
    border-radius: 14px;
    background: var(--surface-2);
    font-size: 14px;
  }

  li span {
    flex: 1;
    display: grid;
    min-width: 0;
  }

  small {
    font-size: 12px;
    color: var(--ink-3);
  }

  b {
    font-size: 12px;
    font-weight: 700;
    color: var(--ink-3);
  }

  .done,
  .approved {
    background: var(--good-soft);
    color: var(--good);
  }

  .done span,
  .approved span {
    color: var(--ink);
  }

  .held {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .held span {
    color: var(--ink);
  }

  .failed {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .yes {
    border: 0;
    border-radius: 999px;
    padding: 7px 14px;
    background: var(--cool);
    color: #fff;
    font-weight: 700;
    font-size: 13px;
  }

  .no {
    border: 0;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    background: var(--surface);
    color: var(--ink-3);
    display: grid;
    place-items: center;
  }
</style>
