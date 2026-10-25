<script>
  import { hub, value, num, nameOf, roomList, coverIcon, isBlind, reachable } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** Roof windows and blinds at a glance, for the home page: one line each,
   *  windows then blinds, room by room (how open, a small gauge, moving or
   *  not), and the rain when the gateway feels it. The Windows page has the
   *  commands. */

  const covers = $derived(roomList().flatMap((r) => r.covers));
  const windows = $derived(covers.filter((d) => coverIcon(d) === 'roof-window'));
  const blinds = $derived(covers.filter((d) => coverIcon(d) !== 'roof-window'));
  const raining = $derived(Object.values(hub.devices).some((d) => value(d.id, 'rain') === true));

  const VENT = 10;
  /** A cover's line: how open (0 closed, 100 open), in words. */
  function line(d) {
    const pct = value(d.id, 'percent_control');
    const vents = coverIcon(d) === 'roof-window';
    const moving = value(d.id, 'moving') === true;
    const target = value(d.id, 'target_position');
    const word = !reachable(d.id)
      ? t('commun.piece.injoignable')
      : moving && typeof target === 'number'
        ? t('maison.accueil.ouvrants.vers', { state: openness(target, vents) })
        : typeof pct === 'number'
          ? openness(pct, vents)
          : '—';
    return { id: d.id, name: nameOf(d.id), pct: typeof pct === 'number' ? pct : 0, open: pct > 0, moving, word, blind: isBlind(d) };
  }
  const openness = (pct, vents) =>
    pct === 0
      ? t('commun.volet.ferme')
      : pct === 100
        ? t('commun.volet.ouvert')
        : vents && pct === VENT
          ? t('commun.volet.ventilation')
          : t('commun.volet.ouvert_a', { pct: num(pct) });

  const groups = $derived(
    [
      { id: 'windows', icon: 'roof-window', title: t('maison.accueil.ouvrants.fenetres'), lines: windows.map(line) },
      { id: 'blinds', icon: 'shutter', title: t('maison.accueil.ouvrants.stores'), lines: blinds.map(line) },
    ].filter((g) => g.lines.length),
  );
  const openCount = $derived(windows.filter((d) => Number(value(d.id, 'percent_control')) > 0).length);
  const summary = $derived(openCount ? t('maison.accueil.ouvrants.ouvertes', { count: openCount }) : windows.length ? t('maison.accueil.ouvrants.toutes_fermees') : '');
</script>

{#if covers.length}
  <a class="card covers" href="#/fenetres" aria-label={t('maison.accueil.ouvrants.detail')}>
    <div class="card-head">
      <h2><Icon name="roof-window" size={18} />{t('maison.accueil.ouvrants.titre')} {#if summary}<span class="count">{summary}</span>{/if}</h2>
      <span class="more" aria-hidden="true"><Icon name="arrow-top-right" size={18} /></span>
    </div>
    {#if raining}
      <p class="rain"><Icon name="water" size={16} />{t('maison.accueil.ouvrants.pluie')}</p>
    {/if}
    <div class="groups">
      {#each groups as g (g.id)}
        <section>
          <h3><Icon name={g.icon} size={15} />{g.title}</h3>
          <ul>
            {#each g.lines as l (l.id)}
              <li class:open={l.open}>
                <b>{l.name}</b>
                <span class="gauge" aria-hidden="true"><i class:moving={l.moving} style="width:{l.pct}%"></i></span>
                <span class="word">{l.word}</span>
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    </div>
  </a>
{/if}

<style>
  .covers {
    display: block;
    color: inherit;
    text-decoration: none;
    transition: transform 0.2s var(--ease);
  }

  .covers:hover {
    transform: translateY(-2px);
  }

  .card-head {
    margin-bottom: 10px;
  }

  .count {
    font-weight: 550;
    color: var(--ink-3);
    margin-left: 4px;
  }

  .rain {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
    padding: 8px 12px;
    border-radius: var(--r-sm);
    background: var(--cool-soft);
    color: var(--cool);
    font-size: 13.5px;
    font-weight: 600;
  }

  .groups {
    display: grid;
    gap: 12px;
  }

  h3 {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-3);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-bottom: 2px;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 56px 7.5em;
    align-items: center;
    gap: 10px;
    padding: 5px 0;
    border-top: 1px solid var(--line);
    font-size: 14px;
  }

  li:first-child {
    border-top: 0;
  }

  li b {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .gauge {
    height: 6px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .gauge i {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: var(--cool);
  }

  .gauge i.moving {
    background: var(--warm);
  }

  .word {
    font-size: 13px;
    color: var(--ink-3);
    text-align: right;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  li.open .word {
    color: var(--cool);
    font-weight: 600;
  }

  @media (max-width: 760px) {
    li {
      grid-template-columns: minmax(0, 1fr) 36px auto;
    }
  }
</style>
