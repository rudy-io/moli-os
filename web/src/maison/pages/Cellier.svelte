<script>
  import { onMount } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import Liste from '../ui/cellier/Liste.svelte';
  import Panier from '../ui/cellier/Panier.svelte';
  import Achats from '../ui/cellier/Achats.svelte';
  import Scan from '../ui/cellier/Scan.svelte';
  import Ticket from '../ui/cellier/Ticket.svelte';
  import Produits from '../ui/cellier/Produits.svelte';
  import Stats from '../ui/cellier/Stats.svelte';
  import Reglages from '../ui/cellier/Reglages.svelte';
  import { cellier, loadCellier, loadPurchases, loadReader, reloadCellier } from '../lib/cellier.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** « Le Cellier »: the household's shopping list, the drives' basket and
   *  what was bought. Humans add needs, choose and check receipts; an agent
   *  reads prices and proposes baskets in the background, so everything here
   *  follows `pantry_changed`. Each page has its address, `#/cellier/<page>`;
   *  a receipt is scanned at `#/cellier/achats/scan` and checked at
   *  `#/cellier/achats/ticket/<id>`. */
  let { route = '' } = $props();

  const PAGES = ['liste', 'panier', 'achats', 'produits', 'stats', 'reglages'];
  const parts = $derived(route.split('/'));
  const page = $derived(PAGES.includes(parts[1]) ? parts[1] : 'liste');
  const sub = $derived(page === 'achats' ? (parts[2] ?? '') : '');
  const ticketId = $derived(sub === 'ticket' && /^\d+$/.test(parts[3] ?? '') ? Number(parts[3]) : null);

  // The purchases too, from the start: the drafts to check badge their page.
  onMount(() => {
    loadCellier();
    loadPurchases();
    loadReader();
  });

  // Every read that failed, once each.
  const errors = $derived([...new Set([cellier.error, cellier.purchasesError, cellier.readerError].filter(Boolean))]);
  let retrying = $state(false);
  async function retry() {
    retrying = true;
    await Promise.all([reloadCellier(), loadReader()]);
    retrying = false;
  }

  // Small cues on the pages: what is on the list, a basket waiting for a
  // choice, receipts read and not yet checked, sheets an agent found that
  // nobody checked yet.
  const pending = $derived(cellier.offers.filter((o) => o.status === 'pending').length);
  const drafts = $derived(cellier.receipts.filter((r) => r.status === 'draft').length);
  const badge = $derived({
    liste: cellier.items.length || null,
    panier: cellier.basket?.status === 'proposed' ? '•' : null,
    achats: drafts || null,
    produits: pending || null,
    stats: null,
    reglages: null,
  });
</script>

<div class="cellier">
  <header>
    <h1 class="page-title">{t('cellier.titre')}</h1>
    <p class="page-sub">{t('cellier.sous_titre')}</p>
  </header>

  {#if !cellier.loaded}
    <p class="muted">…</p>
  {:else if cellier.absent}
    <section class="card absent">
      <span class="ic"><Icon name="basket" size={26} /></span>
      <div>
        <h2>{t('cellier.absent.titre')}</h2>
        <p class="muted">{t('cellier.absent.texte')}</p>
      </div>
    </section>
  {:else}
    {#if errors.length}
      <div class="error" role="alert">
        <Icon name="alert" size={18} />
        <span>{errors.join(' ')}</span>
        <button disabled={retrying} onclick={retry}>{t('cellier.reessayer')}</button>
      </div>
    {/if}

    <nav class="tabs" aria-label={t('cellier.vues')}>
      {#each PAGES as id (id)}
        <a href="#/cellier/{id}" class:on={page === id} aria-current={page === id ? 'page' : undefined}>
          {t(`cellier.onglet.${id}`)}
          {#if badge[id]}<small class="num">{badge[id]}</small>{/if}
        </a>
      {/each}
    </nav>

    {#if page === 'panier'}
      <Panier />
    {:else if page === 'achats' && sub === 'scan'}
      <Scan />
    {:else if page === 'achats' && ticketId != null}
      {#key ticketId}<Ticket id={ticketId} />{/key}
    {:else if page === 'achats'}
      <Achats />
    {:else if page === 'produits'}
      <Produits />
    {:else if page === 'stats'}
      <Stats />
    {:else if page === 'reglages'}
      <Reglages />
    {:else}
      <Liste />
    {/if}
  {/if}
</div>

<style>
  .cellier {
    display: grid;
    gap: 20px;
    max-width: 980px;
  }

  .absent {
    display: flex;
    gap: 16px;
    align-items: flex-start;
  }

  .absent h2 {
    font-size: 18px;
    font-weight: 650;
    margin-bottom: 4px;
  }

  .ic {
    width: 48px;
    height: 48px;
    flex: none;
    border-radius: 16px;
    display: grid;
    place-items: center;
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .error {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 16px;
    border-radius: var(--r-sm);
    background: var(--alert-soft);
    color: var(--alert);
    font-weight: 550;
  }

  .error span {
    flex: 1;
    min-width: 0;
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

  .tabs {
    display: flex;
    gap: 2px;
    padding: 4px;
    border-radius: 999px;
    background: var(--surface-3);
    justify-self: start;
    max-width: 100%;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .tabs a {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 40px;
    padding: 0 16px;
    border-radius: 999px;
    color: var(--ink-3);
    font-size: 14.5px;
    font-weight: 650;
    text-decoration: none;
    white-space: nowrap;
    transition: background 0.2s var(--ease), color 0.2s var(--ease);
  }

  .tabs a.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .tabs small {
    min-width: 20px;
    height: 20px;
    padding: 0 6px;
    border-radius: 999px;
    display: inline-grid;
    place-items: center;
    background: var(--warm-soft);
    color: var(--warm-ink);
    font-size: 12px;
    font-weight: 700;
  }

  @media (max-width: 760px) {
    .tabs {
      justify-self: stretch;
    }

    .tabs a {
      flex: 1;
      justify-content: center;
      padding: 0 10px;
    }
  }

  /* Six pages do not fit one row on a phone: two rows of three, every page
     in sight (a row scrolling sideways would hide some). */
  @media (max-width: 720px) {
    .tabs {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      border-radius: 24px;
    }

    .tabs a {
      padding: 0 4px;
      font-size: 14px;
    }
  }
</style>
