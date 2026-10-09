<script>
  import { hub, refreshSession } from '../lib/home.svelte.js';
  import { i18n, languages, setLanguage, t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** A new house: the installation code (written in Moli's logs, so only
   *  whoever reaches the machine reads it) proves its owner, who chooses the
   *  house's code and language here. The session opens at once; this screen
   *  goes away. A house already in use (it has devices) can put it off. */
  let { onlater } = $props();
  let code = $state('');
  let pin = $state('');
  let again = $state('');
  let error = $state('');
  let busy = $state(false);

  // Each language named in itself, whatever the dashboard speaks.
  const NAMES = { fr: 'Français', en: 'English' };
  // A first guess: the browser's language, when the house can speak it.
  const guess = typeof navigator !== 'undefined' ? navigator.language?.slice(0, 2) : null;
  let language = $state(languages.includes(guess) ? guess : i18n.language);
  $effect(() => {
    setLanguage(language);
  });

  const min = $derived(hub.session?.min_pin ?? 6);
  const inUse = $derived(Object.keys(hub.devices ?? {}).length > 0);
  const ready = $derived(code.replace(/[^a-z0-9]/gi, '').length === 8 && /^\d+$/.test(pin) && pin.length >= min && pin.length <= 8 && again.length > 0);

  async function finish(e) {
    e.preventDefault();
    if (!ready || busy) return;
    if (pin !== again) {
      error = t('installation.differents');
      return;
    }
    busy = true;
    error = '';
    try {
      const res = await fetch('/api/session/setup', {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: JSON.stringify({ code, pin, language }),
      });
      if (res.ok) {
        await refreshSession();
        return;
      }
      error = (await res.json().catch(() => ({}))).error ?? t('installation.erreur', { status: res.status });
      if (res.status === 409) await refreshSession();
    } catch {
      error = t('installation.injoignable');
    } finally {
      busy = false;
    }
  }
</script>

<div class="setup" role="dialog" aria-modal="true" aria-labelledby="setup-title">
  <form class="card" onsubmit={finish}>
    <span class="mark"><Icon name="home" size={28} /></span>
    <h1 id="setup-title">{t('installation.titre')}</h1>
    <p class="lead">{t('installation.intro')}</p>

    {#if languages.length > 1}
      <label>
        <span>{t('installation.langue')}</span>
        <select bind:value={language}>
          {#each languages as l (l)}
            <option value={l}>{NAMES[l] ?? l}</option>
          {/each}
        </select>
      </label>
    {/if}

    <label>
      <span>{t('installation.code_installation')}</span>
      <input
        bind:value={code}
        autocomplete="off"
        autocapitalize="characters"
        spellcheck="false"
        placeholder="ABCD-EFGH"
        maxlength="12"
      />
      <small>{t('installation.code_aide')}</small>
      <code class="command">docker compose logs moli-os</code>
    </label>

    <label>
      <span>{t('installation.code_maison')}</span>
      <input bind:value={pin} type="password" inputmode="numeric" autocomplete="off" placeholder={t('installation.chiffres', { min })} maxlength="8" />
    </label>

    <label>
      <span>{t('installation.encore')}</span>
      <input bind:value={again} type="password" inputmode="numeric" autocomplete="off" maxlength="8" />
    </label>

    <p class="error" role="alert">{error}</p>
    <button class="primary" type="submit" disabled={!ready || busy}>{busy ? t('installation.un_instant') : t('installation.terminer')}</button>
    {#if inUse}
      <button class="later" type="button" onclick={onlater}>{t('installation.plus_tard')}</button>
    {/if}
  </form>
</div>

<style>
  .setup {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: grid;
    place-items: center;
    overflow-y: auto;
    padding: 24px 16px;
    background: var(--bg);
  }

  .card {
    width: min(440px, 100%);
    display: grid;
    gap: 14px;
    padding: 32px 26px 26px;
    border-radius: var(--r-xl);
    background: var(--surface);
    box-shadow: var(--shadow-lift);
  }

  .mark {
    width: 56px;
    height: 56px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--cool-soft);
    color: var(--cool);
  }

  h1 {
    font-size: 24px;
    font-weight: 750;
  }

  .lead {
    color: var(--ink-2);
    font-size: 15px;
  }

  label {
    display: grid;
    gap: 6px;
    font-size: 14px;
    font-weight: 650;
  }

  input,
  select {
    min-height: 46px;
    padding: 0 14px;
    border: 1px solid var(--surface-3);
    border-radius: 14px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 17px;
    letter-spacing: 0.04em;
  }

  input:focus {
    outline: 2px solid var(--cool);
    outline-offset: 1px;
  }

  small {
    color: var(--ink-3);
    font-weight: 500;
  }

  code {
    font-size: 13px;
  }

  .command {
    justify-self: start;
    padding: 4px 8px;
    border-radius: 8px;
    background: var(--surface-2);
  }

  .error {
    min-height: 20px;
    color: var(--alert);
    font-size: 14px;
    font-weight: 600;
  }

  .primary {
    min-height: 48px;
    border: none;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }

  .later {
    min-height: 40px;
    border: none;
    background: none;
    color: var(--ink-3);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
