<script>
  import { onMount } from 'svelte';
  import { value, num, nameOf, roomList, climateIcon, climateOn, climateState, stoveState, stoveAsked, setpointOf, reachable, clock } from '../lib/home.svelte.js';
  import { parseWeek, spans, nowIn, hhmm } from '../lib/program.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The heating at a glance, for the home page: one line per heater or
   *  stove, room by room (its room's temperature, what it does now, and
   *  until when when a week program decides), the departure mode when on.
   *  The Heating page has the commands. */

  const units = $derived(roomList().flatMap((r) => r.climates));

  // A program's order now and its end, ticking each minute.
  let now = $state(new Date());
  // « Je pars »: said here when the house is in departure mode.
  let away = $state(null);
  onMount(() => {
    const tick = setInterval(() => (now = new Date()), 60_000);
    fetch('/api/heating/away')
      .then((res) => (res.ok ? res.json() : null))
      .then((state) => (away = state))
      .catch(() => {});
    return () => clearInterval(tick);
  });

  /** What runs now, by the heater's own week program: { order, celsius, end }. */
  function programNow(id) {
    const week = parseWeek(value(id, 'week_program'));
    const { day, slot } = nowIn(now);
    return spans(week?.[day], value(id, 'device_mode_type')).find((s) => s.at <= slot && slot < s.end) ?? null;
  }

  const WARM = ['comfort', 'comfort_1', 'comfort_2', 'manual', 'chaud', 'ignition', 'awaiting_flame', 'starting', 'on'];
  const COOL = ['hors', 'holiday', 'froid'];
  /** A unit's line: its state word and colour, and the detail under its name. */
  function line(d) {
    const temp = value(d.id, 'temperature') ?? value(d.id, 'temp_current');
    const base = { id: d.id, icon: climateIcon(d), name: nameOf(d.id), temp, word: climateState(d), tone: 'off', detail: null };
    if (!reachable(d.id)) return { ...base, word: t('maison.accueil.chauffage.injoignable'), tone: 'alert' };
    // A stove switched from the dashboard, until its cloud says it took it.
    if (d.id in stoveAsked) return { ...base, word: t(stoveAsked[d.id] ? 'commun.poele.mise_en_marche' : 'commun.poele.mise_a_l_arret') };
    const stove = climateIcon(d) === 'stove' && stoveState(d.id);
    const mode = value(d.id, 'mode');
    if (!stove && !climateOn(d)) return base;
    const setpoint = setpointOf(d.id);
    const target = setpoint != null && setpoint > 0 ? t('maison.accueil.chauffage.consigne', { deg: num(setpoint, 1) }) : null;
    if (mode === 'program') {
      const run = programNow(d.id);
      return {
        ...base,
        word: run?.celsius != null ? `${num(run.celsius, 1)}°` : run?.order ? orderWord(run.order) : base.word,
        tone: toneOf(run?.order),
        detail: run ? t('maison.accueil.chauffage.programme_jusqua', { time: hhmm(run.end) }) : t('commun.piece.mode.programme'),
      };
    }
    // In alarm: the short word in the chip (its column is fixed), the rest and the code below.
    if (mode === 'alarm' || mode === 'mem_alarm') {
      const code = value(d.id, 'alarm');
      const what = [mode === 'mem_alarm' ? t('commun.poele.alarme_memorisee') : null, code && code !== 'OK' ? code : null].filter(Boolean);
      return { ...base, word: t('commun.poele.alarme'), tone: 'alert', detail: what.join(' · ') || target };
    }
    return { ...base, tone: toneOf(mode), detail: target };
  }
  const toneOf = (mode) => (mode === 'eco' || mode === 'stand_by' ? 'good' : WARM.includes(mode) ? 'warm' : COOL.includes(mode) ? 'cool' : 'off');
  // A program's order in the words of the modes (comfort, eco…).
  const ORDERS = {
    comfort: 'commun.piece.mode.confort',
    comfort_1: 'commun.piece.mode.confort_1',
    comfort_2: 'commun.piece.mode.confort_2',
    eco: 'commun.piece.mode.eco',
    hors: 'commun.piece.mode.hors_gel',
    off: 'commun.piece.mode.arret',
  };
  const orderWord = (order) => (ORDERS[order] ? t(ORDERS[order]) : order);

  const lines = $derived(units.map(line));
  // A stove in alarm: the card leads to it, at its alarm.
  const alarmed = $derived(units.find((d) => climateIcon(d) === 'stove' && ['alarm', 'mem_alarm'].includes(value(d.id, 'mode'))));
  const href = $derived(alarmed ? `#/chauffage/${encodeURIComponent(alarmed.id)}` : '#/chauffage');
  // « 4 en Éco · 3 en Confort »: the house's heating in one sentence.
  const tally = $derived.by(() => {
    const by = new Map();
    for (const l of lines) by.set(l.word, (by.get(l.word) ?? 0) + 1);
    return [...by.entries()]
      .sort((a, b) => b[1] - a[1])
      .map(([word, count]) => t('maison.accueil.chauffage.compte', { word, count }))
      .join(' · ');
  });
