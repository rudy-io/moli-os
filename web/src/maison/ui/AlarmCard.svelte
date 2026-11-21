<script>
  import { device, value, nameOf, act, pending, num } from '../lib/home.svelte.js';
  import { alarmPoint, weakensAlarm } from '../lib/capabilities.js';
  import { icons } from '../lib/icons.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import SegmentedControl from './SegmentedControl.svelte';
  import Slider from './Slider.svelte';

  /** A security panel: its arm mode, its health, and the settings it takes.
   *  `compact` fits inside another card (the cameras' safety panel, a room);
   *  the full card is the house's own on the dashboard. An order that the
   *  guard holds (a protected room, quiet hours) asks for the code through
   *  the usual sheet, because `act` sends it as a human. */
  let { id, name = null, compact = false } = $props();

  const d = $derived(device(id));
  const point = $derived(alarmPoint(d));
  const key = $derived(point?.key ?? 'system_arm_type');
  const mode = $derived(point ? value(id, key) : null);
  const offline = $derived(d?.online === false);
  const writable = $derived(!!point && point.access?.write === true && !offline);
  const busy = $derived(pending(id, key));

  const STATES = {
    disarmed: { key: 'maison.alarme.desarmee', icon: 'lock', tone: '' },
    armed: { key: 'maison.alarme.armee', icon: 'shield', tone: 'cool' },
    home: { key: 'maison.alarme.maison', icon: 'home', tone: 'warm' },
  };
  const choices = $derived((point?.kind?.values ?? []).map((v) => ({ value: v, ...(STATES[v] ?? { icon: 'shield', tone: '' }) })));
  const label = $derived(name ?? nameOf(id, t('maison.alarme.titre')));
  const current = $derived(STATES[mode] ?? { icon: 'shield', tone: '' });
  const modeName = (m) => (STATES[m]?.key ? t(STATES[m].key) : (m ?? t('maison.alarme.inconnu')));
  // The three modes as one control: its icon and its word in the house's
  // language, the current one held by the panel itself.
  const modeSegments = $derived(
    choices.map((c) => ({
      value: c.value,
      label: modeName(c.value),
      icon: icons[c.icon] ?? icons.shield,
      disabled: !writable || busy,
    })),
  );

  let confirming = $state(null);
  // A setting about to make the alarm weaker waits here until a human says
  // yes, exactly like arming: a mute panel watches without warning anyone.
  let weakening = $state(null);

  function choose(next) {
    if (!writable || busy || next === mode) {
      confirming = null;
      return;
    }
    weakening = null;
    // Disarming is the safe direction: one tap. Arming asks once more.
    if (next === 'disarmed') return send(next);
    confirming = next;
  }
  function send(next) {
    confirming = null;
    act(`${id}/${key}`, next, t('maison.alarme.commande', { name: label, mode: modeName(next) }));
  }

  const battery = $derived(value(id, 'bat_status'));
  const gsm = $derived(value(id, 'gsm_status'));
  const dc = $derived(value(id, 'dc_status'));
  const siren = $derived(value(id, 'inside_siren_en'));

  const gsmWord = (v) => {
    switch (v) {
      case 'Normal': return t('maison.alarme.gsm.normal');
      case 'NoSimCard': return t('maison.alarme.gsm.sans_carte');
      case 'NoNetWork': return t('maison.alarme.gsm.sans_reseau');
      case 'GsmOff': return t('maison.alarme.gsm.coupe');
      default: return v ?? '—';
    }
  };
  const dcWord = (v) => {
    switch (v) {
      case 'normal': return t('maison.alarme.secteur.normal');
      case 'low': return t('maison.alarme.secteur.faible');
      case 'high': return t('maison.alarme.secteur.eleve');
      case 'off': return t('maison.alarme.secteur.coupe');
      default: return v ?? '—';
    }
  };
  const choiceWord = (key, v) => {
    if (key === 'gsm_en' || key === 'gsm_status') return gsmWord(v);
    if (key === 'dc_status') return dcWord(v);
    return v ?? '—';
  };

  // What a panel lets one change, in the house's words. The arm mode is the
  // card's own buttons; `language` is the panel's, not the house's.
  const SETTINGS = [
    ['arm_delay', 'maison.alarme.reglage.arm_delay'],
    ['alarm_delay', 'maison.alarme.reglage.alarm_delay'],
    ['alarm_sound_duration', 'maison.alarme.reglage.alarm_sound_duration'],
    ['ring_times', 'maison.alarme.reglage.ring_times'],
    ['tel_alarm_cycle', 'maison.alarme.reglage.tel_alarm_cycle'],
    ['inside_siren_sound', 'maison.alarme.reglage.inside_siren_sound'],
    ['inside_siren_en', 'maison.alarme.reglage.inside_siren_en'],
    ['wireless_siren_en', 'maison.alarme.reglage.wireless_siren_en'],
    ['gsm_en', 'maison.alarme.reglage.gsm_en'],
    ['tel_ctrl_en', 'maison.alarme.reglage.tel_ctrl_en'],
    ['arm_sms_en', 'maison.alarme.reglage.arm_sms_en'],
    ['disarm_sms_en', 'maison.alarme.reglage.disarm_sms_en'],
    ['keyboard_tone_en', 'maison.alarme.reglage.keyboard_tone_en'],
    ['arm_delay_tone_en', 'maison.alarme.reglage.arm_delay_tone_en'],
    ['alarm_delay_tone_en', 'maison.alarme.reglage.alarm_delay_tone_en'],
    ['arm_disarm_tone_en', 'maison.alarme.reglage.arm_disarm_tone_en'],
  ];
  // The settings, grouped as one reads a panel: its delays, its siren, what
  // it calls, and what it beeps. A point absent from the panel is skipped.
  const GROUPS = [
    { icon: 'timer', titleKey: 'maison.alarme.groupe.delais', keys: ['arm_delay', 'alarm_delay'] },
    {
      icon: 'bell',
      titleKey: 'maison.alarme.groupe.sirene',
      keys: ['alarm_sound_duration', 'inside_siren_sound', 'inside_siren_en', 'wireless_siren_en'],
    },
    {
      icon: 'alert',
      titleKey: 'maison.alarme.groupe.alertes',
      keys: ['ring_times', 'tel_alarm_cycle', 'gsm_en', 'tel_ctrl_en', 'arm_sms_en', 'disarm_sms_en'],
    },
    {
      icon: 'tune',
      titleKey: 'maison.alarme.groupe.tonalites',
      keys: ['keyboard_tone_en', 'arm_delay_tone_en', 'alarm_delay_tone_en', 'arm_disarm_tone_en'],
    },
  ];
  const settings = $derived(
    new Map(
      SETTINGS.map(([k, labelKey]) => [k, { key: k, labelKey, point: d?.points.find((p) => p.key === k) }]).filter(
        ([, s]) => s.point,
      ),
    ),
  );
  const groups = $derived(
    GROUPS.map((g) => ({ ...g, settings: g.keys.map((k) => settings.get(k)).filter(Boolean) })).filter((g) => g.settings.length),
  );

  const controlOf = (p) =>
    p.kind.type === 'numeric'
      ? 'number'
      : p.kind.type === 'binary'
        ? 'switch'
        : p.kind.type === 'enum' && p.kind.values?.length === 2 && p.kind.values.includes('ON') && p.kind.values.includes('OFF')
          ? 'segments'
          : 'select';

  // What is on screen right after an order: the panel's answer replaces it,
  // or four seconds pass. A held or failed order drops it at once.
  let sent = $state({});
  const timers = new Map();
  function remember(key, v) {
    sent = { ...sent, [key]: v };
    clearTimeout(timers.get(key));
    timers.set(key, setTimeout(() => forget(key), 4000));
  }
  function forget(key) {
    clearTimeout(timers.get(key));
    timers.delete(key);
    if (key in sent) {
      const next = { ...sent };
      delete next[key];
      sent = next;
    }
  }
  $effect(() => () => [...timers.values()].forEach((timer) => clearTimeout(timer)));
  const shown = (p) => (p.key in sent ? sent[p.key] : value(id, p.key));

  async function setSetting(p, v, labelKey, confirmed = false) {
    if (p.access?.write !== true || offline || pending(id, p.key)) return;
    // Re-selecting the value already in force is not an order.
    if (String(v) === String(shown(p))) return;
    if (!confirmed && weakensAlarm(p.key, v)) {
      weakening = { point: p, value: v, labelKey };
      return;
    }
    weakening = null;
    remember(p.key, v);
    const result = await act(
      `${id}/${p.key}`,
      v,
      t('maison.alarme.reglage_commande', { name: label, reglage: t(labelKey) }),
    );
    if (result !== 'ok') forget(p.key);
  }

  // The muting of a sound is not a cut of a capability: each keeps its words.
  const weakeningKey = (w) => (w.point.key === 'inside_siren_sound' ? 'maison.alarme.confirmer_muet' : 'maison.alarme.confirmer_affaiblir');
  const weakeningAction = (w) => t(w.point.key === 'inside_siren_sound' ? 'maison.alarme.mettre_muet' : 'maison.alarme.couper');
  function confirmWeakening() {
    const w = weakening;
    if (!w) return;
    weakening = null;
    setSetting(w.point, w.value, w.labelKey, true);
  }

  const boundsOf = (p) => ({
    min: Number.isFinite(p.kind.min) ? p.kind.min : 0,
    max: Number.isFinite(p.kind.max) ? p.kind.max : 100,
  });
  const digitsOf = (p) => (p.kind.step && p.kind.step < 1 ? 1 : 0);
  const atBound = (p, v, direction) => {
    const { min, max } = boundsOf(p);
    const n = Number(v);
    return Number.isFinite(n) && (direction < 0 ? n <= min : n >= max);
  };
  function nudge(p, direction, labelKey) {
    const { min, max } = boundsOf(p);
    const step = p.kind.step ?? 1;
    const current = Number(shown(p));
    const from = Number.isFinite(current) ? current : min;
    const next = Math.min(max, Math.max(min, from + direction * step));
    setSetting(p, Number(next.toFixed(3)), labelKey);
  }
  // On/off enums on a panel are sounds: loud or muted, in the house's words.
  const segmentChoices = (p, disabled) =>
    (p.kind.values ?? []).map((v) => ({
      value: v,
      label: v === 'ON' ? t('maison.alarme.son.fort') : t('maison.alarme.son.muet'),
      disabled,
    }));
