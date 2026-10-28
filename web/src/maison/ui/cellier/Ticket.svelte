<script>
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
  } from '../../lib/cellier.svelte.js';
  import { gapOf, draftProblems, listMatches, toCents, centsText, toLocalInput, fromLocalInput } from '../../lib/cellier/draft.js';
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
  /** The problems show once a validation was tried. */
  let checked = $state(false);

  function take(r) {
    receipt = r;
    draft = structuredClone($state.snapshot(r.draft));
    dirty = false;
  }

  async function load() {
    const known = cellier.receipts.find((r) => r.id === id);
    if (known) return take(known);
    try {
      take(await loadReceipt(id));
    } catch (e) {
      if (e.status === 404) missing = true;
      else error = e.message;
    }
  }
  load();

  const products = $derived([...cellier.products].sort((a, b) => a.name.localeCompare(b.name, locale())));
  const gap = $derived(draft ? gapOf(draft) : null);
  const problems = $derived(draft ? new Map(draftProblems(draft).map((p) => [p.index, p.why])) : new Map());
  const kept = $derived(draft ? draft.lines.filter((l) => l.keep) : []);
  const keptCents = $derived(kept.reduce((sum, l) => sum + l.line_cents, 0));
  const matches = $derived(draft ? listMatches(draft, cellier.items) : []);
  const twin = $derived(receipt?.duplicate_of != null ? cellier.receipts.find((r) => r.id === receipt.duplicate_of) : null);
  const photos = $derived(receipt?.photos ? Array.from({ length: receipt.photos }, (_, i) => i + 1) : []);
  /** Validated or abandoned elsewhere while open. */
  const gone = $derived(
    receipt?.status === 'draft' && cellier.purchasesLoaded && !busy && !cellier.receipts.some((r) => r.id === id && r.status === 'draft'),
  );

  const touched = () => (dirty = true);
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
    try {
      await task();
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  const save = () =>
    run(async () => {
      take(await saveDraft(id, draft));
      note(t('cellier.ticket.enregistre'));
    });

  function validate() {
    checked = true;
    if (problems.size) {
      error = t('cellier.ticket.a_corriger', { count: problems.size });
      document.getElementById(`cellier-line-${[...problems.keys()][0]}`)?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      return;
    }
    if (!kept.length && !confirm(t('cellier.ticket.aucune_ligne_confirmer'))) return;
    run(async () => {
      if (dirty) take(await saveDraft(id, draft));
      let force = false;
      if (receipt.duplicate_of != null) {
        if (!confirm(t('cellier.ticket.doublon_confirmer'))) return;
        force = true;
      }
      try {
        await validateReceipt(id, force);
      } catch (err) {
        // Recorded elsewhere in the meantime: the same question, then force.
        if (force || err.status !== 400 || !/already recorded/i.test(err.message)) throw err;
        if (!confirm(t('cellier.ticket.doublon_confirmer'))) return;
        await validateReceipt(id, true);
      }
      note(t('cellier.ticket.valide', { count: kept.length }));
      location.hash = '#/cellier/achats';
    });
  }

  function abandon() {
    if (!confirm(t('cellier.ticket.abandonner_confirmer'))) return;
    run(async () => {
      await abandonReceipt(id);
      note(t('cellier.ticket.abandonne'));
      location.hash = '#/cellier/achats';
    });
  }
</script>

<a class="back" href="#/cellier/achats"><Icon name="arrow-left" size={20} />{t('cellier.scan.retour')}</a>

{#if missing}
  <section class="card"><p class="muted">{t('cellier.ticket.introuvable')}</p></section>
{:else if !draft}
  {#if error}<p class="error" role="alert"><Icon name="alert" size={18} />{error}</p>{:else}<p class="muted">…</p>{/if}
{:else if receipt.status === 'validated'}
  <section class="card"><p class="muted">{t('cellier.ticket.deja_valide')}</p></section>
{:else}
  <div class="ticket">
    {#if gone}
      <p class="banner alert" role="alert"><Icon name="alert" size={18} />{t('cellier.ticket.parti')}</p>
    {/if}
    {#if receipt.duplicate_of != null}
      <p class="banner alert">
        <Icon name="alert" size={18} />
        {twin?.draft.at ? t('cellier.ticket.doublon', { date: day(twin.draft.at) }) : t('cellier.ticket.doublon_sans_date')}
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
          <input value={draft.provider} oninput={(e) => set('provider', e.currentTarget.value)} list="cellier-chains" autocomplete="off" />
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

    {#if error}<p class="error" role="alert"><Icon name="alert" size={18} />{error}</p>{/if}

    <div class="actions">
      <button class="ghost danger" disabled={busy || gone} onclick={abandon}><Icon name="trash" size={20} />{t('cellier.ticket.abandonner')}</button>
      <span class="grow"></span>
      <button class="ghost" disabled={busy || gone || !dirty} onclick={save}><Icon name="save" size={20} />{t('cellier.ticket.enregistrer')}</button>
      <button class="primary" disabled={busy || gone} onclick={validate}><Icon name="ok" size={20} />{t('cellier.ticket.valider')}</button>
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
