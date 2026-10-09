<script>
  import { onMount, onDestroy } from 'svelte';
  import { home, greeting } from '../lib/home.svelte.js';
  import { moli, ask, loadStatus, reset, suggestions } from '../lib/moli.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Thread from './Thread.svelte';
  import Composer from './Composer.svelte';
  import Orb from './Orb.svelte';
  import Icon from '../ui/Icon.svelte';
  import { talk, stopTalk } from '../lib/voice/conversation.svelte.js';

  /** The bubble: a quick question from any page. The same conversation
   *  goes on full screen on Moli's page. */
  let { onclose } = $props();

  onMount(loadStatus);

  // The sentence tells where the key goes; the link sits where the catalogue puts `{link}`.
  const asleep = $derived(t('moli.reveil.bulle').split('{link}'));

  // Closing the bubble hangs up, unless the conversation goes on full screen.
  let keep = false;
  onDestroy(() => keep || stopTalk());

  const orbState = $derived(
    talk.state !== 'off' ? talk.state : moli.voice === 'recording' ? 'listening' : moli.busy ? 'thinking' : 'idle',
  );

  function onkey(e) {
    if (e.key === 'Escape' && !home.held) onclose();
  }

  function enlarge() {
    keep = true;
    onclose();
    location.hash = '#/moli';
  }
</script>

<svelte:window onkeydown={onkey} />

<button class="scrim" onclick={onclose} aria-label={t('moli.bulle.fermer')}></button>
<div class="sheet" role="dialog" aria-modal="true" aria-label="Moli">
  <header>
    <Orb size={34} state={orbState} level={talk.level} />
    <h2>Moli</h2>
    {#if moli.messages.length}
      <button class="ghost" onclick={reset} title={t('moli.nouvelle')}>{t('moli.bulle.effacer')}</button>
    {/if}
    <button class="icon" onclick={enlarge} aria-label={t('moli.bulle.agrandir')} title={t('moli.bulle.agrandir')}><Icon name="arrow-top-right" size={20} /></button>
    <button class="icon" onclick={onclose} aria-label={t('commun.fermer')}><Icon name="close" size={20} /></button>
  </header>

  <div class="scroll">
    {#if moli.status && !moli.status.ready}
      <div class="asleep">
        <p><b>{t('moli.reveil.titre')}</b></p>
        <p class="muted">{asleep[0]}<a href="#/systeme" onclick={onclose}>{t('moli.lien_systeme')}</a>{asleep[1]}</p>
      </div>
    {:else if !moli.messages.length}
      <div class="hello">
        <p class="big">{t('moli.bulle.salut', { greeting: greeting(home.now) })}</p>
        <p class="muted">{t('moli.bulle.invitation')}</p>
        <div class="ideas">
          {#each suggestions(4) as s (s)}
            <button onclick={() => ask(s, 'bubble')}>{s}</button>
          {/each}
        </div>
      </div>
    {:else}
      <Thread inline />
    {/if}
  </div>

  <footer>
    <Composer surface="bubble" autofocus />
  </footer>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 34;
    border: 0;
    background: rgb(10 14 22 / 18%);
  }

  .sheet {
    position: fixed;
    right: 24px;
    bottom: 24px;
    z-index: 35;
    width: min(440px, calc(100vw - 32px));
    height: min(720px, calc(100dvh - 48px));
    display: grid;
    grid-template-rows: auto 1fr auto;
    grid-template-columns: minmax(0, 1fr);
    background: var(--bg);
    border-radius: var(--r-xl);
    box-shadow: var(--shadow-lift), 0 0 0 1px var(--line);
    overflow: hidden;
    animation: pop 0.35s var(--ease) both;
    transform-origin: bottom right;
  }

  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 14px 14px 10px 18px;
  }

  h2 {
    flex: 1;
    font-size: 18px;
    font-weight: 700;
  }

  .icon {
    width: 38px;
    height: 38px;
    border-radius: 50%;
    border: 0;
    background: var(--surface);
    color: var(--ink-2);
    display: grid;
    place-items: center;
  }

  .ghost {
    border: 0;
    background: none;
    color: var(--ink-3);
    font-weight: 650;
    font-size: 13px;
    padding: 8px;
  }

  .scroll {
    overflow-y: auto;
    padding: 6px 18px 18px;
    overscroll-behavior: contain;
  }

  footer {
    padding: 10px 14px 14px;
  }

  .hello {
    display: grid;
    gap: 8px;
    padding-top: 18px;
  }

  .big {
    font-size: 24px;
    font-weight: 650;
    letter-spacing: -0.01em;
  }

  .ideas {
    display: grid;
    gap: 8px;
    margin-top: 14px;
  }

  .ideas button {
    text-align: left;
    border: 0;
    border-radius: 16px;
    padding: 12px 16px;
    background: var(--surface);
    box-shadow: var(--shadow);
    font-weight: 600;
    color: var(--ink-2);
  }

  .ideas button:hover {
    color: var(--ink);
  }

  .asleep {
    display: grid;
    gap: 8px;
    padding-top: 18px;
  }

  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(16px) scale(0.96);
    }
  }

  @media (max-width: 760px) {
    .sheet {
      right: 0;
      left: 0;
      bottom: 0;
      width: 100%;
      height: 88dvh;
      border-radius: var(--r-xl) var(--r-xl) 0 0;
      transform-origin: bottom center;
    }

    footer {
      padding-bottom: calc(14px + env(safe-area-inset-bottom));
    }
  }
</style>
