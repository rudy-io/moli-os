<script>
  import { onMount } from 'svelte';
  import { home, device, value, reachable, act, num } from '../lib/home.svelte.js';
  import { energy, useEnergy, kwh, watts, euros } from '../lib/energy-live.svelte.js';
  import { phaseOf, PHASE_TONE, cop, dayCop, deltaT, stepped, DEFAULT_PRESETS, followsRoom } from '../lib/heatpump.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** The heat pump, read from the top: comfort (the room against its set
   *  point, presets), what it does now (electricity in, heat out, COP,
   *  compressor), the water circuit, hot water, the bill, and its settings.
   *  `heatpump` in home.json: `device` (the controller), `power` (a point
   *  measuring its electricity, W), `meter` (its `[energy]` meter id),
   *  `presets`. Every order goes through the hub. */
  onMount(useEnergy);

  const cfg = $derived(home.config?.heatpump ?? {});
  const id = $derived(cfg.device ?? null);
  const d = $derived(id ? device(id) : null);
  const ok = $derived(id ? reachable(id) : false);
  const v = (k) => (id ? value(id, k) : null);
  const kindOf = (k) => d?.points.find((p) => p.key === k)?.kind ?? {};
  const can = (k) => ok && !!d?.points.some((p) => p.key === k && p.access?.write);
  const pointValue = (point) => {
    if (typeof point !== 'string' || !point.includes('/')) return null;
    const i = point.indexOf('/');
    const x = value(point.slice(0, i), point.slice(i + 1));
    return typeof x === 'number' ? x : null;
  };

  const values = $derived({
    power_state: v('power_state'),
    defrost: v('defrost'),
    frequency: v('frequency'),
    operating_mode: v('operating_mode'),
  });
  const phase = $derived(ok ? phaseOf(values) : null);
  const running = $derived(typeof v('frequency') === 'number' && v('frequency') > 0);
  const electric = $derived(pointValue(cfg.power));
  const heat = $derived(typeof v('output_power') === 'number' ? v('output_power') : null);
  const copNow = $derived(cop(heat, electric, running));
  const copYesterday = $derived(dayCop(v('heating_energy_production_yesterday'), v('heating_energy_consumption_yesterday')));
  const drop = $derived(deltaT(v('water_feed_temperature'), v('water_return_temperature')));
  const presets = $derived(cfg.presets ?? DEFAULT_PRESETS);
  const roomSet = $derived(v('room_temperature_setpoint'));
  const byRoom = $derived(followsRoom(v('mode')));
  const month = $derived(cfg.meter ? energy.summary?.month?.meters?.[cfg.meter] : null);
  const cur = $derived(energy.summary?.currency ?? '€');

  // The controller's own words for its modes, said in the house's language.
  const MODE_KEYS = {
    'Heating Room Temp': 'chauffe_piece',
    'Heating Flow Temp': 'chauffe_depart',
    'Heating Heat Curve': 'chauffe_loi',
    'Cooling Room Temp': 'froid_piece',
    'Cooling Flow Temp': 'froid_depart',
  };
  const modeLabel = (m) => (MODE_KEYS[m] ? t(`pac.mode.${MODE_KEYS[m]}`) : m);
  const deg = (x, digits = 1) => (typeof x === 'number' ? `${num(x, x % 1 ? digits : 0)}°` : '—');
  const set = (key, val, text) => act(`${id}/${key}`, val, text);
  function nudge(key, direction, what) {
    const next = stepped(v(key), kindOf(key), direction);
    if (next != null && next !== v(key)) set(key, next, t('pac.act_consigne', { what, deg: num(next, 1) }));
  }

  // The hot water gauge: from 10° to the setpoint's highest value (60° when unknown).
  const tankTop = $derived(Math.max(60, kindOf('hot_water_setpoint').max ?? 0));
  const tankAt = (x) => (typeof x === 'number' ? Math.min(100, Math.max(0, ((x - 10) / (tankTop - 10)) * 100)) : 0);
  // Yesterday: the share of the heat that was paid electricity.
  const paidShare = $derived.by(() => {
    const used = v('heating_energy_consumption_yesterday');
    const made = v('heating_energy_production_yesterday');
    return typeof used === 'number' && typeof made === 'number' && made > 0 ? Math.min(100, (used / made) * 100) : null;
  });
  const stateWord = $derived(ok ? t(`pac.phase.${phase ?? 'inconnu'}`) : t('pac.hors_ligne'));
  const summary = $derived(
    d ? t('pac.resume', { etat: stateWord, dehors: deg(v('outside_temperature')), mode: v('mode') ? modeLabel(v('mode')) : '—' }) : '',
  );
  const moment = $derived(
    running
      ? copNow == null
        ? t('pac.moment_tourne', { hz: num(v('frequency'), 0) })
        : t('pac.moment_chauffe', { hz: num(v('frequency'), 0), cop: num(copNow, 1) })
      : phase === 'off'
        ? t('pac.moment_arretee')
        : t('pac.moment_repos'),
  );

  let confirmOff = $state(false);
  let confirmTimer;
  function powerToggle() {
    const on = v('power_state') === true;
    if (on && !confirmOff) {
      confirmOff = true;
      clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirmOff = false), 5000);
      return;
    }
    confirmOff = false;
    set('power_state', !on, t(on ? 'pac.act_arret' : 'pac.act_marche'));
  }
