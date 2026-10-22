<script>
  import { onMount } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import { mySessions, endMySession } from '../lib/people.svelte.js';
  import Icon from './Icon.svelte';

  /** Famille › Mes connexions: where the person signed in is signed in,
   *  and a button to end one (a forgotten computer, a lost phone). */
  let list = $state([]);
  let message = $state('');
  let loaded = $state(false);

  async function load() {
    try {
      list = (await mySessions()).sessions ?? [];
      message = '';
    } catch (err) {
      message = err.message;
    } finally {
      loaded = true;
    }
  }

  onMount(load);

  async function end(s) {
    if (!confirm(t('personnes.sessions.fermer_confirmer', { device: s.device || t('personnes.sessions.navigateur') }))) return;
    try {
      await endMySession(s.id);
      await load();
    } catch (err) {
      message = err.message;
    }
  }

  const date = (ms) => new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'long', year: 'numeric' });
</script>

<section class="card sessions">
  <header>
    <span class="icon"><Icon name="account" size={20} /></span>
    <div class="what">
      <b>{t('personnes.sessions.titre')}</b>
      <span class="muted">{t('personnes.sessions.intro')}</span>
    </div>
  </header>

  {#if message}<p class="error" role="alert">{message}</p>{/if}

  <ul>
    {#each list as s (s.id)}
      <li>
        <Icon name={s.remote ? 'web' : 'home'} size={18} />
        <div class="which">
          <b>{s.device || t('personnes.sessions.navigateur')}</b>
          <span class="muted">
            {s.remote ? t('personnes.sessions.internet') : t('personnes.sessions.maison')} · {t('personnes.sessions.depuis', { date: date(s.created) })}
          </span>
        </div>
        {#if s.current}
          <span class="here">{t('personnes.sessions.celle_ci')}</span>
        {:else}
          <button class="ghost small" onclick={() => end(s)}>{t('personnes.sessions.fermer')}</button>
        {/if}
      </li>
    {:else}
      {#if loaded}<li class="muted">{t('personnes.sessions.aucune')}</li>{/if}
    {/each}
  </ul>
</section>

<style>
  .sessions {
    display: grid;
    gap: 14px;
  }

  header {
    display: flex;
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
    gap: 2px;
    font-size: 14px;
  }

  ul {
    display: grid;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 12px;
    padding: 10px 12px;
    border-radius: 16px;
    background: var(--surface-2);
  }

  .which {
    display: grid;
    flex: 1;
    min-width: 160px;
    gap: 2px;
    font-size: 14px;
  }

  .here {
    color: var(--good);
    font-size: 13px;
    font-weight: 650;
  }

  .ghost {
    display: inline-flex;
    align-items: center;
    min-height: 34px;
    padding: 0 12px;
    border: 1px solid var(--surface-3);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
  }

  .error {
    color: var(--alert);
    font-size: 14px;
    font-weight: 600;
  }
</style>
