<script>
  import { onMount } from 'svelte';
  import { moli, closeCorrection, undoTurn, chooseActions } from '../lib/moli.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** « Pas ça »: what was meant, picked among what the house can do: its room, its device, what to do
   *  with it. Moli does it, and may keep it for the next time (not for a sentence that leans on what
   *  came before: « éteins-la »). */
  const fix = $derived(moli.fix);
  const choices = $derived(fix?.choices);

  let sheet = $state();
  onMount(() => sheet?.focus());

  let room = $state(null);
  /** the devices ticked, several when they share a command */
  let picked = $state([]);
  let remember = $state(true);

  // The room the sentence named, once the choices are there.
  $effect(() => {
    if (choices && room === null) room = choices.room ?? null;
  });

  const inRoom = $derived((choices?.devices ?? []).filter((d) => d.room === room));
  const group = $derived((choices?.groups ?? []).find((g) => g.room === room));
  const ticked = $derived(inRoom.filter((d) => picked.includes(d.id)));
  const keep = $derived(remember && choices?.remember_allowed);

  /** An action without its device: what two devices must share for one tap to do it on both. */
  const shape = (a) => `${a.tool}|${a.label}|${(a.arguments.point ?? '').split('/').slice(1).join('/')}|${JSON.stringify(a.arguments.value ?? null)}`;
  /** What all the ticked devices can do (reading values is for a single one). */
  const common = $derived.by(() => {
    if (!ticked.length) return [];
    const [first, ...others] = ticked;
    return first.actions
      .filter((a) => (ticked.length === 1 || a.tool !== 'get_device') && others.every((d) => d.actions.some((b) => shape(b) === shape(a))))
      .map((a) => ({ label: a.label, shape: shape(a) }));
  });

  function toggle(id) {
    picked = picked.includes(id) ? picked.filter((p) => p !== id) : [...picked, id];
  }

  function everyone() {
    picked = picked.length === inRoom.length ? [] : inRoom.map((d) => d.id);
  }

  /** The same command, on each ticked device. */
  function apply(shared) {
    const calls = ticked.map((d) => d.actions.find((b) => shape(b) === shared.shape));
    return chooseActions(calls, keep);
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && fix && closeCorrection()} />

{#if fix}
  <button class="scrim" onclick={closeCorrection} aria-label={t('commun.fermer')}></button>
  <div class="sheet" role="dialog" aria-modal="true" aria-label={t('moli.correction.titre')} tabindex="-1" bind:this={sheet}>
    <header>
      <h2>{t('moli.correction.titre')}</h2>
      <button class="icon" onclick={closeCorrection} aria-label={t('commun.fermer')}><Icon name="close" size={18} /></button>
    </header>

    {#if fix.error}<p class="error" role="alert">{fix.error}</p>{/if}

    {#if fix.undo}
      <button class="wide undo" onclick={undoTurn}><Icon name="back" size={16} />{t('moli.correction.defaire')}</button>
    {/if}

    {#if fix.loading}
      <p class="hint">…</p>
    {:else if choices}
      <p class="hint">{t('moli.correction.voulais')}</p>
      <div class="chips" role="group" aria-label={t('moli.correction.piece')}>
        {#each choices.rooms as r (r)}
          <button class="chip" class:on={room === r} aria-pressed={room === r} onclick={() => ((room = r), (picked = []))}>{r}</button>
        {/each}
      </div>

      {#if group}
        <p class="hint">{t('moli.correction.toute_piece')}</p>
        <div class="chips">
          {#each group.actions as a (a.label)}
            <button class="chip" disabled={fix.busy} onclick={() => chooseActions([a], keep)}>{a.label}</button>
          {/each}
        </div>
      {/if}

      {#if room !== null && inRoom.length}
        <p class="hint">{t('moli.correction.plusieurs')}</p>
        <ul class="list">
          <li><button class="row all" onclick={everyone}>{picked.length === inRoom.length ? t('moli.correction.aucun') : t('moli.correction.tous')}</button></li>
          {#each inRoom as d (d.id)}
            <li>
              <label class="row pick"><input type="checkbox" checked={picked.includes(d.id)} onchange={() => toggle(d.id)} />{d.name}</label>
            </li>
          {/each}
        </ul>
      {/if}

      {#if ticked.length}
        <p class="hint">{t('moli.correction.pour_les', { count: ticked.length })}</p>
        {#if common.length}
          <ul class="list">
            {#each common as c (c.shape)}
              <li><button class="row act" disabled={fix.busy} onclick={() => apply(c)}>{c.label}</button></li>
            {/each}
          </ul>
        {:else}
          <p class="hint small">{t('moli.correction.rien_en_commun')}</p>
        {/if}
      {/if}

      <p class="hint">{t('moli.correction.autre')}</p>
      <div class="chips">
        {#each choices.other as a (a.tool)}
          <button class="chip" disabled={fix.busy} onclick={() => chooseAction(a, keep)}>{a.label}</button>
        {/each}
      </div>

      {#if choices.remember_allowed}
        <label class="remember"><input type="checkbox" bind:checked={remember} />{t('moli.correction.retenir')}</label>
      {:else}
        <p class="hint small">{t('moli.correction.pas_retenue')}</p>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 44;
    border: 0;
    background: rgb(10 14 22 / 45%);
  }

  .sheet:focus {
    outline: none;
  }

  .sheet {
    position: fixed;
    left: 50%;
    bottom: 0;
    transform: translateX(-50%);
    z-index: 45;
    width: min(460px, 100vw);
    max-height: min(80dvh, 640px);
    overflow-y: auto;
    display: grid;
    gap: 10px;
    padding: 16px 16px 22px;
    background: var(--surface);
    border-radius: var(--r-xl) var(--r-xl) 0 0;
    box-shadow: var(--shadow-lift);
  }

  header {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  h2 {
    flex: 1;
    font-size: 18px;
    font-weight: 700;
  }

  .icon {
    width: 36px;
    height: 36px;
    border: 0;
    border-radius: 50%;
    background: var(--surface-2);
  }

  .hint {
    color: var(--ink-3);
    font-size: 13px;
    font-weight: 600;
  }

  .small {
    font-weight: 500;
  }

  .error {
    color: var(--alert);
    font-size: 14px;
    font-weight: 600;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .chip {
    border: 0;
    border-radius: 999px;
    padding: 8px 14px;
    background: var(--surface-2);
    font-size: 14px;
  }

  .chip.on {
    background: var(--ink);
    color: var(--bg);
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  .pick {
    justify-content: flex-start;
  }

  .all {
    justify-content: center;
    font-weight: 700;
  }

  .row,
  .wide,

  .act {
    justify-content: flex-start;
  }

  .undo {
    justify-content: center;
    font-weight: 700;
  }


  .remember {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
  }

  button:disabled {
    opacity: 0.5;
  }
</style>
