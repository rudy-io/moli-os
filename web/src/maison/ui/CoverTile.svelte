<script>
  import { hub, device, value, act, nameOf, num, coverIcon, isBlind } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** A shutter, a blind or a roof window: open, stop, close (whatever the
   *  device offers), and a bar where a finger asks for a level directly. */
  let { id, name = null } = $props();

  const d = $derived(device(id));
  const label = $derived(nameOf(id, name));
  const pos = $derived(value(id, 'percent_control'));
  const moving = $derived(value(id, 'moving') === true);
  // Where it is heading, as the device says it (whoever asked).
  const target = $derived(value(id, 'target_position'));
  const offers = $derived(d?.points.find((p) => p.key === 'control')?.kind.values ?? []);
  const settable = $derived(!!d?.points.some((p) => p.key === 'percent_control' && p.access?.write));
  // [command sent to the device, catalogue key of its button, its icon on a phone]
  const BUTTONS = [
    ['open', 'commun.volet.ouvrir', 'chevron-up'],
    ['stop', 'commun.volet.stop', 'stop'],
    ['close', 'commun.volet.fermer', 'chevron-down'],
  ];

  // ---- the bar: a tap or a slide, sent when the finger lifts -----------------------------

  // A blind's bar runs the other way: rolled up (open) on the left, all the
  // way down (closed) on the right. A roof window stops at 10 % to air the
  // room: 5 % asked is 10 %, and the way down from 10 % goes to closed.
  const blind = $derived(!!d && isBlind(d));
  const vents = $derived(!!d && coverIcon(d) === 'roof-window');
  const VENT = 10;
  /** Where an opening sits on the bar (and back: it is its own inverse). */
  const onBar = (pct) => (blind ? 100 - pct : pct);
  /** `dir`: which way the opening was going (+1 more open, −1 less). */
  const settle = (pct, dir) => (vents && pct > 0 && pct < VENT ? (dir < 0 ? 0 : VENT) : pct);

  const POINT = $derived(`${id}/percent_control`);
  const STEP = 5;
  let bar = $state(null);
  /** The opening under the finger, while it is down. */
  let drag = $state(null);
  const asked = $derived(hub.pending[POINT]);
  const shown = $derived(drag ?? (typeof asked === 'number' ? asked : pos) ?? 0);

  function at(e) {
    // The knob's centre runs from one end to the other, half a knob in (the
    // bar is as high as the knob's room).
    const r = bar.getBoundingClientRect();
    const x = Math.min(1, Math.max(0, (e.clientX - r.left - r.height / 2) / (r.width - r.height)));
    return settle(onBar(Math.round((x * 100) / STEP) * STEP), 1);
  }
  function down(e) {
    if (!settable || e.button > 0) return;
    bar.setPointerCapture(e.pointerId);
    drag = at(e);
  }
  function move(e) {
    if (drag != null && bar.hasPointerCapture(e.pointerId)) drag = at(e);
  }
  function up() {
    if (drag == null) return;
    const v = drag;
    drag = null;
    send(v);
  }
  // A vertical swipe scrolls the page: nothing is sent.
  const cancel = () => (drag = null);
  function key(e) {
    const by = { ArrowLeft: -STEP, ArrowDown: -STEP, ArrowRight: STEP, ArrowUp: STEP, Home: -100, End: 100 }[e.key];
    if (by == null) return;
    e.preventDefault();
    const next = onBar(Math.min(100, Math.max(0, Math.round((onBar(shown) + by) / STEP) * STEP)));
    send(settle(next, Math.sign(next - shown)));
  }
  function send(v) {
    if (v === pos && !moving) return;
    act(POINT, v, t('commun.volet.label_niveau', { label, state: openness(v) }));
  }

  /** Closed, open, airing (a roof window at 10 %), or open so much. */
  const openness = (pct) =>
    pct === 0
      ? t('commun.volet.ferme')
      : pct === 100
        ? t('commun.volet.ouvert')
        : vents && pct === VENT
          ? t('commun.volet.ventilation')
          : t('commun.volet.ouvert_a', { pct: num(pct) });
  /** On its way: opening or closing all the way, or heading somewhere. */
  function heading(goal) {
    if (goal === 100) return t('commun.volet.ouverture_en_cours');
    if (goal === 0) return t('commun.volet.fermeture_en_cours');
    return t('commun.volet.en_mouvement', { state: openness(goal) });
  }
  const status = $derived(
    drag != null
      ? t('commun.volet.vers', { state: openness(drag) })
      : moving
        ? heading(target ?? (typeof asked === 'number' ? asked : pos))
        : pos != null
          ? openness(pos)
          : '',
  );
</script>

