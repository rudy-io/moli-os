<script>
  import { onDestroy } from 'svelte';
  import { device, value, reachable, num, act, home, clock } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import ScheduleEditor from './ScheduleEditor.svelte';
  import ValveChart from './ValveChart.svelte';
  import { scheduleSpec } from '../lib/schedule.js';
  import { pointOf, canWrite, numericRange, quantize, writableChoices, climateKeys } from '../lib/capabilities.js';
  import { completionGuard } from '../lib/completion-guard.js';
  import { DAYS, hhmm } from '../lib/schedule.js';
  import { heatsOnly, heatState, valveOpen, nextStep } from '../lib/radiator.js';
  import { hub } from '../../lib/hub.svelte.js';

  // A thermostat that only heats: a Zigbee valve, a valve on another protocol,
  // or a room's thermostat kept by Moli (it drives the room's valves). Drawn as
  // its radiator, warming while it heats, with in words where it is; then its
  // setpoint and its mode. Every order goes through `act`, the hub's normal path and the guard.
  // `schedule`: false where the schedule is edited beside the card (the Chauffage page).
  let { id, name, schedule = true } = $props();

  const d = $derived(device(id));
  const keys = $derived(climateKeys(d));
  const unit = $derived(pointOf(d, keys.target)?.unit);
  const range = $derived(numericRange(d, keys.target, 0.5));
  const modes = $derived(writableChoices(d, keys.mode));
  const temp = $derived(value(id, keys.temperature));
  const target = $derived(value(id, keys.target));
  const mode = $derived(value(id, keys.mode));
  const writable = $derived(!!range && canWrite(d, keys.target));
  const spec = $derived(scheduleSpec(d));
  const hasSchedule = $derived(!!spec);
  // Without a schedule, `heat` is just on.
  const scheduled = $derived(!!pointOf(d, keys.mode)?.kind?.values?.includes('auto'));
  // A thermostat that states hundredths (Matter) is still nudged by half a degree.
  const step = $derived(range ? Math.max(range.step, 0.5) : 0.5);

  // ---- the drawing and its words ----------------------------------------------------------

  /** The valves a room's thermostat drives (its members), as each one stands. */
  const valves = $derived((d?.members ?? []).map(device).filter((m) => m && heatsOnly(m)).map((m) => {
    const k = climateKeys(m);
    const reading = value(m.id, k.temperature);
    return { id: m.id, temp: reading, open: reachable(m.id) ? valveOpen({ running: value(m.id, 'running_state'), temp: reading, target: value(m.id, k.target) }) : null };
  }));
  const room = $derived(valves.length > 0);
  const phase = $derived(heatState({
    reachable: reachable(id) && hub.connected !== false,
    mode,
    temp,
    target,
    running: value(id, 'running_state'),
    room,
  }));
  const kind = $derived(room ? 'piece' : 'vanne');
  const headline = $derived(t(`commun.vanne.etat.${kind}.${phase}`));
  const deg = (v) => `${num(v, v % 1 ? 1 : 0)}°`;
  const upcoming = $derived.by(() => {
    if (mode !== 'auto' || !spec) return null;
    const texts = Object.fromEntries(DAYS.map((day) => [day, value(id, `weekly_schedule_${day}`)]));
    return nextStep(spec, texts, new Date(home.now));
  });
  const detail = $derived.by(() => {
    if (phase === 'offline') return '';
    const parts = [];
    if (room && phase !== 'off') {
      const open = valves.filter((v) => v.open === true).length;
      if (valves.length === 1) {
        const [v] = valves;
        if (v.open != null) parts.push(t(v.open ? 'commun.vanne.une_ouverte' : 'commun.vanne.une_fermee'));
        if (Number.isFinite(v.temp)) parts.push(t('commun.vanne.radiateur', { temp: `${num(v.temp, 1)}°` }));
      } else if (valves.some((v) => v.open != null)) {
        parts.push(t('commun.vanne.ouvertes', { n: open, total: valves.length }));
      }
    }
    if (upcoming) {
      parts.push(t(upcoming.days === 0 ? 'commun.vanne.puis' : upcoming.days === 1 ? 'commun.vanne.puis_demain' : 'commun.vanne.puis_jour', {
        temp: deg(upcoming.deg),
        heure: hhmm(upcoming.min),
        jour: t(`commun.vanne.jour.${DAYS[(new Date(home.now).getDay() + 6 + upcoming.days) % 7]}`),
      }));
    }
    return parts.join(' · ');
  });
  // SVG ids: a device id carries colons.
  const svgId = $derived(id.replace(/[^\w-]/g, '_'));
  // A room thermostat of Moli: a valve turned by hand is left as it is for a while.
  const override = $derived(pointOf(d, 'valve_override') ? value(id, 'valve_override') : null);
  const overrideEnd = $derived.by(() => {
    const at = Date.parse(value(id, 'valve_override_until') ?? '');
    return Number.isFinite(at) ? clock(at) : null;
  });
  const resume = () =>
    reachable(id) && act(`${id}/valve_override`, 'none', t('commun.vanne.reglage.action', { name }));
  let editing = $state(false);
  let charting = $state(false);

  // Taps on − / + add up; one order leaves once the finger rests.
  let wanted = $state(null);
  let timer;
  let resetTimer;
  const completion = completionGuard();
  onDestroy(() => { completion.change(); clearTimeout(timer); clearTimeout(resetTimer); });
  const shown = $derived(wanted ?? target);

  function nudge(delta) {
    if (!writable || !reachable(id)) return;
    const base = wanted ?? target;
    if (!Number.isFinite(base)) return;
    completion.change();
    wanted = quantize(base + delta, range);
    clearTimeout(timer);
    const forId = id;
    timer = setTimeout(() => (id === forId ? flush() : (wanted = null)), 900);
  }

  async function flush() {
    const v = wanted;
    if (!writable || !reachable(id) || v == null || v === target || quantize(v, range) !== v) {
      wanted = null;
      return;
    }
    const owns = completion.capture();
    const done = await act(`${id}/${keys.target}`, v, t('commun.clim.label_consigne', { name }));
    if (!owns()) return;
    // Shown until the valve reports it (or gives up on it).
    if (done !== 'ok') wanted = null;
    else {
      clearTimeout(resetTimer);
      resetTimer = setTimeout(() => owns() && wanted === v && (wanted = null), 15_000);
    }
  }

  $effect(() => {
    if (wanted != null && target === wanted) wanted = null;
  });

  $effect(() => {
    // A pending gesture belongs to the valve it was made on.
    void id;
    completion.change();
    clearTimeout(timer);
    clearTimeout(resetTimer);
    wanted = null;
  });

  /** A mode said in the house's language (an unknown one: as the valve says it). */
  const MODE_LABELS = { off: 'commun.vanne.mode.off', auto: 'commun.vanne.mode.auto', heat: 'commun.vanne.mode.heat' };
  const modeLabel = (key) => (key === 'heat' && !scheduled ? t('commun.vanne.mode.marche') : MODE_LABELS[key] ? t(MODE_LABELS[key]) : (key ?? '—'));
  const setMode = (key) => modes.includes(key) && act(`${id}/${keys.mode}`, key, t('commun.action', { label: name, action: modeLabel(key).toLowerCase() }));
