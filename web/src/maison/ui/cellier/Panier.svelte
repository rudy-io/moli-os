<script>
  import Icon from '../Icon.svelte';
  import { cellier, chooseOption, setBasketStatus, confirmOffer, euros, providerName } from '../../lib/cellier.svelte.js';
  import { home, note, relative } from '../../lib/home.svelte.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';

  /** The open basket: its options side by side (the recommended one first),
   *  what it leaves out, the changes it proposes, then where it stands. */

  const basket = $derived(cellier.basket);
  const options = $derived(basket?.options);
  const proposed = $derived(basket?.status === 'proposed');
  /** The statuses that wait for someone, and say for whom. */
  const STEPS = ['chosen', 'filled', 'ordered'];

  const items = $derived(new Map(cellier.items.map((i) => [i.id, i])));
  const nameOf = (id) => items.get(id)?.text ?? t('cellier.panier.article_inconnu', { id });
  const qtyOf = (id) => {
    const q = items.get(id)?.qty;
    return q && q !== 1 ? `${q.toLocaleString(locale(), { maximumFractionDigits: 2 })} × ` : '';
  };
  const productName = (id) => cellier.products.find((p) => p.id === id)?.name ?? id;
  const label = (key) => (key === 'split' ? t('cellier.panier.reparti') : providerName(key));

  const choices = $derived.by(() => {
    if (!options) return [];
    const all = [...options.singles, ...(options.split ? [options.split] : [])];
    const rank = (c) => (c.key === options.recommended ? 0 : c.valid ? 1 : 2);
    return all.sort((a, b) => rank(a) - rank(b) || a.total_cents - b.total_cents);
  });
  const recommendedTotal = $derived(choices.find((c) => c.key === options?.recommended)?.total_cents ?? null);

  // The ticks are the household's until it chooses: an agent re-reading prices
  // in the meantime must not clear them. A new basket starts from its own.
  let accepted = $state([]);
  let ticksFor = null;
  $effect(() => {
    if (basket?.id !== ticksFor) {
      ticksFor = basket?.id ?? null;
      accepted = [...(basket?.accepted ?? [])];
    }
  });
  const ticked = (i) => (proposed ? accepted : (basket?.accepted ?? [])).includes(i);
  function tick(i, on) {
    accepted = on ? [...accepted, i].sort((a, b) => a - b) : accepted.filter((x) => x !== i);
  }

  // Sheets an agent found and nobody checked, for the products on the list.
  const listed = $derived(new Set(cellier.items.map((i) => i.product).filter(Boolean)));
  const toConfirm = $derived(cellier.offers.filter((o) => o.status === 'pending' && listed.has(o.product)));
  const pendingFor = (itemId, provider) => {
    const product = items.get(itemId)?.product;
    return product ? cellier.offers.filter((o) => o.product === product && o.provider === provider && o.status === 'pending') : [];
  };

  let busy = $state(false);
  async function act(run, done) {
    if (busy) return;
    busy = true;
    try {
      await run();
      if (done) note(done);
    } catch (err) {
      note(err.message, 'error');
    } finally {
      busy = false;
    }
  }

  const choose = (c) => act(() => chooseOption(basket.id, c.key, accepted), t('cellier.panier.choix_fait', { option: label(c.key) }));
  const move = (status) => act(() => setBasketStatus(basket.id, status));
  function drop() {
    if (confirm(t('cellier.panier.abandonner_confirmer'))) move('dropped');
  }
</script>

