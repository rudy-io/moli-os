<script>
  import { device, value, nameOf, act, pending, num } from '../lib/home.svelte.js';
  import { alarmPoint } from '../lib/capabilities.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

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

  let confirming = $state(null);

  function choose(next) {
    if (!writable || busy || next === mode) {
      confirming = null;
      return;
    }
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
  const armDelay = $derived(value(id, 'arm_delay'));
  const alarmDelay = $derived(value(id, 'alarm_delay'));

  const GSM = {
    Normal: 'maison.alarme.gsm.normal',
    NoSimCard: 'maison.alarme.gsm.sans_carte',
    NoNetWork: 'maison.alarme.gsm.sans_reseau',
    GsmOff: 'maison.alarme.gsm.coupe',
  };
  const DC = {
    normal: 'maison.alarme.secteur.normal',
    low: 'maison.alarme.secteur.faible',
    high: 'maison.alarme.secteur.eleve',
    off: 'maison.alarme.secteur.coupe',
  };
  const gsmWord = (v) => (GSM[v] ? t(GSM[v]) : (v ?? '—'));
  const dcWord = (v) => (DC[v] ? t(DC[v]) : (v ?? '—'));

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
  const settings = $derived(
    SETTINGS.map(([k, labelKey]) => ({ point: d?.points.find((p) => p.key === k), labelKey })).filter((s) => s.point),
  );

  const controlOf = (p) =>
    p.kind.type === 'numeric'
      ? 'number'
      : p.kind.type === 'binary' ||
          (p.kind.type === 'enum' && p.kind.values?.length === 2 && p.kind.values.includes('ON') && p.kind.values.includes('OFF'))
        ? 'switch'
        : 'select';
  const isOn = (p, v) => (p.kind.type === 'binary' ? v === true : v === 'ON');
  const nextValue = (p, v) => (p.kind.type === 'binary' ? v !== true : v === 'ON' ? 'OFF' : 'ON');

  function setSetting(p, v, labelKey) {
    if (p.access?.write !== true || offline || pending(id, p.key)) return;
    act(`${id}/${p.key}`, v, t('maison.alarme.reglage_commande', { name: label, reglage: t(labelKey) }));
  }
  function setNumber(p, e, labelKey) {
    const n = Number(e.currentTarget.value);
    if (Number.isFinite(n) && n !== value(id, p.key)) setSetting(p, n, labelKey);
  }
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

    <div class="modes" role="group" aria-label={t('maison.alarme.mode')}>
      {#each choices as c (c.value)}
        <button
          class:on={mode === c.value}
          disabled={!writable || busy}
          aria-pressed={mode === c.value}
          onclick={() => choose(c.value)}
        >
          <Icon name={c.icon} size={18} />{modeName(c.value)}
        </button>
      {/each}
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
      {#if typeof armDelay === 'number' && armDelay > 0}
        <span class="chip"><Icon name="timer" size={15} />{t('maison.alarme.armement_dans', { s: num(armDelay) })}</span>
      {/if}
      {#if typeof alarmDelay === 'number' && alarmDelay > 0}
        <span class="chip"><Icon name="timer" size={15} />{t('maison.alarme.alarme_dans', { s: num(alarmDelay) })}</span>
      {/if}
    </div>

    {#if !compact && settings.length}
      <details class="settings">
        <summary><Icon name="tune" size={16} />{t('maison.alarme.reglages')}</summary>
        <div class="rows">
          {#each settings as s (s.point.key)}
            {@const p = s.point}
            {@const kind = controlOf(p)}
            {@const v = value(id, p.key)}
            {@const held = pending(id, p.key)}
            <div class="row" class:held>
              <span class="row-label">{t(s.labelKey)}</span>
              {#if kind === 'switch'}
                <button
                  class="switch"
                  role="switch"
                  aria-checked={isOn(p, v)}
                  aria-label={t(s.labelKey)}
                  disabled={!writable || held}
                  onclick={() => setSetting(p, nextValue(p, v), s.labelKey)}
                >
                  <span class="knob"></span>
                </button>
              {:else if kind === 'number'}
                <span class="number">
                  <input
                    type="number"
                    class="num"
                    value={v ?? ''}
                    min={p.kind.min}
                    max={p.kind.max}
                    step={p.kind.step ?? 'any'}
                    aria-label={t(s.labelKey)}
                    disabled={!writable || held}
                    onchange={(e) => setNumber(p, e, s.labelKey)}
                  />
                  {#if p.unit}<span class="unit">{p.unit}</span>{/if}
                </span>
              {:else}
                <select
                  value={v ?? ''}
                  aria-label={t(s.labelKey)}
                  disabled={!writable || held}
                  onchange={(e) => setSetting(p, e.currentTarget.value, s.labelKey)}
                >
                  {#each p.kind.values ?? [] as choice (choice)}
                    <option value={choice}>{choice}</option>
                  {/each}
                </select>
              {/if}
            </div>
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

  .modes {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
  }

  .modes button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    min-width: 0;
    padding: 11px 10px;
    border: 0;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 13.5px;
    font-weight: 650;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .modes button:hover:not(:disabled) {
    background: color-mix(in srgb, var(--ink) 10%, var(--surface-2));
  }

  .modes button.on {
    background: var(--ink);
    color: var(--surface);
  }

  .modes button:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .modes button:focus-visible,
  .confirm button:focus-visible,
  .switch:focus-visible,
  input:focus-visible,
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
    display: inline-flex;
    align-items: center;
    gap: 7px;
    color: var(--ink-2);
    font-size: 13.5px;
    font-weight: 650;
    cursor: pointer;
    list-style: none;
  }

  .settings summary::-webkit-details-marker {
    display: none;
  }

  .rows {
    display: grid;
    gap: 2px;
    margin-top: 10px;
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 34px;
    padding: 2px 0;
  }

  .row + .row {
    border-top: 1px solid var(--line);
  }

  .row.held {
    opacity: 0.6;
  }

  .row-label {
    min-width: 0;
    color: var(--ink-2);
    font-size: 13.5px;
    overflow-wrap: anywhere;
  }

  .switch {
    position: relative;
    flex: none;
    width: 40px;
    height: 24px;
    padding: 0;
    border: 0;
    border-radius: 999px;
    background: var(--surface-3);
    cursor: pointer;
    transition: background 0.2s var(--ease);
  }

  .switch:hover:not(:disabled) {
    background: color-mix(in srgb, var(--ink) 8%, var(--surface-3));
  }

  .switch[aria-checked='true'] {
    background: var(--warm);
  }

  .switch:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 0.2s var(--ease);
  }

  .switch[aria-checked='true'] .knob {
    transform: translateX(16px);
  }

  .number {
    display: flex;
    align-items: baseline;
    gap: 4px;
    flex: none;
  }

  input,
  select {
    min-height: 32px;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink);
    padding: 5px 9px;
    font: inherit;
    font-size: 13.5px;
    font-weight: 550;
  }

  input:disabled,
  select:disabled {
    opacity: 0.55;
    cursor: default;
  }

  input {
    width: 86px;
    text-align: right;
  }

  .unit {
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-3);
  }

  @media (max-width: 380px) {
    .modes {
      grid-template-columns: 1fr;
    }
  }
</style>
