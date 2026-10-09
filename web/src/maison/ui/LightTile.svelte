<script>
  import { device, isOn, pending, reachable, toggle, value, act, nameOf, setLook, note, unpowered, isFixture, powerKey } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import LightTile from './LightTile.svelte';

  /** A light (or any on/off thing): tap to switch. With `dimmer`, dimmable
   *  lights show their level and a slider, colour lamps a palette (whites
   *  by temperature, colours as « #rrggbb »). A room's tile (`members`:
   *  its lamps) offers the palette when one of them takes it. */
  // `also`: other lights switched with this one (one tile for the kitchen
  // and its corridor), on when any of them is.
  let { id, name = null, icon = 'light', dimmer = false, compact = false, point = null, members = null, also = [] } = $props();

  const d = $derived(device(id));
  const ok = $derived(reachable(id) || also.some(reachable));
  const on = $derived((reachable(id) && isOn(id, point)) || also.some((a) => reachable(a) && isOn(a)));
  const busy = $derived(pending(id, point));
  const label = $derived(nameOf(id, name));
  const level = $derived(value(id, 'brightness'));
  const dimmable = $derived(dimmer && d?.points.some((p) => p.key === 'brightness' && p.access?.write));

  // A bulb cut at the wall switch: off, and only the switch brings it back.
  const cut = $derived(!!d && !isFixture(d) && unpowered(id));
  const state = $derived(
    !d
      ? t('commun.lampe.introuvable')
      : cut
        ? t('commun.lampe.coupee')
        : !ok
          ? t('commun.lampe.injoignable')
          : busy
            ? '…'
            : on
              ? level != null && dimmable
                ? t('commun.pourcent', { n: Math.round(level) })
                : t('commun.lampe.allumee')
              : t('commun.lampe.eteinte'),
  );

  function tap() {
    if (cut && !also.length) {
      note(t('commun.lampe.coupee_note', { label }));
      return;
    }
    if (!also.length) return toggle(id, label, point);
    // Together: all on, or (one is on) all off.
    for (const a of [id, ...also]) {
      const key = powerKey(device(a));
      if (!key || !reachable(a)) continue;
      act(`${a}/${key}`, typeof value(a, key) === 'string' ? (on ? 'OFF' : 'ON') : !on, label);
    }
  }

  // A fixture's bulbs one by one (a colour each), its relay left out.
  const bulbs = $derived(
    dimmer && isFixture(d)
      ? (d.members ?? []).filter((m) => device(m)?.points.some((p) => ['brightness', 'color', 'color_temp'].includes(p.key)))
      : [],
  );
  let apart = $state(false);

  // A ceiling fan in the same device (or in a fixture's): on/off and speed,
  // whether the light is on or not.
  const hasFan = (m) => device(m)?.points.some((p) => p.key === 'fan_switch' && p.access?.write);
  const fanId = $derived(!d ? null : isFixture(d) ? ((d.members ?? []).find(hasFan) ?? null) : hasFan(id) ? id : null);
  const fanOn = $derived(fanId ? value(fanId, 'fan_switch') === true : false);
  const fanSpeed = $derived(fanId ? value(fanId, 'fan_speed') : null);
  const fanSpec = $derived(fanId ? device(fanId)?.points.find((p) => p.key === 'fan_speed')?.kind : null);
  function fanSet(key, v) {
    if (!reachable(fanId)) {
      note(t('commun.lampe.ventilo_hors_tension'));
      return;
    }
    act(`${fanId}/${key}`, v, t('commun.lampe.label_ventilateur', { label }));
  }

  let dragging = $state(null);
  function setLevel(e) {
    dragging = Number(e.currentTarget.value);
  }
  function commitLevel(e) {
    const v = Number(e.currentTarget.value);
    dragging = null;
    act(`${id}/brightness`, v, label);
  }

  // Colour (« #rrggbb ») and white temperature, where the lamp takes them.
  const writable = (key) => (dimmer ? d?.points.find((p) => p.key === key && p.access?.write) : null);
  const colorPoint = $derived(writable('color'));
  const tempPoint = $derived(writable('color_temp'));
  const anyMember = (key) =>
    dimmer && (members ?? []).some((m) => device(m)?.points.some((p) => p.key === key && p.access?.write));
  const canColor = $derived(!!colorPoint || anyMember('color'));
  const canWhite = $derived(!!tempPoint || anyMember('color_temp'));
  const hex = $derived.by(() => {
    const v = value(id, 'color');
    return typeof v === 'string' && /^#[0-9a-f]{6}$/i.test(v) ? v : null;
  });
  // No temperature means colour mode (Hue): then the bulb wears the colour.
  const tint = $derived(on && hex && value(id, 'color_temp') == null ? hex : null);
  const tintInk = $derived.by(() => {
    if (!tint) return null;
    const [r, g, b] = [1, 3, 5].map((i) => parseInt(tint.slice(i, i + 2), 16) / 255);
    return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.6 ? '#3a2a10' : '#fff';
  });

  // [catalogue key of its name, kelvin, look]; and [key, colour sent to the lamp].
  const WHITES = [
    ['commun.lampe.blanc_chaud', 2700, '#ffc58a'],
    ['commun.lampe.blanc_neutre', 4000, '#ffe9d2'],
    ['commun.lampe.blanc_froid', 6500, '#eef3ff'],
  ];
  const COLORS = [
    ['commun.lampe.couleur_rouge', '#ff1a1a'],
    ['commun.lampe.couleur_orange', '#ff7a00'],
    ['commun.lampe.couleur_jaune', '#ffd000'],
    ['commun.lampe.couleur_vert', '#16d94a'],
    ['commun.lampe.couleur_cyan', '#00d0ff'],
    ['commun.lampe.couleur_bleu', '#1f4dff'],
    ['commun.lampe.couleur_violet', '#8a2be2'],
    ['commun.lampe.couleur_rose', '#ff2d9a'],
  ];

  let palette = $state(false);

  function white(kelvin) {
    if (!tempPoint) return setLook(members, { white: kelvin }, label);
    const kind = tempPoint.kind ?? {};
    // Hue counts in mireds, others in kelvins.
    let v = tempPoint.unit === 'mired' || (kind.max ?? 0) <= 1000 ? Math.round(1e6 / kelvin) : kelvin;
    if (kind.min != null) v = Math.max(kind.min, v);
    if (kind.max != null) v = Math.min(kind.max, v);
    act(`${id}/color_temp`, v, label);
  }

  function paint(c) {
    if (!colorPoint) return setLook(members, { color: c.toLowerCase() }, label);
    act(`${id}/color`, c.toLowerCase(), label);
  }
