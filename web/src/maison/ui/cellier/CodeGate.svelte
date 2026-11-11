<script>
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import { hub, login, refreshSession } from '../../../lib/hub.svelte.js';
  import { needsCode } from '../../lib/cellier/errors.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** Sending a receipt (to read it, then to record it) is for a person: on a
   *  dashboard opened without the code, Moli refuses before reading anything.
   *  This says so up front and takes the code in place, so that what the page
   *  holds (the photos, the draft being corrected) is kept. Shows nothing
   *  once the code is not needed (none set on the house, or already typed). */
  let pin = $state('');
  let error = $state('');
  let busy = $state(false);

  // The session may have expired since the dashboard loaded.
  onMount(() => void refreshSession());

  async function submit(e) {
    e.preventDefault();
    if (busy || !pin) return;
    busy = true;
    error = (await login(pin)) ?? '';
    busy = false;
    pin = '';
  }
</script>

{#if needsCode(hub.session)}
  <form class="gate" onsubmit={submit}>
    <p class="notice"><Icon name="lock" size={18} />{t('cellier.code.requis')}</p>
    <div class="row">
      <input
        type="password"
        inputmode="numeric"
        autocomplete="off"
        bind:value={pin}
        placeholder={t('cellier.code.libelle')}
        aria-label={t('cellier.code.libelle')}
      />
      <button type="submit" class="ghost" disabled={busy || !pin}>{t('cellier.code.valider')}</button>
    </div>
    {#if error}<p class="error" role="alert"><Icon name="alert" size={18} />{error}</p>{/if}
  </form>
{/if}

<style>
  .gate {
    display: grid;
    gap: 10px;
  }

  .notice,
  .error {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 0;
    padding: 12px 16px;
    border-radius: var(--r-sm);
    font-weight: 550;
  }

  .notice {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .error {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .notice :global(svg),
  .error :global(svg) {
    margin-top: 2px;
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }

  input {
    width: 150px;
    min-height: 48px;
    padding: 0 16px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink);
    font-size: 16px;
    letter-spacing: 0.15em;
  }

  .ghost {
    min-height: 48px;
    padding: 0 20px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink-2);
    font-size: 15px;
    font-weight: 650;
  }

  .ghost:disabled {
    opacity: 0.45;
  }

  input:focus-visible,
  .ghost:focus-visible {
    outline: 2px solid var(--ink);
    outline-offset: 2px;
  }
</style>
