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

{#snippet setpointControl(key, what)}
  <div class="set">
    <button class="round" disabled={!can(key)} onclick={() => nudge(key, -1, what)} aria-label={t('pac.moins', { what })}>−</button>
    <b class="num">{deg(v(key))}</b>
    <button class="round" disabled={!can(key)} onclick={() => nudge(key, 1, what)} aria-label={t('pac.plus', { what })}>+</button>
  </div>
{/snippet}

<div class="pac-page">
  <header>
    <h1 class="page-title">{t('pac.titre')}</h1>
    <p class="page-sub">{d?.native_name ?? ''}</p>
  </header>

  {#if !d}
    <p class="muted">{t('pac.aucune')}</p>
  {:else}
    <section class="card comfort">
      <div class="head">
        <h2><Icon name="thermometer" size={18} />{t('pac.confort')}</h2>
        <span class="chip {PHASE_TONE[phase] ?? ''}">{ok ? t(`pac.phase.${phase ?? 'inconnu'}`) : t('pac.hors_ligne')}</span>
      </div>
      <div class="room">
        <div class="big-temp">
          <small>{t('pac.dedans')}</small>
          <strong class="num">{deg(v('room_temperature'))}</strong>
        </div>
        <div class="target">
          <small>{byRoom ? t('pac.consigne') : t('pac.consigne_inactive')}</small>
          {@render setpointControl('room_temperature_setpoint', t('pac.consigne_piece'))}
        </div>
        <div class="outside">
          <small>{t('pac.dehors')}</small>
          <b class="num">{deg(v('outside_temperature'))}</b>
        </div>
      </div>
      {#if can('room_temperature_setpoint')}
        <div class="presets" role="group" aria-label={t('pac.presets')}>
          {#each presets as p (p.value)}
            <button class:active={roomSet === p.value} onclick={() => set('room_temperature_setpoint', p.value, t('pac.act_preset', { name: p.name ?? t(`pac.preset.${p.key}`), deg: num(p.value, 1) }))}>
              <Icon name={p.icon ?? 'thermometer'} size={18} />
              <span>{p.name ?? t(`pac.preset.${p.key}`)}</span>
              <small class="num">{deg(p.value)}</small>
            </button>
          {/each}
        </div>
      {/if}
    </section>

    <div class="grid">
      <section class="card now">
        <h2><Icon name="flash" size={18} />{t('pac.maintenant')}</h2>
        <div class="flow-line">
          <div><small>{t('pac.electricite')}</small><b class="num">{electric == null ? '—' : watts(electric)}</b></div>
          <span class="arrow" aria-hidden="true">→</span>
          <div><small>{t('pac.chaleur')}</small><b class="num">{heat == null ? '—' : watts(heat)}</b></div>
          <div class="cop"><small>{t('pac.cop')}</small><b class="num">{copNow == null ? '—' : num(copNow, 1)}</b></div>
        </div>
        <div class="compressor">
          <small>{t('pac.compresseur')}</small>
          {#if running}
            <div class="bar" aria-hidden="true"><span style:width="{Math.min(100, (v('frequency') / 120) * 100)}%"></span></div>
            <b class="num">{num(v('frequency'), 0)} Hz</b>
          {:else}
            <b class="muted">{t('pac.au_repos')}</b>
          {/if}
        </div>
      </section>

      <section class="card water">
        <h2><Icon name="water" size={18} />{t('pac.circuit')}</h2>
        <div class="loop">
          <div><small>{t('pac.depart')}</small><b class="num hot">{deg(v('water_feed_temperature'))}</b></div>
          <span class="arrow" aria-hidden="true">→ <Icon name="home" size={16} /> →</span>
          <div><small>{t('pac.retour')}</small><b class="num cool">{deg(v('water_return_temperature'))}</b></div>
        </div>
        <p class="muted small">
          {t('pac.ecart', { dt: drop == null ? '—' : num(drop, 1) })} · {t('pac.debit', { lmin: num(v('water_flow'), 0) })}
        </p>
        {#if v('flow_temperature_setpoint') != null}
          <div class="row">
            <span>{byRoom ? t('pac.consigne_depart_auto') : t('pac.consigne_depart')}</span>
            {@render setpointControl('flow_temperature_setpoint', t('pac.consigne_depart'))}
          </div>
        {/if}
      </section>

      <section class="card dhw">
        <h2><Icon name="fire" size={18} />{t('pac.eau_chaude')}</h2>
        <div class="row">
          <span>{t('pac.ballon')}</span>
          <b class="num big-ish">{deg(v('hot_water_temperature'))}</b>
        </div>
        {#if v('hot_water_setpoint') != null}
          <div class="row">
            <span>{t('pac.consigne')}</span>
            {@render setpointControl('hot_water_setpoint', t('pac.consigne_eau'))}
          </div>
        {/if}
        <div class="row actions">
          {#if can('hot_water_mode')}
            <div class="segmented" role="radiogroup" aria-label={t('pac.mode_eau')}>
              {#each kindOf('hot_water_mode').values ?? [] as m (m)}
                <button role="radio" aria-checked={v('hot_water_mode') === m} class:active={v('hot_water_mode') === m} onclick={() => set('hot_water_mode', m, t('pac.act_mode_eau', { mode: m }))}>{m === 'Economy' ? t('pac.eco') : m === 'Normal' ? t('pac.normal') : m}</button>
              {/each}
            </div>
          {/if}
          {#if can('force_dhw')}
            <button class="pill" class:lit={v('force_dhw') === true} aria-pressed={v('force_dhw') === true} onclick={() => set('force_dhw', v('force_dhw') !== true, t('pac.act_forcer'))}>
              <Icon name="fire" size={16} />{v('force_dhw') === true ? t('pac.forcee') : t('pac.forcer')}
            </button>
          {/if}
        </div>
      </section>

      <section class="card bill">
        <h2><Icon name="bolt" size={18} />{t('pac.bilan')}</h2>
        <div class="row">
          <span>{t('pac.hier')}</span>
          <b class="num">{kwh(v('heating_energy_consumption_yesterday'))} → {kwh(v('heating_energy_production_yesterday'))} kWh</b>
        </div>
        <p class="muted small">{t('pac.hier_detail', { cop: copYesterday == null ? '—' : num(copYesterday, 1) })}</p>
        {#if month}
          <div class="row">
            <span>{t('pac.ce_mois')}</span>
            <b class="num">{kwh(month.kwh)} kWh{month.cost != null ? ` · ${euros(month.cost, cur)}` : ''}</b>
          </div>
        {/if}
      </section>
    </div>

    <section class="card settings">
      <h2><Icon name="tune" size={18} />{t('pac.reglages')}</h2>
      <div class="row actions">
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
      {#if can('mode')}
        <label class="row" for="pac-mode">
          <span>{t('pac.mode_chauffe')}</span>
          <select id="pac-mode" value={v('mode')} onchange={(e) => set('mode', e.currentTarget.value, t('pac.act_mode', { mode: e.currentTarget.value }))}>
            {#each kindOf('mode').values ?? [] as m (m)}<option value={m}>{modeLabel(m)}</option>{/each}
          </select>
        </label>
      {/if}
    </section>
  {/if}
</div>

<style>
  .pac-page {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  .card {
    display: grid;
    gap: 14px;
  }

  h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    font-size: 16px;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  small {
    color: var(--ink-3);
    font-size: 12px;
  }

  .room {
    display: grid;
    grid-template-columns: 1.2fr 1fr auto;
    align-items: end;
    gap: 16px;
  }

  .room > div {
    display: grid;
    gap: 4px;
  }

  .big-temp strong {
    font-size: 54px;
    font-weight: 300;
    letter-spacing: -0.03em;
    line-height: 1;
  }

  .outside b {
    font-size: 24px;
    font-weight: 500;
  }

  .set {
    display: inline-flex;
    align-items: center;
    gap: 10px;
  }

  .set b {
    font-size: 24px;
    font-weight: 600;
    min-width: 3.2ch;
    text-align: center;
  }

  .round {
    width: 38px;
    height: 38px;
    border-radius: 50%;
    border: 0;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 20px;
    cursor: pointer;
  }

  .round:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .presets {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(90px, 1fr));
    gap: 8px;
  }

  .presets button {
    display: grid;
    justify-items: center;
    gap: 4px;
    padding: 12px 8px;
    border-radius: 14px;
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  .presets button.active {
    border-color: var(--warm);
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 20px;
    align-items: start;
  }

  .flow-line,
  .loop {
    display: flex;
    align-items: end;
    gap: 14px;
    flex-wrap: wrap;
  }

  .flow-line > div,
  .loop > div {
    display: grid;
    gap: 2px;
  }

  .flow-line b,
  .loop b {
    font-size: 22px;
    font-weight: 600;
  }

  .arrow {
    color: var(--ink-3);
    padding-bottom: 4px;
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .cop {
    margin-left: auto;
    padding: 6px 12px;
    border-radius: 12px;
    background: var(--good-soft);
  }

  .cop b {
    color: var(--good);
  }

  .hot {
    color: var(--warm-ink);
  }

  .cool {
    color: var(--cool);
  }

  .compressor {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 10px;
  }

  .compressor .bar {
    height: 8px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }

  .compressor .bar span {
    display: block;
    height: 100%;
    background: var(--warm);
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px 14px;
    flex-wrap: wrap;
  }

  .row.actions {
    justify-content: flex-start;
  }

  .big-ish {
    font-size: 22px;
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
    padding: 6px 12px;
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
    border: 0;
    border-radius: 999px;
    padding: 8px 14px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  .pill.lit {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .pill.danger {
    background: var(--alert-soft);
    color: var(--alert);
  }

  select {
    font: inherit;
    padding: 6px 10px;
    border-radius: 10px;
    border: 1px solid var(--line);
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

  @media (max-width: 760px) {
    .grid {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  @media (max-width: 520px) {
    .room {
      grid-template-columns: 1fr 1fr;
    }

    .room .target {
      grid-column: 1 / -1;
      grid-row: 2;
    }
  }
</style>
