<script>
  import { settleProposal } from '../lib/moli.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** What Moli proposes to change in the house, waiting for a person's say. */
  let { proposals = [] } = $props();
</script>

{#if proposals.length}
  <ul class="proposals">
    {#each proposals as p (p.id)}
      <li class={p.state}>
        {#if p.state === 'pending'}
          <Icon name="info" size={15} /><span>{p.text}</span>
          <button class="yes" onclick={() => settleProposal(p, true)}>{t('moli.proposition.confirmer')}</button>
          <button class="no" onclick={() => settleProposal(p, false)} aria-label={t('moli.proposition.annuler')}><Icon name="close" size={16} /></button>
        {:else if p.state === 'busy'}
          <Icon name="info" size={15} /><span>{p.text}</span><b>…</b>
        {:else if p.state === 'done'}
          <Icon name="power" size={15} /><span>{p.said ?? p.text}</span><b>{t('moli.proposition.fait')}</b>
        {:else if p.state === 'cancelled'}
          <Icon name="close" size={15} /><span>{p.text}</span><b>{t('moli.proposition.annule')}</b>
        {:else if p.state === 'gone'}
          <Icon name="info" size={15} /><span>{p.text}</span><b>{t('moli.proposition.perimee')}</b>
        {:else}
          <Icon name="alert" size={15} /><span>{p.text}<small>{p.error}</small></span><b>{t('moli.proposition.echec')}</b>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .proposals {
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

  .pending {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .pending span {
    color: var(--ink);
  }

  .done {
    background: var(--good-soft);
    color: var(--good);
  }

  .done span {
    color: var(--ink);
  }

  .error {
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
