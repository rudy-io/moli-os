<script>
  // The VELUX gateway's own automations (the « Planning » of the VELUX
  // ACTIVE app), shown, paused, edited and created here: the gateway runs
  // them, Moli or not. A day chip shows that day alone, and copies it to
  // other days; a card's editor copies a setting to other windows or blinds.
  import { hub, value, act, nameOf, coverIcon, isBlind, num, note } from '../lib/home.svelte.js';
  import { dayName } from '../lib/program.js';
  import { POINT, WEEK, parseCards, grouped, toCloud, sorted, on, same, copyDay, hhmm } from '../lib/velux-plans.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import VeluxCardEditor from './VeluxCardEditor.svelte';

  const gateway = $derived(Object.values(hub.devices).find((d) => d.points?.some((p) => p.key === POINT)));
  const read = $derived(gateway ? parseCards(value(gateway.id, POINT)) : null);
  // What was saved, shown until the gateway says it back.
  let sent = $state(null);
  $effect(() => {
    if (!sent) return;
    if (same(read, sent)) sent = null;
    const late = setTimeout(() => (sent = null), 30_000);
    return () => clearTimeout(late);
  });
  const cards = $derived(sent ?? read);

  // The windows and blinds the gateway moves.
  const covers = $derived(
    gateway
      ? Object.values(hub.devices)
          .filter((d) => d.instance === gateway.instance && d.points.some((p) => p.key === 'percent_control'))
          .map((d) => ({ id: d.id, name: nameOf(d.id), window: coverIcon(d) === 'roof-window', blind: isBlind(d) }))
          .sort((a, b) => Number(b.window) - Number(a.window) || a.name.localeCompare(b.name))
      : [],
  );
  const coverOf = (id) => covers.find((c) => c.id === id);

  /** Closed, open, airing (a window at 10 %), or open so much. */
  function openness(id, pct) {
    if (pct === 0) return t('commun.volet.ferme');
    if (pct === 100) return t('commun.volet.ouvert');
    if (pct === 10 && coverOf(id)?.window) return t('commun.volet.ventilation');
    return t('commun.volet.ouvert_a', { pct: num(pct) });
  }
  /** « 1 h 30 », « 45 min ». */
  function span(min) {
    const h = Math.floor(Math.abs(min) / 60);
    const m = Math.abs(min) % 60;
    return h ? (m ? t('maison.fenetres.auto.h_min', { h, m }) : t('maison.fenetres.auto.h', { h })) : t('maison.fenetres.auto.min', { m });
  }
  /** When: « 07:15 », « Coucher du soleil, 1 h avant » (`inner`: inside
   *  a sentence, « coucher du soleil… »; `short`: the offset alone). */
  function when(ref, min, { inner = false, short = false } = {}) {
    if (ref === 'time') return hhmm(min);
    if (short) return min === 0 ? t('maison.fenetres.auto.pile') : t(min < 0 ? 'maison.fenetres.auto.avant_seul' : 'maison.fenetres.auto.apres_seul', { span: span(min) });
    const sun = t(`maison.fenetres.auto.${ref === 'sunrise' ? 'lever' : 'coucher'}${inner ? '_phrase' : ''}`);
    if (min === 0) return sun;
    return t(min < 0 ? 'maison.fenetres.auto.avant' : 'maison.fenetres.auto.apres', { sun, span: span(min) });
  }

  let day = $state(null);
  // One line for the cards VELUX splits by room (same name, same moment).
  const shown = $derived(cards ? sorted(grouped(cards)).filter((c) => day == null || on(c, day)) : []);
  const letter = (d) => dayName(d, locale(), 'narrow');

  // Always sent as lines: each room's card keeps its id, and a room on two
  // cards of one line (a copy made before) is written once.
  async function save(next, label) {
    const out = grouped(next).map(toCloud);
    const done = await act(`${gateway.id}/${POINT}`, JSON.stringify(out), label ?? t('maison.fenetres.auto.label'));
    if (done === 'ok') {
      sent = out;
      note(t('maison.fenetres.auto.envoye'));
    }
    return done;
  }

  const pause = (line) => save(cards.map((c, i) => (line.at.includes(i) ? { ...c, off: !line.off || undefined } : c)));

  // ---- copying a day ----
  let copying = $state(null);
  function copy() {
    const next = copyDay(cards, day, copying);
    copying = null;
    save(next, t('maison.fenetres.auto.copier_jour', { day: dayName(day, locale(), 'long') }));
  }

  // ---- the editor ----
  let editing = $state(null);
  const open = (card) => (editing = { card, at: card?.at ?? null });
  // A line stands for its cards: edited, it takes the place of the first
  // and the others go (their ids go with it, for their rooms).
  function saved(card) {
    const at = editing.at;
    const line = { ...card, ids: editing.card?.ids ?? [] };
    const next = at == null ? [...cards, line] : cards.flatMap((c, i) => (i === at[0] ? [line] : at.includes(i) ? [] : [c]));
    editing = null;
    save(next);
  }
  function removed() {
    const next = cards.filter((_, i) => !editing.at.includes(i));
    editing = null;
    save(next);
  }
  function duplicated(card) {
    const { id, ids, ...copy } = card;
    editing = { card: { ...copy, name: t('maison.fenetres.auto.copie_de', { name: card.name }) }, at: null };
  }