</script>

<div class="valve" class:unreachable={!reachable(id)}>
  <div class="hero {phase}" role="group" aria-label={`${name} · ${headline}${detail ? ` · ${detail}` : ''}`}>
    <svg class="drawing" viewBox="0 0 64 72" aria-hidden="true">
      <defs>
        <clipPath id="cols-{svgId}">
          {#each [10, 19, 28, 37, 46] as x (x)}<rect {x} y="22" width="8" height="38" rx="4" />{/each}
        </clipPath>
        <linearGradient id="hot-{svgId}" x1="0" y1="0" x2="0" y2="1">
          <stop class="stop-top" offset="0" />
          <stop class="stop-bottom" offset="1" />
        </linearGradient>
      </defs>
      <g class="waves">
        <path class="wave a" d="M20 17 q-3 -3 0 -6 t0 -6" />
        <path class="wave b" d="M32 17 q-3 -3 0 -6 t0 -6" />
        <path class="wave c" d="M44 17 q-3 -3 0 -6 t0 -6" />
      </g>
      <rect class="bar" x="11" y="25" width="42" height="4" rx="2" />
      <rect class="bar" x="11" y="53" width="42" height="4" rx="2" />
      <g clip-path="url(#cols-{svgId})">
        <rect class="cold" x="10" y="22" width="44" height="38" />
        <g class="heat-wrap">
          <rect class="heat" x="10" y="22" width="44" height="38" fill="url(#hot-{svgId})" />
          <rect class="flow" x="10" y="10" width="44" height="7" />
        </g>
      </g>
      {#each [10, 19, 28, 37, 46] as x (x)}<rect class="col" {x} y="22" width="8" height="38" rx="4" />{/each}
      <rect class="foot" x="12" y="60" width="4" height="6" rx="1.5" />
      <rect class="foot" x="48" y="60" width="4" height="6" rx="1.5" />
      <path class="pipe" d="M2 66 V56 H10" />
      <path class="pipe-flow" d="M2 66 V56 H10" />
      <g class="trv">
        <rect class="trv-head" x="0" y="36" width="8" height="15" rx="3" />
        <line x1="1.5" y1="40" x2="6.5" y2="40" />
        <line x1="1.5" y1="43.5" x2="6.5" y2="43.5" />
        <line x1="1.5" y1="47" x2="6.5" y2="47" />
        <rect class="trv-neck" x="2" y="51" width="4" height="5" />
      </g>
      {#if phase === 'reached'}
        <g class="badge tick"><circle cx="54" cy="12" r="7" /><path d="M50.8 12.2 l2.2 2.2 l4.2 -4.4" /></g>
      {:else if phase === 'off'}
        <g class="badge frost"><circle cx="54" cy="12" r="7" /><path d="M54 7.5 V16.5 M50.1 9.75 L57.9 14.25 M50.1 14.25 L57.9 9.75" /></g>
      {/if}
    </svg>
    <div class="words">
      <h3 class="name">{name}</h3>
      <span class="phase">{headline}</span>
      {#if detail}<small class="detail num">{detail}</small>{/if}
    </div>
    {#if Number.isFinite(temp)}
      <b class="num temp" title={room ? t('commun.clim.temp_piece') : t('commun.vanne.temp_vanne')}>{num(temp, 1)}<small>{pointOf(d, keys.temperature)?.unit ?? ''}</small></b>
    {/if}
  </div>
  {#if Number.isFinite(shown) && pointOf(d, keys.target)}
    {#if writable}
      <div class="setpoint" role="group" aria-label={t('commun.clim.consigne')}>
        <button onclick={() => nudge(-step)} aria-label={t('commun.clim.moins_chaud')} disabled={shown <= range.first}>−</button>
        <b class="num">{num(shown, shown % 1 ? 1 : 0)}{unit ?? ''}</b>
        <button onclick={() => nudge(step)} aria-label={t('commun.clim.plus_chaud')} disabled={shown >= range.last}>+</button>
      </div>
    {:else}
      <p class="muted">{t('commun.clim.consigne_val', { temp: num(shown, shown % 1 ? 1 : 0), unit: unit ?? '' })}</p>
    {/if}
  {/if}
  {#if override === 'manual' || override === 'airing'}
    <div class="override" role="status">
      <span>{t(`commun.vanne.reglage.${override === 'airing' ? 'aeration' : 'main'}`, { heure: overrideEnd ?? '—' })}</span>
      <button class="pill" onclick={resume}>{t(mode === 'auto' ? 'commun.vanne.reglage.reprendre' : 'commun.vanne.reglage.reprendre_consigne')}</button>
    </div>
  {:else if override === 'none' && mode === 'auto'}
    <p class="muted">{t('commun.vanne.reglage.programme')}</p>
  {/if}
  {#if modes.length}
    <div class="modes" role="group" aria-label={t('commun.clim.mode')}>
      {#each modes as key (key)}
        <button class:active={mode === key} onclick={() => setMode(key)} aria-pressed={mode === key}>{modeLabel(key)}</button>
      {/each}
    </div>
  {:else if mode != null}
    <p class="muted">{t('commun.clim.mode')} <b>{modeLabel(mode)}</b></p>
  {/if}
  {#if pointOf(d, keys.temperature) || pointOf(d, keys.target)}
    <button class="fold" onclick={() => (charting = !charting)} aria-expanded={charting}>{t('commun.vanne.courbe.titre')}</button>
    {#if charting}
      <ValveChart {id} {name} />
    {/if}
  {/if}
  {#if hasSchedule && schedule}
    <button class="fold" onclick={() => (editing = !editing)} aria-expanded={editing}>{t('commun.vanne.prog.titre')}</button>
    {#if editing}
      <ScheduleEditor {id} {name} />
    {/if}
  {/if}
</div>

<style>
  .valve {
    display: grid;
    gap: 8px;
    padding: 6px 4px;
  }

  .valve.unreachable {
    opacity: 0.55;
  }

  /* ---- the drawing and its words ---- */

  .hero {
    --tone: var(--ink-3);
    --tone-soft: var(--surface-3);
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: 0;
    padding: 10px 14px 10px 10px;
    border-radius: var(--r-md);
    background: var(--surface-2);
    transition: background 0.4s var(--ease), box-shadow 0.4s var(--ease);
  }

  .hero.heating {
    --tone: var(--warm);
    --tone-soft: var(--warm-soft);
  }

  .hero.reached {
    --tone: var(--good);
    --tone-soft: var(--good-soft);
  }

  .hero.off {
    --tone: var(--cool);
    --tone-soft: var(--cool-soft);
  }

  .hero.heating,
  .hero.reached {
    background: linear-gradient(155deg, var(--tone-soft) 0%, var(--surface) 85%);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--tone) 30%, transparent);
  }

  .drawing {
    flex: none;
    width: 58px;
    height: 66px;
    overflow: visible;
  }

  .col {
    fill: none;
    stroke: var(--ink-3);
    stroke-width: 1.4;
  }

  .bar,
  .foot {
    fill: var(--surface-3);
    stroke: var(--ink-3);
    stroke-width: 1.2;
  }

  .cold {
    fill: var(--surface);
  }

  /* The warmth climbs into the columns, top first, as the hot water does. */
  .heat-wrap {
    transform: translateY(38px);
    transition: transform 1.2s var(--ease);
  }

  .heating .heat-wrap {
    transform: translateY(0);
  }

  .reached .heat-wrap {
    transform: translateY(14px);
  }

  .stop-top {
    stop-color: var(--warm);
    stop-opacity: 0.85;
  }

  .stop-bottom {
    stop-color: var(--warm);
    stop-opacity: 0.25;
  }

  .reached .stop-top {
    stop-opacity: 0.4;
  }

  .reached .stop-bottom {
    stop-opacity: 0.1;
  }

  .flow {
    fill: #fff;
    opacity: 0;
  }

  .heating .flow {
    animation: flow 2.6s linear infinite;
  }

  .waves path {
    fill: none;
    stroke: var(--warm);
    stroke-width: 2;
    stroke-linecap: round;
    opacity: 0;
  }

  .heating .wave {
    animation: rise 2.4s ease-out infinite;
  }

  .heating .wave.b {
    animation-delay: 0.8s;
  }

  .heating .wave.c {
    animation-delay: 1.6s;
  }

  .pipe {
    fill: none;
    stroke: var(--ink-3);
    stroke-width: 3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .pipe-flow {
    fill: none;
    stroke: var(--warm);
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-dasharray: 2 3;
    opacity: 0;
  }

  .heating .pipe-flow {
    opacity: 1;
    animation: pipe 0.9s linear infinite;
  }

  .trv-head {
    fill: var(--surface);
    stroke: var(--ink-3);
    stroke-width: 1.3;
  }

  .heating .trv-head {
    fill: color-mix(in srgb, var(--warm) 30%, var(--surface));
    stroke: var(--warm);
  }

  .trv line {
    stroke: var(--ink-3);
    stroke-width: 0.9;
    opacity: 0.7;
  }

  .trv-neck {
    fill: var(--ink-3);
  }

  .badge circle {
    stroke: var(--surface);
    stroke-width: 2;
  }

  .badge path {
    fill: none;
    stroke: #fff;
    stroke-width: 1.8;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .tick circle {
    fill: var(--good);
  }

  .frost circle {
    fill: var(--cool);
  }

  .frost path {
    stroke-width: 1.4;
  }

  @keyframes rise {
    0% {
      opacity: 0;
      transform: translateY(5px);
    }

    35% {
      opacity: 0.9;
    }

    100% {
      opacity: 0;
      transform: translateY(-5px);
    }
  }

  @keyframes flow {
    0% {
      opacity: 0;
      transform: translateY(0);
    }

    20% {
      opacity: 0.35;
    }

    100% {
      opacity: 0;
      transform: translateY(50px);
    }
  }

  @keyframes pipe {
    to {
      stroke-dashoffset: -5;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .heating .wave,
    .heating .flow,
    .heating .pipe-flow {
      animation: none;
    }

    .heating .wave {
      opacity: 0.7;
    }
  }

  .words {
    flex: 1;
    display: grid;
    gap: 2px;
    min-width: 0;
  }

  .name {
    margin: 0;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-2);
    overflow-wrap: anywhere;
  }

  .phase {
    font-size: 17px;
    font-weight: 700;
    line-height: 1.2;
    color: var(--ink);
  }

  .heating .phase {
    color: var(--warm-ink);
  }

  .reached .phase {
    color: var(--good);
  }

  .off .phase {
    color: var(--cool);
  }

  .detail {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .temp {
    flex: none;
    font-size: 26px;
    font-weight: 300;
    letter-spacing: -0.02em;
  }

  .temp small {
    font-size: 15px;
    vertical-align: top;
  }
  .setpoint {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .setpoint button,
  .modes button {
    border: none;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    cursor: pointer;
  }

  .setpoint button {
    width: 34px;
    height: 34px;
    border-radius: 50%;
    font-size: 20px;
    line-height: 1;
  }

  .setpoint button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .setpoint b {
    min-width: 52px;
    text-align: center;
    font-size: 20px;
  }

  .modes {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .modes button {
    padding: 6px 12px;
    border-radius: 999px;
    font-size: 13px;
    color: var(--ink-3);
  }

  .modes button.active {
    background: var(--ink);
    color: var(--surface);
  }

  .fold {
    justify-self: start;
    border: none;
    background: none;
    padding: 0;
    color: var(--ink-2);
    font: inherit;
    font-size: 12.5px;
    text-decoration: underline;
    cursor: pointer;
  }

  .override {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    padding: 8px 10px;
    border-radius: 10px;
    background: var(--surface-2);
    font-size: 13px;
  }

  .override .pill {
    border: none;
    border-radius: 999px;
    padding: 5px 12px;
    background: var(--ink);
    color: var(--surface);
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
  }

  .muted {
    margin: 0;
    font-size: 12.5px;
    color: var(--ink-2);
  }
</style>
