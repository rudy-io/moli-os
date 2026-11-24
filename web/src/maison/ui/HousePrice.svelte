<script>
  import { home, note } from '../lib/home.svelte.js';
  import { energy, refresh } from '../lib/energy-live.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { isoDay, priceOk, rowsFrom, rowsOk, pricesFrom } from '../lib/house-prices.js';

  /** The house's price of a kWh, for the meters without one of their own
   *  (no connected meter, a price that changes every month): changed by a
   *  person (code asked), counted from now on; the hours already counted
   *  keep the price of their time. */
  let open = $state(false);
  let typed = $state('');
  let busy = $state(false);
  // The whole list, being edited: a day (YYYY-MM-DD) and a typed price per
  // row; `since` kept while the day is not changed (a price set at 6:47
  // stays at 6:47).
  let rows = $state(null);

  const s = $derived(energy.summary);
  const cur = $derived(s?.currency ?? '€');
  const prices = $derived([...(s?.house_prices ?? [])].reverse());
  const amount = (v) => v.toLocaleString(locale(), { minimumFractionDigits: 2, maximumFractionDigits: 4 });
  const day = (ts) => new Date(ts).toLocaleDateString(locale(), { day: 'numeric', month: 'long', year: 'numeric' });
  const parsed = $derived(Number(typed.trim().replace(',', '.')));
  const valid = $derived(typed.trim() !== '' && Number.isFinite(parsed) && parsed > 0 && parsed <= 5);

  function start() {
    typed = s?.house_price != null ? amount(s.house_price) : '';
    rows = null;
    open = !open;
  }

  function edit() {
    open = false;
    // Newest first, like the list of past prices.
    rows = rows ? null : rowsFrom(s?.house_prices, amount).reverse();
  }

  function addRow() {
    rows = [{ day: isoDay(Date.now()), since: null, typed: '' }, ...rows];
  }

  const listOk = $derived(rowsOk(rows));

  async function saveAll() {
    if (!listOk) return;
    busy = true;
    try {
      const prices = pricesFrom(rows);
      const res = await fetch('/api/energy/prices', {
        method: 'PUT',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: JSON.stringify({ prices }),
      });
      if (res.status === 403) {
        home.held = { label: t('energie.prix.historique'), reason: 'ton code', custom: saveAll };
        return;
      }
      const body = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(body.error ?? res.statusText);
      rows = null;
      note(t('energie.prix.historique_enregistre', { heures: (body.repriced ?? 0).toLocaleString(locale()) }), 'info');
      await refresh();
    } catch (e) {
      note(t('energie.prix.erreur', { message: e.message }), 'error');
    } finally {
      busy = false;
    }
  }

  async function save() {
    if (!valid) return;
    busy = true;
    try {
      const res = await fetch('/api/energy/price', {
        method: 'PUT',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: JSON.stringify({ price: parsed }),
      });
      if (res.status === 403) {
        // A person's gesture: the code first, then the same request again.
        home.held = { label: t('energie.prix.titre'), reason: 'ton code', custom: save };
        return;
      }
      const body = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(body.error ?? res.statusText);
      open = false;
      note(t('energie.prix.enregistre', { price: amount(parsed), cur }), 'info');
      await refresh();
    } catch (e) {
      note(t('energie.prix.erreur', { message: e.message }), 'error');
    } finally {
      busy = false;
    }
  }
</script>

