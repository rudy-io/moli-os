<script>
  import { hub, value, act, isOn, powerKey, protectedRoom, num, nameOf, deviceKind, lampsOf, doorOpen, leak, motion, lowBattery, unpowered, isFixture, note, reachable } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import LightTile from './LightTile.svelte';
  import ClimateCard from './ClimateCard.svelte';
  import CoverTile from './CoverTile.svelte';
  import AmbiancePicker from './AmbiancePicker.svelte';
  import { phase, PHASES } from '../lib/printers.js';

  /** One room, compact: its temperature and a switch for its lights in the
   *  head, its lights and plugs as small pads (a ⚙ opens a lamp's dimmer,
   *  colours, fan), its clims as one line each (a tap opens the dial), and
   *  what it senses as chips. `icons`: pads show an icon only (names on
   *  hover and for screen readers). `every`: each sensor shows, even
   *  silent (filtered view). */
  let { room, every = false, icons = false } = $props();

  const locked = $derived(protectedRoom(room.name));

  function chips(d) {
    // A silent sensor's last readings are old news: say it is silent.
    if (d.online === false && deviceKind(d) === 'sensor') {
      const dead = lowBattery(d.id);
      return [{ tone: dead ? 'alert' : '', icon: 'alert', text: t(dead ? 'commun.piece.pile_vide' : 'commun.piece.hors_ligne', { name: nameOf(d.id) }) }];
    }
    const out = [];
    const v = (k) => value(d.id, k);
    if (v('smoke') === true) out.push({ tone: 'alert', icon: 'fire', text: t('commun.capteur.fumee') });
    if (leak(d.id)) out.push({ tone: 'alert', icon: 'water', text: t('commun.piece.fuite', { name: nameOf(d.id) }) });
    const open = doorOpen(d.id);
    if (open === true) out.push({ tone: 'warm', icon: 'door-open', text: t('commun.piece.ouverte', { name: nameOf(d.id) }) });
    else if (open === false) out.push({ tone: '', icon: 'door-closed', text: t('commun.piece.fermee', { name: nameOf(d.id) }) });
    // The temperature is the room's, in the head.
    const hum = v('humidity') ?? v('humidity_value');
    if (typeof hum === 'number') out.push({ tone: '', icon: 'humidity', text: t('commun.pourcent', { n: num(hum) }) });
    const co2 = v('co2_value') ?? v('co2');
    if (typeof co2 === 'number') out.push({ tone: co2 > 1200 ? 'warm' : '', icon: 'leaf', text: `CO₂ ${num(co2)} ppm` });
    if (motion(d.id)) out.push({ tone: 'warm', icon: 'motion', text: t('commun.piece.presence') });
    if (lowBattery(d.id)) out.push({ tone: 'alert', icon: 'alert', text: t('commun.piece.pile_faible', { name: nameOf(d.id) }) });
    if (deviceKind(d) === 'camera') out.push({ tone: 'cool', icon: 'cctv', text: nameOf(d.id), href: '#/cameras' });
    if (deviceKind(d) === 'printer') {
      const st = phase(v('state'));
      const text = st === 'printing' || st === 'paused' ? `${PHASES[st].label.toLowerCase()} ${t('commun.pourcent', { n: Math.round(Number(v('progress') ?? 0)) })}` : PHASES[st].label.toLowerCase();
      out.push({ tone: PHASES[st].tone, icon: 'printer3d', text: `${nameOf(d.id)} · ${d.online === false ? t('commun.piece.imprimante_eteinte') : text}`, href: '#/impression' });
    }
    if (every && !out.length) {
      // Its first reading, so that a quiet sensor still shows it is there.
      const p = d.points.find((p) => p.access?.read !== false && value(d.id, p.key) != null && value(d.id, p.key) !== '');
      const x = p ? value(d.id, p.key) : null;
      const shown = typeof x === 'number' ? num(x, 1) : x === true ? t('commun.oui').toLowerCase() : x === false ? t('commun.non').toLowerCase() : x;
      out.push({ tone: '', icon: 'info', text: p ? t('commun.piece.lecture', { name: nameOf(d.id), label: p.label, value: `${shown}${p.unit ? ` ${p.unit}` : ''}` }) : nameOf(d.id) });
    }
    return out;
  }

  const sensorChips = $derived(room.sensors.flatMap(chips));
  const lamps = $derived(lampsOf(room));
  let showLooks = $state(false);

  // ---- the room's temperature ----------------------------------------------------------

  const temp = $derived.by(() => {
    const readings = [...room.sensors, ...room.climates]
      .filter((d) => d.online !== false)
      .map((d) => value(d.id, 'temperature') ?? value(d.id, 'temp_current'))
      .filter((x) => typeof x === 'number');
    return readings.length ? readings.reduce((a, b) => a + b, 0) / readings.length : null;
  });
  const tone = (deg) => (deg < 18 ? 'cold' : deg < 20 ? 'cool' : deg <= 24 ? 'good' : deg <= 27 ? 'warm' : 'hot');
  const gaugeAt = (deg) => Math.min(100, Math.max(0, ((deg - 14) / 16) * 100));

  // ---- the room's switch: its lights, never its TV or plugs (D12) -----------------------

  function roomPower() {
    const on = !room.lit;
    for (const d of room.lights) {
      if (isOn(d.id) === on || (on && unpowered(d.id) && !isFixture(d))) continue;
      const key = powerKey(d);
      if (key) act(`${d.id}/${key}`, typeof value(d.id, key) === 'string' ? (on ? 'ON' : 'OFF') : on, t(on ? 'commun.piece.label_allumer' : 'commun.piece.label_eteindre', { name: room.name }));
    }
  }

  // ---- pads ----------------------------------------------------------------------------

  const hasFan = (id) => hub.devices[id]?.points.some((p) => p.key === 'fan_switch');
  const fine = (d) => d.points.some((p) => ['brightness', 'color', 'color_temp'].includes(p.key) && p.access?.write);

  /** A speaking icon, from what it is and what it is called. */
  function padIcon(d) {
    const kind = deviceKind(d);
    const name = (d.label?.name || d.native_name || '').toLowerCase();
    if (kind === 'tv') return 'tv';
    if (kind === 'speaker') return 'speaker';
    if (kind === 'plug') return /vmc|ventil|\bfan\b|exhaust/.test(name) ? 'fan' : 'socket';
    if (hasFan(d.id) || (isFixture(d) && d.members?.some(hasFan))) return 'ceiling-fan-light';
    if (/spot/.test(name)) return 'spotlight';
    if (/meuble tv|télé|tele|tv unit|tv stand|television/.test(name)) return 'tv-light';
    if (/bureau|desk/.test(name)) return 'desk-lamp';
    if (/applique|sconce/.test(name)) return 'sconce';
    if (/ruban|bandeau|led|strip/.test(name)) return 'led-strip';
    if (/lampe|globe|fraise|lamp/.test(name)) return 'floor-lamp';
    if (/suspens|plafon|principal|cuisine|couloir|véranda|veranda|studio|combles|chambre|entrée|toilette|ceiling|pendant|\bmain\b|kitchen|hallway|bedroom|attic|bathroom|toilet/.test(name)) return 'ceiling-light';
    return 'light';
  }

  /** The pads: lights, then TV and speakers, then plugs. */
  const pads = $derived([
    ...room.lights.map((d) => ({ d, point: null })),
    ...room.media.map((d) => ({ d, point: deviceKind(d) === 'tv' ? 'power' : null, link: deviceKind(d) === 'speaker' ? '#/salon' : null })),
    ...room.plugs.map((d) => ({ d, point: null })),
  ]);

  let open = $state(null);

  function stateOf(d, point) {
    if (unpowered(d.id) && !isFixture(d)) return t('commun.piece.coupee');
    if (!reachable(d.id)) return t('commun.piece.injoignable');
    if (deviceKind(d) === 'speaker') return value(d.id, 'playing') === true ? t('commun.piece.lecture_en_cours') : t('commun.piece.a_l_arret');
    if (!isOn(d.id, point)) return t('commun.piece.eteinte');
    const b = value(d.id, 'brightness');
    return typeof b === 'number' && fine(d) ? t('commun.pourcent', { n: Math.round(b) }) : t('commun.piece.allumee');
  }

  function tap(d, point) {
    if (unpowered(d.id) && !isFixture(d)) {
      note(t('commun.lampe.coupee_note', { label: nameOf(d.id) }));
      return;
    }
    const key = point ?? powerKey(d);
    if (!key) return;
    const cur = value(d.id, key);
    act(`${d.id}/${key}`, typeof cur === 'string' ? (isOn(d.id, point) ? 'OFF' : 'ON') : !isOn(d.id, point), nameOf(d.id));
  }

  // The unit's modes (as it says them) → the catalogue key of their short name.
  const MODES = {
    froid: 'commun.piece.mode.froid',
    chaud: 'commun.piece.mode.chaud',
    auto: 'commun.piece.mode.auto',
    déshumidification: 'commun.piece.mode.deshumidification',
    ventilation: 'commun.piece.mode.ventilation',
  };
  let openClim = $state(null);
