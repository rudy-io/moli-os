<script>
  // The night mode of every heater module that has one (Tuya's
  // `night_btn`), switched on or off for all at once: said how many have it
  // and which differ, then one order per module that is not yet as asked,
  // one after the other (they share their gateway). Its hours stay as each
  // module keeps them.
  import { hub, value, act, note, nameOf, reachable } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  const modules = $derived(
    Object.values(hub.devices)
      .filter((d) => d.points.some((p) => p.key === 'night_btn' && p.access?.write))
      .sort((a, b) => nameOf(a.id).localeCompare(nameOf(b.id))),
  );
  const on = $derived(modules.filter((d) => value(d.id, 'night_btn') === true));
  const off = $derived(modules.filter((d) => value(d.id, 'night_btn') === false));
  let busy = $state(null);

  async function all(wanted) {
    busy = wanted;
    const left = modules.filter((d) => value(d.id, 'night_btn') !== wanted);
    const todo = left.filter((d) => reachable(d.id));
    const missed = left.length - todo.length;
    const failed = [];
    for (const d of todo) {
      const label = t(wanted ? 'commun.nuit.label_active' : 'commun.nuit.label_desactive', { name: nameOf(d.id) });
      const done = await act(`${d.id}/night_btn`, wanted, label);
      if (done === 'error') failed.push(nameOf(d.id));
      // Held for an approval: the guard says it, the others go on.
    }
    busy = null;
    if (failed.length) note(t('commun.nuit.pas_tous', { count: failed.length, names: failed.join(', ') }), 'error');
    else if (todo.length) note(t(wanted ? 'commun.nuit.note_active' : 'commun.nuit.note_desactive', { count: todo.length }));
    if (missed > 0) note(t('commun.nuit.injoignables', { count: missed }), 'error');
  }
</script>

{#if modules.length}
  <section class="night" class:on={on.length === modules.length} aria-live="polite">
    <span class="ico"><Icon name="moon" size={20} /></span>
    <div class="text">
      <b>{t('commun.nuit.titre')}</b>
      <small>
        {#if on.length === modules.length}{t('commun.nuit.partout', { count: modules.length })}
        {:else if off.length === modules.length}{t('commun.nuit.nulle_part', { count: modules.length })}
        {:else}{t('commun.nuit.sur', { n: on.length, count: modules.length })}{/if}
      </small>
      {#if on.length && off.length}
        <small class="which">{t(on.length >= off.length ? 'commun.nuit.sauf' : 'commun.nuit.seulement', { names: (on.length >= off.length ? off : on).map((d) => nameOf(d.id)).join(', ') })}</small>
      {/if}
    </div>
    <div class="buttons">
      <button class="go" disabled={busy != null || on.length === modules.length} onclick={() => all(true)}>
        {busy === true ? t('commun.nuit.patience') : t('commun.nuit.activer')}
      </button>
      <button class="go soft" disabled={busy != null || off.length === modules.length} onclick={() => all(false)}>
        {busy === false ? t('commun.nuit.patience') : t('commun.nuit.desactiver')}
      </button>
    </div>
  </section>
{/if}

<style>
  .night {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
    padding: 14px 16px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .night.on {
    background: color-mix(in srgb, var(--cool) 14%, var(--surface));
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
    color: var(--cool);
  }

  .text {
    flex: 1 1 180px;
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

  .buttons {
    display: flex;
    gap: 8px;
    margin-left: auto;
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

  .go.soft {
    background: var(--surface);
    color: var(--ink);
  }

  .go:disabled {
    opacity: 0.45;
    cursor: default;
  }
</style>
