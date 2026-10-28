<script>
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import TicketLine from './TicketLine.svelte';
  import {
    cellier,
    euros,
    providerName,
    photoUrl,
    loadReceipt,
    saveDraft,
    validateReceipt,
    abandonReceipt,
    explain,
  } from '../../lib/cellier.svelte.js';
  import {
    gapOf,
    draftProblems,
    listMatches,
    pruneItemsDone,
    toCents,
    centsText,
    toLocalInput,
    fromLocalInput,
    dateWarning,
  } from '../../lib/cellier/draft.js';
  import { holdLeave } from '../../../lib/leave.js';
  import { home, note, relative } from '../../lib/home.svelte.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';

  /** A receipt read and not yet recorded: a person checks it, corrects it,
   *  then validates it (its kept lines become purchases) or abandons it.
   *  The edits stay here until saved: a change arriving live (the other
   *  adult, an agent) does not wipe them. */
  let { id } = $props();

  /** The receipt as read or saved last. */
  let receipt = $state(null);
  /** Its draft being edited. */
  let draft = $state(null);
  let dirty = $state(false);
  let missing = $state(false);
  let busy = $state(false);
  let error = $state('');
  /** The server's own words under `error`, when they say more. */
  let detail = $state('');
  /** The problems show once a validation was tried. */
  let checked = $state(false);
  /** The server refused a save or a validation: the draft was saved
   *  elsewhere since it was loaded here. */
  let conflict = $state(false);

  function take(r) {
    receipt = r;
    draft = $state.snapshot(r.draft);
    dirty = false;
  }

  let loading = $state(false);
  async function load() {
    const known = cellier.receipts.find((r) => r.id === id);
    if (known) return take(known);
    loading = true;
    error = '';
    try {
      take(await loadReceipt(id));
    } catch (e) {
      if (e.status === 404) missing = true;
      else error = e.message;
    } finally {
      loading = false;
    }
  }
  load();

  // Edits not saved: leaving asks first (in-app links, the back button,
  // closing the tab).
  onMount(() => {
    const lift = holdLeave(() => !dirty || confirm(t('cellier.ticket.quitter_confirmer')));
    const unload = (e) => {
      if (dirty) e.preventDefault();
    };
    addEventListener('beforeunload', unload);
    return () => {
      lift();
      removeEventListener('beforeunload', unload);
    };
  });

  const products = $derived([...cellier.products].sort((a, b) => a.name.localeCompare(b.name, locale())));
  const gap = $derived(draft ? gapOf(draft) : null);
  const allProblems = $derived(draft ? draftProblems(draft) : []);
  const noProvider = $derived(allProblems.some((p) => p.why === 'provider'));
  const problems = $derived(new Map(allProblems.filter((p) => p.index != null).map((p) => [p.index, p.why])));
  const kept = $derived(draft ? draft.lines.filter((l) => l.keep) : []);
  const keptCents = $derived(kept.reduce((sum, l) => sum + l.line_cents, 0));
  const matches = $derived(draft ? listMatches(draft, cellier.items) : []);
  const twin = $derived(receipt?.duplicate_of != null ? cellier.receipts.find((r) => r.id === receipt.duplicate_of) : null);
  const photos = $derived(receipt?.photos ? Array.from({ length: receipt.photos }, (_, i) => i + 1) : []);
  /** The draft as the last read shows it: another version than the one
   *  loaded here means someone saved it meanwhile (read after this page's
   *  own writes, so not while busy). */
  const listed = $derived(cellier.receipts.find((r) => r.id === id && r.status === 'draft'));
  const changedElsewhere = $derived(
    receipt?.status === 'draft' && (conflict || (!busy && listed != null && listed.version !== receipt.version)),
  );
  const when = $derived(draft ? dateWarning(draft.at, home.now) : null);
  /** Validated or abandoned elsewhere while open: only concluded from a
   *  purchases read that succeeded. */
  const gone = $derived(
    receipt?.status === 'draft' &&
      cellier.purchasesLoaded &&
      !cellier.purchasesError &&
      !busy &&
      !cellier.receipts.some((r) => r.id === id && r.status === 'draft'),
  );

  /** A line changed: a list item ticked for it may no longer be settled. */
  function touched() {
    dirty = true;
    prune();
  }
  function prune() {
    const kept = pruneItemsDone(draft, cellier.items);
    if (kept.length !== draft.items_done.length) draft.items_done = kept;
  }
  function set(field, value) {
    draft[field] = value;
    dirty = true;
  }

  function setTotal(input) {
    const cents = input.value.trim() === '' ? null : toCents(input.value);
    if (cents != null || input.value.trim() === '') set('total_cents', cents);
    input.value = centsText(draft.total_cents, locale());
  }

  function tickItem(item, on) {
    draft.items_done = on ? [...draft.items_done, item.id] : draft.items_done.filter((i) => i !== item.id);
    dirty = true;
  }

  const day = (ms) => new Date(ms).toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' });

  async function run(task) {
    if (busy) return;
    busy = true;
    error = '';
    detail = '';
    try {
      await task();
    } catch (err) {
      if (err.status === 409) conflict = true;
      else ({ text: error, detail } = explain(err));
    } finally {
      busy = false;
    }
  }

  /** Saves the draft as edited, without list items it no longer settles,
   *  over the version loaded here (`version`: another one, to keep this
   *  page's draft over a newer one). */
  async function store(version = receipt.version) {
    prune();
    take(await saveDraft(id, draft, version));
    conflict = false;
  }

  /** Changed elsewhere: take the other version, this page's edits go. */
  const reload = () =>
    run(async () => {
      take(await loadReceipt(id));
      conflict = false;
    });

  /** Changed elsewhere: save this page's draft over the other version. */
  const keepMine = () =>
    run(async () => {
      await store((await loadReceipt(id)).version);
      note(t('cellier.ticket.enregistre'));
    });

  const save = () =>
    run(async () => {
      await store();
      note(t('cellier.ticket.enregistre'));
    });

  function validate() {
    checked = true;
    if (changedElsewhere) return;
    if (noProvider) {
      error = t('cellier.ticket.probleme.provider');
      detail = '';
      document.getElementById('cellier-ticket-provider')?.focus();
      return;
    }
    if (problems.size) {
      error = t('cellier.ticket.a_corriger', { count: problems.size });
      detail = '';
      document.getElementById(`cellier-line-${[...problems.keys()][0]}`)?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      return;
    }
    if (!kept.length && !confirm(t('cellier.ticket.aucune_ligne_confirmer'))) return;
    run(async () => {
      if (dirty) await store();
      else if (pruneItemsDone(draft, cellier.items).length !== draft.items_done.length) await store();
      let force = false;
      if (receipt.duplicate_of != null) {
        if (!confirm(t('cellier.ticket.doublon_confirmer'))) return;
        force = true;
      }
      try {
        await validateReceipt(id, force, receipt.version);
      } catch (err) {
        // Recorded elsewhere in the meantime: the same question, then force.
        if (force || err.status !== 400 || !/already recorded/i.test(err.detail ?? '')) throw err;
        if (!confirm(t('cellier.ticket.doublon_confirmer'))) return;
        await validateReceipt(id, true, receipt.version);
      }
      note(t('cellier.ticket.valide', { count: kept.length }));
      location.hash = '#/cellier/achats';
    });
  }

  function abandon() {
    if (!confirm(t('cellier.ticket.abandonner_confirmer'))) return;
    run(async () => {
      await abandonReceipt(id);
      // The edits went with the draft: nothing left to keep.
      dirty = false;
      note(t('cellier.ticket.abandonne'));
      location.hash = '#/cellier/achats';
    });
  }
