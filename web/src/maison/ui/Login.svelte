<script>
  import { t } from '../../lib/i18n.svelte.js';
  import { signIn, redeem } from '../lib/people.svelte.js';
  import Icon from './Icon.svelte';

  /** From the Internet, before anything of the house: a person signs in
   *  with their password, or sets it with the invitation code an owner gave
   *  them. Then the page reloads into the house. */
  let { onclose = null } = $props();
  let mode = $state('login');
  let login = $state('');
  let password = $state('');
  let code = $state('');
  let again = $state('');
  let error = $state('');
  let busy = $state(false);

  const MIN = 10;
  const ready = $derived(
    mode === 'login'
      ? login.trim().length > 0 && password.length > 0
      : code.replace(/[^a-z0-9]/gi, '').length === 8 && password.length >= MIN && again.length > 0,
  );

  async function submit(e) {
    e.preventDefault();
    if (!ready || busy) return;
    if (mode === 'invite' && password !== again) {
      error = t('personnes.connexion.differents');
      return;
    }
    busy = true;
    error = '';
    try {
      if (mode === 'login') await signIn(login.trim(), password);
      else await redeem(code, password);
      location.reload();
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  function switchTo(next) {
    mode = next;
    error = '';
    password = '';
    again = '';
  }
</script>

<div class="login">
  <form class="card" onsubmit={submit}>
    <span class="mark"><Icon name="home" size={28} /></span>
    <h1>{mode === 'login' ? t('personnes.connexion.titre') : t('personnes.connexion.titre_invitation')}</h1>
    <p class="lead">{mode === 'login' ? t('personnes.connexion.intro') : t('personnes.connexion.intro_invitation')}</p>

    {#if mode === 'login'}
      <label>
        <span>{t('personnes.connexion.identifiant')}</span>
        <input bind:value={login} autocomplete="username" autocapitalize="none" spellcheck="false" />
      </label>
      <label>
        <span>{t('personnes.connexion.mot_de_passe')}</span>
        <input bind:value={password} type="password" autocomplete="current-password" />
      </label>
    {:else}
      <label>
        <span>{t('personnes.connexion.code')}</span>
        <input bind:value={code} autocomplete="off" autocapitalize="characters" spellcheck="false" placeholder="ABCD-EFGH" maxlength="12" />
      </label>
      <label>
        <span>{t('personnes.connexion.nouveau')}</span>
        <input bind:value={password} type="password" autocomplete="new-password" placeholder={t('personnes.connexion.min', { min: MIN })} />
      </label>
      <label>
        <span>{t('personnes.connexion.encore')}</span>
        <input bind:value={again} type="password" autocomplete="new-password" />
      </label>
    {/if}

    <p class="error" role="alert">{error}</p>
    <button class="primary" type="submit" disabled={!ready || busy}>
      {busy ? t('personnes.connexion.un_instant') : mode === 'login' ? t('personnes.connexion.entrer') : t('personnes.connexion.choisir')}
    </button>
    {#if onclose}
      <button class="other" type="button" onclick={onclose}>{t('personnes.carte.annuler')}</button>
    {/if}
    {#if mode === 'login'}
      <button class="other" type="button" onclick={() => switchTo('invite')}>{t('personnes.connexion.j_ai_un_code')}</button>
    {:else}
      <button class="other" type="button" onclick={() => switchTo('login')}>{t('personnes.connexion.j_ai_un_compte')}</button>
    {/if}
  </form>
</div>

<style>
  .login {
    position: fixed;
    inset: 0;
    z-index: 70;
    display: grid;
    place-items: center;
    overflow-y: auto;
    padding: 24px 16px;
    background: var(--bg);
  }

  .card {
    width: min(420px, 100%);
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

  input {
    min-height: 46px;
    padding: 0 14px;
    border: 1px solid var(--surface-3);
    border-radius: 14px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 17px;
  }

  input:focus {
    outline: 2px solid var(--cool);
    outline-offset: 1px;
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
    background: var(--cool);
    color: #fff;
    font: inherit;
    font-size: 16px;
    font-weight: 700;
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .other {
    min-height: 40px;
    border: none;
    background: none;
    color: var(--cool);
    font: inherit;
    font-weight: 650;
    cursor: pointer;
  }
</style>
