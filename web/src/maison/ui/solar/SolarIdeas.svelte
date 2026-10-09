<script>
  import Icon from '../Icon.svelte';
  import { ico } from './solar-icons.js';
  import { pts, eurDelta, ideaWords } from './format.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** Improvements, each computed by running the simulation with it: first
   *  what costs nothing (Moli steering a load), then what needs buying (a
   *  battery), best gain first in each. `onapply(idea)` tries it. */
  let { ideas = [], piloted = false, onapply } = $props();

  const free = $derived(ideas.filter((i) => i.type === 'load'));
  const gear = $derived(ideas.filter((i) => i.type !== 'load'));
</script>

{#if !ideas.length}
  <p class="muted">{t('energie.solaire.idees.rien')}</p>
{:else}
  {#if !free.length && piloted}
    <p class="done"><Icon name="ok" size={18} />{t('energie.solaire.idees.deja')}</p>
  {/if}
  {#each [[t('energie.solaire.idees.gratuit'), free, 'free'], [t('energie.solaire.idees.equipement'), gear, 'gear']] as [title, list, kind] (kind)}
    {#if list.length}
      <h3 class="group">{title}</h3>
      <ol class="ideas">
        {#each list as idea, i (idea.id)}
          {@const words = ideaWords(idea)}
          <li class:first={i === 0 && kind === 'free'}>
            <span class="rank" aria-hidden="true">{i + 1}</span>
            <div class="body">
              <div class="head">
                <span class="badge"><Icon path={ico(idea.icon)} size={20} /></span>
                <b>{words.title}</b>
                {#if idea.hypothetical}<span class="chip warm">{t('energie.solaire.hypothetique')}</span>{/if}
              </div>
              <p class="gains">
                <span class="chip good">{t('energie.solaire.par_mois', { eur: eurDelta(idea.gain) })}</span>
                <span class="chip">{t('energie.solaire.pts_autoconsommation', { pts: pts(idea.points) })}</span>
                <span class="chip">{t('energie.solaire.pts_autoproduction', { pts: pts(idea.autoprod) })}</span>
              </p>
              <p class="text">{words.text}</p>
              <p class="note"><Icon name="info" size={14} />{words.note}</p>
            </div>
            <button class="try" onclick={() => onapply?.(idea)}>{idea.type === 'load' ? t('energie.solaire.idees.piloter') : t('energie.solaire.idees.simuler')}</button>
          </li>
        {/each}
      </ol>
    {/if}
  {/each}
{/if}

<style>
  .done {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 16px;
    font-size: 14px;
    font-weight: 600;
    color: var(--self);
  }

  .group {
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--ink-3);
    margin: 4px 0 10px;
  }

  .group:not(:first-child) {
    margin-top: 20px;
  }

  .ideas {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 10px;
  }

  .ideas li {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: 14px;
    align-items: start;
    padding: 16px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .ideas li.first {
    background: color-mix(in srgb, var(--self) 8%, var(--surface-2));
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--self) 40%, transparent);
  }

  .rank {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    font-size: 13px;
    font-weight: 750;
    background: var(--surface);
    color: var(--ink-2);
  }

  .first .rank {
    background: var(--self);
    color: #fff;
  }

  .body {
    display: grid;
    gap: 8px;
    min-width: 0;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 10px;
    font-size: 15.5px;
    line-height: 1.3;
  }

  .head b {
    font-weight: 700;
  }

  .badge {
    width: 32px;
    height: 32px;
    border-radius: 10px;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-2);
  }

  .gains {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .gains .chip {
    padding: 4px 10px;
    font-size: 12.5px;
  }

  .gains .chip:not(.good) {
    background: var(--surface);
  }

  .text {
    font-size: 13.5px;
    line-height: 1.45;
    color: var(--ink-2);
  }

  .note {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    font-size: 12px;
    line-height: 1.4;
    color: var(--ink-3);
  }

  .note :global(svg) {
    margin-top: 1px;
  }

  .try {
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    font-weight: 700;
    font-size: 13.5px;
    background: var(--ink);
    color: var(--surface);
    transition: transform 0.2s var(--ease);
  }

  .try:hover {
    transform: translateY(-1px);
  }

  @media (max-width: 560px) {
    .ideas li {
      grid-template-columns: auto minmax(0, 1fr);
    }

    .try {
      grid-column: 2;
      justify-self: start;
    }
  }
</style>