</script>

<section class="card room">
  <div class="head">
    <span class="room-icon" class:lit={room.lit}><Icon name={room.icon} size={22} /></span>
    <div class="title">
      <h2>{room.name}{#if locked}<span class="lock" title={t('commun.piece.protegee_titre')}><Icon name="lock" size={13} /></span>{/if}</h2>
      <span class="muted">{room.lit ? t('commun.piece.allumees', { count: room.lit }) : t('commun.piece.tout_eteint')}</span>
    </div>
    {#if temp != null}
      <span class="temp {tone(temp)}" title={t('commun.piece.temperature')}><b class="num">{num(temp, 1)}°</b><i class="gauge"><i style="left:{gaugeAt(temp)}%"></i></i></span>
    {/if}
    {#if lamps.length}
      <button class="round" class:open={showLooks} onclick={() => (showLooks = !showLooks)} aria-expanded={showLooks} title={t('commun.piece.ambiances')} aria-label={t('commun.piece.ambiances_de', { name: room.name })}>
        <Icon name="palette" size={17} />
      </button>
    {/if}
    {#if room.lights.length}
      <button class="room-switch" class:on={room.lit} onclick={roomPower} aria-pressed={!!room.lit} title={room.lit ? t('commun.piece.tout_eteindre') : t('commun.piece.tout_allumer')} aria-label={t(room.lit ? 'commun.piece.label_eteindre' : 'commun.piece.label_allumer', { name: room.name })}><i></i></button>
    {/if}
  </div>

  {#if showLooks}
    <AmbiancePicker lights={lamps} label={room.name} />
  {/if}

  {#if pads.length}
    <div class="pads" class:icons>
      {#each pads as { d, point, link } (d.id)}
        {@const lit = link ? value(d.id, 'playing') === true : isOn(d.id, point)}
        {@const cut = unpowered(d.id) && !isFixture(d)}
        {@const more = !link && !point && !cut && (fine(d) || hasFan(d.id) || (isFixture(d) && d.members?.some(hasFan)))}
        <div class="pad-wrap">
          {#if link}
            <a class="pad" class:on={lit} href={link} title={nameOf(d.id)} aria-label={t('commun.action', { label: nameOf(d.id), action: stateOf(d, point) })}>
              <span class="ico"><Icon name={padIcon(d)} size={icons ? 22 : 18} /></span>
              {#if !icons}<b>{nameOf(d.id)}</b><small>{stateOf(d, point)}</small>{/if}
            </a>
          {:else}
            <button class="pad" class:on={lit} class:cut class:off-grid={!reachable(d.id) && !cut} onclick={() => tap(d, point)} title={nameOf(d.id)} aria-pressed={lit} aria-label={t('commun.action', { label: nameOf(d.id), action: stateOf(d, point) })}>
              <span class="ico"><Icon name={padIcon(d)} size={icons ? 22 : 18} /></span>
              {#if !icons}<b>{nameOf(d.id)}</b><small>{stateOf(d, point)}</small>{/if}
            </button>
          {/if}
          {#if more}
            <button class="more" class:open={open === d.id} onclick={() => (open = open === d.id ? null : d.id)} aria-expanded={open === d.id} title={t('commun.piece.regler')} aria-label={t('commun.piece.regler_nom', { name: nameOf(d.id) })}>
              <Icon name="tune" size={13} />
            </button>
          {/if}
        </div>
      {/each}
    </div>
    {#if open && pads.some((p) => p.d.id === open)}
      <LightTile id={open} dimmer />
    {/if}
  {/if}

  {#each room.covers as d (d.id)}
    <CoverTile id={d.id} />
  {/each}

  {#each room.climates as d (d.id)}
    {@const on = isOn(d.id)}
    <button class="clim" class:on onclick={() => (openClim = openClim === d.id ? null : d.id)} aria-expanded={openClim === d.id}>
      <Icon name="air-conditioner" size={18} />
      <b>{nameOf(d.id)}</b>
      <span class="num">{num(value(d.id, 'temperature'), 0)}° → {num(value(d.id, 'target_temperature'), 1)}°</span>
      <span class="state">{on ? (MODES[value(d.id, 'mode')] ? t(MODES[value(d.id, 'mode')]) : t('commun.piece.en_marche')) : t('commun.piece.eteinte')}</span>
    </button>
    {#if openClim === d.id}
      <ClimateCard id={d.id} name={nameOf(d.id)} />
    {/if}
  {/each}

  {#if sensorChips.length}
    <div class="chips">
      {#each sensorChips as c, i (i)}
        {#if c.href}
          <a class="chip {c.tone}" href={c.href}><Icon name={c.icon} size={15} />{c.text}</a>
        {:else}
          <span class="chip {c.tone}"><Icon name={c.icon} size={15} />{c.text}</span>
        {/if}
      {/each}
    </div>
  {/if}
</section>

<style>
  .room {
    display: grid;
    gap: 12px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .room-icon {
    flex: none;
    width: 40px;
    height: 40px;
    border-radius: 14px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-3);
  }

  .room-icon.lit {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .title {
    flex: 1;
    display: grid;
    min-width: 0;
  }

  .title h2 {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 17px;
    font-weight: 700;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .lock {
    display: inline-grid;
    color: var(--cool);
  }

  .title span {
    font-size: 12.5px;
  }

  /* The room's temperature, with a cold-to-hot gauge. */
  .temp {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 9px;
    border-radius: 999px;
    background: var(--surface-2);
    font-size: 13px;
  }

  .temp b {
    font-weight: 700;
  }

  .gauge {
    position: relative;
    width: 28px;
    height: 5px;
    border-radius: 3px;
    background: linear-gradient(90deg, #4f8cff 0%, #3fc1d9 25%, #4cc26a 40%, #4cc26a 62%, #ffb340 80%, #ff5a3c 100%);
  }

  .gauge i {
    position: absolute;
    top: -3px;
    width: 3px;
    height: 11px;
    border-radius: 2px;
    background: var(--ink);
    box-shadow: 0 0 0 1.5px var(--surface-2);
    transform: translateX(-50%);
  }

  .cold b {
    color: #3b7bea;
  }

  .cool b {
    color: #2aa5bd;
  }

  .good b {
    color: #2f9e55;
  }

  .warm b {
    color: #e08a00;
  }

  .hot b {
    color: #e5482b;
  }

  .round {
    flex: none;
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

  .round.open {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  /* The room's lights, all at once: a small rocker. */
  .room-switch {
    flex: none;
    position: relative;
    width: 42px;
    height: 26px;
    border: none;
    border-radius: 999px;
    background: var(--surface-3);
    cursor: pointer;
    transition: background 0.25s var(--ease);
  }

  .room-switch i {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 0.25s var(--ease);
  }

  .room-switch.on {
    background: var(--warm);
  }

  .room-switch.on i {
    transform: translateX(16px);
  }

  /* Pads: small, side by side. */
  .pads {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(98px, 1fr));
    gap: 8px;
  }

  .pads.icons {
    grid-template-columns: repeat(auto-fill, minmax(54px, 1fr));
  }

  .pad-wrap {
    position: relative;
    min-width: 0;
  }

  .pad {
    box-sizing: border-box;
    width: 100%;
    height: 100%;
    min-height: 74px;
    display: grid;
    align-content: start;
    gap: 3px;
    padding: 10px;
    border: none;
    border-radius: var(--r-md);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    text-align: left;
    text-decoration: none;
    cursor: pointer;
    transition: background 0.3s var(--ease), box-shadow 0.3s var(--ease);
  }

  .icons .pad {
    min-height: 54px;
    padding: 0;
    place-items: center;
    align-content: center;
  }

  .pad.on {
    background: linear-gradient(155deg, var(--warm-soft) 0%, var(--surface) 85%);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--warm) 30%, transparent);
  }

  .pad.cut,
  .pad.off-grid {
    opacity: 0.45;
  }

  .ico {
    width: 30px;
    height: 30px;
    margin-bottom: 3px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-3);
  }

  .icons .ico {
    width: 38px;
    height: 38px;
    margin: 0;
  }

  .pad.on .ico {
    background: var(--warm);
    color: #fff;
  }

  .pad b {
    font-size: 13px;
    font-weight: 650;
    line-height: 1.2;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: anywhere;
  }

  .pad small {
    font-size: 11.5px;
    color: var(--ink-3);
  }

  .pad.on small {
    color: var(--warm-ink);
  }

  .more {
    position: absolute;
    top: 6px;
    right: 6px;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border: none;
    border-radius: 50%;
    background: var(--surface);
    color: var(--ink-3);
    cursor: pointer;
  }

  .more.open {
    background: var(--warm);
    color: #fff;
  }

  /* A clim, on one line. */
  .clim {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 10px 14px;
    border: none;
    border-radius: var(--r-md);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 14px;
    text-align: left;
    cursor: pointer;
  }

  .clim b {
    flex: 1;
    font-weight: 650;
  }

  .clim .state {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink-3);
  }

  .clim.on .state {
    color: var(--cool);
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .chips a {
    text-decoration: none;
  }
</style>