{#if s}
  <section class="card price">
    <div class="row">
      <span class="ico"><Icon name="tune" size={18} /></span>
      <div class="text">
        <b>{t('energie.prix.titre')}</b>
        <span class="muted">
          {s.house_price != null
            ? t('energie.prix.actuel', { price: amount(s.house_price), cur, since: day(prices[0].since) })
            : t('energie.prix.aucun')}
        </span>
      </div>
      <div class="actions">
        <button class="btn" onclick={start} aria-expanded={open}>{s.house_price != null ? t('energie.prix.changer') : t('energie.prix.choisir')}</button>
        <button class="btn" onclick={edit} aria-expanded={!!rows}>{t('energie.prix.historique')}</button>
      </div>
    </div>
    {#if open}
      <form class="form" onsubmit={(e) => (e.preventDefault(), save())} autocomplete="off">
        <label>
          {t('energie.prix.champ', { cur })}
          <input bind:value={typed} inputmode="decimal" placeholder="0,2319" spellcheck="false" />
        </label>
        <button class="btn primary" disabled={busy || !valid}>{busy ? '…' : t('energie.prix.enregistrer')}</button>
        <small class="muted">{t('energie.prix.aide')}</small>
      </form>
    {/if}
    {#if rows}
      <form class="list" onsubmit={(e) => (e.preventDefault(), saveAll())} autocomplete="off">
        <div class="list-actions"><button type="button" class="btn" onclick={addRow}><Icon name="plus" size={16} />{t('energie.prix.ajouter')}</button></div>
        {#if rows.length}
          <div class="line head" aria-hidden="true"><span>{t('energie.prix.debut')}</span><span>{t('energie.prix.champ', { cur })}</span><span></span></div>
        {/if}
        {#each rows as row, i (i)}
          <div class="line">
            <input type="date" bind:value={row.day} required aria-label={t('energie.prix.debut')} />
            <input bind:value={row.typed} inputmode="decimal" placeholder="0,2016" spellcheck="false" aria-label={t('energie.prix.champ', { cur })} aria-invalid={row.typed !== '' && !priceOk(row.typed)} />
            <button type="button" class="btn ghost" onclick={() => (rows = rows.filter((_, j) => j !== i))} aria-label={t('energie.prix.retirer')}><Icon name="trash" size={16} /></button>
          </div>
        {/each}
        <div class="list-actions">
          <button class="btn primary" disabled={busy || !listOk}>{busy ? '…' : t('energie.prix.enregistrer')}</button>
        </div>
        <small class="muted">{t('energie.prix.historique_aide')}</small>
      </form>
    {:else if prices.length > 1}
      <!-- The current price is in the sentence above: the list only when asked. -->
      <details class="past">
        <summary>{t('energie.prix.voir_tous', { n: prices.length, since: day(prices[prices.length - 1].since) })}</summary>
        <ol>
          {#each prices as p (p.since)}
            <li><span>{t('energie.prix.depuis', { since: day(p.since) })}</span><b class="num">{amount(p.price)} {cur}/kWh</b></li>
          {/each}
        </ol>
      </details>
    {/if}
  </section>
{/if}

<style>
  .price {
    display: grid;
    gap: 14px;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .ico {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .text {
    display: grid;
    gap: 2px;
    flex: 1;
    min-width: 200px;
  }

  .text .muted {
    font-size: 13px;
  }

  .actions,
  .list-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .btn.ghost {
    background: none;
    color: var(--ink-3);
    padding: 9px;
  }

  .list {
    display: grid;
    gap: 10px;
  }

  .line {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) 36px;
    gap: 10px;
    align-items: end;
  }

  .line.head {
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-2);
    margin-bottom: -4px;
  }

  input[aria-invalid='true'] {
    border-color: var(--alert);
  }

  .btn {
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    font: inherit;
    font-weight: 650;
    background: var(--surface-2);
    color: var(--ink);
    cursor: pointer;
  }

  .btn.primary {
    background: var(--warm);
    color: var(--on-warm);
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .form {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 200px), 1fr));
    gap: 10px 14px;
    align-items: end;
  }

  label {
    display: grid;
    gap: 4px;
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-2);
  }

  input {
    font: inherit;
    font-size: 14px;
    padding: 9px 12px;
    border-radius: var(--r-sm);
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink);
  }

  small {
    grid-column: 1 / -1;
    font-size: 12px;
  }

  .past {
    font-size: 13px;
  }

  .past summary {
    cursor: pointer;
    color: var(--ink-3);
    font-weight: 600;
    width: fit-content;
  }

  .past ol {
    list-style: none;
    margin: 10px 0 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  .past li {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    color: var(--ink-2);
  }

  .past li:first-child {
    color: var(--ink);
  }
</style>
