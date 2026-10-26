<script>
  import { drinkName, fieldWord } from '../lib/coffee.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** Every drink's recipe as the machine keeps it, side by side: what a
   *  tap on a drink brews. A column only for a field some drink has, the
   *  most read first (a phone scrolls to the others, a shadow says so); a
   *  dash where a drink has not that field. */
  let { recipes, drinks } = $props();

  const rows = $derived(drinks.filter((slug) => recipes?.[slug]));
  const ORDER = ['strength', 'water', 'milk_foam', 'temperature', 'bypass', 'milk_break'];
  const columns = $derived(ORDER.filter((k) => rows.some((slug) => recipes[slug][k])));
</script>

{#if rows.length}
  <details class="recipes">
    <summary>{t('salon.cafe.recettes')}</summary>
    <div class="scroll">
      <table>
        <thead>
          <tr>
            <th scope="col">{t('salon.cafe.boisson')}</th>
            {#each columns as k (k)}<th scope="col">{t(`salon.cafe.champ.${k}`)}</th>{/each}
          </tr>
        </thead>
        <tbody>
          {#each rows as slug (slug)}
            <tr>
              <th scope="row">{drinkName(slug)}</th>
              {#each columns as k (k)}
                <td class="num" class:none={!recipes[slug][k]}>{recipes[slug][k] ? fieldWord(k, recipes[slug][k]) : '—'}</td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <p class="note">{t('salon.cafe.recettes_aide')}</p>
  </details>
{/if}

<style>
  summary {
    cursor: pointer;
    font-weight: 650;
    font-size: 14px;
  }

  /* A shadow on the right while columns are left to see: the cover moves
     with the table and hides it at the end. */
  .scroll {
    margin-top: 10px;
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
    background:
      linear-gradient(to left, var(--surface) 40%, transparent) right / 24px 100% no-repeat local,
      radial-gradient(farthest-side at 100% 50%, rgb(0 0 0 / 22%), transparent) right / 14px 100% no-repeat scroll;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }

  th,
  td {
    padding: 7px 8px;
    text-align: right;
    white-space: nowrap;
    border-bottom: 1px solid var(--line);
  }

  thead th {
    font-size: 11.5px;
    font-weight: 650;
    color: var(--ink-3);
    vertical-align: bottom;
  }

  th[scope='row'],
  thead th:first-child {
    position: sticky;
    left: 0;
    text-align: left;
    background: var(--surface);
  }

  th[scope='row'] {
    font-weight: 600;
  }

  td.none {
    color: var(--ink-3);
  }

  tbody tr:last-child th,
  tbody tr:last-child td {
    border-bottom: 0;
  }

  .note {
    margin-top: 8px;
    font-size: 12px;
    color: var(--ink-3);
    line-height: 1.45;
  }
</style>
