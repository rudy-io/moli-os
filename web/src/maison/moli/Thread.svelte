<script>
  import { tick } from 'svelte';
  import { moli, openCorrection } from '../lib/moli.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Actions from './Actions.svelte';
  import MoliCards from './MoliCards.svelte';
  import Proposals from './Proposals.svelte';
  import Orb from './Orb.svelte';
  import Icon from '../ui/Icon.svelte';

  /** The conversation. `inline`: the latest answer's cards right under it
   *  (the bubble); otherwise answers point at the board (Moli's page). */
  let { inline = false } = $props();

  let list;
  const count = $derived(moli.messages.length + (moli.busy ? 1 : 0));

  $effect(() => {
    void count;
    tick().then(() => list?.lastElementChild?.scrollIntoView({ behavior: 'smooth', block: 'nearest' }));
  });
</script>

<ol class="thread" bind:this={list} aria-live="polite">
  {#each moli.messages as m, i (i)}
    {#if m.role === 'user'}
      <li class="user"><p>{#if m.spoken}<Icon name="mic" size={14} />{/if}{m.content}</p></li>
    {:else}
      <li class="moli" class:error={m.error} class:focused={!inline && moli.focus === i}>
        <Orb size={26} />
        <div class="body">
          <p>{m.content}</p>
          <Actions actions={m.actions ?? []} />
          <Proposals proposals={m.proposals ?? []} />
          {#if m.house && m.turn_id && !m.fixed}
            <p class="notit">
              {#if m.rule}<small>{t('moli.correction.appris')}</small>{/if}
              <button onclick={() => openCorrection(i)} aria-label={t('moli.correction.pas_ca_pour', { text: m.content.slice(0, 40) })}>{t('moli.correction.pas_ca')}</button>
            </p>
          {/if}
          {#if m.remembered}
            <p class="notit"><small>{t('moli.correction.retenu')}</small></p>
          {:else if m.note}
            <p class="notit"><small>{m.note}</small></p>
          {/if}
          {#if m.cards?.length}
            {#if inline && i === moli.messages.length - 1}
              <div class="cards"><MoliCards cards={m.cards} compact /></div>
            {:else if !inline}
              <button class="see" onclick={() => (moli.focus = i)} aria-pressed={moli.focus === i}>
                {moli.focus === i ? t('moli.a_lecran') : t('moli.fil.revoir', { count: m.cards.length })}
              </button>
            {/if}
          {/if}
        </div>
      </li>
    {/if}
  {/each}
  {#if moli.busy || moli.voice === 'transcribing'}
    <li class="moli pending">
      <Orb size={26} state="thinking" />
      <div class="body"><p class="dots"><i></i><i></i><i></i></p></div>
    </li>
  {/if}
</ol>

<style>
  .thread {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 14px;
  }

  li {
    display: flex;
    gap: 10px;
    animation: in 0.35s var(--ease) both;
  }

  .user {
    justify-content: flex-end;
  }

  .user p {
    max-width: 82%;
    padding: 10px 16px;
    border-radius: 20px 20px 6px 20px;
    background: var(--ink);
    color: var(--bg);
    font-weight: 550;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .moli .body {
    flex: 1;
    min-width: 0;
  }

  .moli p {
    padding-top: 2px;
    font-size: 16px;
    line-height: 1.45;
  }

  .moli.error p {
    color: var(--alert);
  }

  .moli.focused p {
    font-weight: 600;
  }

  .cards {
    margin-top: 12px;
  }

  .see {
    margin-top: 8px;
    border: 0;
    border-radius: 999px;
    padding: 6px 12px;
    background: var(--surface-2);
    color: var(--ink-2);
    font-size: 13px;
    font-weight: 650;
  }

  .see[aria-pressed='true'] {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .dots {
    display: flex;
    gap: 5px;
    padding-top: 10px;
  }

  .dots i {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ink-3);
    animation: blink 1.2s infinite;
  }

  .dots i:nth-child(2) {
    animation-delay: 0.2s;
  }

  .dots i:nth-child(3) {
    animation-delay: 0.4s;
  }

  @keyframes blink {
    50% {
      opacity: 0.25;
    }
  }

  @keyframes in {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }

  .notit {
    margin: 4px 0 0;
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .notit button {
    border: 0;
    background: transparent;
    padding: 2px 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-3);
    text-decoration: underline;
    text-underline-offset: 3px;
  }

  .notit small {
    font-size: 12px;
    color: var(--ink-3);
  }
</style>
