<script>
  import { device, value, reachable, nameOf, act, num } from '../lib/home.svelte.js';
  import { STATE_TONE, stateWord, drinkName, programName, whyNot } from '../lib/coffee.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import DrinkIcon from './DrinkIcon.svelte';

  /** A coffee machine on the home page: its state, and an espresso and a
   *  coffee as the machine keeps them. A drink asks twice (a cup under the
   *  spout?): the first tap names it, the second sends it. Everything else
   *  is on the Coffee page. */
  let { id } = $props();

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

<section class="card coffee-home">
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
