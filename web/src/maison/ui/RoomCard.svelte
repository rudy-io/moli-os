<script>
  import { tick } from 'svelte';
  import { hub, value, act, isOn, powerKey, protectedRoom, num, nameOf, deviceKind, lampsOf, doorOpen, leak, motion, lowBattery, unpowered, isFixture, note, reachable, quiet, outletsOf, climateIcon, setpointOf, climateOn, stoveState, climateOffLabel, switchStove, stoveAsked, sensorIcon, climateState, modeWord } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { isCoffee, STATE_TONE, stateWord } from '../lib/coffee.js';
  import Icon from './Icon.svelte';
  import LightTile from './LightTile.svelte';
  import ClimateCard from './ClimateCard.svelte';
  import CoverTile from './CoverTile.svelte';
  import AmbiancePicker from './AmbiancePicker.svelte';
  import { phase, PHASES } from '../lib/printers.js';
  import { writeState } from '../lib/program.js';

  /** One room, compact: its temperature and a switch for its lights in the
   *  head, its lights and plugs as small pads (a ⚙ opens a lamp's dimmer,
   *  colours, fan), its clims as one line each (a tap opens the dial), and
   *  what it senses as chips (one line per sensor, chips after its name,
   *  when two or more sensors read something). `icons`: pads show an icon only (names on
   *  hover and for screen readers). `every`: each sensor shows, even
   *  silent (filtered view). `focus`: a clim of the room to open and
   *  scroll to, at its alarm if it has one (the home page's notice). */
  let { room, every = false, icons = false, focus = null } = $props();

  const locked = $derived(protectedRoom(room.name));

  function chips(d) {
    // An appliance that is no button: what it draws, nothing when unplugged.
    if (quiet(d.id)) {
      if (d.online === false) return every ? [{ tone: '', icon: 'alert', text: t('commun.piece.hors_ligne', { name: nameOf(d.id) }) }] : [];
      const w = value(d.id, 'cur_power') ?? value(d.id, 'power');
      if (typeof w === 'number' && w >= 3) return [{ tone: 'warm', icon: 'power', text: t('commun.piece.consomme', { name: nameOf(d.id), watts: num(w) }) }];
      return every ? [{ tone: '', icon: 'power', text: t('commun.piece.au_repos', { name: nameOf(d.id) }) }] : [];
    }
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
    // A sensor's own temperature (the head says the room's), coloured as the
    // head's gauge; its humidity, dry, comfortable or damp.
    const deg = v('temperature') ?? v('temp_current');
    if (typeof deg === 'number') out.push({ tone: CHIP_TONE[tone(deg)], icon: 'thermometer', text: `${num(deg, 1)}°` });
    const hum = v('humidity') ?? v('humidity_value');
    if (typeof hum === 'number')
      out.push({ tone: hum < 35 ? 'warm' : hum <= 65 ? 'good' : 'cool', icon: 'humidity', text: t('commun.pourcent', { n: num(hum) }) });
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
    if (isCoffee(d)) {
      const st = d.online === false ? null : value(d.id, 'status');
      out.push({ tone: STATE_TONE[st] ?? '', icon: 'coffee-maker', text: `${nameOf(d.id)} · ${stateWord(st).toLowerCase()}`, href: '#/cafe' });
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

  // Two sensors or more reading something: each on its line, its name in
  // front of its readings (a room's two thermometers no longer mix). What
  // else senses (a plug drawing, a camera, a printer) says its own name.
  const readers = $derived(room.sensors.filter((d) => deviceKind(d) === 'sensor' && !quiet(d.id) && chips(d).length));
  const apart = $derived(readers.length >= 2 ? readers : []);
  const sensorChips = $derived(room.sensors.filter((d) => !apart.includes(d)).flatMap(chips));
  const lamps = $derived(lampsOf(room));
  let showLooks = $state(false);

  // ---- the room's temperature ----------------------------------------------------------

  // Its sensors' when it has some: a heater's or a unit's own probe, next to
  // what it heats or cools, only stands in for them.
  const temp = $derived.by(() => {
    const read = (list) =>
      list
        .filter((d) => d.online !== false)
        .map((d) => value(d.id, 'temperature') ?? value(d.id, 'temp_current'))
        .filter((x) => typeof x === 'number');
    // A filtered room keeps its sensors for this alone (`probes`).
    const sensed = read(room.probes ?? room.sensors);
    const readings = sensed.length ? sensed : read(room.climates);
    return readings.length ? readings.reduce((a, b) => a + b, 0) / readings.length : null;
  });
  // A heater's reading and setpoint: their tenth when they have one (18,5°, 19°).
  const degrees = (v) => num(v, typeof v === 'number' && v % 1 ? 1 : 0);
  const tone = (deg) => (deg < 18 ? 'cold' : deg < 20 ? 'cool' : deg <= 24 ? 'good' : deg <= 27 ? 'warm' : 'hot');
  // The gauge's tones as a chip's background.
  const CHIP_TONE = { cold: 'cool', cool: 'cool', good: 'good', warm: 'warm', hot: 'alert' };
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

  /** A strip's pad for all its outlets (`ALL`), then one per outlet. */
  const ALL = '*';
  const plugPads = (d) => {
    const outlets = outletsOf(d.id);
    if (!outlets.length) return [{ d, point: null }];
    return [{ d, point: ALL, outlets }, ...outlets.map((o) => ({ d, point: o.key, name: o.name }))];
  };
  const litCount = (outlets, id) => outlets.filter((o) => isOn(id, o.key)).length;

  /** The pads: lights, then TV and speakers, then plugs. */
  const pads = $derived([
    ...room.lights.map((d) => ({ d, point: null })),
    ...room.media.map((d) => ({ d, point: deviceKind(d) === 'tv' ? 'power' : null, link: deviceKind(d) === 'speaker' ? '#/salon' : null })),
    ...room.plugs.flatMap(plugPads),
  ]);

  let open = $state(null);

  function stateOf(d, point, outlets) {
    if (unpowered(d.id) && !isFixture(d)) return t('commun.piece.coupee');
    if (!reachable(d.id)) return t('commun.piece.injoignable');
    if (outlets) return t('commun.piece.prises_allumees', { on: litCount(outlets, d.id), count: outlets.length });
    if (deviceKind(d) === 'speaker') return value(d.id, 'playing') === true ? t('commun.piece.lecture_en_cours') : t('commun.piece.a_l_arret');
    if (!isOn(d.id, point)) return t('commun.piece.eteinte');
    const b = value(d.id, 'brightness');
    return typeof b === 'number' && fine(d) ? t('commun.pourcent', { n: Math.round(b) }) : t('commun.piece.allumee');
  }

  function tap(d, point, outlets, name) {
    if (unpowered(d.id) && !isFixture(d)) {
      note(t('commun.lampe.coupee_note', { label: nameOf(d.id) }));
      return;
    }
    if (outlets) {
      // All on → all off; otherwise everything on.
      const on = litCount(outlets, d.id) < outlets.length;
      for (const o of outlets) if (isOn(d.id, o.key) !== on) act(`${d.id}/${o.key}`, on, `${nameOf(d.id)} · ${o.name}`);
      return;
    }
    if (name) {
      act(`${d.id}/${point}`, !isOn(d.id, point), `${nameOf(d.id)} · ${name}`);
      return;
    }
    const key = point ?? powerKey(d);
    if (!key) return;
    const cur = value(d.id, key);
    act(`${d.id}/${key}`, typeof cur === 'string' ? (isOn(d.id, point) ? 'OFF' : 'ON') : !isOn(d.id, point), nameOf(d.id));
  }

  let openClim = $state(null);
  // Once per link: the room's devices are read again at each reading, the
  // card must not open again (nor the page scroll) every minute.
  let focused = null;
  $effect(() => {
    if (!focus || focused === focus || !room.climates.some((d) => d.id === focus)) return;
    focused = focus;
    openClim = focus;
    // Once open: its alarm block if it shows one, else its line.
    tick().then(() => {
      const at = document.getElementById(`alarme-${focus}`) ?? document.getElementById(`clim-${focus}`);
      at?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    });
  });

  // A stove Moli switches has its button on its line too, as on its card:
  // lit from off only, put out unless already going out (its driver says why).
  const stoveSwitch = (d) => climateIcon(d) === 'stove' && d.points.some((p) => p.key === 'on' && p.access?.write);
  const stoveLit = (d) => value(d.id, 'on') === true;
  const stoveReady = (d) => (stoveLit(d) ? !['shut_off', 'cleaning'].includes(value(d.id, 'mode')) : value(d.id, 'mode') === 'off');
  const stovePower = (d) => switchStove(d.id, !stoveLit(d), nameOf(d.id));
</script>

<section class="card room">
  <div class="head">
    <span class="room-icon" class:lit={room.lit}><Icon name={room.icon} size={22} /></span>
    <div class="title">
      <h2>{room.name}{#if locked}<span class="lock" title={t('commun.piece.protegee_titre')}><Icon name="lock" size={13} /></span>{/if}</h2>
      {#if room.lights.length}
        <span class="muted">{room.lit ? t('commun.piece.allumees', { count: room.lit }) : t('commun.piece.tout_eteint')}</span>
      {:else if room.climates.length === 1}
        <span class="muted">{climateState(room.climates[0])}</span>
      {/if}
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
      {#each pads as { d, point, link, outlets, name } (`${d.id}/${point}`)}
        {@const lit = link ? value(d.id, 'playing') === true : outlets ? litCount(outlets, d.id) > 0 : isOn(d.id, point)}
        {@const cut = unpowered(d.id) && !isFixture(d)}
        {@const more = !link && !point && !cut && (fine(d) || hasFan(d.id) || (isFixture(d) && d.members?.some(hasFan)))}
        {@const label = name ?? nameOf(d.id)}
        <div class="pad-wrap">
          {#if link}
            <a class="pad" class:on={lit} href={link} title={label} aria-label={t('commun.action', { label, action: stateOf(d, point) })}>
              <span class="ico"><Icon name={padIcon(d)} size={icons ? 22 : 18} /></span>
              {#if !icons}<b>{label}</b><small>{stateOf(d, point)}</small>{/if}
            </a>
          {:else}
            <button class="pad" class:on={lit} class:cut class:off-grid={!reachable(d.id) && !cut} onclick={() => tap(d, point, outlets, name)} title={label} aria-pressed={lit} aria-label={t('commun.action', { label, action: stateOf(d, point, outlets) })}>
              <span class="ico"><Icon name={outlets ? 'power' : padIcon(d)} size={icons ? 22 : 18} /></span>
              {#if !icons}<b>{label}</b><small>{stateOf(d, point, outlets)}</small>{/if}
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
    {@const on = climateOn(d)}
    {@const setpoint = setpointOf(d.id)}
    {@const notTaken = writeState(value(d.id, 'week_program')).missing.length}
    <div class="clim-row" id="clim-{d.id}">
      <button class="clim" class:on onclick={() => (openClim = openClim === d.id ? null : d.id)} aria-expanded={openClim === d.id}>
        <Icon name={climateIcon(d)} size={18} />
        <b>{nameOf(d.id)}</b>
        <!-- Parts of its program the cloud never took: said here too (a program copied to several heaters). -->
        {#if notTaken}
          <span class="not-taken" role="img" title={t('commun.programme.non_pris_court', { count: notTaken })} aria-label={t('commun.programme.non_pris_court', { count: notTaken })}>
            <Icon name="warning" size={15} />
          </span>
        {/if}
        <!-- A pilot wire on four or six orders has no setpoint of its own. -->
        <span class="num">{degrees(value(d.id, 'temperature'))}°{#if setpoint != null} → {degrees(setpoint)}°{/if}</span>
        <span class="state">{d.id in stoveAsked ? t('commun.poele.en_cours') : climateIcon(d) === 'stove' && stoveState(d.id) ? climateState(d) : on ? modeWord(d.id) : t(climateOffLabel(d))}</span>
      </button>
      {#if stoveSwitch(d)}
        {@const lit = stoveLit(d)}
        {@const asked = d.id in stoveAsked}
        <!-- Asked and not yet read back: in progress, not the old state again. -->
        <button class="round power" class:lit={lit && !asked} class:busy={asked} onclick={() => stovePower(d)} disabled={asked || !stoveReady(d)} aria-busy={asked} aria-pressed={lit} title={t(asked ? (stoveAsked[d.id] ? 'commun.poele.mise_en_marche' : 'commun.poele.mise_a_l_arret') : lit ? 'commun.poele.eteindre' : 'commun.poele.allumer')} aria-label={t(lit ? 'commun.clim.label_arret' : 'commun.clim.label_marche', { name: nameOf(d.id) })}>
          {#if asked}<span class="busy-ring" aria-hidden="true"></span>{:else}<Icon name="power" size={17} />{/if}
        </button>
      {/if}
    </div>
    {#if openClim === d.id}
      <ClimateCard id={d.id} name={nameOf(d.id)} />
    {/if}
  {/each}

  {#each apart as d (d.id)}
    <div class="sensor-row">
      <span class="bubble"><Icon name={sensorIcon(d)} size={16} /></span>
      <b>{nameOf(d.id)}</b>
      <span class="chips">
        {#each chips(d) as c, i (i)}
          <span class="chip {c.tone}"><Icon name={c.icon} size={14} />{c.text}</span>
        {/each}
      </span>
    </div>
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
  .not-taken {
    display: inline-flex;
    color: var(--warm-ink);
  }

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

  /* A clim, on one line; a stove's button at its end. */
  .clim-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .clim-row .clim {
    flex: 1;
    min-width: 0;
  }

  .round.power {
    width: 40px;
    height: 40px;
  }

  .round.power.lit {
    background: var(--warm);
    color: #fff;
  }

  .round.power:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .round.power.busy:disabled {
    opacity: 1;
    color: var(--warm-ink);
    font-size: 17px;
    cursor: progress;
  }

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

  /* A long name gives way, the temperature and the state stay. */
  .clim b {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 650;
  }

  .clim .num,
  .clim .state {
    flex: none;
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

  /* A chip wider than the card wraps instead of spilling out. */
  .chips :global(.chip) {
    max-width: 100%;
    white-space: normal;
  }

  .chips a {
    text-decoration: none;
  }

  /* A sensor's line, as a heater's: its bubble, its name, its readings
     under the name. */
  .sensor-row {
    display: grid;
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 4px 10px;
    padding: 8px 12px 8px 8px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .bubble {
    grid-row: span 2;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-3);
  }

  .sensor-row b {
    min-width: 0;
    font-size: 14px;
    font-weight: 650;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sensor-row .chips {
    gap: 5px;
  }

  .sensor-row .chip {
    padding: 3px 9px;
    font-size: 12.5px;
  }

  /* A plain chip on the line's grey stays visible. */
  .sensor-row .chip:not(.warm, .cool, .alert, .good) {
    background: var(--surface);
  }
</style>