</script>

{#if units.length}
  <a class="card heating" {href} aria-label={t('maison.accueil.chauffage.detail')}>
    <div class="card-head">
      <h2><Icon name="radiator" size={18} />{t('maison.accueil.chauffage.titre')}</h2>
      <span class="more" aria-hidden="true"><Icon name="arrow-top-right" size={18} /></span>
    </div>
    {#if away?.away}
      <p class="away"><Icon name="leaf" size={16} />{t('maison.accueil.chauffage.depart', { time: clock(away.since) })}</p>
    {/if}
    <ul>
      {#each lines as l (l.id)}
        <li>
          <span class="ic {l.tone}"><Icon name={l.icon} size={18} /></span>
          <span class="who">
            <b>{l.name}</b>
            {#if l.detail}<small>{l.detail}</small>{/if}
          </span>
          <span class="temp num">{l.temp != null ? `${num(l.temp, 1)}°` : '—'}</span>
          <span class="chip {l.tone}">{l.word}</span>
        </li>
      {/each}
    </ul>
    <p class="tally muted">{tally}</p>
  </a>
{/if}

<style>
  .heating {
    display: block;
    color: inherit;
    text-decoration: none;
    transition: transform 0.2s var(--ease);
  }

  .heating:hover {
    transform: translateY(-2px);
  }

  .card-head {
    margin-bottom: 10px;
  }

  .away {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
    padding: 8px 12px;
    border-radius: var(--r-sm);
    background: var(--good-soft);
    color: var(--good);
    font-size: 13.5px;
    font-weight: 600;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
  }

  li {
    display: grid;
    /* Fixed columns: temperatures and modes line up whatever the words. */
    grid-template-columns: auto minmax(0, 1fr) 3.4em 7.5em;
    align-items: center;
    gap: 10px;
    padding: 6px 0;
    border-top: 1px solid var(--line);
  }

  li:first-child {
    border-top: 0;
  }

  .ic {
    width: 30px;
    height: 30px;
    border-radius: 10px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-3);
  }

  .ic.warm {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .ic.good {
    background: var(--good-soft);
    color: var(--good);
  }

  .ic.cool {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .ic.alert {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .who {
    display: grid;
    min-width: 0;
  }

  .who b,
  .who small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .who b {
    font-size: 14px;
    font-weight: 600;
  }

  .who small {
    font-size: 12px;
    color: var(--ink-3);
  }

  .temp {
    font-size: 14px;
    font-weight: 650;
    text-align: right;
  }

  .chip {
    padding: 4px 8px;
    font-size: 12.5px;
    justify-content: center;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  @media (max-width: 760px) {
    li {
      grid-template-columns: auto minmax(0, 1fr) 3.2em 6.2em;
      gap: 8px;
    }
  }

  .tally {
    margin-top: 8px;
    font-size: 12.5px;
  }
</style>
