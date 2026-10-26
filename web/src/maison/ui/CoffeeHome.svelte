<script>
  import { device, value, reachable, nameOf, act, num } from '../lib/home.svelte.js';
  import { STATE_TONE, stateWord, drinkName, programName, whyNot } from '../lib/coffee.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import DrinkIcon from './DrinkIcon.svelte';
  import CoffeeUsage from './CoffeeUsage.svelte';

  /** A coffee machine on the home page: its state, an espresso and a
   *  coffee as the machine keeps them, and today's cups hour by hour. A
   *  drink asks twice (a cup under the spout?): the first tap names it, the
   *  second sends it. Everything else is on the Coffee page. `wide`: across
   *  the top of the page (the machine on), today's cups beside the buttons
   *  when there is room. */
  let { id, wide = false } = $props();

  const QUICK = ['espresso', 'coffee'];

  const d = $derived(device(id));
  const ok = $derived(reachable(id));
  const status = $derived(ok ? value(id, 'status') : null);
  const why = $derived(whyNot(ok, status));
  const drinks = $derived(QUICK.filter((k) => d?.points.find((p) => p.key === 'brew')?.kind.values?.includes(k)));
  const doing = $derived(ok ? value(id, 'activity') || '' : '');
  const progress = $derived(Number(value(id, 'progress') ?? 0));
  const doingName = $derived(!doing ? '' : d?.points.find((p) => p.key === 'brew')?.kind.values?.includes(doing) ? drinkName(doing) : programName(doing));

  let asked = $state(null);
  let timer;
  async function tap(slug) {
    if (why) return;
    if (asked !== slug) {
      asked = slug;
      clearTimeout(timer);
      timer = setTimeout(() => (asked = null), 5000);
      return;
    }
    asked = null;
    clearTimeout(timer);
    await act(`${id}/brew`, slug, t('salon.cafe.act_preparer', { boisson: drinkName(slug), nom: nameOf(id) }));
  }
</script>

<section class="card coffee-home" class:wide>
  <div class="card-head">
    <h2><Icon name="coffee-maker" size={18} />{t('salon.cafe.titre')}</h2>
    <span class="chip {STATE_TONE[status] ?? ''}">{stateWord(status)}</span>
    <a class="more" href="#/cafe" aria-label={t('maison.accueil.cafe_page')}><Icon name="arrow-top-right" size={18} /></a>
  </div>
  {#if doingName && why === 'occupee'}
    <p class="line"><b>{doingName}</b><span class="num">{num(progress)} %</span></p>
  {:else if why}
    <p class="line muted">{t(`salon.cafe.pourquoi.${why}`)}</p>
  {/if}
  {#if drinks.length}
    <div class="quick">
      {#each drinks as slug (slug)}
        <button class:confirm={asked === slug} disabled={!!why} onclick={() => tap(slug)} aria-live="polite">
          <DrinkIcon drink={slug} size={40} />
          <span>{asked === slug ? t('maison.accueil.cafe_encore') : drinkName(slug)}</span>
        </button>
      {/each}
    </div>
  {/if}
  <div class="usage"><CoffeeUsage {id} compact /></div>
</section>

<style>
  /* Under the energy card, in the home page's side column. */
  .coffee-home {
    display: grid;
    gap: 12px;
  }

  .coffee-home:not(:first-child) {
    margin-top: 20px;
  }

  /* Across the page: state and buttons on the left, today's cups on the
     right, when the card is wide enough. */
  @container (min-width: 720px) {
    .coffee-home.wide {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1.25fr);
      column-gap: 28px;
      align-items: start;
    }

    .wide > :not(.usage) {
      grid-column: 1;
    }

    .wide .usage {
      grid-column: 2;
      grid-row: 1 / span 3;
    }
  }

  .coffee-home .card-head {
    margin-bottom: 0;
    justify-content: flex-start;
    gap: 10px;
  }

  .card-head h2 {
    flex: 1;
  }

  .line {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    font-size: 13.5px;
  }

  .muted {
    color: var(--ink-3);
  }

  .quick {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }

  .quick button {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 56px;
    padding: 6px 12px 6px 8px;
    border: 0;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-weight: 650;
    text-align: left;
    cursor: pointer;
  }

  .quick button.confirm {
    background: var(--warm);
    color: #fff;
  }

  .quick button:disabled {
    opacity: 0.45;
    cursor: default;
  }
</style>