</script>

<a class="back" href="#/cellier/achats"><Icon name="arrow-left" size={20} />{t('cellier.ticket.retour')}</a>

{#if missing}
  <section class="card"><p class="muted">{t('cellier.ticket.introuvable')}</p></section>
{:else if !draft}
  {#if error}
    <div class="error" role="alert">
      <Icon name="alert" size={18} />
      <span>{error}</span>
      <button disabled={loading} onclick={load}>{t('cellier.reessayer')}</button>
    </div>
  {:else}
    <p class="muted">…</p>
  {/if}
{:else if receipt.status === 'validated'}
  <section class="card"><p class="muted">{t('cellier.ticket.deja_valide')}</p></section>
{:else}
  <div class="ticket">
    {#if gone}
      <p class="banner alert" role="alert"><Icon name="alert" size={18} />{t('cellier.ticket.parti')}</p>
    {:else if changedElsewhere}
      <div class="banner alert conflict" role="alert">
        <Icon name="alert" size={18} />
        <span>{t('cellier.ticket.modifie_ailleurs')}</span>
        <span class="choices">
          <button disabled={busy} onclick={reload}>{t('cellier.ticket.recharger')}</button>
          <button disabled={busy} onclick={keepMine}>{t('cellier.ticket.garder_ma_version')}</button>
        </span>
      </div>
    {/if}
    {#if receipt.duplicate_of != null}
      <p class="banner alert">
        <Icon name="alert" size={18} />
        {twin?.draft.at ? t('cellier.ticket.doublon', { date: day(twin.draft.at) }) : t('cellier.ticket.doublon_sans_date')}
      </p>
    {/if}
    {#if receipt.drive_twin}
      <p class="banner warm">
        <Icon name="warning" size={18} />
        {t('cellier.ticket.drive_double', { date: day(receipt.drive_twin.at), montant: euros(receipt.drive_twin.total_cents) })}
      </p>
    {/if}
    {#if when}
      <p class="banner warm">
        <Icon name="warning" size={18} />
        {when === 'future' ? t('cellier.ticket.date_future') : t('cellier.ticket.date_ancienne')}
      </p>
    {/if}
    {#if gap != null && Math.abs(gap) > 1}
      <p class="banner warm">
        <Icon name="warning" size={18} />
        {gap > 0 ? t('cellier.ticket.manque', { montant: euros(gap) }) : t('cellier.ticket.depasse', { montant: euros(-gap) })}
      </p>
    {/if}

    <section class="card head" aria-labelledby="cellier-ticket-title">
      <h2 id="cellier-ticket-title">{t('cellier.ticket.titre')}</h2>
      <p class="muted small">
        {t('cellier.ticket.lu_par', { lecteur: receipt.reader, nom: receipt.sent_by, quand: relative(receipt.sent_at, home.now) })}
      </p>
      <div class="fields">
        <label>
          <span>{t('cellier.ticket.enseigne')}</span>
          <input
            id="cellier-ticket-provider"
            value={draft.provider}
            oninput={(e) => set('provider', e.currentTarget.value)}
            list="cellier-chains"
            autocomplete="off"
            aria-invalid={checked && noProvider}
          />
        </label>
        <label>
          <span>{t('cellier.ticket.magasin')}</span>
          <input value={draft.store} oninput={(e) => set('store', e.currentTarget.value)} autocomplete="off" />
        </label>
        <label>
          <span>{t('cellier.ticket.date')}</span>
          <input type="datetime-local" value={toLocalInput(draft.at)} onchange={(e) => set('at', fromLocalInput(e.currentTarget.value))} />
        </label>
        <label>
          <span>{t('cellier.ticket.total')}</span>
          <input value={centsText(draft.total_cents, locale())} onchange={(e) => setTotal(e.currentTarget)} inputmode="decimal" autocomplete="off" />
        </label>
      </div>
      <datalist id="cellier-chains">
        {#each cellier.providers as p (p.info.id)}<option value={p.info.id}>{p.info.name}</option>{/each}
      </datalist>
      {#if draft.provider && providerName(draft.provider) !== draft.provider}
        <small class="muted">{providerName(draft.provider)}</small>
      {/if}

      {#if photos.length}
        <div class="photos">
          {#each photos as n (n)}
            <a href={photoUrl(receipt.id, n)} target="_blank" rel="noopener">
              <img src={photoUrl(receipt.id, n)} alt={t('cellier.achats.photo', { n })} />
            </a>
          {/each}
        </div>
      {/if}
    </section>

    <section class="card" aria-labelledby="cellier-ticket-lines">
      <div class="lines-head">
        <h2 id="cellier-ticket-lines">{t('cellier.ticket.lignes')}</h2>
        <span class="muted small">{t('cellier.ticket.gardees', { n: kept.length, total: draft.lines.length, montant: euros(keptCents) })}</span>
      </div>
      <p class="muted small">{t('cellier.ticket.lignes_aide')}</p>
      <ul class="lines">
        {#each draft.lines as _, i (i)}
          <TicketLine bind:line={draft.lines[i]} index={i} {products} problem={checked ? (problems.get(i) ?? null) : null} onchange={touched} />
        {/each}
      </ul>
    </section>

    {#if matches.length}
      <section class="card" aria-labelledby="cellier-ticket-items">
        <h2 id="cellier-ticket-items">{t('cellier.ticket.liste')}</h2>
        <p class="muted small">{t('cellier.ticket.liste_aide')}</p>
        <ul class="items">
          {#each matches as item (item.id)}
            <li>
              <label>
                <input type="checkbox" checked={draft.items_done.includes(item.id)} onchange={(e) => tickItem(item, e.currentTarget.checked)} />
                <span>{item.text}</span>
              </label>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if error}
      <div class="error" role="alert">
        <Icon name="alert" size={18} />
        <span>{error}{#if detail}<small>{detail}</small>{/if}</span>
      </div>
    {/if}

    <div class="actions">
      <button class="ghost danger" disabled={busy || gone} onclick={abandon}><Icon name="trash" size={20} />{t('cellier.ticket.abandonner')}</button>
      <span class="grow"></span>
      <button class="ghost" disabled={busy || gone || changedElsewhere || !dirty} onclick={save}><Icon name="save" size={20} />{t('cellier.ticket.enregistrer')}</button>
      <button class="primary" disabled={busy || gone || changedElsewhere} onclick={validate}><Icon name="ok" size={20} />{t('cellier.ticket.valider')}</button>
    </div>
  </div>
{/if}

<style>
  .back {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    justify-self: start;
    min-height: 44px;
    color: var(--ink-2);
    font-weight: 600;
    text-decoration: none;
  }

  .ticket {
    display: grid;
    gap: 16px;
  }

  h2 {
    font-size: 17px;
    font-weight: 650;
  }

  .small {
    font-size: 13px;
  }

  .banner,
  .error {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 12px 16px;
    border-radius: var(--r-sm);
    font-weight: 600;
  }

  .banner :global(svg),
  .error :global(svg) {
    margin-top: 2px;
  }

  .error span {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }

  .error small {
    font-size: 12.5px;
    font-weight: 500;
    opacity: 0.8;
    overflow-wrap: anywhere;
  }

  .error button {
    flex: none;
    min-height: 40px;
    padding: 0 16px;
    border: 0;
    border-radius: 999px;
    background: var(--surface);
    color: var(--alert);
    font-weight: 650;
  }

  .banner.conflict {
    flex-wrap: wrap;
    align-items: center;
  }

  .banner.conflict > span:first-of-type {
    flex: 1;
    min-width: min(100%, 220px);
  }

  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .choices button {
    min-height: 40px;
    padding: 0 16px;
    border: 0;
    border-radius: 999px;
    background: var(--surface);
    color: var(--alert);
    font-weight: 650;
  }

  .fields input[aria-invalid='true'] {
    border-color: var(--alert);
  }

  .banner.warm {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .banner.alert,
  .error {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .head {
    display: grid;
    gap: 10px;
  }

  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 200px), 1fr));
    gap: 10px;
  }

  label {
    display: grid;
    gap: 4px;
    font-size: 13px;
    font-weight: 600;
    color: var(--ink-2);
  }

  .fields input {
    min-height: 44px;
    min-width: 0;
    width: 100%;
    padding: 0 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 16px;
  }

  .photos {
    display: flex;
    gap: 8px;
    overflow-x: auto;
  }

  .photos a {
    flex: none;
  }

  .photos img {
    display: block;
    width: 84px;
    height: 112px;
    object-fit: cover;
    /* The head of a receipt (chain, store, date) tells which one it is. */
    object-position: top;
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }

  .lines-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 4px 12px;
  }

  .lines,
  .items {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
  }

  .lines > :global(li + li),
  .items li + li {
    border-top: 1px solid var(--line);
  }

  .items label {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 48px;
    font-size: 15px;
    font-weight: 500;
    color: var(--ink);
    cursor: pointer;
  }

  .items input {
    width: 22px;
    height: 22px;
    flex: none;
    accent-color: var(--good);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
  }

  .grow {
    flex: 1;
  }

  .primary,
  .ghost {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 50px;
    padding: 0 22px;
    border-radius: 999px;
    font-size: 15px;
    font-weight: 650;
  }

  .primary {
    border: 0;
    background: var(--ink);
    color: var(--surface);
  }

  .ghost {
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink-2);
  }

  .ghost.danger:hover:not(:disabled) {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .primary:disabled,
  .ghost:disabled {
    opacity: 0.45;
  }

  /* Phones: Validate first and full width, Abandon last. */
  @media (max-width: 760px) {
    .actions {
      flex-direction: column-reverse;
      align-items: stretch;
    }

    .grow {
      display: none;
    }
  }
</style>
