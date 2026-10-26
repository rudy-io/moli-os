<script>
  import { clock } from '../lib/home.svelte.js';
  import { alarms } from '../lib/stove-alarms.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  // A pellet stove's past alarms, from the button under its card: when each
  // came (and the lighting it stopped), its code and what it means, when it
  // was reset. From Moli's history: as far back as it keeps (30 days unless
  // `history_days` says otherwise), read when the window opens.
  let { id, name } = $props();

  let dialog;
  let list = $state(null);
  let since = $state(null);
  let failed = $state(false);

  // The whole history of a point (Moli bounds the window to what it keeps).
  async function changes(key) {
    const res = await fetch(`/api/history?point=${encodeURIComponent(`${id}/${key}`)}&hours=8784&points=2000`);
    if (!res.ok) throw new Error(res.statusText);
    const h = await res.json();
    return [...(h.before ? [h.before] : []), ...(h.raw ?? [])];
  }
  async function open() {
    dialog.showModal();
    // On the window itself: its close button would wear the ring.
    dialog.focus();
    failed = false;
    try {
      const [modes, codes] = await Promise.all([changes('mode'), changes('alarm')]);
      list = alarms(modes, codes);
      since = modes[0]?.[0] ?? null;
    } catch {
      failed = true;
    }
  }

  const known = (key) => t(key) !== key;
  const meaning = (code) => (code && known(`commun.poele.code.${code}`) ? t(`commun.poele.code.${code}`) : null);
  const day = (ts) => new Date(ts).toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long' });
  /** « 2 h 47 », « 9 min ». */
  function lasted(ms) {
    const min = Math.max(1, Math.round(ms / 60_000));
    return min < 60 ? t('commun.poele.historique_minutes', { min }) : t('commun.poele.historique_heures', { h: Math.floor(min / 60), min: String(min % 60).padStart(2, '0') });
  }
</script>

<button class="link" onclick={open}><Icon name="history" size={16} />{t('commun.poele.historique')}</button>

<dialog bind:this={dialog} onclick={(e) => e.target === dialog && dialog.close()} aria-labelledby="alarmes-{id}" tabindex="-1">
  <header>
    <div>
      <h3 id="alarmes-{id}">{t('commun.poele.historique')}</h3>
      <p class="muted">{name}</p>
    </div>
    <button class="round" onclick={() => dialog.close()} aria-label={t('commun.fermer')}><Icon name="close" size={20} /></button>
  </header>
  {#if failed}
    <p class="muted">{t('commun.poele.historique_illisible')}</p>
  {:else if !list}
    <p class="muted">{t('commun.poele.historique_lecture')}</p>
  {:else if !list.length}
    <p class="none"><Icon name="ok" size={18} />{t('commun.poele.historique_aucune')}</p>
  {:else}
    <ol>
      {#each list as a (a.start)}
        <li class:now={a.stopped && a.end == null}>
          <p class="title">
            <b>{day(a.start)} · {clock(a.start)}</b>
            {#if a.code}<span class="code">{a.code}</span>{/if}
          </p>
          {#if meaning(a.code)}<p>{meaning(a.code)}</p>{:else if !a.code}<p>{t('commun.poele.historique_sans_code')}</p>{/if}
          {#if !a.stopped}
            <p class="muted">{t('commun.poele.alarme_explique_code')}</p>
          {:else}
            {#if a.lighting}<p class="muted">{t('commun.poele.historique_allumage', { start: clock(a.lighting) })}</p>{/if}
            <p class="muted">
              {a.end == null
                ? t('commun.poele.historique_en_cours')
                : t('commun.poele.historique_rearmee', { day: day(a.end), time: clock(a.end), duree: lasted(a.end - a.start) })}
            </p>
          {/if}
        </li>
      {/each}
    </ol>
  {/if}
  {#if since}<p class="muted small">{t('commun.poele.historique_depuis', { day: day(since) })}</p>{/if}
</dialog>

<style>
  .link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    justify-self: start;
    padding: 6px 12px 6px 9px;
    border: none;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 12.5px;
    font-weight: 650;
    cursor: pointer;
  }

  dialog {
    border: 0;
    padding: 18px;
    width: min(520px, 94vw);
    max-height: 90dvh;
    overflow-y: auto;
    border-radius: var(--r-xl);
    background: var(--surface);
    color: var(--ink);
    box-shadow: 0 30px 80px rgb(0 0 0 / 40%);
  }

  dialog:focus {
    outline: none;
  }

  dialog::backdrop {
    background: rgb(4 6 12 / 60%);
    backdrop-filter: blur(4px);
  }

  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 12px;
  }

  h3 {
    font-size: 18px;
    font-weight: 750;
  }

  .round {
    width: 38px;
    height: 38px;
    flex: none;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink);
    cursor: pointer;
  }

  ol {
    display: grid;
    gap: 10px;
    list-style: none;
    padding: 0;
    margin: 0;
  }

  li {
    display: grid;
    gap: 4px;
    padding: 10px 12px;
    border-radius: 12px;
    background: var(--surface-2);
    font-size: 13.5px;
  }

  li.now {
    background: var(--alert-soft);
  }

  .title {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .title b {
    font-weight: 700;
  }

  .title b::first-letter {
    text-transform: uppercase;
  }

  .code {
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--surface);
    color: var(--alert);
    font-size: 12px;
    font-weight: 700;
  }

  .none {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--good);
    font-weight: 650;
  }

  .muted {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .small {
    margin-top: 12px;
    font-size: 11.5px;
  }
</style>