</script>

{#snippet setpointControl(key, what, size = '')}
  <div class="set {size}">
    <button class="round" disabled={!can(key)} onclick={() => nudge(key, -1, what)} aria-label={t('pac.moins', { what })}>−</button>
    <b class="num">{deg(v(key))}</b>
    <button class="round" disabled={!can(key)} onclick={() => nudge(key, 1, what)} aria-label={t('pac.plus', { what })}>+</button>
  </div>
{/snippet}

<div class="pac-page">
  <header class="top">
    <div>
      <h1 class="page-title">{t('pac.titre')}</h1>
      <p class="page-sub">{summary}</p>
    </div>
    {#if d}
      <div class="top-actions">
        {#if can('power_state')}
          <button class="pill" class:danger={confirmOff} class:lit={v('power_state') === true} onclick={powerToggle} aria-pressed={v('power_state') === true}>
            <Icon name="power" size={16} />{confirmOff ? t('pac.confirmer_arret') : v('power_state') === true ? t('pac.en_marche') : t('pac.arretee')}
          </button>
        {/if}
        {#if can('holiday_mode_switch')}
          <button class="pill" class:lit={v('holiday_mode_switch') === true || v('holiday_mode') === 'On'} onclick={() => set('holiday_mode_switch', !(v('holiday_mode_switch') === true), t('pac.act_vacances'))}>
            <Icon name="palm" size={16} />{t('pac.vacances')}
          </button>
        {/if}
      </div>
    {/if}
  </header>

  {#if !d}
    <p class="muted">{t('pac.aucune')}</p>
  {:else}
    <section class="card comfort" aria-label={t('pac.confort')}>
      <div class="inside">
        <small class="label">{t('pac.dedans')}</small>
        <strong class="num">{deg(v('room_temperature'))}</strong>
        <span class="muted">{t('pac.dehors')} {deg(v('outside_temperature'))}</span>
      </div>
      <div class="aim">
        <div class="aim-row">
          <div class="target">
            <small class="label">{byRoom ? t('pac.consigne') : t('pac.consigne_inactive')}</small>
            {@render setpointControl('room_temperature_setpoint', t('pac.consigne_piece'), 'large')}
          </div>
          <span class="chip {PHASE_TONE[phase] ?? ''}"><i aria-hidden="true"></i>{stateWord}</span>
        </div>
        {#if can('room_temperature_setpoint')}
          <div class="presets" role="radiogroup" aria-label={t('pac.presets')}>
            {#each presets as p (p.value)}
              <button role="radio" aria-checked={roomSet === p.value} class:active={roomSet === p.value} onclick={() => set('room_temperature_setpoint', p.value, t('pac.act_preset', { name: p.name ?? t(`pac.preset.${p.key}`), deg: num(p.value, 1) }))}>
                <Icon name={p.icon ?? 'thermometer'} size={17} />
                <span>{p.name ?? t(`pac.preset.${p.key}`)}</span>
                <small class="num">{deg(p.value)}</small>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    </section>

    <div class="cards">
      <section class="card">
        <h2><Icon name="flash" size={18} />{t('pac.maintenant')}</h2>
        <div class="trio">
          <div><small>{t('pac.electricite')}</small><b class="num">{electric == null ? '—' : watts(electric)}</b></div>
          <div class="cop" class:on={copNow != null}><small>{t('pac.cop')}</small><b class="num">{copNow == null ? '–' : num(copNow, 1)}</b></div>
          <div class="end"><small>{t('pac.chaleur')}</small><b class="num" class:hot={running}>{heat == null ? '—' : watts(heat)}</b></div>
        </div>
        <p class="muted small">{moment}</p>
      </section>

      <section class="card">
        <h2><Icon name="fire" size={18} />{t('pac.eau_chaude')}</h2>
        <p class="tank"><b class="num">{deg(v('hot_water_temperature'))}</b> <span class="muted">{t('pac.dans_ballon')}</span></p>
        {#if v('hot_water_setpoint') != null}
          <div class="gauge" aria-hidden="true">
            <div class="track"><span style:width="{tankAt(v('hot_water_temperature'))}%"></span><i style:left="{tankAt(v('hot_water_setpoint'))}%"></i></div>
            <div class="scale"><span>10°</span><span>{t('pac.jauge_consigne', { deg: deg(v('hot_water_setpoint')) })}</span><span>{deg(tankTop)}</span></div>
          </div>
        {/if}
        <div class="row">
          {#if v('hot_water_setpoint') != null}
            {@render setpointControl('hot_water_setpoint', t('pac.consigne_eau'))}
          {/if}
          {#if can('hot_water_mode')}
            <div class="segmented" role="radiogroup" aria-label={t('pac.mode_eau')}>
              {#each kindOf('hot_water_mode').values ?? [] as m (m)}
                <button role="radio" aria-checked={v('hot_water_mode') === m} class:active={v('hot_water_mode') === m} onclick={() => set('hot_water_mode', m, t('pac.act_mode_eau', { mode: m }))}>{m === 'Economy' ? t('pac.eco') : m === 'Normal' ? t('pac.normal') : m}</button>
              {/each}
            </div>
          {/if}
        </div>
        {#if can('force_dhw')}
          <button class="pill outline" class:lit={v('force_dhw') === true} aria-pressed={v('force_dhw') === true} onclick={() => set('force_dhw', v('force_dhw') !== true, t('pac.act_forcer'))}>
            <Icon name="fire" size={16} />{v('force_dhw') === true ? t('pac.forcee') : t('pac.forcer')}
          </button>
        {/if}
      </section>

      <section class="card">
        <h2><Icon name="bolt" size={18} />{t('pac.bilan')}</h2>
        <div class="stack">
          <small class="label">{t('pac.hier')}</small>
          <p class="sentence">
            <b class="num">{kwh(v('heating_energy_consumption_yesterday'))} kWh</b> {t('pac.hier_pour')}
            <b class="num">{kwh(v('heating_energy_production_yesterday'))} kWh</b> {t('pac.hier_chaleur')}
          </p>
          {#if paidShare != null}
            <div class="split">
              <div class="bar" aria-hidden="true"><span class="paid" style:width="{paidShare}%"></span><span class="free"></span></div>
              {#if copYesterday != null}<b class="num good">{t('pac.cop')} {num(copYesterday, 1)}</b>{/if}
            </div>
            <small>{t('pac.legende_bilan')}</small>
          {/if}
        </div>
        {#if month}
          <div class="row month">
            <small class="label">{t('pac.ce_mois')}</small>
            <span><b class="num">{kwh(month.kwh)} kWh</b>{#if month.cost != null} · <b class="num">{euros(month.cost, cur)}</b>{/if}</span>
          </div>
        {/if}
      </section>

      <section class="card wide">
        <h2><Icon name="water" size={18} />{t('pac.circuit')}</h2>
        <div class="trio">
          <div><small>{t('pac.depart')}</small><b class="num hot">{deg(v('water_feed_temperature'))}</b></div>
          <div class="via" aria-hidden="true">
            <svg width="44" height="18" viewBox="0 0 44 18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M2 9h38" /><path d="M34 3l6 6-6 6" /></svg>
            <small>{t('pac.radiateurs')}</small>
          </div>
          <div class="end"><small>{t('pac.retour')}</small><b class="num cool">{deg(v('water_return_temperature'))}</b></div>
        </div>
        <p class="muted small">
          {t('pac.ecart', { dt: drop == null ? '—' : num(drop, 1) })} · {t('pac.debit', { lmin: num(v('water_flow'), 0) })}
        </p>
      </section>

      <section class="card wide">
        <h2><Icon name="tune" size={18} />{t('pac.reglages')}</h2>
        {#if can('mode')}
          <label class="field" for="pac-mode">
            <small>{t('pac.mode_chauffe')}</small>
            <select id="pac-mode" value={v('mode')} onchange={(e) => set('mode', e.currentTarget.value, t('pac.act_mode', { mode: e.currentTarget.value }))}>
              {#each kindOf('mode').values ?? [] as m (m)}<option value={m}>{modeLabel(m)}</option>{/each}
            </select>
          </label>
        {/if}
        {#if v('flow_temperature_setpoint') != null}
          <div class="row">
            <small>{t('pac.consigne_depart')}</small>
            {#if byRoom}
              <small>{t('pac.depart_inutile')}</small>
            {:else}
              {@render setpointControl('flow_temperature_setpoint', t('pac.consigne_depart'))}
            {/if}
          </div>
        {/if}
      </section>
    </div>
  {/if}
</div>

<style>
  .pac-page {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
    container: pac / inline-size;
  }

  .top {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px 16px;
  }

  .top-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .card {
    display: grid;
    gap: 14px;
    align-content: start;
  }

  h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    font-size: 16px;
  }

  small {
    color: var(--ink-3);
    font-size: 12px;
  }

  .label {
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .comfort {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 24px 44px;
  }

  .inside {
    display: grid;
    gap: 2px;
    min-width: 140px;
  }

  .inside strong {
    font-size: 68px;
    font-weight: 300;
    letter-spacing: -0.04em;
    line-height: 1;
  }

  .aim {
    flex: 999 1 380px;
    display: grid;
    gap: 14px;
  }

  .aim-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .target {
    display: grid;
    justify-items: center;
    gap: 2px;
  }

  .chip i {
    display: inline-block;
    width: 8px;
    height: 8px;
    margin-right: 6px;
    border-radius: 50%;
    background: currentColor;
    vertical-align: 1px;
  }

  .set {
    display: inline-flex;
    align-items: center;
    gap: 10px;
  }

  .set b {
    font-size: 20px;
    font-weight: 700;
    min-width: 3.2ch;
    text-align: center;
  }

  .set.large {
    gap: 14px;
  }

  .set.large b {
    font-size: 36px;
  }

  .round {
    width: 40px;
    height: 40px;
    border-radius: 50%;
    border: 0;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 20px;
    cursor: pointer;
  }

  .set.large .round {
    width: 46px;
    height: 46px;
    font-size: 22px;
  }

  .round:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .presets {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(96px, 1fr));
    gap: 6px;
    padding: 6px;
    border-radius: 18px;
    background: var(--surface-2);
  }

  .presets button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 48px;
    padding: 0 10px;
    border: 0;
    border-radius: 13px;
    background: transparent;
    color: var(--ink-2);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  .presets button.active {
    background: var(--surface);
    color: var(--warm-ink);
    box-shadow: var(--shadow);
  }

  /* Wide: three cards, then two wider ones; narrower: two columns; a phone: one. */
  .cards {
    display: grid;
    grid-template-columns: repeat(6, minmax(0, 1fr));
    gap: 20px;
    align-items: stretch;
  }

  .cards > section {
    grid-column: span 2;
  }

  .cards > section.wide {
    grid-column: span 3;
  }

  @container pac (max-width: 1000px) {
    .cards {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }

    .cards > section,
    .cards > section.wide {
      grid-column: span 1;
    }
  }

  @container pac (max-width: 640px) {
    .cards {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .trio {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    align-items: center;
    gap: 10px;
  }

  .trio > div {
    display: grid;
    gap: 2px;
  }

  .trio .end {
    justify-items: end;
    text-align: right;
  }

  .trio b {
    font-size: 26px;
    font-weight: 700;
  }

  .cop {
    justify-items: center;
    padding: 8px 14px;
    border-radius: 14px;
    background: var(--surface-2);
  }

  .cop b {
    font-size: 19px;
    color: var(--ink-3);
  }

  .cop.on {
    background: var(--good-soft);
  }

  .cop.on b {
    color: var(--good);
  }

  .via {
    justify-items: center;
    color: var(--ink-3);
  }

  .hot {
    color: var(--warm-ink);
  }

  .cool {
    color: var(--cool);
  }

  .good {
    color: var(--good);
  }

  .tank {
    margin: 0;
  }

  .tank b {
    font-size: 32px;
    font-weight: 700;
  }

  .gauge {
    display: grid;
    gap: 6px;
  }

  .track {
    position: relative;
    height: 12px;
    border-radius: 999px;
    background: var(--surface-2);
  }

  .track span {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: 999px;
    background: var(--warm);
  }

  .track i {
    position: absolute;
    top: -5px;
    width: 3px;
    height: 22px;
    margin-left: -1px;
    border-radius: 2px;
    background: var(--ink);
  }

  .scale {
    display: flex;
    justify-content: space-between;
    font-size: 12px;
    color: var(--ink-3);
  }

  .stack {
    display: grid;
    gap: 6px;
  }

  .sentence {
    margin: 0;
    font-size: 16px;
    line-height: 1.45;
  }

  .split {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .split .bar {
    flex: 1;
    display: flex;
    height: 10px;
    border-radius: 999px;
    overflow: hidden;
    background: var(--surface-2);
  }

  .split .paid {
    background: var(--cool);
  }

  .split .free {
    flex: 1;
    background: var(--good-soft);
  }

  .month {
    padding-top: 12px;
    border-top: 1px solid var(--line);
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px 14px;
    flex-wrap: wrap;
  }

  .segmented {
    display: inline-flex;
    gap: 2px;
    padding: 3px;
    border-radius: 999px;
    background: var(--surface-2);
  }

  .segmented button {
    border: 0;
    background: transparent;
    color: var(--ink-2);
    min-height: 36px;
    padding: 0 14px;
    border-radius: 999px;
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  .segmented button.active {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 40px;
    border: 0;
    border-radius: 999px;
    padding: 0 16px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
    justify-self: start;
  }

  .pill.outline {
    border: 1.5px solid var(--line);
  }

  .pill.lit {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .pill.danger {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .field {
    display: grid;
    gap: 6px;
  }

  select {
    font: inherit;
    min-height: 44px;
    padding: 0 12px;
    border-radius: 12px;
    border: 1.5px solid var(--line);
    background: var(--surface);
    color: var(--ink);
  }

  .small {
    font-size: 13px;
    margin: 0;
  }

  button:focus-visible,
  select:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 2px;
  }
</style>
