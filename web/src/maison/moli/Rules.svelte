<script>
  import { onMount } from 'svelte';
  import { rules, loadRules, showRules, switchRule, removeRule } from '../lib/moli.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** What Moli learned from « Pas ça »: each sentence and what it now does. A person switches a rule
   *  off or deletes it. */
  onMount(loadRules);
  const day = (ms) => new Date(ms).toLocaleDateString(locale(), { day: 'numeric', month: 'short' });
</script>

<section class="learned">
  <h3>{t('moli.correction.regles_titre')}</h3>
  {#if rules.locked}
    <p class="hint">{t('moli.correction.regles_code')}</p>
    <button class="show" onclick={showRules}>{t('moli.correction.regles_voir')}</button>
  {:else if rules.list === null}
    <p class="hint">…</p>
  {:else if rules.list.length === 0}
    <p class="hint">{t('moli.correction.regles_vide')}</p>
  {:else}
    <ul>
      {#each rules.list as r (r.id)}
        <li class:off={!r.enabled}>
          <span class="said">« {r.said} »</span>
          <span class="does">{r.words}{#if r.orphan}<b class="orphan"> · {t('moli.correction.orpheline')}</b>{/if}</span>
          <span class="when">{day(r.created)}</span>
          <label class="switch"><input type="checkbox" checked={r.enabled} onchange={() => switchRule(r)} aria-label={t('moli.correction.active')} /></label>
          <button class="del" onclick={() => removeRule(r)} aria-label={t('moli.correction.supprimer')}><Icon name="trash" size={16} /></button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .learned {
    display: grid;
    gap: 8px;
  }

  h3 {
    font-size: 16px;
    font-weight: 700;
  }

  .hint {
    color: var(--ink-3);
    font-size: 14px;
  }

  .show {
    justify-self: start;
    padding: 8px 14px;
    border: 0;
    border-radius: 999px;
    background: var(--surface-2);
    font-weight: 600;
    font-size: 14px;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  li {
    display: grid;
    grid-template-columns: 1fr auto auto;
    grid-template-areas: 'said when switch' 'does does del';
    gap: 2px 10px;
    align-items: center;
    padding: 10px 12px;
    border-radius: 14px;
    background: var(--surface-2);
    font-size: 14px;
  }

  li.off {
    opacity: 0.55;
  }

  .said {
    grid-area: said;
    font-weight: 600;
  }

  .does {
    grid-area: does;
    color: var(--ink-2);
  }

  .when {
    grid-area: when;
    color: var(--ink-3);
    font-size: 12px;
  }

  .switch {
    grid-area: switch;
  }

  .del {
    grid-area: del;
    justify-self: end;
    border: 0;
    background: transparent;
    color: var(--ink-3);
  }

  .orphan {
    color: var(--alert);
  }
</style>