{#snippet status()}
  <section class="card status" aria-label={t('cellier.panier.statut')}>
    <div class="line">
      <span class="chip {proposed ? 'warm' : 'good'}">{t(`cellier.panier.etat.${basket.status}`)}</span>
      <small class="muted">{t('cellier.panier.propose_par', { nom: basket.created_by, quand: relative(basket.created_at, home.now) })}</small>
    </div>
    {#if basket.chosen}
      <p>{t('cellier.panier.choisi_par', { option: label(basket.chosen), nom: basket.chosen_by ?? '' })}</p>
    {/if}
    {#if STEPS.includes(basket.status)}
      <p class="muted">{t(`cellier.panier.etape.${basket.status}`)}</p>
    {/if}
    <div class="actions">
      {#if basket.status === 'filled'}
        <button class="primary" disabled={busy} onclick={() => move('ordered')}>{t('cellier.panier.commande')}</button>
      {:else if basket.status === 'ordered'}
        <button class="primary" disabled={busy} onclick={() => move('done')}>{t('cellier.panier.recupere')}</button>
      {/if}
      <button class="ghost" disabled={busy} onclick={drop}>{t('cellier.panier.abandonner')}</button>
    </div>
  </section>
{/snippet}

{#if !basket}
  <section class="card none">
    <Icon name="basket" size={28} />
    <p class="muted">{t('cellier.panier.aucun')}</p>
  </section>
{:else}
  <!-- Where the basket stands: first once chosen, after the options while one is to pick. -->
  {#if !proposed}{@render status()}{/if}

  <section aria-labelledby="cellier-options">
    <h2 id="cellier-options" class="title">{t('cellier.panier.options')}</h2>
    <div class="options">
      {#each choices as c (c.key)}
        {@const best = c.key === options.recommended}
        {@const chosen = c.key === basket.chosen}
        <article class="card option" class:best class:chosen class:invalid={!c.valid}>
          <header>
            <h3>{label(c.key)}</h3>
            {#if chosen}
              <span class="chip good"><Icon name="ok" size={14} />{t('cellier.panier.choisie')}</span>
            {:else if best}
              <span class="chip warm">{t('cellier.panier.recommande')}</span>
            {/if}
          </header>
          <p class="total num">{euros(c.total_cents)}</p>
          {#if c.key === 'split' && options.saving_cents > 0}
            <p class="saving">{t('cellier.panier.economie', { montant: euros(options.saving_cents), pct: Math.round(options.saving_ratio * 100) })}</p>
          {:else if recommendedTotal != null && c.total_cents > recommendedTotal}
            <p class="muted">{t('cellier.panier.de_plus', { montant: euros(c.total_cents - recommendedTotal) })}</p>
          {/if}
          {#if c.missing.length}
            <p class="missing">
              <span class="chip alert">{t('cellier.panier.manquants', { count: c.missing.length })}</span>
              <small class="muted">{c.missing.map(nameOf).join(', ')}</small>
            </p>
          {:else}
            <p class="muted small">{t('cellier.panier.complet')}</p>
          {/if}
          {#if !c.valid}
            <p class="chip alert">{t('cellier.panier.sous_minimum')}</p>
          {:else if proposed}
            <button class="primary" disabled={busy} onclick={() => choose(c)}>{t('cellier.panier.choisir')}</button>
          {/if}
        </article>
      {/each}
    </div>
  </section>

  {#if basket.deviations.length}
    <section class="card">
      <h2 class="title">{t('cellier.panier.ecarts')}</h2>
      {#if proposed}<p class="muted small">{t('cellier.panier.ecarts_aide')}</p>{/if}
      <ul class="devs">
        {#each basket.deviations as d, i (i)}
          <li>
            <label>
              <input type="checkbox" checked={ticked(i)} disabled={!proposed} onchange={(e) => tick(i, e.currentTarget.checked)} />
              <span>{d.text}</span>
              {#if d.gain_cents > 0}<b class="num">{t('cellier.panier.gain', { montant: euros(d.gain_cents) })}</b>{/if}
            </label>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if options.split}
    <section class="card">
      <h2 class="title">{t('cellier.panier.detail_reparti')}</h2>
      <div class="groups">
        {#each Object.entries(options.split.groups) as [provider, ids] (provider)}
          <div>
            <h3>{providerName(provider)}</h3>
            <ul>
              {#each ids as id (id)}
                <li>
                  <span>{qtyOf(id)}{nameOf(id)}</span>
                  {#each pendingFor(id, provider) as o (o.id)}
                    <small class="chip warm" title={t('cellier.panier.a_confirmer_libelle', { libelle: o.label || o.reference })}>{t('cellier.panier.a_confirmer')}</small>
                  {/each}
                </li>
              {/each}
            </ul>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  {#if toConfirm.length}
    <section class="card">
      <h2 class="title">{t('cellier.panier.fiches_a_confirmer')}</h2>
      <p class="muted small">{t('cellier.panier.fiches_aide')}</p>
      <ul class="sheets">
        {#each toConfirm as o (o.id)}
          <li>
            <div>
              <b>{o.label || o.reference}</b>
              <small class="muted">{productName(o.product)} · {providerName(o.provider)}</small>
            </div>
            <button
              class="ghost"
              disabled={busy}
              onclick={() => act(() => confirmOffer(o.id), t('cellier.produits.confirmee_note'))}
              aria-label={t('cellier.produits.confirmer_label', { libelle: o.label || o.reference, enseigne: providerName(o.provider) })}
            >
              <Icon name="ok" size={18} />{t('cellier.produits.confirmer')}
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if options.nowhere.length || options.elsewhere.length}
    <div class="apart">
      {#if options.nowhere.length}
        <section class="card">
          <h2 class="title">{t('cellier.panier.introuvables')}</h2>
          <p class="muted small">{t('cellier.panier.introuvables_aide')}</p>
          <ul class="names">{#each options.nowhere as id (id)}<li>{qtyOf(id)}{nameOf(id)}</li>{/each}</ul>
        </section>
      {/if}
      {#if options.elsewhere.length}
        <section class="card">
          <h2 class="title">{t('cellier.panier.ailleurs')}</h2>
          <p class="muted small">{t('cellier.panier.ailleurs_aide')}</p>
          <ul class="names">{#each options.elsewhere as id (id)}<li>{qtyOf(id)}{nameOf(id)}</li>{/each}</ul>
        </section>
      {/if}
    </div>
  {/if}
  {#if proposed}{@render status()}{/if}
{/if}

<style>
  section + section,
  section + .apart,
  .apart {
    margin-top: 16px;
  }

  .none {
    display: flex;
    align-items: center;
    gap: 14px;
    color: var(--ink-3);
  }

  .status {
    display: grid;
    gap: 10px;
  }

  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .title {
    font-size: 15px;
    font-weight: 650;
    color: var(--ink-2);
    margin-bottom: 10px;
  }

  .small {
    font-size: 13px;
  }

  .options {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 14px;
  }

  .option {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .option.best {
    box-shadow: var(--shadow), inset 0 0 0 2px var(--warm);
  }

  .option.chosen {
    box-shadow: var(--shadow), inset 0 0 0 2px var(--good);
  }

  .option.invalid {
    opacity: 0.75;
  }

  .option header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .option h3 {
    font-size: 17px;
    font-weight: 650;
  }

  .total {
    font-size: 30px;
    font-weight: 300;
    letter-spacing: -0.02em;
  }

  .saving {
    color: var(--good);
    font-weight: 600;
  }

  .missing {
    display: grid;
    gap: 4px;
    justify-items: start;
  }

  .option .chip.alert {
    justify-self: start;
    align-self: flex-start;
  }

  .option button {
    margin-top: auto;
  }

  .primary,
  .ghost {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
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

  .devs,
  .sheets,
  .names,
  .groups ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .devs li + li,
  .sheets li + li {
    border-top: 1px solid var(--line);
  }

  .devs label {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 48px;
    padding: 6px 0;
    cursor: pointer;
  }

  .devs input {
    width: 22px;
    height: 22px;
    flex: none;
    accent-color: var(--good);
  }

  .devs span {
    flex: 1;
    min-width: 0;
  }

  .devs b {
    color: var(--good);
    font-weight: 650;
    white-space: nowrap;
  }

  .groups {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 16px;
  }

  .groups h3 {
    font-size: 15px;
    font-weight: 650;
    margin-bottom: 6px;
  }

  .groups li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 4px 0;
  }

  .groups .chip {
    padding: 2px 8px;
    font-size: 12px;
  }

  .sheets li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 0;
  }

  .sheets div {
    display: grid;
    gap: 2px;
    min-width: 0;
  }

  .sheets b {
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .apart {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 14px;
  }

  .apart section {
    margin: 0;
  }

  .names li {
    padding: 4px 0;
  }
</style>
