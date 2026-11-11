<script>
  import Icon from '../Icon.svelte';
  import Passage from './Passage.svelte';
  import { cellier, euros, providerName, readerWhy } from '../../lib/cellier.svelte.js';
  import { passages, byMonth } from '../../lib/cellier/passages.js';
  import { dots } from '../../lib/cellier/draft.js';
  import { home, relative } from '../../lib/home.svelte.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';

  /** What was bought: the receipts read and not yet checked first, then the
   *  trips to the till month by month (the last 12 months), each with its
   *  lines. Every line counts here only if it belongs in the Cellier. */

  const drafts = $derived(cellier.receipts.filter((r) => r.status === 'draft').sort((a, b) => b.sent_at - a.sent_at));
  const months = $derived(byMonth(passages(cellier.purchases, cellier.receipts)));
  const receipts = $derived(new Map(cellier.receipts.map((r) => [r.id, r])));
  /** Why the scan is greyed out, or ''. */
  const refused = $derived(cellier.reader && !cellier.reader.allowed ? readerWhy(cellier.reader) : '');

  /** « Octobre 2026 » from « 2026-10 ». */
  function monthName(month) {
    const [y, m] = month.split('-').map(Number);
    const s = new Date(y, m - 1, 1).toLocaleDateString(locale(), { month: 'long', year: 'numeric' });
    return s.charAt(0).toUpperCase() + s.slice(1);
  }

  const day = (ms) => new Date(ms).toLocaleDateString(locale(), { weekday: 'short', day: 'numeric', month: 'short' });
  const chain = (id) => (id ? providerName(id) : t('cellier.achats.enseigne_inconnue'));
</script>

<div class="achats">
  <div class="top">
    {#if refused}
      <button class="scan" disabled aria-describedby="cellier-scan-off"><Icon name="camera" size={22} />{t('cellier.achats.scanner')}</button>
      <p id="cellier-scan-off" class="muted small">{refused}</p>
    {:else}
      <a class="scan" href="#/cellier/achats/scan"><Icon name="camera" size={22} />{t('cellier.achats.scanner')}</a>
    {/if}
  </div>

  {#if drafts.length}
    <section class="card drafts" aria-labelledby="cellier-drafts">
      <h2 id="cellier-drafts" class="title">
        {t('cellier.achats.a_valider')}
        <small class="chip warm num">{drafts.length}</small>
      </h2>
      <p class="muted small">{t('cellier.achats.a_valider_aide')}</p>
      <ul>
        {#each drafts as r (r.id)}
          <li>
            <a href="#/cellier/achats/ticket/{r.id}">
              <span class="ic"><Icon name="receipt" size={22} /></span>
              <span class="what">
                <b>{dots([chain(r.draft.provider), r.draft.store])}</b>
                <small class="muted">
                  {dots([
                    r.draft.at && day(r.draft.at),
                    t('cellier.achats.lignes', { count: r.draft.lines.length }),
                    t('cellier.achats.scanne_par', { nom: r.sent_by, quand: relative(r.sent_at, home.now) }),
                  ])}
                </small>
              </span>
              {#if r.draft.total_cents != null}<b class="num sum">{euros(r.draft.total_cents)}</b>{/if}
              <Icon name="chevron-right" size={22} />
            </a>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if !cellier.purchasesLoaded}
    <p class="muted">…</p>
  {:else if !months.length}
    <section class="card none">
      <Icon name="receipt" size={28} />
      <p class="muted">{t('cellier.achats.aucun')}</p>
    </section>
  {:else}
    {#each months as m (m.month)}
      <section class="month" aria-label={monthName(m.month)}>
        <header>
          <h2>{monthName(m.month)}</h2>
          <span class="muted small">{t('cellier.achats.passages', { count: m.passages.length })}</span>
          <b class="num">{euros(m.total_cents)}</b>
        </header>
        <div class="card list">
          {#each m.passages as p (p.key)}
            <Passage passage={p} receipt={p.receipt != null ? receipts.get(p.receipt) : null} />
          {/each}
        </div>
      </section>
    {/each}
    <p class="muted small foot">{t('cellier.achats.periode')}</p>
  {/if}
</div>

<style>
  .achats {
    display: grid;
    gap: 18px;
  }

  .top {
    display: grid;
    justify-items: start;
    gap: 6px;
  }

  .scan {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    min-height: 52px;
    padding: 0 24px;
    border: 0;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font-size: 16px;
    font-weight: 650;
    text-decoration: none;
  }

  .scan:disabled {
    opacity: 0.4;
  }

  .small {
    font-size: 13px;
  }

  .title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 15px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .title .chip {
    padding: 2px 9px;
    font-size: 12.5px;
  }

  .drafts {
    box-shadow: var(--shadow), inset 0 0 0 2px var(--warm);
  }

  .drafts > p {
    margin-top: 2px;
  }

  .drafts ul {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
  }

  .drafts li + li {
    border-top: 1px solid var(--line);
  }

  .drafts a {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 60px;
    padding: 8px 0;
    color: var(--ink);
    text-decoration: none;
  }

  .ic {
    width: 40px;
    height: 40px;
    flex: none;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .what {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }

  .what b {
    font-weight: 650;
    overflow-wrap: anywhere;
  }

  .what small {
    font-size: 12.5px;
  }

  .sum {
    font-weight: 650;
    white-space: nowrap;
  }

  .drafts a > :global(svg:last-child) {
    color: var(--ink-3);
  }

  .none {
    display: flex;
    align-items: center;
    gap: 14px;
    color: var(--ink-3);
  }

  .month {
    display: grid;
    gap: 8px;
  }

  .month header {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 0 4px;
  }

  .month h2 {
    font-size: 17px;
    font-weight: 650;
  }

  .month header b {
    margin-left: auto;
    font-size: 17px;
    font-weight: 650;
  }

  .list {
    padding-top: 6px;
    padding-bottom: 6px;
  }

  .foot {
    text-align: center;
  }

  @media (max-width: 760px) {
    .top {
      justify-items: stretch;
    }
  }
</style>
