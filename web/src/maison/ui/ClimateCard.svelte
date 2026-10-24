<script>
  import { onMount } from 'svelte';
  import { device, value, reachable, num, act } from '../lib/home.svelte.js';
  import { energy, useEnergy, watts, euros } from '../lib/energy-live.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import ProgramWeek from './ProgramWeek.svelte';

  let { id, name } = $props();

  const d = $derived(device(id));
  // The dial spans what the unit accepts (a heater goes down to frost guard),
  // within what a room is ever set to.
  const targetPoint = $derived(d?.points.find((p) => p.key === 'target_temperature'));
  const MIN = $derived(Math.max(5, targetPoint?.kind.min ?? 16));
  const MAX = $derived(Math.min(30, targetPoint?.kind.max ?? 30));
  const on = $derived(value(id, 'on') === true);
  const temp = $derived(value(id, 'temperature'));
  // A pilot-wire heater's wiring (Tuya's `device_mode_type`) says the orders
  // it takes, warmest first, then its program and off; on four or six orders
  // it has no setpoint of its own.
  const WIRING = {
    four: { label: 'commun.clim.cablage_4', modes: ['comfort', 'eco', 'hors', 'program', 'off'], setpoint: false },
    six: { label: 'commun.clim.cablage_6', modes: ['comfort', 'comfort_1', 'comfort_2', 'eco', 'hors', 'program', 'off'], setpoint: false },
    switch: { label: 'commun.clim.cablage_interrupteur', modes: ['manual', 'eco', 'hors', 'holiday', 'program', 'off'], setpoint: true },
  };
  const wiring = $derived(WIRING[value(id, 'device_mode_type')]);
  // The wiring is set here too (the module then takes other orders); only
  // the choices the module declares.
  const wirings = $derived.by(() => {
    const point = d?.points.find((p) => p.key === 'device_mode_type');
    return point?.access?.write ? Object.keys(WIRING).filter((key) => point.kind.values?.includes(key)) : [];
  });
  const setWiring = (key) =>
    act(`${id}/device_mode_type`, key, t('commun.action', { label: name, action: t(WIRING[key].label).toLowerCase() }));
  const target = $derived(wiring?.setpoint === false ? null : value(id, 'target_temperature'));
  const mode = $derived(value(id, 'mode'));
  const outdoor = $derived(value(id, 'outdoor_temperature'));

  // What it draws, when a measured circuit feeds it (Daikin units do not
  // measure themselves).
  onMount(useEnergy);
  const feeder = $derived(energy.summary?.meters.find((m) => m.feeds?.includes(id)));
  const draw = $derived.by(() => {
    if (!feeder?.power) return null;
    const i = feeder.power.indexOf('/');
    const v = value(feeder.power.slice(0, i), feeder.power.slice(i + 1));
    return typeof v === 'number' ? v : (feeder.power_w ?? null);
  });
  // What it cost, from its circuit's meter (today, this month).
  const costOf = (span) => (feeder ? (energy.summary?.[span]?.meters?.[feeder.id]?.cost ?? null) : null);
  const dayCost = $derived(costOf('today'));
  const monthCost = $derived(costOf('month'));
  // The circuit feeds something else too (« PAC salon + pompe piscine »).
  const shared = $derived(!!feeder && (feeder.feeds.length > 1 || feeder.name.includes('+')));
  const writable = $derived(wiring?.setpoint !== false && d?.points.some((p) => p.key === 'target_temperature' && p.access?.write));
  // Switched or set to a mode, even without a setpoint (a pilot wire on four orders).
  const controllable = $derived(writable || d?.points.some((p) => ['on', 'mode'].includes(p.key) && p.access?.write));
  const STEP = 0.5;

  // Taps on − / + and drags on the dial add up; one order leaves once the
  // finger rests (each order re-reads the unit and sends all its settings: no
  // machine-gunning it). Off, the unit keeps the new setpoint for later.
  let wanted = $state(null);
  let dragging = $state(false);
  let timer;
  const shownTarget = $derived(wanted ?? target);
  const clamp = (deg) => Math.min(MAX, Math.max(MIN, Math.round(deg / STEP) * STEP));

  function nudge(delta) {
    const base = wanted ?? (typeof target === 'number' ? target : 21);
    wanted = clamp(base + delta);
    send(900);
  }

  function send(after) {
    clearTimeout(timer);
    timer = setTimeout(flush, after);
  }

  async function flush() {
    clearTimeout(timer);
    const v = wanted;
    if (v == null || v === target) {
      wanted = null;
      return;
    }
    const done = await act(`${id}/target_temperature`, v, t('commun.clim.label_consigne', { name }));
    // Shown until the unit reports it (or gives up on it).
    if (done !== 'ok') wanted = null;
    else setTimeout(() => wanted === v && (wanted = null), 15_000);
  }

  $effect(() => {
    if (wanted != null && !dragging && target === wanted) wanted = null;
  });

  // The dial follows the finger: the angle on the 270° ring is the setpoint.
  function tempAt(e, box) {
    const x = e.clientX - (box.left + box.width / 2);
    const y = e.clientY - (box.top + box.height / 2);
    // Too near the middle, the angle means nothing.
    if (Math.hypot(x, y) < box.width * 0.15) return null;
    let a = (((Math.atan2(y, x) * 180) / Math.PI - 135) % 360 + 360) % 360;
    // In the gap at the bottom: the nearest end.
    if (a > 270) a = a > 315 ? 0 : 270;
    return clamp(MIN + (a / 270) * (MAX - MIN));
  }

  function grab(e) {
    if (!writable || !reachable(id)) return;
    dragging = true;
    clearTimeout(timer);
    e.currentTarget.setPointerCapture(e.pointerId);
    wanted = tempAt(e, e.currentTarget.getBoundingClientRect()) ?? shownTarget;
  }

  function drag(e) {
    if (!dragging) return;
    wanted = tempAt(e, e.currentTarget.getBoundingClientRect()) ?? wanted;
  }

  function release() {
    if (!dragging) return;
    dragging = false;
    send(250);
  }

  function key(e) {
    const delta = { ArrowUp: STEP, ArrowRight: STEP, ArrowDown: -STEP, ArrowLeft: -STEP }[e.key];
    if (!delta || !writable) return;
    e.preventDefault();
    nudge(delta);
  }

  async function power() {
    // A setpoint just dragged leaves first: the unit starts on it.
    if (!on && wanted != null) await flush();
    act(`${id}/on`, !on, t(on ? 'commun.clim.label_arret' : 'commun.clim.label_marche', { name }));
  }
  // A pilot-wire module takes `mode = off` and stays as it was: it stops on
  // its switch. Off, an order switches it on first.
  const switched = $derived(!!wiring && d?.points.some((p) => p.key === 'on' && p.access?.write));
  async function setMode(key) {
    if (switched && key === 'off') return on && power();
    if (switched && !on && (await act(`${id}/on`, true, t('commun.clim.label_marche', { name }))) !== 'ok') return;
    act(`${id}/mode`, key, t('commun.action', { label: name, action: modeLabel(key).toLowerCase() }));
  }
  const active = (key) => (switched ? (key === 'off' ? !on : on && mode === key) : mode === key);

  // The keys are the unit's modes (sent as they are); `label` is a catalogue key;
  // `ac`: an air conditioner's, offered to a unit that declares none.
  const MODES = {
    froid: { label: 'commun.clim.mode.froid', icon: 'snowflake', tone: 'cool', ac: true },
    chaud: { label: 'commun.clim.mode.chaud', icon: 'fire', tone: 'warm', ac: true },
    auto: { label: 'commun.clim.mode.auto', icon: 'air-conditioner', tone: 'good', ac: true },
    déshumidification: { label: 'commun.clim.mode.deshumidification', icon: 'humidity', tone: 'cool', ac: true },
    ventilation: { label: 'commun.clim.mode.ventilation', icon: 'fan', tone: 'good', ac: true },
    // A heater on a pilot wire (Tuya).
    comfort: { label: 'commun.piece.mode.confort', icon: 'fire', tone: 'warm' },
    eco: { label: 'commun.piece.mode.eco', icon: 'leaf', tone: 'good' },
    hors: { label: 'commun.piece.mode.hors_gel', icon: 'snowflake', tone: 'cool' },
    manual: { label: 'commun.piece.mode.manuel', icon: 'hand', tone: 'warm' },
    program: { label: 'commun.piece.mode.programme', icon: 'clock', tone: 'good' },
    holiday: { label: 'commun.piece.mode.vacances', icon: 'palm', tone: 'cool' },
    // Comfort less one or two degrees: said, not drawn.
    comfort_1: { label: 'commun.piece.mode.confort_1', text: '−1', tone: 'warm' },
    comfort_2: { label: 'commun.piece.mode.confort_2', text: '−2', tone: 'warm' },
    off: { label: 'commun.piece.mode.arret', icon: 'power', tone: 'cool' },
    // A pellet stove's states (Micronova), shown, never sent.
    ignition: { label: 'commun.poele.allumage', icon: 'fire', tone: 'warm' },
    awaiting_flame: { label: 'commun.poele.attente_flamme', icon: 'fire', tone: 'warm' },
    starting: { label: 'commun.poele.demarrage', icon: 'fire', tone: 'warm' },
    on: { label: 'commun.poele.marche', icon: 'fire', tone: 'warm' },
    cleaning: { label: 'commun.poele.nettoyage', icon: 'fan', tone: 'good' },
    shut_off: { label: 'commun.poele.extinction', icon: 'power', tone: 'cool' },
    stand_by: { label: 'commun.poele.veille', icon: 'moon', tone: 'good' },
    alarm: { label: 'commun.poele.alarme', icon: 'alert', tone: 'warm' },
    mem_alarm: { label: 'commun.poele.alarme_memorisee', icon: 'alert', tone: 'warm' },
  };
  const AIR_CONDITIONER = Object.keys(MODES).filter((key) => MODES[key].ac);
  /** A mode said in the house's language (an unknown one: as the unit says it). */
  const modeLabel = (key) => (MODES[key] ? t(MODES[key].label) : (key ?? '—'));
  // The buttons are the modes the unit declares, in this card's order (a
  // heater's wiring's), then any it alone knows; a unit that says none gets
  // an air conditioner's.
  const offered = $derived.by(() => {
    const declared = d?.points.find((p) => p.key === 'mode')?.kind.values;
    if (wiring) return wiring.modes.filter((key) => declared?.includes(key) || (key === 'off' && switched));
    if (!declared?.length) return AIR_CONDITIONER;
    return [...Object.keys(MODES).filter((key) => declared.includes(key)), ...declared.filter((key) => !MODES[key])];
  });
  const m = $derived(MODES[mode] ? { ...MODES[mode], label: modeLabel(mode) } : { label: mode ?? '—', icon: 'air-conditioner', tone: 'good' });

  // A 270° ring: the target as a bead, the room temperature in the middle.
  const R = 62;
  const C = 2 * Math.PI * R;
  const ARC = 0.75;
  const frac = (deg) => Math.min(1, Math.max(0, ((deg ?? MIN) - MIN) / (MAX - MIN)));
  const at = (deg) => {
    const a = (135 + frac(deg) * 270) * (Math.PI / 180);
    return { x: 80 + R * Math.cos(a), y: 80 + R * Math.sin(a) };
  };
  const bead = $derived(at(shownTarget));
  const here = $derived(typeof temp === 'number' ? at(temp) : null);
