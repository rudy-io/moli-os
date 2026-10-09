<script>
  import { onMount } from 'svelte';
  import { home, greeting, longDate } from '../lib/home.svelte.js';
  import { moli, ask, loadStatus, reset, suggestions } from '../lib/moli.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Thread from '../moli/Thread.svelte';
  import Composer from '../moli/Composer.svelte';
  import MoliCards from '../moli/MoliCards.svelte';
  import Orb from '../moli/Orb.svelte';
  import { talk } from '../lib/voice/conversation.svelte.js';
  import Icon from '../ui/Icon.svelte';

  // The agentic dashboard: no fixed layout. You say what you want, the
  // screen composes itself with exactly that, and stays live.
  let narrow = $state(false);

  onMount(() => {
    loadStatus();
    const media = matchMedia('(max-width: 980px)');
    narrow = media.matches;
    const update = (e) => (narrow = e.matches);
    media.addEventListener('change', update);
    return () => media.removeEventListener('change', update);
  });

  const focused = $derived(moli.focus != null ? moli.messages[moli.focus] : null);
  const orbState = $derived(
    talk.state !== 'off'
      ? talk.state
      : moli.voice === 'recording'
        ? 'listening'
        : moli.busy || moli.voice === 'transcribing'
          ? 'thinking'
          : 'idle',
  );
  const date = $derived(longDate(home.now));

  // What Moli does: icon, then the keys of its title and its examples.
  const SKILLS = [
    ['power', 'moli.page.agir'],
    ['tune', 'moli.page.montrer'],
    ['bolt', 'moli.page.expliquer'],
  ];

  // The sentence tells where the key goes; the link sits where the catalogue puts `{link}`.
  const asleep = $derived(t('moli.reveil.page').split('{link}'));
</script>

<div class="moli-page" class:started={moli.messages.length}>
  {#if !moli.messages.length}
    <section class="hero">
      <Orb size={132} state={orbState} level={talk.level} />
      <p class="date">{date}</p>
      <h1>{t('moli.page.titre', { greeting: greeting(home.now) })}</h1>
      {#if moli.status && !moli.status.ready}
        <p class="asleep">
          {asleep[0]}<a href="#/systeme">{t('moli.lien_systeme')}</a>{asleep[1]}
        </p>
      {:else}
        <div class="composer"><Composer surface="page" big autofocus placeholder={t('moli.page.placeholder')} /></div>
        <div class="ideas">
          {#each suggestions(6) as s (s)}
            <button onclick={() => ask(s, 'page')}>{s}</button>
          {/each}
        </div>
      {/if}
      <ul class="skills">
        {#each SKILLS as [icon, skill] (icon)}
          <li><span><Icon name={icon} size={20} /></span><b>{t(`${skill}_titre`)}</b><small>{t(`${skill}_texte`)}</small></li>
        {/each}
      </ul>
      <p class="trust"><Icon name="lock" size={15} />{t('moli.page.confiance')}</p>
    </section>
  {:else}
    <section class="talk">
      <header>
        <Orb size={34} state={orbState} level={talk.level} />
        <h1>Moli</h1>
        <button class="ghost" onclick={reset}>{t('moli.nouvelle')}</button>
      </header>
      <div class="scroll"><Thread inline={narrow} /></div>
      <div class="dock"><Composer surface="page" autofocus /></div>
    </section>

    {#if !narrow}
      <section class="board" aria-label={t('moli.a_lecran')}>
        {#if focused?.cards?.length}
          <p class="caption">{focused.content}</p>
          {#key moli.focus}
            <MoliCards cards={focused.cards} />
          {/key}
        {:else}
          <div class="empty">
            <Orb size={64} state={orbState} level={talk.level} />
            <p>{t('moli.page.vide')}</p>
          </div>
        {/if}
      </section>
    {/if}
  {/if}
</div>

<style>
  .moli-page {
    min-height: calc(100dvh - 56px);
  }

  /* ---- before the first question ---- */
  .hero {
    max-width: 760px;
    margin: 0 auto;
    padding: 6vh 0 40px;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    justify-items: center;
    text-align: center;
    gap: 14px;
  }

  .date {
    margin-top: 14px;
    color: var(--ink-3);
    font-weight: 650;
  }

  h1 {
    font-size: clamp(28px, 4vw, 42px);
    font-weight: 700;
    letter-spacing: -0.03em;
    line-height: 1.1;
  }

  .composer {
    width: 100%;
    margin-top: 18px;
  }

  .ideas {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    margin-top: 4px;
  }

  .ideas button {
    border: 0;
    border-radius: 999px;
    padding: 10px 16px;
    background: var(--surface);
    box-shadow: var(--shadow);
    color: var(--ink-2);
    font-weight: 600;
    font-size: 14px;
    transition: transform 0.15s var(--ease), color 0.15s;
  }

  .ideas button:hover {
    transform: translateY(-1px);
    color: var(--ink);
  }

  .skills {
    list-style: none;
    margin: 36px 0 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 14px;
    width: 100%;
    text-align: left;
  }

  .skills li {
    display: grid;
    gap: 4px;
    padding: 16px;
    border-radius: var(--r-md);
    background: color-mix(in srgb, var(--surface) 60%, transparent);
  }

  .skills span {
    width: 38px;
    height: 38px;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-2);
    margin-bottom: 6px;
  }

  .skills small {
    color: var(--ink-3);
    font-size: 13px;
  }

  .trust {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 10px;
    color: var(--ink-3);
    font-size: 13px;
    max-width: 560px;
  }

  .asleep {
    color: var(--ink-2);
    max-width: 520px;
  }

  code {
    font-size: 13px;
    background: var(--surface-2);
    padding: 2px 6px;
    border-radius: 6px;
  }

  /* ---- the conversation and its board ---- */
  .started {
    display: grid;
    grid-template-columns: minmax(340px, 420px) minmax(0, 1fr);
    gap: 28px;
    align-items: start;
  }

  .talk {
    position: sticky;
    top: 20px;
    height: calc(100dvh - 48px);
    display: grid;
    grid-template-rows: auto 1fr auto;
    background: var(--surface);
    border-radius: var(--r-xl);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  .talk header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 16px 16px 8px 20px;
  }

  .talk h1 {
    flex: 1;
    font-size: 20px;
    letter-spacing: -0.01em;
  }

  .ghost {
    border: 0;
    background: none;
    color: var(--ink-3);
    font-weight: 650;
    font-size: 13px;
  }

  .scroll {
    overflow-y: auto;
    padding: 8px 20px 20px;
  }

  .dock {
    padding: 10px 14px 14px;
    background: linear-gradient(transparent, var(--surface) 30%);
  }

  .dock :global(form.composer) {
    background: var(--surface-2);
  }

  .board {
    display: grid;
    gap: 16px;
    padding-top: 6px;
  }

  .caption {
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.01em;
    max-width: 720px;
  }

  .empty {
    min-height: 60dvh;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 14px;
    color: var(--ink-3);
    font-weight: 600;
  }

  @media (max-width: 980px) {
    .started {
      display: block;
    }

    .talk {
      position: static;
      height: auto;
      min-height: calc(100dvh - 180px);
      background: none;
      box-shadow: none;
      overflow: visible;
    }

    .talk header,
    .scroll {
      padding-left: 0;
      padding-right: 0;
    }

    .dock {
      position: sticky;
      bottom: 76px;
      padding: 10px 0 0;
      background: linear-gradient(transparent, var(--bg) 35%);
    }

    .dock :global(form.composer) {
      background: var(--surface);
    }

    .skills {
      grid-template-columns: 1fr;
    }
  }
</style>
