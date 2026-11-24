<script>
  import { device, value, nameOf, reachable, clock, num, act, home } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { hub } from '../../lib/hub.svelte.js';
  import { pointOf, climateKeys } from '../lib/capabilities.js';
  import { DAYS, scheduleSpec, hhmm } from '../lib/schedule.js';
  import { radiatorState, nextChange, warm, heatsOnly, valveOpen } from '../lib/radiator.js';
  import { temporaryOf } from '../lib/temporary.js';

  /** A radiator behind a thermostat, drawn: heat rises from it while it heats,
   *  it glows at its setpoint, a breeze goes through it while the window is
   *  open. Under it, in words, what it does, the room's temperature, whether
   *  the room's valves are open, and what comes next. A valve on its own (no
   *  room around it) says whether it is open or closed. `onopen`: the whole tile opens the valve's card (its setpoint,
   *  its modes); `open`: that card is open. */
  let { id, icons = false, onopen = null, open = false } = $props();

  const d = $derived(device(id));
  const label = $derived(nameOf(id));
  const keys = $derived(climateKeys(d));
  const unitOf = (key) => pointOf(d, key)?.unit ?? '°';
  const temperature = $derived(value(id, keys.temperature));
  const target = $derived(value(id, keys.target));
  const mode = $derived(value(id, keys.mode));
  const override = $derived(pointOf(d, 'valve_override') ? value(id, 'valve_override') : null);
  const live = $derived(reachable(id) && hub.connected === true);

  /** The valves a room's thermostat drives (its members), as each one stands. */
  const valves = $derived((d?.members ?? []).map(device).filter((m) => m && heatsOnly(m)).map((m) => {
    const k = climateKeys(m);
    const reading = value(m.id, k.temperature);
    return { temperature: reading, open: reachable(m.id) ? valveOpen({ running: value(m.id, 'running_state'), temperature: reading, target: value(m.id, k.target) }) : null };
  }));
  // A room's thermostat (Moli's: it drives valves, corrects them, keeps a valve
  // turned by hand) reads the room; anything else is a valve on its own.
  const valve = $derived(!(d?.members ?? []).length && !pointOf(d, 'valve_correction') && !pointOf(d, 'valve_override'));

  const state = $derived(radiatorState({
    reachable: reachable(id),
    mode,
    temperature,
    target,
    override,
    running: pointOf(d, 'running_state') ? value(id, 'running_state') : null,
    valve,
  }));
  const heating = $derived(warm(state.phase));

  const deg = (v, key) => (typeof v === 'number' && Number.isFinite(v) ? `${num(v, v % 1 ? 1 : 0)}${unitOf(key)}` : null);
  const until = $derived.by(() => {
    const at = Date.parse(value(id, 'valve_override_until') ?? '');
    return Number.isFinite(at) ? clock(at) : '—';
  });
  const temporary = $derived(temporaryOf((key) => value(id, key), home.now));
  // What the schedule does next, said once a minute.
  const next = $derived.by(() => {
    if (mode !== 'auto') return null;
    const texts = Object.fromEntries(DAYS.map((day) => [day, value(id, `weekly_schedule_${day}`)]));
    return nextChange(scheduleSpec(d), texts, new Date(home.now));
  });

  const headline = $derived(t(`commun.radiateur.phase.${valve && state.phase === 'heating' ? 'open' : state.phase}`));
  // The room's valves: open or closed, and how warm by the radiator.
  const valvesSaid = $derived.by(() => {
    if (valve || ['off', 'offline', 'airing'].includes(state.phase)) return null;
    if (valves.length === 1) {
      const [v] = valves;
      const parts = [];
      if (v.open != null) parts.push(t(v.open ? 'commun.radiateur.vanne_ouverte' : 'commun.radiateur.vanne_fermee'));
      if (Number.isFinite(v.temperature)) parts.push(t('commun.radiateur.pres_du_radiateur', { temp: `${num(v.temperature, 1)}°` }));
      return parts.join(' · ') || null;
    }
    if (!valves.some((v) => v.open != null)) return null;
    return t('commun.radiateur.vannes_ouvertes', { n: valves.filter((v) => v.open === true).length, total: valves.length });
  });
  const temps = $derived.by(() => {
    const a = deg(temperature, keys.temperature);
    const b = deg(target, keys.target);
    if (state.phase === 'off' || state.phase === 'offline') return a;
    return a && b ? t('commun.radiateur.temps', { temp: a, target: b }) : (a ?? b);
  });
  const detail = $derived.by(() => {
    if (state.phase === 'airing') return t('commun.radiateur.aeration', { heure: until });
    if (state.hold === 'manual') return t('commun.vanne.reglage.main', { heure: until });
    if (temporary.boost && mode !== 'off') {
      return t('commun.radiateur.boost', { deg: deg(temporary.boost.by, keys.target), heure: clock(temporary.boost.until) });
    }
    if (temporary.hold && mode === 'auto') return t('commun.radiateur.maintien', { heure: clock(temporary.hold.until) });
    if (next && next.days < 2) {
      return t(next.days ? 'commun.radiateur.ensuite_demain' : 'commun.radiateur.ensuite', { deg: deg(next.deg, keys.target), heure: hhmm(next.min) });
    }
    if (mode === 'auto') return t('commun.vanne.mode.auto');
    if (mode === 'heat') return t('commun.radiateur.fixe');
    return null;
  });
  const held = $derived(state.phase === 'airing' || state.hold === 'manual');
  const resume = () =>
    live && act(`${id}/valve_override`, 'none', t('commun.vanne.reglage.action', { name: label }));
  const said = $derived([label, headline, temps, valvesSaid, detail].filter(Boolean).join(' · '));