</script>

<!-- Its own width decides the layout: side by side, or stacked when narrow (a phone, a column). -->
<div class="frame">
<div class="clim" class:on data-tone={m.tone} class:unreachable={!reachable(id)}>
  <div
    class="dial"
    class:live={writable}
    class:dragging
    onpointerdown={grab}
    onpointermove={drag}
    onpointerup={release}
    onpointercancel={release}
    onkeydown={key}
    role={writable ? 'slider' : undefined}
    tabindex={writable ? 0 : undefined}
    aria-label={writable ? t('commun.clim.consigne_nom', { name }) : undefined}
    aria-valuemin={writable ? MIN : undefined}
    aria-valuemax={writable ? MAX : undefined}
    aria-valuenow={writable ? shownTarget : undefined}>
    <svg viewBox="0 0 160 160" aria-hidden="true">
      <circle class="track" cx="80" cy="80" r={R} stroke-dasharray="{C * ARC} {C}" transform="rotate(135 80 80)" />
      {#if on && shownTarget != null}
        <circle class="fill" cx="80" cy="80" r={R} stroke-dasharray="{C * ARC * frac(shownTarget)} {C}" transform="rotate(135 80 80)" />
      {/if}
      {#if here}
        <circle class="here" cx={here.x} cy={here.y} r="4.5"><title>{t('commun.clim.temp_piece')}</title></circle>
      {/if}
      {#if shownTarget != null}
        <circle class="bead" cx={bead.x} cy={bead.y} r={dragging ? 10 : 8} />
      {/if}
    </svg>
    <div class="reading">
      {#if dragging}
        <strong class="num">{num(wanted, wanted % 1 ? 1 : 0)}<small>°</small></strong>
        <span>{t('commun.clim.consigne')}</span>
      {:else}
        <strong class="num">{num(temp, 0)}<small>°</small></strong>
        {#if shownTarget != null}<span>{t('commun.clim.consigne_val', { temp: num(shownTarget, shownTarget % 1 ? 1 : 0) })}</span>{/if}
      {/if}
    </div>
  </div>
  <div class="facts">
    <h3>{name}</h3>
    {#if wirings.length}
      <label class="wiring">
        <span>{t('commun.clim.cablage')}</span>
        <select value={value(id, 'device_mode_type')} onchange={(e) => setWiring(e.currentTarget.value)} disabled={!reachable(id)}>
          {#each wirings as key (key)}
            <option value={key}>{t(WIRING[key].label)}</option>
          {/each}
        </select>
      </label>
    {/if}
    {#if controllable}
      <!-- A heater on a pilot wire is switched off from its modes: its chip only says. -->
      {#if switched}
        <span class="chip {on ? m.tone : ''}"><Icon name={on ? (m.icon ?? 'fire') : 'power'} size={16} />{on ? m.label : t('commun.clim.eteinte')}</span>
      {:else}
        <button class="chip power {on ? m.tone : ''}" onclick={power} aria-pressed={on}>
          <Icon name="power" size={16} />{on ? m.label : t('commun.clim.eteinte')}
        </button>
      {/if}
      {#if on && writable}
        <div class="setpoint" role="group" aria-label={t('commun.clim.consigne')}>
          <button onclick={() => nudge(-STEP)} aria-label={t('commun.clim.moins_chaud')} disabled={shownTarget <= MIN}>−</button>
          <b class="num">{num(shownTarget, shownTarget % 1 ? 1 : 0)}°</b>
          <button onclick={() => nudge(STEP)} aria-label={t('commun.clim.plus_chaud')} disabled={shownTarget >= MAX}>+</button>
        </div>
      {/if}
      {#if on || switched}
        <div class="modes" role="group" aria-label={t('commun.clim.mode')}>
          {#each offered as key (key)}
            <button class:active={active(key)} class:off={key === 'off'} onclick={() => setMode(key)} title={modeLabel(key)} aria-label={modeLabel(key)} aria-pressed={active(key)}>
              {#if MODES[key]?.text}<span class="num">{MODES[key].text}</span>{:else}<Icon name={MODES[key]?.icon ?? 'tune'} size={16} />{/if}
            </button>
          {/each}
        </div>
      {/if}
      {#if wiring}
        <ProgramWeek {id} wiring={value(id, 'device_mode_type')} {name} {modeLabel} modes={MODES} compact={!on || mode !== 'program'} />
      {/if}
    {:else}
      <span class="chip {on ? m.tone : ''}"><Icon name={on ? m.icon : 'power'} size={16} />{on ? m.label : t('commun.clim.eteinte')}</span>
    {/if}
    <!-- Outside, only from a unit that measures it (a heat pump's outdoor unit). -->
    {#if typeof outdoor === 'number' || (draw != null && draw >= 5)}
      <p class="muted">
        {#if typeof outdoor === 'number'}{t('commun.clim.dehors', { temp: num(outdoor, 0) })}{/if}
        {#if draw != null && draw >= 5}
          <span class="draw num" title={t('commun.clim.mesure_tableau', { name: feeder.name })}>
            {typeof outdoor === 'number' ? '· ' : ''}{watts(draw)}{#if shared}<small> {t('commun.clim.circuit_partage_paren')}</small>{/if}
          </span>
        {/if}
      </p>
    {/if}
    {#if energy.summary}
      <p class="costs">
        {#if feeder}
          <span>{t('commun.clim.aujourdhui')} <b class="num">{euros(dayCost, energy.summary.currency)}</b></span>
          <span>{t('commun.clim.ce_mois')} <b class="num">{euros(monthCost, energy.summary.currency)}</b></span>
          {#if shared}<small>{t('commun.clim.circuit_partage')}</small>{/if}
        {:else}
          <small>{t('commun.clim.cout_non_mesure')}</small>
        {/if}
      </p>
    {/if}
  </div>
</div>
</div>

<style>
  .costs {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 12px;
    font-size: 12.5px;
    color: var(--ink-2);
  }

  .costs b {
    font-weight: 650;
    color: var(--ink);
  }

  .costs small {
    color: var(--ink-3);
  }

  .draw {
    color: var(--warm-ink);
    font-weight: 650;
  }

  .draw small {
    font-weight: 500;
    color: var(--ink-3);
  }

  .frame {
    container-type: inline-size;
  }

  .clim {
    display: grid;
    grid-template-columns: 148px minmax(0, 1fr);
    align-items: center;
    gap: 14px;
    padding: 6px 4px;
  }

  /* Narrow: the dial above, the rest below, full width. */
  @container (max-width: 420px) {
    .clim {
      grid-template-columns: minmax(0, 1fr);
      justify-items: center;
    }

    .facts {
      justify-items: center;
      width: 100%;
    }
  }

  .dial {
    position: relative;
    width: 148px;
    height: 148px;
  }

  svg {
    width: 100%;
    height: 100%;
  }

  .track,
  .fill {
    fill: none;
    stroke-width: 12;
    stroke-linecap: round;
  }

  .track {
    stroke: var(--surface-3);
  }

  .fill {
    stroke: var(--tone);
    transition: stroke-dasharray 0.6s var(--ease);
  }

  .bead {
    fill: var(--surface);
    stroke: var(--ink-3);
    stroke-width: 4;
    transition: r 0.15s var(--ease);
    filter: drop-shadow(0 1px 2px rgb(0 0 0 / 25%));
  }

  .on .bead {
    stroke: var(--tone);
  }

  /* The room's temperature: a grey dot on the ring. */
  .here {
    fill: var(--ink-3);
    stroke: var(--surface);
    stroke-width: 2;
  }

  .dial.live {
    cursor: grab;
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
    border-radius: 50%;
  }

  .dial.dragging {
    cursor: grabbing;
  }

  .dial.live:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }

  .dial.dragging .reading strong {
    color: var(--tone);
  }

  .clim {
    --tone: var(--good);
  }

  .clim[data-tone='cool'] {
    --tone: var(--cool);
  }

  .clim[data-tone='warm'] {
    --tone: var(--warm);
  }

  .reading {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    text-align: center;
  }

  .reading strong {
    font-size: 40px;
    font-weight: 300;
    letter-spacing: -0.03em;
    line-height: 1;
  }

  .reading small {
    font-size: 22px;
    vertical-align: top;
  }

  .reading span {
    font-size: 12px;
    color: var(--ink-3);
    font-weight: 600;
    margin-top: 4px;
  }

  .facts {
    display: grid;
    gap: 8px;
    justify-items: start;
    min-width: 0;
  }

  h3 {
    font-size: 18px;
    font-weight: 700;
  }

  .wiring {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .wiring select {
    padding: 4px 8px;
    border: none;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  button.chip {
    border: none;
    font: inherit;
    cursor: pointer;
  }

  .setpoint {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .setpoint button,
  .modes button {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    border: none;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 20px;
    line-height: 1;
    cursor: pointer;
  }

  .setpoint button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .setpoint b {
    min-width: 44px;
    text-align: center;
    font-size: 18px;
  }

  .modes {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .modes button {
    width: 32px;
    height: 32px;
    color: var(--ink-3);
  }

  .modes button.active {
    background: var(--tone);
    color: #fff;
  }

  /* Off, the heater is no colour. */
  .modes button.off.active {
    background: var(--ink-2);
    color: var(--surface);
  }

  /* « −1 », « −2 »: as large as the icons beside them. */
  .modes button span {
    font-size: 14px;
    font-weight: 750;
    letter-spacing: -0.02em;
  }

  .unreachable {
    opacity: 0.5;
  }
</style>
