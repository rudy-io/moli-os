<script>
  import { onMount, onDestroy } from 'svelte';
  import { device, value, reachable, num, act } from '../lib/home.svelte.js';
  import { energy, useEnergy, watts, euros } from '../lib/energy-live.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { pointOf, canWrite, numericRange, quantize, writableChoices } from '../lib/capabilities.js';
  import { completionGuard } from '../lib/completion-guard.js';
  import { climateMode as modeStyle } from '../lib/climate.js';

  let { id, name } = $props();

  const d = $derived(device(id));
  const range = $derived(numericRange(d, 'target_temperature'));
  // Without advertised limits the dial remains a reading, never a command.
  const MIN = $derived(range?.min ?? 0);
  const MAX = $derived(range?.max ?? 1);
  const STEP = $derived(range?.step ?? 0.5);
  const modes = $derived(writableChoices(d, 'mode'));
  const fans = $derived(writableChoices(d, 'fan_speed'));
  const fan = $derived(value(id, 'fan_speed'));
  const onValue = $derived(value(id, 'on'));
  const on = $derived(value(id, 'on') === true);
  const temp = $derived(value(id, 'temperature'));
  const target = $derived(value(id, 'target_temperature'));
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
  const writable = $derived(!!range && canWrite(d, 'target_temperature'));

  // Taps on − / + and drags on the dial add up; one order leaves once the
  // finger rests (each order re-reads the unit and sends all its settings: no
  // machine-gunning it). Off, the unit keeps the new setpoint for later.
  let wanted = $state(null);
  let dragging = $state(false);
  let timer;
  let resetTimer;
  const completion = completionGuard();
  onDestroy(() => { completion.change(); clearTimeout(timer); clearTimeout(resetTimer); });
  const shownTarget = $derived(wanted ?? target);
  const clamp = (deg) => quantize(deg, range);

  function nudge(delta) {
    if (!writable || !reachable(id)) return;
    const base = wanted ?? target;
    if (!Number.isFinite(base)) return;
    completion.change();
    wanted = clamp(base + delta);
    send(900);
  }

  function send(after) {
    clearTimeout(timer);
    const forId = id;
    timer = setTimeout(() => {
      if (id === forId) flush();
      else wanted = null;
    }, after);
  }

  async function flush() {
    clearTimeout(timer);
    const v = wanted;
    if (!writable || !reachable(id) || v == null || v === target || clamp(v) !== v) {
      wanted = null;
      return;
    }
    const owns = completion.capture();
    const done = await act(`${id}/target_temperature`, v, t('commun.clim.label_consigne', { name }));
    if (!owns()) return;
    // Shown until the unit reports it (or gives up on it).
    if (done !== 'ok') wanted = null;
    else {
      clearTimeout(resetTimer);
      resetTimer = setTimeout(() => owns() && wanted === v && (wanted = null), 15_000);
    }
  }

  $effect(() => {
    if (wanted != null && !dragging && target === wanted) wanted = null;
  });

  $effect(() => {
    // A reused card must never send its previous device's pending gesture.
    void id;
    completion.change();
    clearTimeout(timer);
    clearTimeout(resetTimer);
    wanted = null;
    dragging = false;
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
    completion.change();
    dragging = true;
    clearTimeout(timer);
    e.currentTarget.setPointerCapture(e.pointerId);
    wanted = tempAt(e, e.currentTarget.getBoundingClientRect()) ?? shownTarget;
  }

  function drag(e) {
    if (!dragging) return;
    completion.change();
    wanted = tempAt(e, e.currentTarget.getBoundingClientRect()) ?? wanted;
  }

  function release() {
    if (!dragging) return;
    dragging = false;
    send(250);
  }

  function cancelDrag() {
    completion.change();
    dragging = false;
    wanted = null;
    clearTimeout(timer);
  }

  function key(e) {
    const delta = { ArrowUp: STEP, ArrowRight: STEP, ArrowDown: -STEP, ArrowLeft: -STEP }[e.key];
    if (!delta || !writable) return;
    e.preventDefault();
    nudge(delta);
  }

  async function power() {
    if (!canWrite(d, 'on') || typeof onValue !== 'boolean') return;
    const forId = id;
    const desired = !on;
    // A setpoint just dragged leaves first: the unit starts on it.
    if (desired && wanted != null) await flush();
    if (id === forId && canWrite(d, 'on')) act(`${forId}/on`, desired, t(desired ? 'commun.clim.label_marche' : 'commun.clim.label_arret', { name }));
  }
  const setMode = (key) => modes.includes(key) && act(`${id}/mode`, key, t('commun.action', { label: name, action: modeLabel(key).toLowerCase() }));

  /** A mode said in the house's language (an unknown one: as the unit says it). */
  const modeLabel = (key) => (modeStyle(key) ? t(modeStyle(key).label) : (key ?? '—'));
  const m = $derived(modeStyle(mode) ? { ...modeStyle(mode), label: modeLabel(mode) } : { label: mode ?? '—', icon: 'air-conditioner', tone: 'good' });

  // A 270° ring: the target as a bead, the room temperature in the middle.
  const R = 62;
  const C = 2 * Math.PI * R;
  const ARC = 0.75;
  const frac = (deg) => Math.min(1, Math.max(0, ((deg ?? MIN) - MIN) / (MAX - MIN)));
  const at = (deg) => {
    const a = (135 + frac(deg) * 270) * (Math.PI / 180);
    return { x: 80 + R * Math.cos(a), y: 80 + R * Math.sin(a) };
  };
  const bead = $derived(range && Number.isFinite(shownTarget) ? at(shownTarget) : null);
  const here = $derived(range && Number.isFinite(temp) ? at(temp) : null);
</script>

<div class="clim" class:on data-tone={m.tone} class:unreachable={!reachable(id)}>
  {#if pointOf(d, 'temperature') || pointOf(d, 'target_temperature')}
  <div
    class="dial"
    class:live={writable}
    class:dragging
    onpointerdown={grab}
    onpointermove={drag}
    onpointerup={release}
    onpointercancel={cancelDrag}
    onkeydown={key}
    role={writable ? 'slider' : undefined}
    tabindex={writable ? 0 : undefined}
    aria-label={writable ? t('commun.clim.consigne_nom', { name }) : undefined}
    aria-valuemin={writable ? MIN : undefined}
    aria-valuemax={writable ? MAX : undefined}
    aria-valuenow={writable ? shownTarget : undefined}>
    <svg viewBox="0 0 160 160" aria-hidden="true">
      <circle class="track" cx="80" cy="80" r={R} stroke-dasharray="{C * ARC} {C}" transform="rotate(135 80 80)" />
      {#if on && bead}
        <circle class="fill" cx="80" cy="80" r={R} stroke-dasharray="{C * ARC * frac(shownTarget)} {C}" transform="rotate(135 80 80)" />
      {/if}
      {#if here}
        <circle class="here" cx={here.x} cy={here.y} r="4.5"><title>{t('commun.clim.temp_piece')}</title></circle>
      {/if}
      {#if bead}
        <circle class="bead" cx={bead.x} cy={bead.y} r={dragging ? 10 : 8} />
      {/if}
    </svg>
    <div class="reading">
      {#if dragging}
        <strong class="num">{num(wanted, wanted % 1 ? 1 : 0)}<small>°</small></strong>
        <span>{t('commun.clim.consigne')}</span>
      {:else}
        <strong class="num">{num(temp, 1)}<small>°</small></strong>
        {#if shownTarget != null}<span>{t('commun.clim.consigne_val', { temp: num(shownTarget, shownTarget % 1 ? 1 : 0) })}</span>{/if}
      {/if}
    </div>
  </div>
  {/if}
  <div class="facts">
    <h3>{name}</h3>
    {#if canWrite(d, 'on')}
      <button class="chip power {on ? m.tone : ''}" onclick={power} aria-pressed={on} disabled={typeof onValue !== 'boolean'}>
        <Icon name="power" size={16} />{onValue === false ? t('commun.clim.eteinte') : m.label}
      </button>
    {:else}
      <span class="chip {on ? m.tone : ''}"><Icon name={on ? m.icon : 'power'} size={16} />{onValue === false ? t('commun.clim.eteinte') : m.label}</span>
    {/if}
      {#if writable && Number.isFinite(shownTarget)}
        <div class="setpoint" role="group" aria-label={t('commun.clim.consigne')}>
          <button onclick={() => nudge(-STEP)} aria-label={t('commun.clim.moins_chaud')} disabled={shownTarget <= MIN}>−</button>
          <b class="num">{num(shownTarget, shownTarget % 1 ? 1 : 0)}°</b>
          <button onclick={() => nudge(STEP)} aria-label={t('commun.clim.plus_chaud')} disabled={shownTarget >= MAX}>+</button>
        </div>
      {/if}
      {#if modes.length}
        <div class="modes" role="group" aria-label={t('commun.clim.mode')}>
          {#each modes as key (key)}
            <button class:active={mode === key} onclick={() => setMode(key)} title={modeLabel(key)} aria-label={modeLabel(key)} aria-pressed={mode === key}>
              <Icon name={modeStyle(key)?.icon ?? 'air-conditioner'} size={16} />
              {#if !modeStyle(key)}<span>{modeLabel(key)}</span>{/if}
            </button>
          {/each}
        </div>
      {/if}
      {#if fans.length}
        <label>
          {t('commun.clim.ventilation')}
          <select value={fan ?? ''} onchange={(e) => fans.includes(e.currentTarget.value) && act(`${id}/fan_speed`, e.currentTarget.value, t('commun.clim.ventilation'))}>
            {#if fan == null}<option value="" disabled>—</option>{/if}
            {#each fans as speed (speed)}<option value={speed}>{speed === '0' || speed === 'auto' ? t('commun.clim.mode.auto') : speed}</option>{/each}
          </select>
        </label>
      {/if}
    {#if pointOf(d, 'outdoor_temperature') || (draw != null && draw >= 5)}
    <p class="muted">
      {#if pointOf(d, 'outdoor_temperature')}{t('commun.clim.dehors', { temp: num(outdoor, 0) })}{/if}
      {#if draw != null && draw >= 5}
        <span class="draw num" title={t('commun.clim.mesure_tableau', { name: feeder.name })}>
          {#if pointOf(d, 'outdoor_temperature')}· {/if}{watts(draw)}{#if shared}<small> {t('commun.clim.circuit_partage_paren')}</small>{/if}
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

  .clim {
    display: grid;
    grid-template-columns: 148px 1fr;
    align-items: center;
    gap: 14px;
    padding: 6px 4px;
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
  }

  h3 {
    font-size: 18px;
    font-weight: 700;
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

  .unreachable {
    opacity: 0.5;
  }
</style>