</script>

{#snippet drawing()}
  <svg class="drawing" viewBox="0 0 64 72" aria-hidden="true">
    <defs>
      <clipPath id={`fins-${id.replace(/[^\w-]/g, '_')}`}>
        {#each [4, 13, 22, 31, 40] as x (x)}<rect {x} y="22" width="7" height="40" rx="3.5" />{/each}
      </clipPath>
    </defs>
    <g class="waves">
      <path class="wave a" d="M14 18 q-3 -3 0 -6 t0 -6" />
      <path class="wave b" d="M26 18 q-3 -3 0 -6 t0 -6" />
      <path class="wave c" d="M38 18 q-3 -3 0 -6 t0 -6" />
    </g>
    <rect class="pipe" x="3" y="26" width="46" height="4" rx="2" />
    <rect class="pipe" x="3" y="54" width="46" height="4" rx="2" />
    {#each [4, 13, 22, 31, 40] as x (x)}<rect class="fin" {x} y="22" width="7" height="40" rx="3.5" />{/each}
    <g clip-path={`url(#fins-${id.replace(/[^\w-]/g, '_')})`}>
      <rect class="glow" x="0" y="22" width="52" height="40" />
    </g>
    {#each [4, 13, 22, 31, 40] as x (x)}<rect class="shine" x={x + 1.6} y="25" width="1.4" height="34" rx="0.7" />{/each}
    <rect class="foot" x="7" y="62" width="4" height="6" rx="1.5" />
    <rect class="foot" x="40" y="62" width="4" height="6" rx="1.5" />
    <path class="stem" d="M47 56 h6 v-4" />
    <rect class="head" x="50" y="36" width="10" height="17" rx="3.5" />
    <line class="grip" x1="52.5" y1="41" x2="57.5" y2="41" />
    <line class="grip" x1="52.5" y1="44" x2="57.5" y2="44" />
    <line class="grip" x1="52.5" y1="47" x2="57.5" y2="47" />
    <circle class="led" cx="55" cy="33" r="1.8" />
    <g class="breeze">
      <path class="gust a" d="M-6 32 h14 q4 0 4 -3" />
      <path class="gust b" d="M-10 42 h20" />
      <path class="gust c" d="M-6 51 h12 q4 0 4 3" />
    </g>
    {#if state.hold === 'manual'}
      <!-- A knob turned by hand. -->
      <g class="hand"><circle cx="59" cy="60" r="5.5" /><circle class="knob" cx="59" cy="60" r="2.6" /><path d="M59 60 l2.2 -2.6" /></g>
    {/if}
  </svg>
{/snippet}

<div class="radiator {state.phase}" class:icons class:open title={icons ? said : undefined} role="group" aria-label={said}>
  {#if onopen}
    <button type="button" class="face" onclick={onopen} aria-expanded={open} aria-label={t('commun.radiateur.ouvrir', { name: label })}>
      {@render drawing()}
      {#if icons}<small class="short">{headline}</small>{:else}{@render words()}{/if}
    </button>
  {:else}
    <div class="face">
      {@render drawing()}
      {#if icons}<small class="short">{headline}</small>{:else}{@render words()}{/if}
    </div>
  {/if}
  {#if held && !icons && override}
    <button type="button" class="resume" disabled={!live} onclick={resume}>{t(mode === 'auto' ? 'commun.vanne.reglage.reprendre' : 'commun.vanne.reglage.reprendre_consigne')}</button>
  {/if}
</div>

{#snippet words()}
  <div class="words">
    <b class="name">{label}</b>
    <span class="phase">{headline}{#if temps}{' · '}<span class="temps num">{temps}</span>{/if}</span>
    {#if valvesSaid}<small class="detail num">{valvesSaid}</small>{/if}
    {#if detail}<small class="detail">{detail}</small>{/if}
  </div>
{/snippet}

<style>
  .radiator {
    --tone: var(--ink-3);
    --tone-soft: var(--surface-3);
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
    min-width: 0;
    padding: 12px 14px;
    border-radius: var(--r-md);
    background: var(--surface-2);
    color: var(--ink);
    transition: background 0.4s var(--ease), box-shadow 0.4s var(--ease);
  }

  .radiator.heating,
  .radiator.holding,
  .radiator.closed {
    --tone: var(--warm);
    --tone-soft: var(--warm-soft);
  }

  .radiator.airing {
    --tone: var(--cool);
    --tone-soft: var(--cool-soft);
  }

  .radiator.heating,
  .radiator.holding,
  .radiator.closed,
  .radiator.airing {
    background: linear-gradient(155deg, var(--tone-soft) 0%, var(--surface) 85%);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--tone) 30%, transparent);
  }

  .radiator.open {
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--tone) 60%, var(--line));
  }

  .radiator.off,
  .radiator.offline {
    opacity: 0.55;
  }

  .face {
    flex: 1 1 100%;
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: 0;
    padding: 0;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
  }

  button.face {
    cursor: pointer;
  }

  .icons {
    padding: 8px;
  }

  .icons .face {
    flex-direction: column;
    gap: 4px;
    text-align: center;
  }

  /* ---- the drawing ---- */

  .drawing {
    flex: none;
    width: 58px;
    height: 66px;
    overflow: visible;
  }

  .icons .drawing {
    width: 40px;
    height: 46px;
  }

  .fin,
  .pipe {
    fill: var(--surface-3);
    stroke: var(--ink-3);
    stroke-width: 1.2;
  }

  .pipe {
    stroke-width: 1;
  }

  .shine {
    fill: var(--surface);
    opacity: 0.55;
  }

  .foot,
  .stem {
    fill: var(--ink-3);
    stroke: var(--ink-3);
    stroke-width: 1.4;
    stroke-linecap: round;
  }

  .stem {
    fill: none;
  }

  .head {
    fill: var(--surface);
    stroke: var(--ink-3);
    stroke-width: 1.2;
  }

  .grip {
    stroke: var(--ink-3);
    stroke-width: 1;
    stroke-linecap: round;
    opacity: 0.6;
  }

  .led {
    fill: var(--surface-3);
    transition: fill 0.4s var(--ease);
  }

  .heating .led,
  .holding .led,
  .closed .led,
  .airing .led {
    fill: var(--tone);
  }

  .heating .led {
    animation: blink 1.8s ease-in-out infinite;
  }

  /* The heat fills the fins from the bottom: all of them while it heats, the
     lower half at its setpoint. */
  .glow {
    fill: color-mix(in srgb, var(--warm) 70%, var(--alert));
    transform: translateY(40px);
    opacity: 0;
    transition: transform 1.2s var(--ease), opacity 1.2s var(--ease);
  }

  .heating .glow {
    transform: translateY(0);
    opacity: 0.85;
    animation: breathe 3.2s ease-in-out infinite;
  }

  .holding .glow,
  .closed .glow {
    transform: translateY(22px);
    opacity: 0.45;
  }

  .wave {
    fill: none;
    stroke: var(--warm);
    stroke-width: 1.8;
    stroke-linecap: round;
    opacity: 0;
  }

  .heating .wave {
    animation: rise 2.4s ease-in-out infinite;
  }

  .heating .wave.b {
    animation-delay: 0.8s;
  }

  .heating .wave.c {
    animation-delay: 1.6s;
  }

  .airing .fin,
  .airing .pipe {
    fill: color-mix(in srgb, var(--cool) 22%, var(--surface));
  }

  .gust {
    fill: none;
    stroke: var(--cool);
    stroke-width: 1.8;
    stroke-linecap: round;
    opacity: 0;
  }

  .airing .gust {
    animation: blow 2.6s linear infinite;
  }

  .airing .gust.b {
    animation-delay: 0.9s;
  }

  .airing .gust.c {
    animation-delay: 1.7s;
  }

  .hand circle {
    fill: var(--sun);
    stroke: var(--surface);
    stroke-width: 2;
  }

  .hand .knob {
    fill: none;
    stroke: #fff;
    stroke-width: 1.3;
  }

  .hand path {
    fill: none;
    stroke: #fff;
    stroke-width: 1.8;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  @keyframes rise {
    0% {
      transform: translateY(6px);
      opacity: 0;
    }
    40% {
      opacity: 0.9;
    }
    100% {
      transform: translateY(-8px);
      opacity: 0;
    }
  }

  @keyframes blow {
    0% {
      transform: translateX(0);
      opacity: 0;
    }
    20% {
      opacity: 0.9;
    }
    100% {
      transform: translateX(62px);
      opacity: 0;
    }
  }

  @keyframes breathe {
    50% {
      opacity: 0.65;
    }
  }

  @keyframes blink {
    50% {
      opacity: 0.35;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .heating .wave,
    .heating .glow,
    .heating .led,
    .airing .gust {
      animation: none;
    }

    .heating .wave,
    .airing .gust {
      opacity: 0.7;
    }
  }

  /* ---- the words ---- */

  .words {
    flex: 1;
    display: grid;
    gap: 2px;
    min-width: 0;
  }

  .name {
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-2);
    overflow-wrap: anywhere;
  }

  .phase {
    font-size: 17px;
    font-weight: 700;
    line-height: 1.2;
  }

  .heating .phase,
  .holding .phase,
  .closed .phase {
    color: var(--warm-ink);
  }

  .airing .phase {
    color: var(--cool);
  }

  .temps {
    font-weight: 600;
    color: var(--ink-2);
  }

  .detail {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .short {
    font-size: 11px;
    font-weight: 600;
    color: var(--ink-3);
    line-height: 1.2;
  }

  /* Under the words, where it does not squeeze them. */
  .resume {
    flex: none;
    margin-left: 72px;
    height: 34px;
    padding: 0 14px;
    border: none;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font: inherit;
    font-size: 12.5px;
    font-weight: 650;
    cursor: pointer;
  }

  .resume:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