<div class="cover">
  <div class="top">
    <span class="ico" class:open={pos > 0}><Icon name={d ? coverIcon(d) : 'shutter'} size={18} /></span>
    <span class="name"><b>{label}</b><small class:live={drag != null || moving}>{status}</small></span>
    {#if offers.length}
      <div class="seg">
        {#each BUTTONS.filter(([cmd]) => offers.includes(cmd)) as [cmd, key, icon] (cmd)}
          <button onclick={() => act(`${id}/control`, cmd, t('commun.action', { label, action: t(key).toLowerCase() }))} title={t(key)} aria-label={t('commun.action', { label, action: t(key).toLowerCase() })}>
            <span class="word">{t(key)}</span><span class="glyph"><Icon name={icon} size={18} /></span>
          </button>
        {/each}
      </div>
    {/if}
  </div>
  {#if settable}
    <div
      class="bar"
      class:held={drag != null}
      bind:this={bar}
      style:--at={onBar(shown) / 100}
      role="slider"
      tabindex="0"
      aria-label={t('commun.volet.niveau', { label })}
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={onBar(shown)}
      aria-valuetext={openness(shown)}
      onpointerdown={down}
      onpointermove={move}
      onpointerup={up}
      onpointercancel={cancel}
      onkeydown={key}>
      <i class="fill"></i>
      {#if vents}<i class="vent" style:--at={VENT / 100} title={t('commun.volet.ventilation')}></i>{/if}
      <i class="knob"></i>
    </div>
    <div class="ends" aria-hidden="true">
      <span>{t(blind ? 'commun.volet.ouvert' : 'commun.volet.ferme')}</span><span>{t(blind ? 'commun.volet.ferme' : 'commun.volet.ouvert')}</span>
    </div>
  {/if}
</div>

<style>
  .cover {
    display: grid;
    gap: 8px;
    padding: 12px 14px;
    background: var(--surface-2);
    border-radius: var(--r-md);
    color: var(--ink-2);
    container-type: inline-size;
  }

  .top {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .ico {
    flex: none;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-3);
  }

  .ico.open {
    background: var(--cool-soft);
    color: var(--cool);
  }

  /* A long name takes two lines, never the buttons' room. */
  .name {
    flex: 1;
    min-width: 0;
    display: grid;
  }

  .name b {
    font-weight: 650;
    color: var(--ink);
    line-height: 1.2;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: anywhere;
  }

  small {
    font-size: 12.5px;
    font-weight: 500;
    color: var(--ink-3);
  }

  small.live {
    color: var(--cool);
  }

  /* Open, stop, close: one pill, three parts. */
  .seg {
    flex: none;
    display: flex;
    background: var(--surface);
    border-radius: 999px;
    box-shadow: var(--shadow);
  }

  .seg button {
    display: grid;
    place-items: center;
    border: 0;
    background: none;
    color: var(--ink-2);
    padding: 8px 12px;
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
  }

  .glyph {
    display: none;
  }

  .seg button + button {
    border-left: 1px solid var(--line);
  }

  .seg button:active {
    color: var(--cool);
  }

  /* The bar: closed on the left, open on the right (a blind the other
     way: rolled up on the left, down on the right). As high as the knob's
     room (--room): a thick rail for a finger, a fine one under a mouse. */
  .bar {
    --rail: 30px;
    --knob: 24px;
    --room: max(var(--rail), var(--knob));
    position: relative;
    height: var(--room);
    cursor: pointer;
    touch-action: pan-y;
    user-select: none;
    -webkit-user-select: none;
  }

  .bar::before,
  .fill {
    position: absolute;
    top: 50%;
    left: 0;
    height: var(--rail);
    border-radius: 999px;
    transform: translateY(-50%);
  }

  .bar::before {
    content: '';
    right: 0;
    background: var(--surface-3);
  }

  .bar:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 2px;
    border-radius: 999px;
  }

  .fill {
    width: calc(var(--room) + (100% - var(--room)) * var(--at));
    background: color-mix(in srgb, var(--cool) 30%, var(--surface));
    transition: width 0.25s var(--ease);
  }

  .knob {
    position: absolute;
    top: 50%;
    left: calc((var(--room) - var(--knob)) / 2 + (100% - var(--room)) * var(--at));
    width: var(--knob);
    height: var(--knob);
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 4px rgb(0 0 0 / 25%);
    transform: translateY(-50%);
    transition: left 0.25s var(--ease);
  }

  /* A roof window's airing stop, 10 % along: a notch across the rail. */
  .vent {
    position: absolute;
    top: 50%;
    left: calc(var(--room) / 2 - 1px + (100% - var(--room)) * var(--at));
    width: 2px;
    height: max(12px, var(--rail) * 0.6);
    border-radius: 1px;
    background: var(--ink-3);
    opacity: 0.6;
    transform: translateY(-50%);
  }

  /* A mouse: a fine rail, the knob standing out of it. */
  @media (hover: hover) and (pointer: fine) {
    .bar {
      --rail: 8px;
      --knob: 18px;
    }

    .fill {
      background: var(--cool);
      opacity: 0.55;
    }
  }

  /* Under the finger, no lag. */
  .held .fill,
  .held .knob {
    transition: none;
  }

  .ends {
    display: flex;
    justify-content: space-between;
    margin-top: -4px;
    padding: 0 6px;
    font-size: 11px;
    color: var(--ink-3);
  }

  /* A narrow tile (a phone, a room's card): arrows for buttons, the
     name keeps its room and the line never breaks. */
  @container (max-width: 420px) {
    .seg button {
      padding: 6px 9px;
    }

    .word {
      display: none;
    }

    .glyph {
      display: block;
    }

    .top {
      gap: 8px;
    }
  }
</style>
