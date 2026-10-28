<script>
  import Icon from '../Icon.svelte';
  import { cellier, euros, providerName, photoUrl, updatePurchase, deletePurchase, explain } from '../../lib/cellier.svelte.js';
  import { cost } from '../../lib/cellier/passages.js';
  import { toCents, parseAmount, centsText, qtyText, qtyPrice, dots } from '../../lib/cellier/draft.js';
  import { clock, note } from '../../lib/home.svelte.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';

  /** One trip to the till (a receipt, a drive basket, or lines recorded
   *  together): folded to its total, unfolded to its lines, each of which can
   *  be corrected or removed (a misreading seen afterwards). */
  let { passage, receipt = null } = $props();

  let open = $state(false);
  /** The line being corrected: `{id, product, qty, price}` (typed text). */
  let editing = $state(null);
  let busy = $state(false);
  /** Why the last correction failed: `{text, detail}`, or null. */
  let failure = $state(null);

  const products = $derived([...cellier.products].sort((a, b) => a.name.localeCompare(b.name, locale())));
  const productName = (id) => cellier.products.find((p) => p.id === id)?.name ?? id;
  const when = $derived(
    `${new Date(passage.at).toLocaleDateString(locale(), { weekday: 'short', day: 'numeric', month: 'long' })}, ${clock(passage.at)}`,
  );
  const photos = $derived(receipt?.photos ? Array.from({ length: receipt.photos }, (_, i) => i + 1) : []);

  function edit(line) {
    editing = { id: line.id, product: line.product, qty: qtyText(line.qty, locale()), price: centsText(line.price_cents, locale()) };
  }

  async function save(e, line) {
    e.preventDefault();
    const qty = parseAmount(editing.qty);
    const price = toCents(editing.price);
    if (!qty || qty <= 0) return (failure = { text: t('cellier.achats.quantite_invalide'), detail: '' });
    if (price == null) return (failure = { text: t('cellier.achats.prix_invalide'), detail: '' });
    busy = true;
    failure = null;
    try {
      // The cost the line had stays when only its product changes; otherwise
      // the server works it out again (quantity, price, discount).
      const same = qty === line.qty && price === line.price_cents;
      await updatePurchase(line.id, { qty, price_cents: price, line_cents: same ? (line.line_cents ?? null) : null, product: editing.product });
      editing = null;
      note(t('cellier.achats.corrige', { nom: productName(line.product) }));
    } catch (err) {
      failure = explain(err);
    } finally {
      busy = false;
    }
  }

  async function remove(line) {
    if (!confirm(t('cellier.achats.supprimer_confirmer', { nom: productName(line.product) }))) return;
    busy = true;
    failure = null;
    try {
      await deletePurchase(line.id);
      note(t('cellier.achats.supprime', { nom: productName(line.product) }));
    } catch (err) {
      failure = explain(err);
    } finally {
      busy = false;
    }
  }

  const detail = (line) => qtyPrice(line, euros, locale());
</script>

