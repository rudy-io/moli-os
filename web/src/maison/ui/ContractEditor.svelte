<script>
  // The electricity contracts, edited from the Énergie page: each from its
  // own day (past or future), its off-peak windows (a « + » adds one, each
  // can be changed or removed), its off-peak and peak prices (one price
  // without off-peak hours) and its monthly subscription. The strip shows
  // the day as it is being written. Nothing leaves before « Save », which
  // asks for the house's code and sends the whole list: the past is
  // repriced at once (a contract applies to every hour since its day).
  import { onMount } from 'svelte';
  import { asHuman, CANCELLED, note } from '../lib/home.svelte.js';
  import { request } from '../../lib/api.js';
  import { refresh } from '../lib/energy-live.svelte.js';
  import { toDraft, toContract, problems, inForce, freeWindow, hhmm, toMin, isoDay } from '../lib/contracts.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import DayStrip from './DayStrip.svelte';

  let { contracts, currency = '€', now, onsaved, onclose } = $props();

  let dialog;
  // Opened on the dialog itself: no field (a phone's keyboard), no ring.
  onMount(() => {
    dialog.showModal();
    dialog.focus();
  });

  const today = $derived(isoDay(now));
  // The drafts start from the contracts as opened (a copy, edited apart).
  // svelte-ignore state_referenced_locally
  const start = contracts.map(toDraft);
  let drafts = $state(start.length ? start : [{ from: isoDay(Date.now()), windows: [], hc: null, hp: null, base: null, fee: null }]);
  let sel = $state(Math.max(0, inForce(start, isoDay(Date.now())), 0));
  const d = $derived(drafts[sel]);

  // The contracts in the order of their days, each with what it is now.
  const tabs = $derived(
    drafts
      .map((x, i) => ({ i, from: x.from, when: i === inForce(drafts, today) ? 'now' : x.from > today ? 'later' : 'before' }))
      .sort((a, b) => (a.from ?? '').localeCompare(b.from ?? '')),
  );
  const dayText = (iso) => {
    if (!iso || Number.isNaN(Date.parse(iso))) return '—';
    return new Date(`${iso}T00:00`).toLocaleDateString(locale(), { day: 'numeric', month: 'short', year: 'numeric' });
  };

  // A new contract starts as a copy of the one in force, from tomorrow.
  function add() {
    const base = drafts[inForce(drafts, today)] ?? drafts.at(-1);
    const tomorrow = isoDay(now + 24 * 3_600_000);
    drafts.push({ ...$state.snapshot(base), from: tomorrow, windows: base.windows.map((w) => ({ ...w })) });
    sel = drafts.length - 1;
    confirming = false;
  }

  let confirming = $state(false);
  function remove() {
    drafts.splice(sel, 1);
    sel = Math.max(0, inForce(drafts, today));
    confirming = false;
  }

  function addWindow() {
    const w = freeWindow(d.windows);
    if (w) d.windows.push(w);
  }
  const setTime = (w, key, value) => {
    const m = toMin(value);
    if (m != null) w[key] = m;
  };

  const issues = $derived(problems(drafts));
  const mine = $derived(issues.filter((p) => p.index === sel));
  const elsewhere = $derived(issues.some((p) => p.index !== sel));
  const PROBLEM = {
    date: 'energie.contrat.pb_date',
    same_date: 'energie.contrat.pb_meme_date',
    window: 'energie.contrat.pb_plage',
    overlap: 'energie.contrat.pb_chevauche',
    price: 'energie.contrat.pb_prix',
    fee: 'energie.contrat.pb_abonnement',
  };
  const changed = $derived(JSON.stringify(drafts.map(toContract)) !== JSON.stringify(start.map(toContract)));

  let busy = $state(false);
  async function save() {
    busy = true;
    try {
      const body = { contracts: drafts.map(toContract) };
      const saved = await asHuman(t('energie.contrat.enregistrer_label'), () => request('PUT', '/api/energy/contracts', body));
      await refresh();
      note(t('energie.contrat.enregistre'));
      onsaved?.(saved);
      dialog.close();
    } catch (err) {
      if (err.message !== CANCELLED) note(err.message, 'error');
    } finally {
      busy = false;
    }
  }
</script>

