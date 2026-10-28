<script>
  import { onMount } from 'svelte';
  import { note, asPerson } from '../lib/home.svelte.js';
  import { request } from '../../lib/api.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The Tuya cloud project's keys, given by a person (code asked): filed
   *  encrypted in Moli, never shown again. The Tuya driver restarts with
   *  them within the minute. */
  let state = $state(null);
  let open = $state(false);
  let id = $state('');
  let secret = $state('');
  let busy = $state(false);

  async function load() {
    try {
      const res = await fetch('/api/integrations/tuya-cloud');
      if (res.ok) state = await res.json();
    } catch {
      /* the card stays hidden */
    }
  }
  onMount(load);

  let looking = $state(false);

  /** Moli asks the account itself, with the keys it holds (code asked). */
  async function look() {
    looking = true;
    try {
      const done = await asPerson(t('systeme.tuya.chercher'), () => request('POST', '/api/integrations/tuya-cloud/discover'));
      if (!done) return;
      note(done.added.length ? t('systeme.tuya.trouves', { names: done.added.join(', ') }) : t('systeme.tuya.rien_trouve'), 'info');
    } catch (e) {
      note(t('systeme.tuya.erreur_recherche', { message: e.message }), 'error');
    } finally {
      looking = false;
    }
  }

  const configured = $derived(state?.instances?.some((i) => i.configured));

  async function save() {
    busy = true;
    try {
      const keys = { access_id: id.trim(), access_secret: secret.trim() };
      // A person's gesture: the code first if asked, then the same request.
      const done = await asPerson(t('systeme.tuya.cle_titre'), () => request('PUT', '/api/integrations/tuya-cloud', keys));
      if (!done) return;
      id = '';
      secret = '';
      open = false;
      note(t('systeme.tuya.enregistrees'), 'info');
      await load();
    } catch (e) {
      note(t('systeme.tuya.erreur', { message: e.message }), 'error');
    } finally {
      busy = false;
    }
  }
</script>

{#if state?.instances?.length}
  <section class="keys" class:done={configured}>
    <div class="row">
      <span class="ico"><Icon name={configured ? 'ok' : 'lock'} size={18} /></span>
      <div class="text">
        <b>{t('systeme.tuya.titre')}</b>
        <span class="muted">
          {configured
            ? t('systeme.tuya.configure')
            : t('systeme.tuya.a_configurer')}
        </span>
      </div>
      {#if configured}
        <button class="btn" disabled={looking} title={t('systeme.tuya.chercher_aide')} onclick={look}>{looking ? '…' : t('systeme.tuya.chercher')}</button>
      {/if}
      <button class="btn" onclick={() => (open = !open)}>{configured ? t('systeme.tuya.remplacer') : t('systeme.tuya.ajouter')}</button>
    </div>
    {#if open}
      <form class="form" onsubmit={(e) => (e.preventDefault(), save())} autocomplete="off">
        <label>Access ID<input bind:value={id} spellcheck="false" autocomplete="off" /></label>
        <label>Access Secret<input bind:value={secret} type="password" spellcheck="false" autocomplete="new-password" /></label>
        <button class="btn primary" disabled={busy || !id.trim() || !secret.trim()}>{busy ? '…' : t('systeme.tuya.enregistrer')}</button>
        <small class="muted">{t('systeme.tuya.code_demande')}</small>
      </form>
    {/if}
  </section>
{/if}

<style>
  .keys {
    display: grid;
    gap: 14px;
    padding: 16px 18px;
    border-radius: var(--r-md);
    background: var(--warm-soft);
  }

  .keys.done {
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
    color: var(--warm-ink);
  }

  .done .ico {
    color: var(--good);
  }

  .text {
    display: grid;
    gap: 2px;
    flex: 1;
    min-width: 220px;
  }

  .text .muted {
    font-size: 13px;
  }

  .btn {
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    font: inherit;
    font-weight: 650;
    background: var(--surface);
    color: var(--ink);
    cursor: pointer;
  }

  .btn.primary {
    background: var(--warm);
    color: #fff;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .form {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 220px), 1fr));
    gap: 10px 14px;
    align-items: end;
  }

  label {
    display: grid;
    gap: 4px;
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-2);
  }

  input {
    font: inherit;
    font-size: 14px;
    padding: 9px 12px;
    border-radius: var(--r-sm);
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink);
  }

  small {
    grid-column: 1 / -1;
    font-size: 12px;
  }
</style>