</script>

{#if point}
  <section class="card alarm-card" class:compact class:offline aria-label={label}>
    <div class="head">
      <span class="badge {current.tone}"><Icon name={current.icon} size={22} /></span>
      <div class="who">
        <h2>{label}</h2>
        <p class="state {current.tone}">{offline ? t('maison.alarme.hors_ligne') : modeName(mode)}</p>
      </div>
      {#if !writable}
        <span class="chip warn">{offline ? t('maison.alarme.hors_ligne') : t('maison.alarme.lecture_seule')}</span>
      {/if}
    </div>

    <div class="modes">
      <SegmentedControl
        segments={modeSegments}
        value={mode}
        onchange={(next) => choose(next)}
        label={t('maison.alarme.mode')}
      />
    </div>

    {#if confirming}
      <div class="confirm" role="alert">
        <p>{t(confirming === 'home' ? 'maison.alarme.confirmer_maison' : 'maison.alarme.confirmer_armee', { name: label })}</p>
        <button class="yes" onclick={() => send(confirming)}>{t('maison.alarme.confirmer')}</button>
        <button onclick={() => (confirming = null)}>{t('maison.alarme.annuler')}</button>
      </div>
    {/if}

    <div class="status">
      {#if battery != null}
        <span class="chip" class:alert={typeof battery === 'number' && battery <= 20}>
          <Icon name="battery" size={15} />{num(battery)} %
        </span>
      {/if}
      {#if gsm != null}
        <span class="chip" class:alert={gsm === 'NoSimCard' || gsm === 'NoNetWork'} class:warn={gsm === 'GsmOff'}>
          <Icon name="phone" size={15} />{gsmWord(gsm)}
        </span>
      {/if}
      {#if dc != null}
        <span class="chip" class:alert={dc === 'off' || dc === 'low'} class:warn={dc === 'high'}>
          <Icon name="bolt" size={15} />{dcWord(dc)}
        </span>
      {/if}
      {#if siren != null}
        <span class="chip" class:warn={siren === false}>
          <Icon name={siren === false ? 'volume-off' : 'bell'} size={15} />{siren === false ? t('maison.alarme.sirene_coupee') : t('maison.alarme.sirene_prete')}
        </span>
      {/if}
    </div>

    {#if !compact && groups.length}
      <details class="settings">
        <summary>
          <Icon name="tune" size={16} />{t('maison.alarme.reglages')}
          <span class="chevron"><Icon name="chevron-down" size={16} /></span>
        </summary>
        <div class="groups">
          {#each groups as g (g.titleKey)}
            <section class="group" aria-label={t(g.titleKey)}>
              <h3><Icon name={g.icon} size={15} />{t(g.titleKey)}</h3>
              {#each g.settings.filter((s) => controlOf(s.point) !== 'switch') as s (s.key)}
                {@const p = s.point}
                {@const v = shown(p)}
                {@const held = pending(id, p.key)}
                {@const { min, max } = boundsOf(p)}
                <div class="setting" class:held>
                  <div class="setting-head">
                    <span class="row-label">{t(s.labelKey)}</span>
                    {#if controlOf(p) === 'number'}
                      <span class="stepper">
                        <button
                          type="button"
                          class="step"
                          disabled={!writable || held || atBound(p, v, -1)}
                          aria-label={t('maison.alarme.diminuer', { reglage: t(s.labelKey) })}
                          onclick={() => nudge(p, -1, s.labelKey)}>−</button
                        >
                        <output class="value"
                          >{num(v, digitsOf(p))}{#if p.unit} <span class="unit">{p.unit}</span>{/if}</output
                        >
                        <button
                          type="button"
                          class="step"
                          disabled={!writable || held || atBound(p, v, 1)}
                          aria-label={t('maison.alarme.augmenter', { reglage: t(s.labelKey) })}
                          onclick={() => nudge(p, 1, s.labelKey)}>+</button
                        >
                      </span>
                    {/if}
                  </div>
                  {#if controlOf(p) === 'number'}
                    <div class="slider-wrap">
                      <Slider
                        {min}
                        {max}
                        step={p.kind.step ?? 1}
                        value={Number.isFinite(Number(v)) ? Number(v) : min}
                        disabled={!writable || held}
                        label={t(s.labelKey)}
                        valuetext={(n) => `${num(n, digitsOf(p))} ${p.unit ?? ''}`.trim()}
                        oncommit={(n) => setSetting(p, n, s.labelKey)}
                      />
                    </div>
                  {:else if controlOf(p) === 'segments'}
                    <SegmentedControl
                      segments={segmentChoices(p, !writable || held)}
                      value={v}
                      onchange={(choice) => setSetting(p, choice, s.labelKey)}
                      label={t(s.labelKey)}
                    />
                  {:else}
                    <select
                      value={v ?? ''}
                      aria-label={t(s.labelKey)}
                      disabled={!writable || held}
                      onchange={(e) => setSetting(p, e.currentTarget.value, s.labelKey)}
                    >
                      {#each p.kind.values ?? [] as choice (choice)}
                        <option value={choice}>{choiceWord(s.key, choice)}</option>
                      {/each}
                    </select>
                  {/if}
                </div>
              {/each}
              {#if g.settings.some((s) => controlOf(s.point) === 'switch')}
                <div class="tiles">
                  {#each g.settings.filter((s) => controlOf(s.point) === 'switch') as s (s.key)}
                    {@const p = s.point}
                    {@const v = shown(p)}
                    {@const held = pending(id, p.key)}
                    <button
                      type="button"
                      class="tile"
                      role="switch"
                      aria-checked={v === true}
                      disabled={!writable || held}
                      onclick={() => setSetting(p, v !== true, s.labelKey)}
                    >
                      <span class="tile-label">{t(s.labelKey)}</span>
                      <span class="track" aria-hidden="true"><span class="thumb"></span></span>
                    </button>
                  {/each}
                </div>
              {/if}
              {#if weakening && g.keys.includes(weakening.point.key)}
                <div class="confirm" role="alert">
                  <p>{t(weakeningKey(weakening), { reglage: t(weakening.labelKey) })}</p>
                  <button class="yes" onclick={confirmWeakening}>{weakeningAction(weakening)}</button>
                  <button onclick={() => (weakening = null)}>{t('maison.alarme.annuler')}</button>
                </div>
              {/if}
            </section>
          {/each}
        </div>
      </details>
    {/if}
  </section>
{/if}

<style>
  .alarm-card {
    display: grid;
    gap: 14px;
    min-width: 0;
  }

  .compact {
    padding: 0;
    border: 0;
    background: none;
    box-shadow: none;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }

  .badge {
    flex: none;
    display: grid;
    place-items: center;
    width: 46px;
    height: 46px;
    border-radius: 14px;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .badge.cool {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .badge.warm {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .who {
    min-width: 0;
    flex: 1;
  }

  .who h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 700;
    letter-spacing: -0.01em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .state {
    margin: 2px 0 0;
    font-size: 13.5px;
    font-weight: 650;
    color: var(--ink-3);
  }

  .state.cool {
    color: var(--cool);
  }

  .state.warm {
    color: var(--warm-ink);
  }

  .offline .badge {
    opacity: 0.5;
  }

  /* The three modes fill the card: the shared pill, one third each. */
  .modes :global(.segmented-control),
  .modes :global(.segments) {
    width: 100%;
  }

  .modes :global(.segment) {
    flex: 1 1 0;
    min-width: 0;
    gap: 7px;
    padding: 11px 8px;
    font-size: 13.5px;
    font-weight: 650;
  }

  .modes :global(.segment span) {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .confirm button:focus-visible,
  .step:focus-visible,
  .tile:focus-visible,
  select:focus-visible,
  summary:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }

  .confirm {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    padding: 12px 14px;
    border-radius: var(--r-sm);
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .confirm p {
    flex: 1 1 220px;
    margin: 0;
    font-size: 13.5px;
    font-weight: 600;
  }

  .confirm button {
    border: 0;
    border-radius: 999px;
    padding: 8px 14px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
  }

  .confirm button.yes {
    background: var(--warm-ink);
    color: var(--surface);
  }

  .status {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .status .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 5px 10px;
    font-size: 12.5px;
    font-weight: 600;
  }

  .status .chip.warn {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .status .chip.alert {
    background: color-mix(in srgb, var(--alert) 16%, var(--surface-2));
    color: var(--alert);
  }

  .settings {
    border-top: 1px solid var(--line);
    padding-top: 10px;
  }

  .settings summary {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 9px 12px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink-2);
    font-size: 13.5px;
    font-weight: 650;
    cursor: pointer;
    list-style: none;
    transition: background 0.15s;
  }

  .settings summary:hover {
    background: color-mix(in srgb, var(--ink) 6%, var(--surface-2));
  }

  .settings summary::-webkit-details-marker {
    display: none;
  }

  .chevron {
    display: inline-flex;
    margin-left: auto;
    color: var(--ink-3);
    transition: transform 0.2s var(--ease);
  }

  .settings[open] .chevron {
    transform: rotate(180deg);
  }

  .groups {
    display: grid;
    gap: 10px;
    margin-top: 10px;
  }

  .group {
    display: grid;
    gap: 10px;
    padding: 12px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }

  .group h3 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    color: var(--ink-3);
    font-size: 11.5px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .setting {
    display: grid;
    gap: 2px;
    min-width: 0;
  }

  .setting.held {
    opacity: 0.6;
  }

  .setting-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    min-width: 0;
  }

  .row-label {
    min-width: 0;
    color: var(--ink-2);
    font-size: 13.5px;
    overflow-wrap: anywhere;
  }

  .stepper {
    display: inline-flex;
    align-items: center;
    gap: 1px;
    flex: none;
    padding: 2px;
    border-radius: 999px;
    background: var(--surface);
  }

  .step {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: none;
    color: var(--ink-2);
    font: inherit;
    font-size: 16px;
    font-weight: 600;
    line-height: 1;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .step:hover:not(:disabled) {
    background: var(--surface-3);
    color: var(--ink);
  }

  .step:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .value {
    min-width: 54px;
    padding: 0 4px;
    color: var(--ink);
    font-size: 13px;
    font-weight: 700;
    text-align: center;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .value .unit {
    color: var(--ink-3);
    font-size: 11.5px;
    font-weight: 600;
  }

  .slider-wrap :global(.slider) {
    margin: 6px 2px 2px;
  }

  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 8px;
  }

  .tile {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    min-width: 0;
    padding: 9px 11px;
    border: 0;
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    text-align: left;
    cursor: pointer;
    transition: background 0.15s;
  }

  .tile:hover:not(:disabled) {
    background: var(--surface-3);
  }

  .tile:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .tile-label {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .track {
    display: flex;
    flex: none;
    width: 36px;
    height: 22px;
    padding: 2px;
    border-radius: 999px;
    background: var(--surface-3);
    transition: background 0.2s var(--ease);
  }

  .tile[aria-checked='true'] .track {
    background: var(--warm);
  }

  .thumb {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 0.2s var(--ease);
  }

  .tile[aria-checked='true'] .thumb {
    transform: translateX(14px);
  }

  select {
    justify-self: start;
    min-height: 32px;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--ink);
    padding: 5px 9px;
    font: inherit;
    font-size: 13.5px;
    font-weight: 550;
  }

  select:disabled {
    opacity: 0.55;
    cursor: default;
  }

  @media (prefers-reduced-motion: reduce) {
    .chevron,
    .track,
    .thumb {
      transition: none;
    }
  }

  @media (max-width: 380px) {
    .modes :global(.segment) {
      gap: 4px;
      padding: 10px 4px;
      font-size: 12.5px;
    }
  }
</style>
