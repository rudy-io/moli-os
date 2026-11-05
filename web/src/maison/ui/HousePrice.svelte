<script>
  import { home, note } from '../lib/home.svelte.js';
  import { energy, refresh } from '../lib/energy-live.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The house's price of a kWh, for the meters without one of their own
   *  (no connected meter, a price that changes every month): changed by a
   *  person (code asked), counted from now on; the hours already counted
   *  keep the price of their time. */
  let open = $state(false);
  let typed = $state('');
  let busy = $state(false);

  const s = $derived(energy.summary);
  const cur = $derived(s?.currency ?? '€');
  const prices = $derived([...(s?.house_prices ?? [])].reverse());
  const amount = (v) => v.toLocaleString(locale(), { minimumFractionDigits: 2, maximumFractionDigits: 4 });
  const day = (ts) => new Date(ts).toLocaleDateString(locale(), { day: 'numeric', month: 'long', year: 'numeric' });
  const parsed = $derived(Number(typed.trim().replace(',', '.')));
  const valid = $derived(typed.trim() !== '' && Number.isFinite(parsed) && parsed > 0 && parsed <= 5);

  function start() {
    typed = s?.house_price != null ? amount(s.house_price) : '';
    open = !open;
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
      <button class="btn" onclick={start} aria-expanded={open}>{s.house_price != null ? t('energie.prix.changer') : t('energie.prix.choisir')}</button>
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
    {#if prices.length > 1}
      <ol class="past">
        {#each prices as p (p.since)}
          <li><span>{t('energie.prix.depuis', { since: day(p.since) })}</span><b class="num">{amount(p.price)} {cur}/kWh</b></li>
        {/each}
      </ol>
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
    color: #fff;
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
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
    font-size: 13px;
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