<details class="passage" bind:open>
  <summary>
    <span class="what">
      <b>
        {providerName(passage.provider)}
        {#if passage.kind === 'drive'}<small class="chip cool">{t('cellier.achats.drive')}</small>{/if}
      </b>
      <small class="muted">
        {dots([passage.store, when, t('cellier.achats.lignes', { count: passage.lines.length })])}
      </small>
    </span>
    <span class="sum">
      <b class="num">{euros(passage.total_cents)}</b>
      {#if passage.printed_cents != null}
        <small class="muted">{t('cellier.achats.sur_un_ticket', { montant: euros(passage.printed_cents) })}</small>
      {/if}
    </span>
    <span class="chev"><Icon name="chevron-down" size={22} /></span>
  </summary>

  {#if open}
    <div class="body">
      {#if photos.length}
        <div class="photos">
          {#each photos as n (n)}
            <a href={photoUrl(receipt.id, n)} target="_blank" rel="noopener">
              <img src={photoUrl(receipt.id, n)} alt={t('cellier.achats.photo', { n })} loading="lazy" />
            </a>
          {/each}
        </div>
      {/if}

      {#if failure}
        <p class="error" role="alert">
          <Icon name="alert" size={18} />
          <span>{failure.text}{#if failure.detail}<small>{failure.detail}</small>{/if}</span>
        </p>
      {/if}

      <ul class="lines">
        {#each passage.lines as line (line.id)}
          <li>
            {#if editing?.id === line.id}
              <form class="fix" onsubmit={(e) => save(e, line)}>
                <label class="wide">
                  <span>{t('cellier.achats.produit')}</span>
                  <select bind:value={editing.product}>
                    {#each products as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
                  </select>
                </label>
                <label>
                  <span>{t('cellier.achats.quantite')}{#if line.unit} ({line.unit}){/if}</span>
                  <input bind:value={editing.qty} inputmode="decimal" autocomplete="off" />
                </label>
                <label>
                  <span>{t('cellier.achats.prix_unitaire')}</span>
                  <input bind:value={editing.price} inputmode="decimal" autocomplete="off" />
                </label>
                <div class="actions">
                  <button
                    type="button"
                    class="ghost"
                    disabled={busy}
                    onclick={() => {
                      editing = null;
                      failure = null;
                    }}
                  >{t('cellier.achats.annuler')}</button>
                  <button class="primary" disabled={busy}>{t('cellier.achats.enregistrer')}</button>
                </div>
              </form>
            {:else}
              <div class="line">
                <span class="what">
                  <b>{productName(line.product)}</b>
                  <small class="muted">
                    {dots([
                      line.label,
                      detail(line),
                      line.discount_cents > 0 && t('cellier.achats.remise', { montant: euros(line.discount_cents) }),
                    ])}
                  </small>
                </span>
                <b class="num cost">{euros(cost(line))}</b>
                <button class="icon" disabled={busy} onclick={() => edit(line)} aria-label={t('cellier.achats.corriger', { nom: productName(line.product) })}>
                  <Icon name="pencil" size={20} />
                </button>
                <button class="icon danger" disabled={busy} onclick={() => remove(line)} aria-label={t('cellier.achats.supprimer', { nom: productName(line.product) })}>
                  <Icon name="trash" size={20} />
                </button>
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</details>

<style>
  .passage + :global(.passage) {
    border-top: 1px solid var(--line);
  }

  summary {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 64px;
    padding: 8px 0;
    list-style: none;
    cursor: pointer;
  }

  summary::-webkit-details-marker {
    display: none;
  }

  .what {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }

  .what b {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-weight: 650;
    overflow-wrap: anywhere;
  }

  .what small {
    font-size: 12.5px;
    overflow-wrap: anywhere;
  }

  .what .chip {
    padding: 1px 8px;
    font-size: 12px;
    font-weight: 600;
  }

  .sum {
    display: grid;
    justify-items: end;
    gap: 2px;
    text-align: right;
  }

  .sum b {
    font-size: 16px;
    font-weight: 650;
    white-space: nowrap;
  }

  .sum small {
    font-size: 12px;
  }

  .chev {
    color: var(--ink-3);
    transition: transform 0.2s var(--ease);
  }

  .passage[open] .chev {
    transform: rotate(180deg);
  }

  .body {
    padding: 0 0 12px;
  }

  .photos {
    display: flex;
    gap: 8px;
    overflow-x: auto;
    padding-bottom: 8px;
  }

  .photos a {
    flex: none;
  }

  .photos img {
    display: block;
    width: 72px;
    height: 96px;
    object-fit: cover;
    /* The head of a receipt (chain, store, date) tells which one it is. */
    object-position: top;
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }

  .error {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin-bottom: 8px;
    padding: 10px 14px;
    border-radius: var(--r-sm);
    background: var(--alert-soft);
    color: var(--alert);
    font-weight: 600;
  }

  .error :global(svg) {
    margin-top: 2px;
  }

  .error span {
    display: grid;
    gap: 2px;
    min-width: 0;
  }

  .error small {
    font-size: 12.5px;
    font-weight: 500;
    opacity: 0.8;
    overflow-wrap: anywhere;
  }

  .lines {
    list-style: none;
    margin: 0;
    padding: 0 0 0 12px;
    border-left: 3px solid var(--surface-3);
  }

  .lines li + li {
    border-top: 1px solid var(--line);
  }

  .line {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 52px;
    padding: 4px 0;
  }

  .line .what b {
    font-weight: 600;
  }

  .cost {
    font-weight: 600;
    white-space: nowrap;
    margin-right: 4px;
  }

  .icon {
    width: 44px;
    height: 44px;
    flex: none;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 999px;
    background: none;
    color: var(--ink-3);
  }

  .icon:hover {
    background: var(--surface-2);
    color: var(--ink);
  }

  .icon.danger:hover {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .icon:disabled {
    opacity: 0.4;
  }

  .fix {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
    padding: 12px 0;
  }

  .fix .wide,
  .fix .actions {
    grid-column: 1 / -1;
  }

  .fix label {
    display: grid;
    gap: 4px;
    font-size: 13px;
    font-weight: 600;
    color: var(--ink-2);
  }

  .fix input,
  .fix select {
    min-height: 44px;
    min-width: 0;
    padding: 0 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 16px;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .primary,
  .ghost {
    min-height: 44px;
    padding: 0 18px;
    border-radius: 999px;
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

  .primary:disabled,
  .ghost:disabled {
    opacity: 0.5;
  }

  @media (max-width: 420px) {
    .line {
      flex-wrap: wrap;
    }

    .line .what {
      flex-basis: 100%;
    }

    .cost {
      margin-right: auto;
    }
  }
</style>
