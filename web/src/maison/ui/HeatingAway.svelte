<script>
  // « Je pars »: every heater to eco, whatever it was doing, each one's
  // setting kept by Moli; « Je suis rentré » puts each back as it was. Kept
  // on the server: the return works from any phone, after any restart.
  import { onMount } from 'svelte';
  import { home, note, nameOf, clock } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  let state = $state(null);
  let busy = $state(false);

  async function load() {
    try {
      const res = await fetch('/api/heating/away');
      if (res.ok) state = await res.json();
    } catch {
      /* the button waits */
    }
  }
  onMount(load);

  async function go(away) {
    busy = true;
    try {
      const res = await fetch('/api/heating/away', {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: JSON.stringify({ away }),
      });
      const body = await res.json().catch(() => ({}));
      // The bedrooms' heaters are among them: the house's code, once.
      if (res.status === 403) {
        home.held = { label: t(away ? 'commun.depart.label_part' : 'commun.depart.label_rentre'), reason: body.error, custom: () => go(away) };
        return;
      }
      if (!res.ok) throw new Error(body.error ?? res.statusText);
      state = { ...state, away: body.away, since: body.since };
      const failed = body.failed ?? [];
      if (failed.length) note(t('commun.depart.pas_tous', { count: failed.length, names: failed.map((f) => nameOf(f.device)).join(', ') }), 'error');
      else note(t(away ? 'commun.depart.note_part' : 'commun.depart.note_rentre'));
    } catch (err) {
      note(t('commun.echec', { label: t('commun.depart.titre'), error: err.message }), 'error');
    } finally {
      busy = false;
    }
  }
</script>

{#if state && (state.heaters > 0 || state.away)}
  <section class="away" class:on={state.away} aria-live="polite">
    <span class="ico"><Icon name={state.away ? 'leaf' : 'home'} size={20} /></span>
    <div class="text">
      {#if state.away}
        <b>{t('commun.depart.en_cours')}</b>
        <small>{t('commun.depart.depuis', { time: clock(state.since) })}</small>
      {:else}
        <b>{t('commun.depart.titre')}</b>
        <small>{t('commun.depart.explication', { count: state.heaters })}</small>
      {/if}
    </div>
    <button class="go" disabled={busy} onclick={() => go(!state.away)}>
      {busy ? t('commun.depart.patience') : t(state.away ? 'commun.depart.rentre' : 'commun.depart.part')}
    </button>
  </section>
{/if}

<style>
  .away {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px 16px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .away.on {
    background: color-mix(in srgb, var(--good, #2e9d5b) 14%, var(--surface));
  }

  .ico {
    flex: none;
    width: 38px;
    height: 38px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-3);
  }

  .on .ico {
    color: var(--good, #2e9d5b);
  }

  .text {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }

  b {
    font-weight: 650;
    color: var(--ink);
  }

  small {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .go {
    flex: none;
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    font: inherit;
    font-size: 14px;
    font-weight: 650;
    background: var(--ink);
    color: var(--surface);
    cursor: pointer;
  }

  .go:disabled {
    opacity: 0.6;
    cursor: wait;
  }
</style>
