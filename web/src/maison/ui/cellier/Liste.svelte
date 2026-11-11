<script>
  import Icon from '../Icon.svelte';
  import { cellier, addItem, setQty, removeItem, inCart } from '../../lib/cellier.svelte.js';
  import { home, note, relative } from '../../lib/home.svelte.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';

  /** The open shopping list: add in one line, change a quantity, remove. */

  let text = $state('');
  let busy = $state(false);

  async function add(e) {
    e.preventDefault();
    const words = text.trim();
    if (!words || busy) return;
    busy = true;
    // A known product typed by its name: the line is that product, so its
    // prices count in the basket.
    const known = cellier.products.find((p) => p.name.toLocaleLowerCase() === words.toLocaleLowerCase());
    try {
      await addItem(words, known?.id ?? null);
      text = '';
    } catch (err) {
      note(err.message, 'error');
    } finally {
      busy = false;
    }
  }

  async function act(run) {
    try {
      await run();
    } catch (err) {
      note(err.message, 'error');
    }
  }

  async function remove(item) {
    await act(async () => {
      await removeItem(item.id);
      note(t('cellier.liste.retire', { nom: item.text }));
    });
  }

  const qty = (n) => n.toLocaleString(locale(), { maximumFractionDigits: 2 });
  /** A person's name, or « agent » for whatever an agent added. */
  const who = (item) => (item.channel === 'ui' ? item.added_by : t('cellier.liste.agent'));
</script>

<section class="card">
  <form class="add" onsubmit={add}>
    <input
      bind:value={text}
      placeholder={t('cellier.liste.placeholder')}
      aria-label={t('cellier.liste.ajouter_label')}
      list="cellier-produits"
      autocomplete="off"
      enterkeyhint="done"
    />
    <button class="go" disabled={!text.trim() || busy} aria-label={t('cellier.liste.ajouter')}>
      <Icon name="plus" size={22} /><span>{t('cellier.liste.ajouter')}</span>
    </button>
    <datalist id="cellier-produits">
      {#each cellier.products as p (p.id)}<option value={p.name}></option>{/each}
    </datalist>
  </form>

  {#if cellier.items.length}
    <p class="count muted">{t('cellier.liste.compte', { count: cellier.items.length })}</p>
    <ul class="items">
      {#each cellier.items as item (item.id)}
        <li>
          <div class="what">
            <b>{item.text}{#if inCart(item)} <small class="chip cool">{t('cellier.liste.en_commande')}</small>{/if}</b>
            <small class="muted" title={item.added_by}>{t('cellier.liste.par', { nom: who(item) })} · {relative(item.added_at, home.now)}</small>
          </div>
          <div class="qty" role="group" aria-label={t('cellier.liste.quantite', { nom: item.text })}>
            <button onclick={() => act(() => setQty(item.id, item.qty - 1))} disabled={item.qty <= 1} aria-label={t('cellier.liste.moins', { nom: item.text })}>
              <Icon name="minus" size={18} />
            </button>
            <output class="num">{qty(item.qty)}</output>
            <button onclick={() => act(() => setQty(item.id, item.qty + 1))} aria-label={t('cellier.liste.plus', { nom: item.text })}>
              <Icon name="plus" size={18} />
            </button>
          </div>
          <button class="remove" onclick={() => remove(item)} aria-label={t('cellier.liste.retirer', { nom: item.text })}>
            <Icon name="trash" size={20} />
          </button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="empty muted">{t('cellier.liste.vide')}</p>
  {/if}
</section>

<style>
  .add {
    display: flex;
    gap: 10px;
  }

  .add input {
    flex: 1;
    min-width: 0;
    height: 52px;
    padding: 0 18px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 16px;
  }

  .go {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 52px;
    padding: 0 20px;
    border: 0;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font-weight: 650;
  }

  .go:disabled {
    opacity: 0.4;
  }

  .count {
    margin-top: 18px;
    font-size: 13px;
    font-weight: 600;
  }

  .items {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
  }

  .items li {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 0;
    border-top: 1px solid var(--line);
  }

  .items li:first-child {
    border-top: 0;
  }

  .what {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }

  .what b {
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .what small {
    font-size: 12.5px;
    overflow-wrap: anywhere;
  }

  .what .chip {
    margin-left: 6px;
    padding: 1px 8px;
    font-size: 12px;
    font-weight: 600;
    vertical-align: 1px;
  }

  .qty {
    display: flex;
    align-items: center;
    border-radius: 999px;
    background: var(--surface-2);
  }

  .qty button,
  .remove {
    width: 44px;
    height: 44px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 999px;
    background: none;
    color: var(--ink-2);
  }

  .qty button:disabled {
    opacity: 0.3;
  }

  .qty output {
    min-width: 28px;
    text-align: center;
    font-weight: 650;
  }

  .remove {
    color: var(--ink-3);
  }

  .remove:hover {
    color: var(--alert);
    background: var(--alert-soft);
  }

  .empty {
    margin-top: 18px;
  }

  @media (max-width: 760px) {
    .go span {
      display: none;
    }

    .go {
      width: 52px;
      padding: 0;
      justify-content: center;
    }

    .items li {
      gap: 6px;
    }
  }
</style>
