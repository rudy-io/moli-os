<script>
  import { onMount } from 'svelte';
  import { drinkName, fieldWord, FIELDS, TEMPERATURES } from '../lib/coffee.js';
  import { changesFrom } from '../lib/coffee-recipes.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import DrinkIcon from './DrinkIcon.svelte';

  /** A drink about to be brewed: its recipe as the machine keeps it, each
   *  field changeable within the machine's bounds for this cup only, then
   *  « Brew » or « Cancel ». No timer: it waits as long as it takes. Sent
   *  as the plain drink when nothing changed (`brew`), else with its
   *  changes (`brew_custom`); the machine keeps its recipe. */
  let { slug, recipe, double = false, onbrew, onclose } = $props();

  let dialog;
  // Focused on the window itself: its first « − » would wear the ring.
  onMount(() => {
    dialog.showModal();
    dialog.focus();
  });

  const fields = $derived(FIELDS.filter((k) => recipe?.[k]));
  // Fields with a word of help, on the line under them.
  const HELPED = ['water', 'bypass', 'milk_foam', 'milk_break'];
  // The values chosen, from the recipe as it was when the window opened: a
  // reading meanwhile does not undo what is being set.
  const asKept = () => Object.fromEntries(Object.entries(recipe ?? {}).map(([k, f]) => [k, f.value]));
  let chosen = $state(asKept());
  const changes = $derived(changesFrom(recipe, chosen));
  const at = (k) => ({ ...recipe[k], value: chosen[k] });
  const nudge = (k, sign) => {
    const f = recipe[k];
    chosen[k] = Math.min(f.max, Math.max(f.min, chosen[k] + sign * f.step));
  };
  const back = () => (chosen = asKept());

  let busy = $state(false);
  async function brew() {
    busy = true;
    const done = await onbrew(changes);
    busy = false;
    if (done !== 'error') dialog.close();
  }
</script>

<dialog bind:this={dialog} {onclose} onclick={(e) => e.target === dialog && dialog.close()} aria-labelledby="boisson-{slug}" tabindex="-1">
  <header>
    <span class="logo"><DrinkIcon drink={slug} size={36} /></span>
    <div>
      <h3 id="boisson-{slug}">{drinkName(slug)}</h3>
      <p class="muted">{t(changes ? 'salon.cafe.boisson_modifiee' : 'salon.cafe.boisson_recette')}</p>
    </div>
  </header>

  {#if fields.length}
    <ul class="fields">
      {#each fields as k (k)}
        {@const f = recipe[k]}
        <li class:changed={chosen[k] !== f.value}>
          <b class="what">{t(`salon.cafe.champ.${k}`)}</b>
          {#if k === 'temperature'}
            <div class="choice" role="radiogroup" aria-label={t('salon.cafe.champ.temperature')}>
              {#each TEMPERATURES as word, i (word)}
                <button role="radio" aria-checked={chosen[k] === i} class:active={chosen[k] === i} onclick={() => (chosen[k] = i)}>{t(`salon.cafe.temperature.${word}`)}</button>
              {/each}
            </div>
          {:else}
            <div class="step" role="group" aria-label={t(`salon.cafe.champ.${k}`)}>
              <button onclick={() => nudge(k, -1)} disabled={chosen[k] <= f.min} aria-label={t('salon.cafe.moins')}>−</button>
              <b class="num">{fieldWord(k, at(k))}</b>
              <button onclick={() => nudge(k, 1)} disabled={chosen[k] >= f.max} aria-label={t('salon.cafe.plus')}>+</button>
            </div>
          {/if}
          {#if HELPED.includes(k)}<small>{t(`salon.cafe.champ_aide.${k}`)}</small>{/if}
        </li>
      {/each}
    </ul>
    {#if double}<p class="muted">{t('salon.cafe.double_par_tasse')}</p>{/if}
    {#if changes}
      <button class="link" onclick={back}><Icon name="repeat" size={16} />{t('salon.cafe.remettre_recette')}</button>
      <p class="muted">{t('salon.cafe.pour_cette_tasse')}</p>
    {/if}
  {:else}
    <p class="muted">{t('salon.cafe.recette_pas_lue')}</p>
  {/if}

  <p class="cup"><Icon name="info" size={16} />{t('salon.cafe.tasse')}</p>
  <footer>
    <button class="soft" onclick={() => dialog.close()}>{t('commun.annuler')}</button>
    <button class="primary" onclick={brew} disabled={busy} aria-busy={busy}>
      {busy ? t('salon.cafe.envoi') : t('salon.cafe.lancer_boisson')}
    </button>
  </footer>
</dialog>

<style>
  dialog {
    border: 0;
    padding: 20px;
    width: min(440px, 94vw);
    max-height: 92dvh;
    overflow-y: auto;
    border-radius: var(--r-xl);
    background: var(--surface);
    color: var(--ink);
    box-shadow: 0 30px 80px rgb(0 0 0 / 40%);
  }

  dialog:focus {
    outline: none;
  }

  dialog::backdrop {
    background: rgb(4 6 12 / 60%);
    backdrop-filter: blur(4px);
  }

  header {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 14px;
  }

  .logo {
    width: 48px;
    height: 48px;
    flex: none;
    border-radius: 14px;
    display: grid;
    place-items: center;
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  h3 {
    font-size: 19px;
    font-weight: 750;
  }

  .muted {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .fields {
    display: grid;
    gap: 6px;
    list-style: none;
    padding: 0;
    margin: 0 0 10px;
  }

  /* The field and its control on one line, its help under them. */
  .fields li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: 2px 10px;
    padding: 8px 10px;
    border-radius: 12px;
    background: var(--surface-2);
  }

  .fields li.changed {
    box-shadow: inset 3px 0 0 var(--warm);
  }

  .what {
    min-width: 0;
    font-size: 14px;
    font-weight: 650;
  }

  .fields small {
    grid-column: 1 / -1;
    font-size: 11.5px;
    color: var(--ink-3);
  }

  .step {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .step b {
    min-width: 64px;
    text-align: center;
    font-size: 15px;
    font-weight: 700;
  }

  .step button {
    width: 36px;
    height: 36px;
    border: 0;
    border-radius: 50%;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 19px;
    cursor: pointer;
  }

  .step button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .choice {
    display: flex;
    gap: 4px;
    padding: 3px;
    border-radius: 999px;
    background: var(--surface);
  }

  .choice button {
    border: 0;
    border-radius: 999px;
    padding: 6px 11px;
    background: none;
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .choice button.active {
    background: var(--ink);
    color: var(--surface);
  }

  .link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 0;
    border: 0;
    background: none;
    color: var(--cool);
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
  }

  .cup {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 12px;
    font-size: 13px;
    color: var(--ink-2);
  }

  footer {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 16px;
  }

  .soft,
  .primary {
    border: 0;
    border-radius: 999px;
    padding: 10px 20px;
    font: inherit;
    font-weight: 650;
    cursor: pointer;
    background: var(--surface-2);
    color: var(--ink);
  }

  .primary {
    background: var(--ink);
    color: var(--surface);
  }

  .primary:disabled {
    opacity: 0.6;
    cursor: wait;
  }
</style>