<dialog bind:this={dialog} {onclose} onclick={(e) => e.target === dialog && dialog.close()} aria-label={t('energie.contrat.titre')} tabindex="-1">
  <header>
    <div>
      <h2>{t('energie.contrat.titre')}</h2>
      <p class="sub">{t('energie.contrat.sous_titre')}</p>
    </div>
    <button class="round" onclick={() => dialog.close()} aria-label={t('commun.fermer')}><Icon name="close" size={22} /></button>
  </header>

  <div class="tabs" role="tablist" aria-label={t('energie.contrat.liste')}>
    {#each tabs as tab (tab.i)}
      <button role="tab" aria-selected={tab.i === sel} class:active={tab.i === sel} class:bad={issues.some((p) => p.index === tab.i)} onclick={() => ((sel = tab.i), (confirming = false))}>
        <span>{t(tab.when === 'later' ? 'energie.contrat.a_partir' : 'energie.contrat.depuis', { date: dayText(tab.from) })}</span>
        <small class={tab.when}>{t(`energie.contrat.etat_${tab.when}`)}</small>
      </button>
    {/each}
    <button class="add" onclick={add}><Icon name="plus" size={16} />{t('energie.contrat.nouveau')}</button>
  </div>

  {#if d}
    <label class="field">
      {t('energie.contrat.date')}
      <input type="date" bind:value={d.from} />
      <small class="muted">{t('energie.contrat.date_aide')}</small>
    </label>

    <fieldset>
      <legend>{t('energie.contrat.plages')}</legend>
      <DayStrip windows={d.windows} label={t('energie.contrat.apercu')} />
      {#if d.windows.length}
        <ul class="windows">
          {#each d.windows as w, i (i)}
            <li class:bad={mine.some((p) => (p.what === 'window' && p.window === i) || (p.what === 'overlap' && p.windows.includes(i)))}>
              <i class="dot"></i>
              <span class="muted">{t('energie.contrat.de')}</span>
              <input type="time" step="60" value={hhmm(w.from ?? 0)} onchange={(e) => setTime(w, 'from', e.currentTarget.value)} aria-label={t('energie.contrat.debut', { n: i + 1 })} />
              <span class="muted">{t('energie.contrat.a')}</span>
              <input type="time" step="60" value={hhmm((w.to ?? 0) % 1440)} onchange={(e) => setTime(w, 'to', e.currentTarget.value)} aria-label={t('energie.contrat.fin', { n: i + 1 })} />
              <button class="icon" onclick={() => d.windows.splice(i, 1)} aria-label={t('energie.contrat.retirer', { n: i + 1 })} title={t('energie.contrat.retirer', { n: i + 1 })}><Icon name="trash" size={17} /></button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="muted small">{t('energie.contrat.sans_plage')}</p>
      {/if}
      <button class="link" onclick={addWindow}><Icon name="plus" size={16} />{t('energie.contrat.ajouter_plage')}</button>
    </fieldset>

    <fieldset>
      <legend>{t('energie.contrat.tarifs')}</legend>
      <div class="prices">
        {#if d.windows.length}
          <label class="field price">
            <span><i class="tag cool"></i>{t('energie.contrat.prix_hc')}</span>
            <span class="unit"><input type="number" inputmode="decimal" min="0" step="0.0001" bind:value={d.hc} /><small>{currency}/kWh</small></span>
          </label>
          <label class="field price">
            <span><i class="tag warm"></i>{t('energie.contrat.prix_hp')}</span>
            <span class="unit"><input type="number" inputmode="decimal" min="0" step="0.0001" bind:value={d.hp} /><small>{currency}/kWh</small></span>
          </label>
        {:else}
          <label class="field price">
            <span>{t('energie.contrat.prix_base')}</span>
            <span class="unit"><input type="number" inputmode="decimal" min="0" step="0.0001" bind:value={d.base} /><small>{currency}/kWh</small></span>
          </label>
        {/if}
        <label class="field price">
          <span>{t('energie.contrat.abonnement')}</span>
          <span class="unit"><input type="number" inputmode="decimal" min="0" step="0.01" bind:value={d.fee} /><small>{t('energie.contrat.par_mois', { cur: currency })}</small></span>
        </label>
      </div>
    </fieldset>

    {#each mine as p (p.what)}
      <p class="problem">{t(PROBLEM[p.what])}</p>
    {/each}
    {#if elsewhere}<p class="problem">{t('energie.contrat.pb_ailleurs')}</p>{/if}
    <p class="muted small">{t('energie.contrat.recalcul')}</p>
  {/if}

  <footer>
    {#if drafts.length > 1}
      {#if confirming}
        <button class="danger" onclick={remove}>{t('energie.contrat.supprimer_oui')}</button>
      {:else}
        <button class="ghost" onclick={() => (confirming = true)}><Icon name="trash" size={15} />{t('energie.contrat.supprimer')}</button>
      {/if}
    {/if}
    <span class="push"></span>
    <button class="ghost" onclick={() => dialog.close()}>{t('commun.annuler')}</button>
    <button class="primary" disabled={busy || issues.length > 0 || !changed} onclick={save}>{t('energie.contrat.enregistrer')}</button>
  </footer>
</dialog>

<style>
  dialog {
    border: 0;
    padding: 18px;
    width: min(560px, 96vw);
    max-height: 94dvh;
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
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }

  h2 {
    margin: 0;
    font-size: 21px;
    font-weight: 750;
  }

  .sub {
    margin: 2px 0 0;
    font-size: 13px;
    color: var(--ink-3);
    font-weight: 600;
  }

  button {
    font: inherit;
    cursor: pointer;
  }

  .round {
    width: 40px;
    height: 40px;
    flex: none;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink);
  }

  /* The contracts, one tab each in the order of their days. */
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 16px;
  }

  .tabs button {
    display: inline-flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    border: 1.5px solid transparent;
    border-radius: 14px;
    padding: 7px 12px;
    background: var(--surface-2);
    color: var(--ink-2);
    font-size: 13.5px;
    font-weight: 650;
    text-align: left;
  }

  .tabs button.active {
    border-color: var(--ink);
    color: var(--ink);
    background: var(--surface);
  }

  .tabs button.bad {
    border-color: var(--alert);
  }

  .tabs small {
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-3);
  }

  .tabs small.now {
    color: var(--good);
  }

  .tabs small.later {
    color: var(--cool);
  }

  .tabs .add {
    flex-direction: row;
    align-items: center;
    gap: 5px;
    background: none;
    border: 1.5px dashed var(--line);
    color: var(--cool);
  }

  fieldset {
    border: 0;
    margin: 0 0 16px;
    padding: 0;
    display: grid;
    gap: 10px;
  }

  legend {
    padding: 0;
    margin-bottom: 8px;
    font-size: 12.5px;
    font-weight: 750;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--ink-3);
  }

  .field {
    display: grid;
    gap: 6px;
    margin-bottom: 16px;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .field small {
    font-weight: 550;
  }

  input[type='date'],
  input[type='time'],
  input[type='number'] {
    font: inherit;
    font-size: 15px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--surface-2);
    color: var(--ink);
    min-width: 0;
  }

  input[type='date'] {
    justify-self: start;
  }

  .windows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  .windows li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 6px 6px 12px;
    border-radius: var(--r-md);
    background: var(--surface-2);
    border: 1.5px solid transparent;
    font-size: 14px;
  }

  .windows li.bad {
    border-color: var(--alert);
  }

  .windows input {
    background: var(--surface);
    padding: 6px 8px;
  }

  .dot {
    flex: none;
    width: 10px;
    height: 10px;
    border-radius: 3px;
    background: var(--cool);
  }

  .icon {
    margin-left: auto;
    width: 36px;
    height: 36px;
    flex: none;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: none;
    color: var(--ink-3);
  }

  .link {
    justify-self: start;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    border: 0;
    padding: 4px 0;
    background: none;
    color: var(--cool);
    font-size: 14px;
    font-weight: 650;
  }

  .prices {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 12px;
  }

  .price {
    margin: 0;
  }

  .unit {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .unit input {
    width: 100%;
  }

  .unit small {
    flex: none;
    color: var(--ink-3);
  }

  .tag {
    display: inline-block;
    width: 9px;
    height: 9px;
    border-radius: 2px;
    margin-right: 6px;
  }

  .tag.cool {
    background: var(--cool);
  }

  .tag.warm {
    background: var(--warm);
  }

  .small {
    font-size: 13px;
    margin: 0;
  }

  .problem {
    margin: 0 0 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--alert);
  }

  footer {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 16px;
  }

  .push {
    flex: 1;
  }

  .primary,
  .ghost,
  .danger {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    font-size: 14px;
    font-weight: 650;
  }

  .primary {
    background: var(--ink);
    color: var(--surface);
  }

  .ghost {
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .danger {
    background: #c0392b;
    color: #fff;
  }

  .primary:disabled,
  .ghost:disabled {
    opacity: 0.5;
    cursor: default;
  }

  @media (max-width: 560px) {
    .windows li {
      gap: 6px;
      padding-left: 10px;
      font-size: 13px;
    }

    .windows input {
      font-size: 14px;
      padding: 5px 6px;
    }
  }
</style>
