<script>
  import { drinkName, fieldWord, hasMilk } from '../lib/coffee.js';
  import { t } from '../../lib/i18n.svelte.js';
  import DrinkIcon from './DrinkIcon.svelte';

  /** The drinks in two columns, coffee or water on one side, with milk on
   *  the other, each followed by its recipe as the machine keeps it (what
   *  a tap brews), its figures in aligned columns: a column only for a
   *  field some drink of its side has, a dash where a drink has not that
   *  field. On a narrow screen the figures go under the name, still in
   *  their columns. A tap on a drink calls `onpick` (its window opens). `waiting`:
   *  the machine cannot take an order now (the buttons say it, greyed). */
  let { drinks, recipes, waiting = false, onpick } = $props();

  const FIELDS = {
    plain: ['strength', 'water', 'bypass', 'temperature'],
    milk: ['strength', 'water', 'milk_foam', 'temperature'],
  };
  const groups = $derived(
    [
      { key: 'plain', title: 'salon.cafe.groupe.cafe', drinks: drinks.filter((slug) => !hasMilk(slug, recipes?.[slug])) },
      { key: 'milk', title: 'salon.cafe.groupe.lait', drinks: drinks.filter((slug) => hasMilk(slug, recipes?.[slug])) },
    ]
      .filter((g) => g.drinks.length)
      .map((g) => ({ ...g, fields: FIELDS[g.key].filter((k) => g.drinks.some((slug) => recipes?.[slug]?.[k])) })),
  );
</script>

<div class="menu">
  {#each groups as g (g.key)}
    <div class="side"><section class="group" style:--fields={g.fields.length}>
      <div class="row head">
        <h4>{t(g.title)}</h4>
        {#each g.fields as k (k)}<span class="col" title={t(`salon.cafe.champ.${k}`)}>{t(`salon.cafe.court.${k}`)}</span>{/each}
      </div>
      {#each g.drinks as slug (slug)}
        {@const r = recipes?.[slug]}
        <button class="row drink" class:waiting onclick={() => onpick(slug)}>
          <span class="icon"><DrinkIcon drink={slug} size={34} /></span>
          <span class="name">{drinkName(slug)}</span>
          {#each g.fields as k (k)}
            <span class="col num" class:none={!r?.[k]}>
              {r?.[k] ? fieldWord(k, r[k]) : '—'}
              {#if k === 'milk_foam' && r?.milk_break?.value}<small>{t('salon.cafe.pause', { n: r.milk_break.value })}</small>{/if}
            </span>
          {/each}
        </button>
      {/each}
    </section></div>
  {/each}
</div>

<style>
  .menu {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 330px), 1fr));
    gap: 14px 24px;
    align-items: start;
  }

  /* One grid per side: the drink's picture and name, then a column per
     field, sized by the widest figure; every row lays its cells on the
     same columns. */
  /* Each side measures its own width (narrow: the figures under the name). */
  .side {
    container-type: inline-size;
  }

  .group {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) repeat(var(--fields), auto);
    column-gap: 12px;
    row-gap: 4px;
  }

  .row {
    grid-column: 1 / -1;
    display: grid;
    grid-template-columns: subgrid;
    align-items: center;
  }

  .head h4 {
    grid-column: 1 / 3;
  }

  .head {
    padding: 0 10px 4px 0;
    border-bottom: 1px solid var(--line);
    margin-bottom: 2px;
  }

  h4 {
    margin: 0;
    font-size: 13px;
    font-weight: 700;
    color: var(--ink-2);
  }

  .head .col {
    font-size: 11px;
    font-weight: 650;
    color: var(--ink-3);
    text-align: right;
    white-space: nowrap;
  }

  .drink {
    min-height: 48px;
    padding: 4px 10px 4px 6px;
    border: 0;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .drink:hover {
    background: color-mix(in srgb, var(--ink) 8%, var(--surface-2));
  }

  .drink.waiting {
    opacity: 0.45;
  }

  .icon {
    display: flex;
  }

  .name {
    min-width: 0;
    font-weight: 600;
    font-size: 14.5px;
    line-height: 1.25;
  }

  /* Narrow: the name on its line, the figures under it in their columns. */
  @container (max-width: 440px) {
    .group {
      grid-template-columns: auto repeat(var(--fields), minmax(auto, 1fr));
    }

    .head h4 {
      grid-column: 1 / -1;
    }

    .head .col:first-of-type {
      grid-column-start: 2;
    }

    .drink {
      row-gap: 2px;
      padding-block: 6px;
    }

    .drink .icon {
      grid-column: 1;
      grid-row: 1 / span 2;
    }

    .drink .name {
      grid-column: 2 / -1;
      grid-row: 1;
    }
  }

  .drink .col {
    display: grid;
    justify-items: end;
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  .col small {
    font-size: 10.5px;
    font-weight: 500;
    color: var(--ink-3);
  }

  .col.none {
    color: var(--ink-3);
    font-weight: 400;
  }
</style>
