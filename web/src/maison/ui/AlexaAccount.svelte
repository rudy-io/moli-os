<script>
  import { hub, home, value } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** Each Alexa account Moli drives: connected (its speakers), or the three
   *  steps of the sign-in. Amazon's own page takes the password; Moli only
   *  gets the address it ends on. */
  const accounts = $derived(
    Object.values(hub.devices ?? {}).filter((d) => d.manufacturer === 'Amazon' && d.model === 'Alexa' && d.points?.some((p) => p.key === 'sign_in')),
  );
  const speakersOf = (account) =>
    Object.values(hub.devices ?? {}).filter((d) => d.instance === account.instance && d.manufacturer === 'Amazon' && d.model !== 'Alexa').length;

  let pasted = $state('');
  let busy = $state(false);
  let error = $state('');

  async function connect(id) {
    busy = true;
    error = '';
    try {
      const res = await fetch('/api/command', {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: JSON.stringify({ point: `${id}/sign_in`, value: pasted.trim() }),
      });
      if (res.status === 403) {
        home.held = { label: t('systeme.alexa.connecter'), reason: 'ton code', custom: () => connect(id) };
        return;
      }
      const body = await res.json().catch(() => ({}));
      if (!res.ok) {
        error = body.error ?? res.statusText;
        return;
      }
      pasted = '';
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }
</script>

{#each accounts as a (a.id)}
  {@const status = value(a.id, 'status')}
  {@const url = value(a.id, 'sign_in_url')}
  <section class="card" class:done={status === 'connected'}>
    <div class="row">
      <span class="ico"><Icon name="speaker" size={18} /></span>
      <div class="text">
        <b>{t('systeme.alexa.titre')}</b>
        <span class="muted">
          {#if status === 'connected'}
            {t('systeme.alexa.connecte', { n: speakersOf(a) })}
          {:else if status === 'sign_in'}
            {t('systeme.alexa.a_connecter')}
          {:else if status === 'error'}
            {t('systeme.alexa.erreur')}
          {:else}
            {t('systeme.alexa.connexion')}
          {/if}
        </span>
      </div>
    </div>

    {#if status === 'sign_in' && url}
      <ol class="steps">
        <li>
          {t('systeme.alexa.etape1')}
          <a class="btn primary" href={url} target="_blank" rel="noopener noreferrer">{t('systeme.alexa.ouvrir')}</a>
        </li>
        <li>{t('systeme.alexa.etape2')}</li>
        <li>{t('systeme.alexa.etape3')}</li>
      </ol>
      <form class="form" onsubmit={(e) => (e.preventDefault(), connect(a.id))} autocomplete="off">
        <label for="alexa-pasted">{t('systeme.alexa.coller')}</label>
        <input id="alexa-pasted" bind:value={pasted} placeholder="https://www.amazon.com/ap/maplanding?…" spellcheck="false" />
        <button class="btn primary" disabled={busy || !pasted.includes('maplanding')}>{busy ? t('systeme.alexa.verification') : t('systeme.alexa.connecter')}</button>
        {#if error}<p class="error">{error}</p>{/if}
      </form>
      <small class="muted">{t('systeme.alexa.aide')}</small>
    {/if}
    <small class="muted">{t('systeme.alexa.officieux')}</small>
  </section>
{/each}

<style>
  .card {
    display: grid;
    gap: 14px;
    padding: 16px 18px;
    border-radius: var(--r-md);
    background: var(--warm-soft);
  }

  .card.done {
    background: var(--surface-2);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 12px;
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
    min-width: 0;
  }

  .steps {
    margin: 0;
    padding-left: 1.3em;
    display: grid;
    gap: 8px;
  }

  .steps li {
    overflow-wrap: anywhere;
  }

  .steps .btn {
    margin-left: 8px;
  }

  .form {
    display: grid;
    gap: 8px;
  }

  .form input {
    font: inherit;
    padding: 9px 12px;
    border-radius: 10px;
    border: 1px solid var(--line, rgba(0, 0, 0, 0.15));
    background: var(--surface);
    color: inherit;
    min-width: 0;
  }

  .btn {
    display: inline-block;
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    font: inherit;
    font-weight: 650;
    background: var(--surface);
    color: var(--ink);
    cursor: pointer;
    text-decoration: none;
    justify-self: start;
  }

  .btn.primary {
    background: var(--warm);
    color: #fff;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn:focus-visible,
  .form input:focus-visible {
    outline: 2px solid currentColor;
    outline-offset: 2px;
  }

  .error {
    margin: 0;
    color: var(--danger, #b3261e);
  }
</style>
