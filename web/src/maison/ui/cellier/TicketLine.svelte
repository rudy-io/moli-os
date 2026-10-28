<script>
  import { euros } from '../../lib/cellier.svelte.js';
  import { lineCost, parseAmount, toCents, centsText, qtyText, qtyPrice, dots } from '../../lib/cellier/draft.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';

  /** One line of a receipt draft. The usual corrections take one gesture:
   *  untick it, change its product, open « Correct » for quantity and price.
   *  `line` is edited in place; `onchange` says the draft moved. */
  let { line = $bindable(), index, products, problem = null, onchange } = $props();

  const NEW = '__new';
  let open = $state(false);

  const choice = $derived(line.product ?? (line.new_product != null ? NEW : ''));
  const id = $derived(`cellier-line-${index}`);

  function changed() {
    onchange?.();
  }

  function choose(value) {
    if (value === NEW) {
      line.product = null;
      line.new_product = line.new_product ?? (line.name || line.label);
    } else {
      line.product = value || null;
      line.new_product = null;
    }
    changed();
  }

  /** The cost follows quantity, price and discount, as the server counts it. */
  function recost() {
    line.line_cents = lineCost(line.qty, line.price_cents, line.discount_cents);
    changed();
  }

  function setQty(input) {
    const qty = parseAmount(input.value);
    if (qty && qty > 0) {
      line.qty = qty;
      recost();
    }
    input.value = qtyText(line.qty, locale());
  }

  function setCents(input, field, empty) {
    const cents = input.value.trim() === '' ? empty : toCents(input.value);
    if (cents != null) {
      line[field] = cents;
      recost();
    }
    input.value = centsText(line[field], locale());
  }

  /** « 1,29 € », « 2 × 1,29 € », « 0,532 kg × 12,99 €/kg ». */
  const detail = $derived(qtyPrice(line, euros, locale()));
</script>

<li {id} class:off={!line.keep} class:bad={problem}>
  <label class="keep">
    <input type="checkbox" bind:checked={line.keep} onchange={changed} aria-label={t('cellier.ticket.garder', { nom: line.name || line.label })} />
  </label>
  <div class="main">
    <div class="head">
      <b class="name">{line.name || line.label}</b>
      <b class="num cost">{euros(line.line_cents)}</b>
    </div>
    <small class="muted">
      {#if line.label && line.label !== line.name}<span class="printed">{line.label}</span>{' · '}{/if}{dots([
        detail,
        line.discount_cents > 0 && t('cellier.achats.remise', { montant: euros(line.discount_cents) }),
      ])}
    </small>
    {#if line.reason}<small class="reason">{line.reason}</small>{/if}

    {#if line.keep}
      <div class="product">
        <select value={choice} onchange={(e) => choose(e.currentTarget.value)} aria-label={t('cellier.ticket.produit_de', { nom: line.name || line.label })}>
          <option value="">{t('cellier.ticket.choisir_produit')}</option>
          <option value={NEW}>{t('cellier.ticket.nouveau_produit')}</option>
          {#each products as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
        </select>
        {#if choice === NEW}
          <input
            value={line.new_product}
            oninput={(e) => {
              line.new_product = e.currentTarget.value;
              changed();
            }}
            placeholder={t('cellier.ticket.nouveau_nom')}
            aria-label={t('cellier.ticket.nouveau_nom')}
            autocomplete="off"
          />
        {/if}
      </div>
    {/if}

    {#if problem}<p class="problem" role="alert">{t(`cellier.ticket.probleme.${problem}`)}</p>{/if}

    <button type="button" class="link" aria-expanded={open} aria-controls="{id}-fix" onclick={() => (open = !open)}>
      {open ? t('cellier.ticket.fermer') : t('cellier.ticket.corriger')}
    </button>
    {#if open}
      <div class="fix" id="{id}-fix">
        <label class="wide">
          <span>{t('cellier.ticket.nom')}</span>
          <input
            value={line.name}
            oninput={(e) => {
              line.name = e.currentTarget.value;
              changed();
            }}
            autocomplete="off"
          />
        </label>
        <label>
          <span>{t('cellier.achats.quantite')}{#if line.unit} ({line.unit}){/if}</span>
          <input value={qtyText(line.qty, locale())} onchange={(e) => setQty(e.currentTarget)} inputmode="decimal" autocomplete="off" />
        </label>
        <label>
          <span>{t('cellier.achats.prix_unitaire')}</span>
          <input value={centsText(line.price_cents, locale())} onchange={(e) => setCents(e.currentTarget, 'price_cents', null)} inputmode="decimal" autocomplete="off" />
        </label>
        <label>
          <span>{t('cellier.ticket.remise')}</span>
          <input value={centsText(line.discount_cents, locale())} onchange={(e) => setCents(e.currentTarget, 'discount_cents', 0)} inputmode="decimal" autocomplete="off" />
        </label>
      </div>
    {/if}
  </div>
</li>

<style>
  li {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 12px 0;
    scroll-margin: 120px;
  }

  li.off .head,
  li.off .muted {
    opacity: 0.55;
  }

  li.off .name {
    text-decoration: line-through;
    text-decoration-color: var(--ink-3);
  }

  li.bad {
    margin: 0 -10px;
    padding: 12px 10px;
    border-radius: var(--r-sm);
    background: var(--alert-soft);
  }

  /* A 44px target around a 24px box. */
  .keep {
    width: 44px;
    height: 44px;
    flex: none;
    margin: -10px -6px -10px -10px;
    display: grid;
    place-items: center;
    cursor: pointer;
  }

  .keep input {
    width: 24px;
    height: 24px;
    margin: 0;
    accent-color: var(--good);
    cursor: pointer;
  }

  .main {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 4px;
  }

  .head {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }

  .name {
    flex: 1;
    min-width: 0;
    font-weight: 650;
    overflow-wrap: anywhere;
  }

  .cost {
    font-weight: 650;
    white-space: nowrap;
  }

  small {
    font-size: 12.5px;
    overflow-wrap: anywhere;
  }

  .printed {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    letter-spacing: 0.02em;
  }

  .reason {
    color: var(--warm-ink);
  }

  .product {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 220px), 1fr));
    gap: 8px;
    margin-top: 6px;
  }

  select,
  .main input {
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

  .problem {
    color: var(--alert);
    font-size: 13.5px;
    font-weight: 600;
  }

  .link {
    justify-self: start;
    min-height: 36px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--ink-2);
    font-size: 13.5px;
    font-weight: 650;
    text-decoration: underline;
    text-underline-offset: 3px;
    cursor: pointer;
  }

  .fix {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 10px;
  }

  .fix .wide {
    grid-column: 1 / -1;
  }

  .fix label {
    display: grid;
    gap: 4px;
    font-size: 13px;
    font-weight: 600;
    color: var(--ink-2);
  }

  @media (max-width: 420px) {
    .fix {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