</script>

<div class="tile" class:on class:busy class:off-grid={!ok || !d || cut} class:compact>
  <button class="hit" onclick={tap} disabled={!d || (!ok && !cut)} aria-pressed={on} aria-label={t('commun.action', { label, action: state })}>
    <span class="bulb" style:background={tint} style:color={tintInk} style:--glow={tint && `${tint}66`}><Icon name={icon} size={compact ? 22 : 26} /></span>
    <span class="text">
      <b>{label}</b>
      <small>{state}</small>
    </span>
    <span class="switch" aria-hidden="true"><i></i></span>
  </button>
  {#if on && (dimmable || canColor || canWhite)}
    <div class="controls">
      {#if dimmable}
        <input
          class="level"
          type="range"
          min="1"
          max="100"
          value={dragging ?? level ?? 100}
          oninput={setLevel}
          onchange={commitLevel}
          aria-label={t('commun.lampe.luminosite', { label })} />
      {/if}
      {#if bulbs.length > 1}
        <button class="apart" class:open={apart} onclick={() => (apart = !apart)} aria-expanded={apart} title={t('commun.lampe.ampoule_par_ampoule')} aria-label={t('commun.action', { label, action: t('commun.lampe.ampoule_par_ampoule').toLowerCase() })}>
          <Icon name="light-group" size={18} />
        </button>
      {/if}
      {#if canColor || canWhite}
        <button
          class="swatch-toggle"
          class:open={palette}
          style:--current={hex}
          onclick={() => (palette = !palette)}
          aria-expanded={palette}
          aria-label={t('commun.lampe.couleur', { label })}><i></i></button>
      {/if}
    </div>
    {#if palette}
      <div class="palette" role="group" aria-label={t('commun.lampe.couleurs', { label })}>
        {#if canWhite}
          {#each WHITES as [key, kelvin, look] (kelvin)}
            <button class="dot white" style:background={look} onclick={() => white(kelvin)} title={t(key)} aria-label={t(key)}></button>
          {/each}
        {/if}
        {#if canColor}
          {#each COLORS as [key, c] (c)}
            <button class="dot" style:background={c} onclick={() => paint(c)} title={t(key)} aria-label={t(key)}></button>
          {/each}
          <label class="dot other" title={t('commun.lampe.autre_couleur')}>
            <input type="color" value={hex ?? '#ffffff'} onchange={(e) => paint(e.currentTarget.value)} aria-label={t('commun.lampe.autre_couleur')} />
          </label>
        {/if}
      </div>
    {/if}
    {#if apart}
      <div class="bulbs">
        {#each bulbs as m (m)}
          <LightTile id={m} compact dimmer />
        {/each}
      </div>
    {/if}
  {/if}
  {#if fanId}
    <div class="fan" class:spin={fanOn}>
      <button class="fan-toggle" class:on={fanOn} onclick={() => fanSet('fan_switch', !fanOn)} aria-pressed={fanOn} aria-label={fanOn ? t('commun.lampe.ventilo_marche') : t('commun.lampe.ventilo_arret')}>
        <Icon name="fan" size={18} /><span>{t('commun.lampe.ventilateur')}</span>
      </button>
      {#if fanOn && typeof fanSpeed === 'number'}
        <div class="speed" role="group" aria-label={t('commun.lampe.vitesse')}>
          <button onclick={() => fanSet('fan_speed', fanSpeed - 1)} disabled={fanSpeed <= (fanSpec?.min ?? 1)} aria-label={t('commun.lampe.moins_vite')}>−</button>
          <b class="num">{fanSpeed}</b>
          <button onclick={() => fanSet('fan_speed', fanSpeed + 1)} disabled={fanSpeed >= (fanSpec?.max ?? 6)} aria-label={t('commun.lampe.plus_vite')}>+</button>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .fan {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    margin: -6px 16px 14px;
  }

  .fan-toggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border: none;
    border-radius: 999px;
    background: var(--surface-3);
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .fan-toggle.on {
    background: var(--cool);
    color: #fff;
  }

  .spin .fan-toggle :global(svg) {
    animation: spin 1.6s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .spin .fan-toggle :global(svg) {
      animation: none;
    }
  }

  .speed {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .speed button {
    width: 30px;
    height: 30px;
    border: none;
    border-radius: 50%;
    background: var(--surface-3);
    color: var(--ink);
    font: inherit;
    font-size: 18px;
    cursor: pointer;
  }

  .speed button:disabled {
    opacity: 0.35;
  }

  .apart {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border: none;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink-2);
    cursor: pointer;
  }

  .apart.open {
    background: var(--warm);
    color: #fff;
  }

  .bulbs {
    display: grid;
    gap: 8px;
    padding: 0 12px 12px;
  }

  .tile {
    position: relative;
    border-radius: var(--r-md);
    background: var(--surface-2);
    transition: background 0.35s var(--ease), box-shadow 0.35s var(--ease);
    overflow: hidden;
  }

  .tile.on {
    background: linear-gradient(155deg, var(--warm-soft) 0%, var(--surface) 70%);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--warm) 25%, transparent);
  }

  .hit {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    min-height: 112px;
    padding: 16px;
    display: grid;
    grid-template-columns: auto 1fr;
    grid-template-rows: auto 1fr;
    gap: 10px 12px;
    cursor: pointer;
  }

  .compact .hit {
    min-height: 76px;
    grid-template-columns: auto 1fr auto;
    grid-template-rows: auto;
    align-items: center;
  }

  .hit:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: -3px;
    border-radius: var(--r-md);
  }

  .bulb {
    width: 46px;
    height: 46px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-3);
    transition: all 0.35s var(--ease);
  }

  .compact .bulb {
    width: 40px;
    height: 40px;
  }

  .on .bulb {
    background: var(--warm);
    color: #fff;
    box-shadow: 0 0 0 6px var(--glow, var(--warm-glow)), 0 6px 22px var(--glow, var(--warm-glow));
  }

  .text {
    grid-column: 1 / -1;
    align-self: end;
    display: grid;
    gap: 2px;
  }

  .compact .text {
    grid-column: auto;
    align-self: center;
  }

  .text b {
    font-weight: 650;
    font-size: 16px;
  }

  .text small {
    color: var(--ink-3);
    font-size: 13px;
    font-weight: 550;
  }

  .on .text small {
    color: var(--warm-ink);
  }

  /* A small rocker, top right. */
  .switch {
    position: absolute;
    top: 18px;
    right: 16px;
    width: 40px;
    height: 24px;
    border-radius: 999px;
    background: var(--surface-3);
    transition: background 0.3s var(--ease);
  }

  .compact .switch {
    position: relative;
    top: auto;
    right: auto;
  }

  .switch i {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 0.3s var(--ease);
  }

  .on .switch {
    background: var(--warm);
  }

  .on .switch i {
    transform: translateX(16px);
  }

  .busy .bulb {
    animation: pulse 1s ease-in-out infinite;
  }

  @keyframes pulse {
    50% {
      opacity: 0.55;
    }
  }

  .off-grid {
    opacity: 0.55;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 12px;
    margin: -6px 16px 16px;
  }

  .level {
    flex: 1;
    min-width: 0;
    margin: 0;
    accent-color: var(--warm);
    height: 28px;
  }

  /* The current colour, ringed by the hue wheel. */
  .swatch-toggle {
    all: unset;
    box-sizing: border-box;
    flex: none;
    margin-left: auto;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    padding: 4px;
    background: conic-gradient(#ff1a1a, #ffd000, #16d94a, #00d0ff, #1f4dff, #8a2be2, #ff2d9a, #ff1a1a);
    cursor: pointer;
    transition: transform 0.2s var(--ease);
  }

  .swatch-toggle i {
    display: block;
    width: 100%;
    height: 100%;
    border-radius: 50%;
    background: var(--current, var(--surface));
    box-shadow: 0 0 0 2px var(--surface);
  }

  .swatch-toggle.open {
    transform: scale(1.1);
  }

  .swatch-toggle:focus-visible,
  .dot:focus-visible,
  .other:focus-within {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }

  .palette {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    padding: 0 16px 16px;
  }

  .dot {
    all: unset;
    box-sizing: border-box;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    cursor: pointer;
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 12%);
    transition: transform 0.15s var(--ease);
  }

  .dot:hover {
    transform: scale(1.12);
  }

  .dot.white {
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 18%);
  }

  .other {
    position: relative;
    background: conic-gradient(#ff1a1a, #ffd000, #16d94a, #00d0ff, #1f4dff, #8a2be2, #ff2d9a, #ff1a1a);
    overflow: hidden;
  }

  .other input {
    position: absolute;
    inset: 0;
    opacity: 0;
    width: 100%;
    height: 100%;
    cursor: pointer;
    border: 0;
    padding: 0;
  }
</style>
