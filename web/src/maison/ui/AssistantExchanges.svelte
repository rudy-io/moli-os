<script>
  import { onMount } from 'svelte';
  import { home } from '../lib/home.svelte.js';
  import { t, i18n } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** What the house asked Moli lately: the day's counts, then each exchange
   *  (newest first), from the satellite or the app. The same as the evening
   *  recap on Telegram, which can be sent from here to try it. */
  let hours = $state(24);
  let all = $state(null);
  let shown = $state(20);
  let sending = $state(false);
  let sent = $state('');

  async function load() {
    try {
      const res = await fetch(`/api/assistant/exchanges?hours=${hours}`);
      if (res.ok) all = (await res.json()).exchanges;
    } catch {
      /* the card stays as it was */
    }
  }
  onMount(load);

  function period(h) {
    hours = h;
    shown = 20;
    load();
  }

  const turns = $derived((all ?? []).filter((e) => e.kind !== 'silence'));
  const silences = $derived((all ?? []).length - turns.length);
  const orders = $derived(turns.reduce((n, e) => n + (e.orders?.length ?? 0), 0));
  const searches = $derived(turns.filter((e) => e.tools?.includes('web_search')).length);
  const speed = $derived.by(() => {
    const ok = turns.filter((e) => e.kind === 'turn');
    if (!ok.length) return null;
    const mean = ok.reduce((s, e) => s + e.ms, 0) / ok.length / 1000;
    return mean.toLocaleString(i18n.language, { maximumFractionDigits: 1 });
  });

  function when(at) {
    const d = new Date(at);
    const time = d.toLocaleTimeString(i18n.language, { hour: '2-digit', minute: '2-digit' });
    if (d.toDateString() === new Date().toDateString()) return time;
    return `${d.toLocaleDateString(i18n.language, { weekday: 'short' })} ${time}`;
  }

  async function sendRecap() {
    sending = true;
    sent = '';
    try {
      const res = await fetch('/api/assistant/recap', { method: 'POST', headers: { 'x-moli-origin': 'ui' } });
      if (res.status === 403) {
        home.held = { label: t('moli.echanges.recap'), reason: 'ton code', custom: sendRecap };
        return;
      }
      const body = res.ok ? await res.json() : { sent: false };
      sent = body.sent ? t('moli.echanges.recap_ok') : t('moli.echanges.recap_rien');
    } catch {
      sent = t('moli.echanges.recap_rien');
    } finally {
      sending = false;
    }
  }
</script>

{#if all}
  <section class="card">
    <div class="row">
      <span class="ico"><Icon name="talk" size={18} /></span>
      <div class="text">
        <b>{t('moli.echanges.titre')}</b>
        <span class="muted">
          {t('moli.echanges.resume', { turns: turns.length, orders, searches })}{#if silences} · {t('moli.echanges.silences', { n: silences })}{/if}{#if speed} · {t('moli.echanges.vitesse', { s: speed })}{/if}
        </span>
      </div>
      <div class="tones" role="radiogroup" aria-label={t('moli.echanges.periode')}>
        <button role="radio" aria-checked={hours === 24} class:on={hours === 24} onclick={() => period(24)}>{t('moli.echanges.jour')}</button>
        <button role="radio" aria-checked={hours === 168} class:on={hours === 168} onclick={() => period(168)}>{t('moli.echanges.semaine')}</button>
      </div>
    </div>

    {#if !all.length}
      <p class="muted">{t('moli.echanges.aucun')}</p>
    {:else}
      <ol class="list">
        {#each all.slice(0, shown) as e (e.at + e.kind + e.question)}
          <li class:quiet={e.kind === 'silence'} class:failed={e.kind === 'failed'}>
            <span class="when">{when(e.at)}</span>
            <span class="where">{e.surface === 'satellite' ? t('moli.echanges.boitier') : t('moli.echanges.appli')}</span>
            {#if e.kind === 'silence'}
              <span class="what muted">{t('moli.echanges.silence')}</span>
            {:else}
              <span class="what">
                <span class="q">{e.question}</span>
                <span class="a">
                  {#if e.kind === 'failed'}{t('moli.echanges.echec', { error: e.reply })}{:else if e.reply}{e.reply}{:else}{t('moli.echanges.fin')}{/if}
                </span>
                {#if e.orders?.length || e.tools?.includes('web_search')}
                  <span class="tags">
                    {#each e.orders ?? [] as o, i (i)}<span class="tag" class:held={o.status !== 'done'}>{o.device}</span>{/each}
                    {#if e.tools?.includes('web_search')}<span class="tag">{t('moli.echanges.recherche')}</span>{/if}
                  </span>
                {/if}
              </span>
              <span class="ms muted">{(e.ms / 1000).toLocaleString(i18n.language, { maximumFractionDigits: 1 })} s</span>
            {/if}
          </li>
        {/each}
      </ol>
      {#if all.length > shown}
        <button class="btn" onclick={() => (shown += 40)}>{t('moli.echanges.plus')}</button>
      {/if}
    {/if}

    <div class="row foot">
      <button class="btn" disabled={sending} onclick={sendRecap}>{t('moli.echanges.recap')}</button>
      {#if sent}<small class="muted">{sent}</small>{/if}
    </div>
  </section>
{/if}

<style>
  .card {
    display: grid;
    gap: 14px;
    padding: 16px 18px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .ico {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
  }

  .text {
    display: grid;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }

  .tones {
    display: flex;
    gap: 4px;
  }

  .tones button {
    padding: 6px 12px;
    border-radius: 999px;
    border: 1px solid transparent;
    background: var(--surface);
    color: inherit;
    font: inherit;
    cursor: pointer;
  }

  .tones button.on {
    border-color: currentColor;
    font-weight: 600;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }

  .list li {
    display: grid;
    grid-template-columns: 5.5em 4.5em minmax(0, 1fr) auto;
    gap: 10px;
    align-items: baseline;
    padding: 8px 10px;
    border-radius: var(--r-sm, 8px);
    background: var(--surface);
  }

  .list li.quiet {
    background: transparent;
  }

  .when,
  .ms {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .where {
    font-size: 0.85em;
    opacity: 0.75;
  }

  .what {
    display: grid;
    gap: 2px;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .q {
    font-weight: 600;
  }

  .failed .a {
    opacity: 0.75;
    font-style: italic;
  }

  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .tag {
    font-size: 0.8em;
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--surface-2);
  }

  .tag.held {
    opacity: 0.6;
    text-decoration: line-through;
  }

  .foot {
    justify-content: flex-start;
  }

  @media (max-width: 560px) {
    .list li {
      grid-template-columns: 4.5em minmax(0, 1fr);
    }

    .where,
    .ms {
      display: none;
    }
  }
</style>