</script>

{#if gateway && cards}
  <section class="plans" aria-labelledby="velux-plans">
    <header>
      <div>
        <h2 id="velux-plans">{t('maison.fenetres.auto.titre')}</h2>
        <p>{t('maison.fenetres.auto.sous_titre')}</p>
      </div>
      <button class="add" onclick={() => open(null)}><Icon name="plus" size={16} />{t('maison.fenetres.auto.nouveau')}</button>
    </header>

    <div class="days" role="radiogroup" aria-label={t('maison.fenetres.auto.jour')}>
      <button role="radio" aria-checked={day == null} class:active={day == null} onclick={() => ((day = null), (copying = null))}>{t('maison.fenetres.auto.semaine')}</button>
      {#each WEEK as d (d)}
        <button class="d" role="radio" aria-checked={day === d} class:active={day === d} onclick={() => ((day = d), (copying = null))} title={dayName(d, locale(), 'long')}>{dayName(d, locale(), 'short')}</button>
      {/each}
    </div>

    {#if day != null}
      <div class="copy">
        {#if copying}
          <span>{t('maison.fenetres.auto.copier_vers', { day: dayName(day, locale(), 'long') })}</span>
          <div class="targets">
            {#each WEEK.filter((d) => d !== day) as d (d)}
              <button class="d" class:active={copying.includes(d)} aria-pressed={copying.includes(d)} onclick={() => (copying = copying.includes(d) ? copying.filter((x) => x !== d) : [...copying, d])}>{dayName(d, locale(), 'short')}</button>
            {/each}
          </div>
          <small>{t('maison.fenetres.auto.copier_aide', { day: dayName(day, locale(), 'long') })}</small>
          <div class="row">
            <button class="ghost" onclick={() => (copying = null)}>{t('commun.annuler')}</button>
            <button class="primary" disabled={!copying.length} onclick={copy}>{t('maison.fenetres.auto.copier')}</button>
          </div>
        {:else}
          <button class="ghost" onclick={() => (copying = [])}><Icon name="repeat" size={15} />{t('maison.fenetres.auto.copier_jour', { day: dayName(day, locale(), 'long') })}</button>
        {/if}
      </div>
    {/if}

    {#if !shown.length}
      <p class="empty">{t(day == null ? 'maison.fenetres.auto.aucun' : 'maison.fenetres.auto.aucun_jour')}</p>
    {:else}
      <ul>
        {#each shown as card (card.at[0])}
          {@const devices = Object.entries(card.set)}
          <li class:off={card.off}>
            <button class="item" onclick={() => open(card)} aria-label={t('maison.fenetres.auto.modifier', { name: card.name })}>
              <span class="when">
                {#if card.ref !== 'time'}<Icon name={card.ref === 'sunrise' ? 'sunrise' : 'sunset'} size={16} />{/if}
                <b>{when(card.ref, card.min)}</b>
                {#if card.end != null}<small>{t('maison.fenetres.auto.jusqu', { when: when(card.ref, card.end, { inner: true }) })}</small>{/if}
              </span>
              <span class="what">
                <b>{card.name}</b>
                <small>
                  {#each devices.slice(0, 3) as [id, pct] (id)}
                    <span class="dev"><Icon name={coverOf(id)?.window ? 'roof-window' : 'shutter'} size={13} />{nameOf(id)} · {openness(id, pct)}</span>
                  {/each}
                  {#if devices.length > 3}<span class="dev">{t('maison.fenetres.auto.autres', { count: devices.length - 3 })}</span>{/if}
                </small>
                <span class="week" aria-label={card.days.map((d) => dayName(d, locale(), 'long')).join(', ')}>
                  {#each WEEK as d (d)}<i class:on={on(card, d)}>{letter(d)}</i>{/each}
                </span>
              </span>
            </button>
            <button class="toggle" class:paused={card.off} onclick={() => pause(card)} aria-pressed={!card.off} title={t(card.off ? 'maison.fenetres.auto.reprendre' : 'maison.fenetres.auto.pause')} aria-label={t(card.off ? 'maison.fenetres.auto.reprendre' : 'maison.fenetres.auto.pause')}>
              <i></i>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  {#if editing}
    {#key editing}<VeluxCardEditor card={editing.card} {covers} {openness} {when} isNew={editing.at == null} onsave={saved} ondelete={removed} onduplicate={duplicated} onclose={() => (editing = null)} />{/key}
  {/if}
{/if}

<style>
  .plans {
    display: grid;
    gap: 12px;
    padding: 16px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
  }

  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
  }

  h2 {
    margin: 0;
    font-size: 18px;
    font-weight: 750;
    color: var(--ink);
  }

  header p {
    margin: 2px 0 0;
    font-size: 13px;
    color: var(--ink-3);
  }

  button {
    font: inherit;
    cursor: pointer;
  }

  .add,
  .primary,
  .ghost {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    border-radius: 999px;
    padding: 8px 14px;
    font-size: 13.5px;
    font-weight: 650;
  }

  .add {
    flex: none;
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .primary {
    background: var(--ink);
    color: var(--surface);
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .ghost {
    background: var(--surface);
    color: var(--ink-2);
  }

  .days,
  .targets {
    display: flex;
    gap: 6px;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .days button,
  .targets button {
    flex: none;
    border: 0;
    border-radius: 999px;
    padding: 6px 11px;
    background: var(--surface);
    color: var(--ink-2);
    font-size: 13px;
    font-weight: 650;
  }

  .d {
    text-transform: capitalize;
  }

  .days button.active,
  .targets button.active {
    background: var(--cool);
    color: #fff;
  }

  .copy {
    display: grid;
    gap: 8px;
    justify-items: start;
    font-size: 13.5px;
    color: var(--ink-2);
  }

  .copy small {
    color: var(--ink-3);
  }

  .row {
    display: flex;
    gap: 8px;
  }

  .empty {
    margin: 0;
    color: var(--ink-3);
    font-size: 14px;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }

  li {
    display: flex;
    align-items: center;
    gap: 6px;
    border-radius: var(--r-md);
    background: var(--surface);
  }

  li.off {
    opacity: 0.55;
  }

  .item {
    flex: 1;
    min-width: 0;
    display: grid;
    grid-template-columns: minmax(84px, auto) 1fr;
    gap: 12px;
    align-items: start;
    padding: 12px 4px 12px 14px;
    border: 0;
    background: none;
    color: var(--ink-2);
    text-align: left;
  }

  .when {
    display: grid;
    gap: 2px;
    justify-items: start;
    color: var(--ink);
  }

  .when b {
    font-size: 15px;
    font-weight: 750;
    font-variant-numeric: tabular-nums;
    line-height: 1.2;
  }

  .when small,
  .what small {
    font-size: 12px;
    color: var(--ink-3);
  }

  .what {
    display: grid;
    gap: 4px;
    min-width: 0;
  }

  .what b {
    font-weight: 650;
    color: var(--ink);
    overflow-wrap: anywhere;
  }

  .what small {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 10px;
  }

  .dev {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .week {
    display: flex;
    gap: 3px;
  }

  .week i {
    width: 18px;
    height: 18px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    font-style: normal;
    font-size: 10.5px;
    font-weight: 700;
    text-transform: uppercase;
    color: var(--ink-3);
    background: var(--surface-2);
  }

  .week i.on {
    background: var(--cool-soft);
    color: var(--cool);
  }

  /* On or paused: a switch. */
  .toggle {
    flex: none;
    width: 46px;
    height: 28px;
    margin-right: 12px;
    border: 0;
    border-radius: 999px;
    background: var(--cool);
    position: relative;
  }

  .toggle i {
    position: absolute;
    top: 3px;
    left: 21px;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: #fff;
    transition: left 0.2s var(--ease);
  }

  .toggle.paused {
    background: var(--surface-3);
  }

  .toggle.paused i {
    left: 3px;
  }

  /* A phone: the time over the rest. */
  @media (max-width: 520px) {
    header {
      flex-direction: column;
    }

    .item {
      grid-template-columns: 1fr;
      gap: 6px;
    }

    .when {
      grid-auto-flow: column;
      align-items: center;
      gap: 6px;
    }
  }
</style>
