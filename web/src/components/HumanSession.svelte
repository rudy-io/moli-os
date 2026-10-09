<script>
  import { hub, login, logout } from '../lib/hub.svelte.js';
  import { t } from '../lib/i18n.svelte.js';

  let open = $state(false);
  let pin = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(e) {
    e.preventDefault();
    busy = true;
    error = (await login(pin)) ?? '';
    busy = false;
    pin = '';
    if (!error) open = false;
  }
</script>

{#if hub.session.pin_configured}
  <div class="session">
    {#if hub.session.human}
      <button class="chip human" onclick={logout} title={t('systeme.session.ouverte')}>
        {t('systeme.session.toi_present')}
      </button>
    {:else if open}
      <form onsubmit={submit}>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          type="password"
          inputmode="numeric"
          autocomplete="off"
          bind:value={pin}
          placeholder={t('systeme.session.code')}
          aria-label={t('systeme.session.code_tableau')}
          autofocus
        />
        <button type="submit" disabled={busy || !pin}>OK</button>
        <button type="button" class="ghost" onclick={() => (open = false)}>×</button>
        {#if error}<span class="error" role="alert">{error}</span>{/if}
      </form>
    {:else}
      <button
        class="chip"
        onclick={() => (open = true)}
        title={t('systeme.session.aide')}
      >
        {t('systeme.session.je_suis_la')}
      </button>
    {/if}
  </div>
{/if}

<style>
  .session {
    display: flex;
    align-items: center;
  }
  .chip {
    padding: 4px 12px;
    border-radius: 999px;
    border: var(--stroke) solid var(--ink);
    background: var(--card);
    font-size: 13px;
    font-weight: 700;
    cursor: pointer;
  }
  .chip.human {
    background: var(--sun);
    color: #161512;
  }
  form {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
  }
  input {
    width: 90px;
    padding: 4px 8px;
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    background: var(--bg);
  }
  form button {
    padding: 4px 10px;
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    background: var(--sun);
    color: #161512;
    font-weight: 700;
    cursor: pointer;
  }
  form .ghost {
    background: transparent;
    color: var(--ink);
  }
  .error {
    color: var(--alarm);
    font-size: 13px;
  }
  button:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--copper);
    outline-offset: 2px;
  }
</style>
