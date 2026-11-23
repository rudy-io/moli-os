<script>
  import { hub, value, act, toggle, isOn, powerKey, protectedRoom, num, nameOf, deviceKind, lampsOf, doorOpen, leak, motion, lowBattery, unpowered, isFixture, note, reachable, quiet, outletsOf, plantNeeds, coverPosition, lastPress, pressWord, relative, home, isSwitchHelper, helperIcon } from '../lib/home.svelte.js';
  import { onDestroy } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { impulseIcon } from '../lib/icons.js';
  import LightTile from './LightTile.svelte';
  import ClimateCard from './ClimateCard.svelte';
  import ValveCard from './ValveCard.svelte';
  import RadiatorTile from './RadiatorTile.svelte';
  import CoverTile from './CoverTile.svelte';
  import ImpulseTile from './ImpulseTile.svelte';
  import StripTile from './StripTile.svelte';
  import AmbiancePicker from './AmbiancePicker.svelte';
  import { watts } from '../lib/energy-live.svelte.js';
  import AlarmCard from './AlarmCard.svelte';
  import { phase, PHASES } from '../lib/printers.js';
  import { canWrite, pointOf, climateKeys, impulsePoints } from '../lib/capabilities.js';
  import { heatsOnly } from '../lib/radiator.js';
  import { coverCommands } from '../lib/cover-controls.js';
  import { climateMode } from '../lib/climate.js';

  /** One room, compact: its temperature and a switch for its lights in the
   *  head, its lights and plugs as small pads (a ⚙ opens a lamp's dimmer,
   *  colours, fan), its clims as one line each (a tap opens the dial), and
   *  what it senses as chips. `icons`: pads show an icon only (names on
   *  hover and for screen readers). `every`: each sensor shows, even
   *  silent (filtered view). */
  let { room, every = false, icons = false } = $props();

  const locked = $derived(protectedRoom(room.name));

  /** What a quiet sensor shows when nothing is wrong (the « Capteurs » view): one
   *  small icon by what it measures, a value only when it is a measure. */
  const NOISE = /^(linkquality|voltage|power_outage_count|motion_timeout|illumination|action|side|operation_mode|update|identify|countdown|cycle_time|random_time|switch_inching|relay_status|light_mode|child_lock|battery_state|.*_calibration)/;
  const ORDER = ['occupancy', 'contact', 'smoke', 'water_leak', 'temperature', 'humidity', 'illuminance', 'battery'];
  const ICON_OF = { temperature: 'thermometer', humidity: 'humidity', illuminance: 'sun', contact: 'door-closed', occupancy: 'walk', smoke: 'smoke', water_leak: 'water-alert' };
  function quietChip(d) {
    const name = nameOf(d.id);
    const read = (p) => value(d.id, p.key);
    const rank = (p) => {
      const i = ORDER.indexOf(p.semantic);
      return i < 0 ? ORDER.length : i;
    };
    const p = d.points
      .filter((p) => p.access?.read !== false && !NOISE.test(p.key) && read(p) != null && read(p) !== '')
      .sort((a, b) => rank(a) - rank(b))[0];
    if (!p) return null; // nothing read yet: nothing to show (no mystery icon)
    const x = read(p);
    const shown = typeof x === 'number' ? num(x, p.semantic === 'battery' ? 0 : 1) : x === true ? t('commun.oui').toLowerCase() : x === false ? t('commun.non').toLowerCase() : x;
    const title = t('commun.piece.lecture', { name, label: p.label, value: `${shown}${p.unit && typeof x === 'number' ? ` ${p.unit}` : ''}` });
    if (p.semantic === 'battery' && typeof x === 'number') {
      return { tone: x >= 50 ? 'good' : '', icon: x >= 80 ? 'battery' : x >= 30 ? 'battery-50' : 'battery-20', text: t('commun.pourcent', { n: num(x) }), title };
    }
    const icon = ICON_OF[p.semantic];
    if (!icon) return null; // a reading Moli has no picture for stays quiet
    if (typeof x === 'number') return { tone: '', icon, text: `${shown}${p.unit ? ` ${p.unit}` : ''}`, title };
    return { tone: '', icon, text: '', title };
  }

  /** A reading graded by the usual norms: the tone of its chip says whether the air is
   *  fine ('good'), worth watching ('warm') or bad ('alert'). The tooltip says it in words.
   *  CO: WHO 9 ppm over 8 h, 30 ppm over 1 h. CO₂: 1000 ppm is the French indoor
   *  guideline, 1500 ppm calls for fresh air. */
  const LEVELS = [
    ['commun.niveau.bon', 'good'],
    ['commun.niveau.attention', 'warm'],
    ['commun.niveau.mauvais', 'alert'],
  ];
  const grade = (x, limits) => LEVELS[limits.filter((l) => x >= l).length];
  const GRADES = {
    co: (x) => grade(x, [9, 30]),
    co2: (x) => grade(x, [1000, 1500]),
    // Humidity has no universal norm (it depends on the room and the season): always blue.
    humidity: () => [null, 'cool'],
  };
  // Every sensor chip says which device tells it: « <name> : <value> ».
  const said = (id, value) => t('commun.piece.mesure', { name: nameOf(id), value });
  const measure = (kind, x, id, text) => {
    const [word, tone] = GRADES[kind](x);
    return { tone, icon: kind, text, title: word ? `${said(id, text)} · ${t(word)}` : said(id, text) };
  };

  /** How long a press stays on show outside the « Capteurs » view. */
  const PRESS_FRESH_MS = 10 * 60_000;

  function chips(d) {
    // A helper that is no switch: its value, said with its name.
    if (deviceKind(d) === 'helper') {
      const key = d.points?.[0]?.key;
      const v = key == null ? null : value(d.id, key);
      const icon = helperIcon(d);
      const helper = t('commun.piece.aide');
      if (v == null || v === '') return every ? [{ tone: '', icon, text: '', title: `${nameOf(d.id)} · ${helper}`, helper: true }] : [];
      const shown = typeof v === 'number' ? num(v) : String(v);
      return [{ tone: '', icon, text: shown, title: `${said(d.id, shown)} · ${helper}`, helper: true }];
    }
    // An appliance that is no button: what it draws, nothing when unplugged.
    if (quiet(d.id)) {
      if (d.online === false) return every ? [{ tone: '', icon: 'offline', text: t('commun.piece.hors_ligne', { name: nameOf(d.id) }) }] : [];
      const w = value(d.id, 'cur_power') ?? value(d.id, 'power');
      if (typeof w === 'number' && w >= 3) return [{ tone: 'warm', icon: 'power', text: t('commun.piece.consomme', { name: nameOf(d.id), watts: num(w) }) }];
      return every ? [{ tone: '', icon: 'power', text: t('commun.piece.au_repos', { name: nameOf(d.id) }) }] : [];
    }
    // A silent sensor's last readings are old news: say it is silent.
    if (d.online === false && ['sensor', 'plant'].includes(deviceKind(d))) {
      const dead = lowBattery(d.id);
      return [{ tone: dead ? 'alert' : '', icon: dead ? 'battery-alert' : 'offline', text: t(dead ? 'commun.piece.pile_vide' : 'commun.piece.hors_ligne', { name: nameOf(d.id) }) }];
    }
    // A plant says its soil, and whether it is thirsty; its pot's air is no room's.
    if (deviceKind(d) === 'plant') {
      const { moisture, thirsty } = plantNeeds(d);
      const shown = moisture == null ? '' : t('commun.pourcent', { n: num(moisture) });
      const plant = thirsty
        ? [{ tone: 'warm', icon: 'sprout', text: shown, title: t('commun.piece.plante_soif', { name: nameOf(d.id), n: shown }) }]
        : every || moisture != null
          ? [{ tone: 'good', icon: 'sprout', text: shown, title: t('commun.piece.plante', { name: nameOf(d.id), n: shown }) }]
          : [];
      if (lowBattery(d.id)) plant.push({ tone: 'alert', icon: 'battery-alert', text: t('commun.piece.pile_faible', { name: nameOf(d.id) }) });
      return plant;
    }
    const out = [];
    const v = (k) => value(d.id, k);
    if (v('smoke') === true) out.push({ tone: 'alert', icon: 'smoke', text: t('commun.capteur.fumee') });
    if (v('carbon_monoxide') === true) out.push({ tone: 'alert', icon: 'smoke', text: t('commun.capteur.monoxyde') });
    if (leak(d.id)) out.push({ tone: 'alert', icon: 'water-alert', text: t('commun.piece.fuite', { name: nameOf(d.id) }) });
    const open = doorOpen(d.id);
    if (open === true) out.push({ tone: 'warm', icon: 'door-open', text: '', title: said(d.id, t('commun.piece.ouverte')) });
    else if (open === false) out.push({ tone: '', icon: 'door-closed', text: '', title: said(d.id, t('commun.piece.fermee')) });
    // The temperature is the room's, in the head.
    const hum = v('humidity') ?? v('humidity_value') ?? v('va_humidity');
    if (typeof hum === 'number') out.push(measure('humidity', hum, d.id, t('commun.pourcent', { n: num(hum) })));
    const co2 = v('co2_value') ?? v('co2');
    if (typeof co2 === 'number') out.push(measure('co2', co2, d.id, `${num(co2)} ppm`));
    const co = v('co');
    if (typeof co === 'number') out.push(measure('co', co, d.id, `${num(co)} ppm`));
    const lx = v('illuminance') ?? v('illuminance_lux');
    if (typeof lx === 'number') out.push({ tone: '', icon: 'sun', text: `${num(lx)} lx`, title: said(d.id, `${num(lx)} lx`) });
    if (motion(d.id)) out.push({ tone: 'warm', icon: 'walk', text: '', title: said(d.id, t('commun.piece.presence_vue')) });
    // A button says its last press: shown while fresh, always among the sensors.
    const press = lastPress(d);
    if (press?.action) {
      const fresh = press.ts != null && home.now - press.ts < PRESS_FRESH_MS;
      if (fresh || every) {
        const word = pressWord(press.action);
        const when = relative(press.ts, home.now);
        // A remote with several buttons says which one (« Bouton 2 · simple »).
        const what = press.button ? `${press.button} · ${word}` : word;
        const name = press.button ? `${nameOf(d.id)} · ${press.button}` : nameOf(d.id);
        out.push({ tone: fresh ? 'warm' : '', icon: 'hand', text: when ? `${what} · ${when}` : what, title: t('commun.piece.dernier_appui', { name, action: word, when }) });
      }
    }
    if (lowBattery(d.id)) out.push({ tone: 'alert', icon: 'battery-alert', text: t('commun.piece.pile_faible', { name: nameOf(d.id) }) });
    if (deviceKind(d) === 'camera') out.push({ tone: 'cool', icon: 'cctv', text: nameOf(d.id), href: '#/cameras' });
    if (deviceKind(d) === 'printer') {
      const st = phase(v('state'));
      const text = st === 'printing' || st === 'paused' ? `${PHASES[st].label.toLowerCase()} ${t('commun.pourcent', { n: Math.round(Number(v('progress') ?? 0)) })}` : PHASES[st].label.toLowerCase();
      out.push({ tone: PHASES[st].tone, icon: 'printer3d', text: `${nameOf(d.id)} · ${d.online === false ? t('commun.piece.imprimante_eteinte') : text}`, href: '#/impression' });
    }
    // A quiet sensor in the filtered view: an icon (its reading in the tooltip), not a sentence.
    if (every && !out.length) {
      const quiet = quietChip(d);
      if (quiet) out.push(quiet);
    }
    return out;
  }

  const sensorChips = $derived([...room.sensors, ...(room.helpers ?? []).filter((d) => !isSwitchHelper(d))].flatMap(chips));
  const lamps = $derived(lampsOf(room));
  let showLooks = $state(false);

  // ---- the room's temperature ----------------------------------------------------------

  const roomTemperature = $derived.by(() => {
    const readings = [...room.sensors, ...room.climates]
      .filter((d) => d.online !== false && deviceKind(d) !== 'plant')
      // Tuya's Zigbee probes say `va_temperature`.
      .map((d) => {
        const key = ['temperature', 'temp_current', 'va_temperature'].find((key) => Number.isFinite(value(d.id, key)));
        return { value: key ? value(d.id, key) : null, unit: pointOf(d, key)?.unit };
      })
      .filter((reading) => Number.isFinite(reading.value));
    // An average cannot mix Celsius/Fahrenheit or readings without a known scale.
    if (!readings.length || !readings[0].unit || readings.some((reading) => reading.unit !== readings[0].unit)) return null;
    return { value: readings.reduce((sum, reading) => sum + reading.value, 0) / readings.length, unit: readings[0].unit };
  });
  const temp = $derived(roomTemperature?.value ?? null);
  const tone = (deg) => (deg < 18 ? 'cold' : deg < 20 ? 'cool' : deg <= 24 ? 'good' : deg <= 27 ? 'warm' : 'hot');
  const gaugeAt = (deg) => Math.min(100, Math.max(0, ((deg - 14) / 16) * 100));

  // ---- the room's switch: its lights, never its TV or plugs (D12) -----------------------

  const writableLights = $derived(room.lights.filter((d) => canWrite(d, powerKey(d))));
  const writableLit = $derived(writableLights.some((d) => isOn(d.id)));
  function roomPower() {
    const on = !writableLit;
    for (const d of writableLights) {
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
    if (kind === 'cover') return (coverPosition(d.id) ?? 0) > 0 ? 'shutter-open' : 'shutter';
    if (kind === 'tv') return 'tv';
    if (kind === 'speaker') return 'speaker';
    if (kind === 'helper') return helperIcon(d);
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

  /** A power strip gets its own tile, drawn: a socket per outlet. */
  const strips = $derived(room.plugs.filter((d) => outletsOf(d.id).length));
  const canTap = (d, point) => canWrite(d, point ?? powerKey(d));

  /** A cover pad follows its tile's access rule: live, and the command it would
   *  send is writable (any command when the position is unknown). */
  const coverPadEnabled = (d) => {
    if (!reachable(d.id) || hub.connected !== true) return false;
    const pct = coverPosition(d.id);
    return coverCommands(d).some(({ command, key, value }) => value && canWrite(d, key)
      && (pct == null || command === (pct > 0 ? 'close' : 'open')));
  };

  /** A trigger device with one relay is a pad like the others (a tap sends the
   *  pulse); with several relays it keeps its tile, one button per relay. */
  const lonePulse = (d) => impulsePoints(d).length === 1;
  const pulseTiles = $derived(room.impulses.filter((d) => !lonePulse(d)));

  /** The pads: lights, then TV and speakers, then plugs, then covers, then
   *  single-relay triggers. */
  const pads = $derived([
    ...room.lights.map((d) => ({ d, point: null })),
    ...room.media.map((d) => ({ d, point: deviceKind(d) === 'tv' ? 'power' : null, link: deviceKind(d) === 'speaker' ? '#/salon' : null })),
    ...room.plugs.filter((d) => !outletsOf(d.id).length).map((d) => ({ d, point: null })),
    ...room.covers.map((d) => ({ d, point: null, isCover: true })),
    ...room.impulses.filter(lonePulse).map((d) => ({ d, point: impulsePoints(d)[0].key, isPulse: true })),
    // A helper switch: a pad, Oui / Non (« Lave-linge en cours »).
    ...(room.helpers ?? []).filter(isSwitchHelper).map((d) => ({ d, point: 'on' })),
  ]);

  // ---- single-relay triggers: no state to show, only an order and its outcome ----------

  /** Per device: 'sending', then 'sent', 'held' or 'error' for a few seconds. */
  let pulses = $state({});
  const pulseTimers = new Map();
  onDestroy(() => pulseTimers.forEach(clearTimeout));
  const pulseEnabled = (d, key) => reachable(d.id) && hub.connected === true && canWrite(d, key) && pulses[d.id] !== 'sending';
  async function pulse(d, key) {
    if (!pulseEnabled(d, key)) return;
    clearTimeout(pulseTimers.get(d.id));
    pulses[d.id] = 'sending';
    const result = await act(`${d.id}/${key}`, 'cycle', t('commun.action', { label: nameOf(d.id), action: t('commun.volet.impulsion').toLowerCase() }));
    pulses[d.id] = result === 'ok' ? 'sent' : result;
    pulseTimers.set(d.id, setTimeout(() => {
      delete pulses[d.id];
      pulseTimers.delete(d.id);
    }, 4000));
  }

  let open = $state(null);

  /** What a device measuring its own draw (a plug with a meter) pulls now, in W;
   *  null when it measures nothing or has no reading. */
  function drawOf(d) {
    const p = d.points?.find((q) => q.semantic === 'power' && q.kind?.type === 'numeric' && (q.unit === 'W' || q.unit === 'kW'));
    const v = p ? value(d.id, p.key) : null;
    if (typeof v !== 'number' || !Number.isFinite(v)) return null;
    return p.unit === 'kW' ? v * 1000 : v;
  }

  function stateOf(d, point, isCover) {
    if (isCover) {
      if (!reachable(d.id)) return t('commun.piece.injoignable');
      const pct = coverPosition(d.id);
      if (pct == null) return '';
      const shown = Math.max(0, Math.min(100, Math.round(Number(pct))));
      return shown === 0 ? t('commun.volet.ferme') : shown === 100 ? t('commun.volet.ouvert') : t('commun.volet.ouvert_a', { pct: num(shown) });
    }
    if (unpowered(d.id) && !isFixture(d)) return t('commun.piece.coupee');
    if (!reachable(d.id)) return t('commun.piece.injoignable');
    if (deviceKind(d) === 'speaker') return value(d.id, 'playing') === true ? t('commun.piece.lecture_en_cours') : t('commun.piece.a_l_arret');
    if (deviceKind(d) === 'helper') return t(isOn(d.id, point) ? 'commun.oui' : 'commun.non');
    if (!isOn(d.id, point)) return t('commun.piece.eteinte');
    const draw = point ? null : drawOf(d);
    if (draw != null) return t('commun.piece.allumee_conso', { watts: watts(draw) });
    const b = value(d.id, 'brightness');
    return typeof b === 'number' && fine(d) ? t('commun.pourcent', { n: Math.round(b) }) : t('commun.piece.allumee');
  }

  function tap(d, point, isCover) {
    if (isCover) {
      const pct = coverPosition(d.id);
      // Without a measured position, the pad opens the tile: open, stop, close.
      if (pct == null) {
        open = d.id;
        return;
      }
      const isOpen = pct > 0;
      const action = coverCommands(d).find((c) => c.command === (isOpen ? 'close' : 'open'));
      if (!action?.value || !canWrite(d, action.key)) return;
      const actionLabel = t(isOpen ? 'commun.volet.fermer' : 'commun.volet.ouvrir').toLowerCase();
      act(`${d.id}/${action.key}`, action.value, t('commun.action', { label: nameOf(d.id), action: actionLabel }));
      return;
    }
    if (!canTap(d, point)) return;
    if (unpowered(d.id) && !isFixture(d)) {
      note(t('commun.lampe.coupee_note', { label: nameOf(d.id) }));
      return;
    }
    toggle(d.id, nameOf(d.id), point);
  }

  function climateState(on, mode) {
    if (on === false || mode === 'off') return t('commun.piece.eteinte');
    const style = climateMode(mode);
    return style ? t(style.label) : mode ?? (on === true ? t('commun.piece.en_marche') : '—');
  }
  let openClim = $state(null);
</script>

<section class="card room">
  <div class="head">
    <span class="room-icon" class:lit={room.lit || room.opened}><Icon name={room.icon} size={22} /></span>
    <div class="title">
      <h2>{room.name}{#if locked}<span class="lock" title={t('commun.piece.protegee_titre')}><Icon name="lock" size={13} /></span>{/if}</h2>
      <span class="muted">
        {room.lit ? t('commun.piece.allumees', { count: room.lit }) : t('commun.piece.tout_eteint')}
        {#if room.opened}
          · {t('commun.piece.ouverts', { count: room.opened })}
        {:else if room.covers.length && !room.coversUnknown}
          · {t('commun.piece.tout_ferme')}
        {:else if room.covers.length}
          · {t('commun.volet.position_inconnue')}
        {/if}
      </span>
    </div>
    {#if temp != null}
      <span class="temp {roomTemperature.unit === '°C' ? tone(temp) : ''}" title={t('commun.piece.temperature')}><b class="num">{num(temp, 1)}{roomTemperature.unit}</b>{#if roomTemperature.unit === '°C'}<i class="gauge"><i style="left:{gaugeAt(temp)}%"></i></i>{/if}</span>
    {/if}
    {#if lamps.length}
      <button class="round" class:open={showLooks} onclick={() => (showLooks = !showLooks)} aria-expanded={showLooks} title={t('commun.piece.ambiances')} aria-label={t('commun.piece.ambiances_de', { name: room.name })}>
        <Icon name="palette" size={17} />
      </button>
    {/if}
    {#if room.lights.length}
      <button class="room-switch" class:on={writableLit} disabled={!writableLights.length} onclick={roomPower} aria-pressed={writableLit} title={writableLit ? t('commun.piece.tout_eteindre') : t('commun.piece.tout_allumer')} aria-label={t(writableLit ? 'commun.piece.label_eteindre' : 'commun.piece.label_allumer', { name: room.name })}><i></i></button>
    {/if}
  </div>

  {#if showLooks}
    <AmbiancePicker lights={lamps} label={room.name} />
  {/if}

  {#if pads.length}
    <div class="pads" class:icons>
      {#each pads as { d, point, link, isCover, isPulse } (`${d.id}/${point}`)}
        {@const coverPct = isCover ? coverPosition(d.id) : null}
        {@const lit = isPulse ? pulses[d.id] === 'sending' || pulses[d.id] === 'sent' : isCover ? (coverPct ?? 0) > 0 : link ? value(d.id, 'playing') === true : isOn(d.id, point)}
        {@const cut = !isPulse && unpowered(d.id) && !isFixture(d)}
        {@const more = isPulse ? false : isCover ? true : !link && !point && !cut && (fine(d) || hasFan(d.id) || (isFixture(d) && d.members?.some(hasFan)))}
        {@const label = nameOf(d.id)}
        {@const coverCommand = coverPct != null ? coverCommands(d).find((c) => c.command === (lit ? 'close' : 'open'))?.command : undefined}
        <div class="pad-wrap">
          {#if link}
            <a class="pad" class:on={lit} href={link} title={label} aria-label={t('commun.action', { label, action: stateOf(d, point) })}>
              <span class="ico"><Icon name={padIcon(d)} size={icons ? 22 : 18} /></span>
              {#if !icons}<b>{label}</b><small>{stateOf(d, point)}</small>{/if}
            </a>
          {:else if isPulse}
            {@const said = t(`commun.impulsion.etat.${pulses[d.id] ?? 'repos'}`)}
            <button class="pad" class:on={lit} class:off-grid={!reachable(d.id)} disabled={!pulseEnabled(d, point)} onclick={() => pulse(d, point)} title={[label, said].join(' · ')} data-pulse={pulses[d.id] ?? 'idle'} aria-label={t('commun.action', { label, action: t('commun.volet.impulsion').toLowerCase() })}>
              <span class="ico"><Icon name={reachable(d.id) ? impulseIcon(label) : 'offline'} size={icons ? 22 : 18} /></span>
              {#if !icons}<b>{label}</b><small>{said}</small>{/if}
            </button>
          {:else}
            <button class="pad" class:on={lit} class:cut class:helper={deviceKind(d) === 'helper'} class:off-grid={!reachable(d.id) && !cut} disabled={isCover ? !coverPadEnabled(d) : !canTap(d, point)} onclick={() => tap(d, point, isCover)} title={deviceKind(d) === 'helper' ? `${label} · ${t('commun.piece.aide')}` : isCover || (lit && !point && drawOf(d) != null) ? [label, stateOf(d, point, isCover)].filter(Boolean).join(' · ') : label} data-cover-command={coverCommand} aria-pressed={lit} aria-label={t('commun.action', { label, action: stateOf(d, point, isCover) })}>
              <span class="ico"><Icon name={padIcon(d)} size={icons ? 22 : 18} /></span>
              {#if !icons}<b>{label}</b><small>{stateOf(d, point, isCover)}</small>{/if}
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
      {@const pad = pads.find((p) => p.d.id === open)}
      {#if pad?.isCover}
        <CoverTile id={open} />
      {:else}
        <LightTile id={open} dimmer />
      {/if}
    {/if}
  {/if}

  {#each room.alarms as d (d.id)}
    <AlarmCard id={d.id} compact />
  {/each}

  {#each strips as d (d.id)}
    <StripTile id={d.id} {icons} />
  {/each}

  {#each pulseTiles as d (d.id)}
    <ImpulseTile id={d.id} {icons} />
  {/each}

  {#each room.climates as d (d.id)}
    {#if heatsOnly(d)}
      <RadiatorTile id={d.id} {icons} open={openClim === d.id} onopen={() => (openClim = openClim === d.id ? null : d.id)} />
      {#if openClim === d.id}<ValveCard id={d.id} name={nameOf(d.id)} bare />{/if}
    {:else}
    {@const keys = climateKeys(d)}
    {@const mode = pointOf(d, keys.mode) ? value(d.id, keys.mode) : null}
    {@const on = pointOf(d, 'on') ? value(d.id, 'on') : mode == null ? null : mode !== 'off'}
    {@const temperature = pointOf(d, keys.temperature) ? value(d.id, keys.temperature) : null}
    {@const target = pointOf(d, keys.target) ? value(d.id, keys.target) : null}
    <button class="clim" class:on={on === true} onclick={() => (openClim = openClim === d.id ? null : d.id)} aria-expanded={openClim === d.id}>
      <Icon name="air-conditioner" size={18} />
      <b>{nameOf(d.id)}</b>
      {#if Number.isFinite(temperature) || Number.isFinite(target)}
        <span class="num">{#if Number.isFinite(temperature)}{num(temperature, 0)}{pointOf(d, keys.temperature)?.unit ?? ''}{/if}{#if Number.isFinite(temperature) && Number.isFinite(target)} → {/if}{#if Number.isFinite(target)}{num(target, 1)}{pointOf(d, keys.target)?.unit ?? ''}{/if}</span>
      {/if}
      <span class="state">{climateState(on, mode)}</span>
    </button>
    {#if openClim === d.id}
      <ClimateCard id={d.id} name={nameOf(d.id)} />
    {/if}
    {/if}
  {/each}

  {#if sensorChips.length}
    <div class="chips">
      {#each sensorChips as c, i (i)}
        {#if c.href}
          <a class="chip {c.tone}" href={c.href} title={c.title ?? c.text}><Icon name={c.icon} size={15} />{c.text}</a>
        {:else}
          <span class="chip {c.tone}" class:helper={c.helper} title={c.title ?? c.text}><Icon name={c.icon} size={c.text ? 15 : 18} label={c.text ? null : c.title} />{c.text}</span>
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

  /* A phone: the name wraps instead of being cut by the temperature gauge. */
  @media (max-width: 520px) {
    .title h2 {
      white-space: normal;
      overflow-wrap: anywhere;
    }
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

  /* A helper is no device: a value Moli keeps. A thin dashed line in the
     theme's warm accent, like a draft. */
  .pad.helper,
  .chip.helper {
    outline: 1px dashed color-mix(in srgb, var(--warm) 70%, transparent);
    outline-offset: -1px;
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
    color: var(--warm-deep);
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
    color: var(--warm-deep);
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
