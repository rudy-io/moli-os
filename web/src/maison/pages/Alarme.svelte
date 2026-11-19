<script>
  import { hub, value, since, act, home, relative, clock } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import AlarmSettings from '../ui/AlarmSettings.svelte';
  import { isAlarm, setupOf, MODES, sensorActive, blocking, toneOf, secondsLeft, delay, journal } from '../lib/alarm.js';

  const alarm = $derived(Object.values(hub.devices).find(isAlarm));
  const id = $derived(alarm?.id);
  const setup = $derived(setupOf(alarm));
  const modes = $derived(MODES.filter((m) => setup.modes[m]));
  const state = $derived(value(id, 'state') ?? 'disarmed');
  const current = $derived(value(id, 'mode') ?? 'off');
  const tone = $derived(toneOf(state));
  const cause = $derived(value(id, 'cause') ?? '');
  const until = $derived(value(id, 'until') ?? 0);
  const valueOf = (point) => {
    const [device, key] = point.split('/');
    return value(device, key);
  };

  // A countdown moves every second while a delay runs.
  let now = $state(Date.now());
  $effect(() => {
    if (!until) return;
    const every = setInterval(() => (now = Date.now()), 500);
    return () => clearInterval(every);
  });
  // The clock read when it is drawn: `now` only says when to draw again. A
  // new delay is drawn before any tick, and the last tick may be old (the
  // page opened long ago): it would show a wrong number at first.
  const left = $derived.by(() => {
    void now;
    return secondsLeft(until, Date.now());
  });

  const ICONS = { home: 'home', night: 'moon', away: 'walk' };
  const modeName = (m) => t(`alarme.mode.${m}`);
  const words = (s) => {
    const d = delay(s);
    return d ? t(d.unit === 's' ? 'alarme.secondes' : 'alarme.minutes', { n: d.n }) : t('alarme.aucun');
  };

  const title = $derived.by(() => {
    const m = current !== 'off' ? modeName(current) : '';
    if (state === 'arming') return t('alarme.etat.armement', { mode: m });
    if (state === 'pending') return t('alarme.etat.entree');
    if (state === 'triggered') return t('alarme.etat.declenchee');
    if (state.startsWith('armed_')) return t('alarme.etat.armee', { mode: m });
    return t('alarme.etat.desarmee');
  });
  const detail = $derived.by(() => {
    if (state === 'arming') return t('alarme.detail.sortie', { n: left ?? 0 });
    if (state === 'pending') return t('alarme.detail.entree', { n: left ?? 0, cause });
    if (state === 'triggered') return cause ? t('alarme.detail.cause', { cause }) : '';
    const ts = since(id, 'state');
    return ts ? t('alarme.detail.depuis', { quand: relative(ts, home.now) }) : '';
  });

  let busy = $state(null);
  let editing = $state(false);
  async function set(mode) {
    busy = mode;
    await act(`${id}/mode`, mode, mode === 'off' ? t('alarme.desarmer') : t('alarme.armer_en', { mode: modeName(mode) }));
    busy = null;
  }

  // What a sensor says now, in words.
  function reading(s) {
    const v = valueOf(s.point);
    if (v === null || v === undefined) {
      // A camera zone says nothing until something walks in.
      return { text: t(s.point.includes(':zone-') ? 'alarme.capteur.rien_vu' : 'alarme.capteur.inconnu'), on: false };
    }
    const on = sensorActive(s.point, v);
    if (s.point.endsWith('/contact')) return { text: t(on ? 'alarme.capteur.ouverte' : 'alarme.capteur.fermee'), on };
    if (typeof v === 'number') return { text: on ? t('alarme.capteur.presents', { n: v }) : t('alarme.capteur.personne'), on };
    return { text: t(on ? 'alarme.capteur.actif' : 'alarme.capteur.calme'), on };
  }

  // What happened, from the history.
  let past = $state([]);
  async function series(key) {
    const r = await fetch(`/api/history?point=${encodeURIComponent(`${id}/${key}`)}&hours=168&points=500`);
    return r.ok ? ((await r.json()).raw ?? []) : [];
  }
  $effect(() => {
    if (!id) return;
    state; // reread when it moves
    Promise.all([series('event'), series('cause'), series('silent')])
      .then(([events, causes, silents]) => (past = journal(events, causes, silents).slice(0, 30)))
      .catch(() => {});
  });
  const EVENT_ICONS = { armed: 'lock', arming: 'timer', disarmed: 'shield', arm_failure: 'warning', pending: 'walk', triggered: 'alert', silent: 'warning' };
</script>

<div class="alarme">
  <header>
    <h1 class="page-title">{t('alarme.titre')}</h1>
    <p class="page-sub">{t('alarme.sous_titre')}</p>
  </header>

  {#if !alarm}
    <p class="muted">{t('alarme.aucune')}</p>
  {:else}
    <!-- One block: where the alarm stands, and the choice of its mode. -->
    <section class="panel tone-{tone}">
      <div class="status" aria-live="polite">
        <span class="badge"><Icon name={tone === 'off' ? 'shield' : tone === 'alert' ? 'alert' : 'lock'} size={30} /></span>
        <div class="what">
          <h2>{title}</h2>
          {#if detail}<p>{detail}</p>{/if}
        </div>
        {#if left !== null && (state === 'arming' || state === 'pending')}
          <span class="count" aria-hidden="true">{left}</span>
        {/if}
      </div>

      <div class="switch" role="group" aria-label={t('alarme.modes')} style:--n={modes.length + 1}>
        <button class="opt" class:on={current === 'off'} aria-pressed={current === 'off'} disabled={busy !== null || current === 'off'} onclick={() => set('off')}>
          <span class="opt-head"><Icon name="shield" size={20} /><b>{t('alarme.etat.desarmee')}</b></span>
          <small>{t('alarme.rien_surveille')}</small>
        </button>
        {#each modes as m (m)}
          {@const d = setup.modes[m]}
          {@const open = blocking(setup, m, valueOf)}
          {@const watched = setup.sensors.filter((s) => s.modes.includes(m)).length}
          {@const on = current === m}
          <!-- Once a mode is chosen (leaving, armed, entering, ringing), only
               « off » is taken: the alarm refuses another mode, so it is not
               offered. -->
          <button class="opt" class:on class:blocked={open.length > 0 && !on && current === 'off'} aria-pressed={on} disabled={busy !== null || on || current !== 'off' || open.length > 0} onclick={() => set(m)}>
            <span class="opt-head">
              <Icon name={open.length && !on && current === 'off' ? 'warning' : ICONS[m]} size={20} /><b>{modeName(m)}</b>
            </span>
            <small>
              {t('alarme.sortie')} {words(d.exit)} · {t('alarme.entree')} {words(d.entry)} · {t('alarme.sirene')}
              {d.trigger ? words(d.trigger) : t('alarme.jusqu_a_desarmer')} · {t('alarme.capteurs_surveilles', { n: watched })}
            </small>
          </button>
        {/each}
      </div>

      <div class="tools">
        {#if state === 'disarmed'}
          <button class="link" onclick={() => (editing = !editing)} aria-expanded={editing}>
            <Icon name="tune" size={16} />{t('alarme.reglages.titre')}
          </button>
        {:else}
          <small class="muted">{t('alarme.desarmer_pour_changer')}</small>
        {/if}
      </div>

      {#each modes.filter((m) => current === 'off' && blocking(setup, m, valueOf).length) as m (m)}
        <p class="why"><Icon name="warning" size={15} />{t('alarme.impossible_mode', { mode: modeName(m), capteurs: blocking(setup, m, valueOf).join(', ') })}</p>
      {/each}
    </section>

    {#if editing && state === 'disarmed'}
      <AlarmSettings {id} {setup} onclose={() => (editing = false)} />
    {/if}

    <div class="cols">
      <section class="card sensors">
        <h3>{t('alarme.capteurs')}</h3>
        <ul>
          {#each setup.sensors as s (s.point)}
            {@const r = reading(s)}
            {@const [dev, key] = s.point.split('/')}
            <li>
              <span class="dot" class:on={r.on}></span>
              <span class="name">{s.name}</span>
              <span class="state" class:on={r.on}>{r.text}</span>
              <span class="where">
                {#each s.modes as m (m)}<span class="tag">{modeName(m)}</span>{/each}
                {#if since(dev, key)}<small>{relative(since(dev, key), home.now)}</small>{/if}
              </span>
            </li>
          {/each}
        </ul>
      </section>

      <section class="card past">
        <h3>{t('alarme.journal')}</h3>
        {#if past.length}
          <ul>
            {#each past as e (e.ts + e.what)}
              <li class="ev-{e.what}">
                <Icon name={EVENT_ICONS[e.what] ?? 'history'} size={16} />
                <span>
                  <b>{t(`alarme.evenement.${e.what}`)}</b>
                  {#if e.cause}<small>{e.cause}</small>{/if}
                </span>
                <time>{clock(e.ts)} · {relative(e.ts, home.now)}</time>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="muted">{t('alarme.rien')}</p>
        {/if}
      </section>
    </div>
  {/if}
</div>

<style>
  .alarme {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  .panel {
    display: grid;
    gap: 18px;
    padding: 22px 24px;
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
    --tone: var(--good);
    --tone-soft: var(--good-soft);
  }

  .panel.tone-armed {
    --tone: var(--cool);
    --tone-soft: var(--cool-soft);
  }

  .panel.tone-arming,
  .panel.tone-warn {
    --tone: var(--warm);
    --tone-soft: var(--warm-soft);
  }

  .panel.tone-alert {
    --tone: var(--alert);
    --tone-soft: var(--alert-soft);
    animation: pulse 1.6s ease-in-out infinite;
  }

  @keyframes pulse {
    50% {
      box-shadow: 0 0 0 6px var(--alert-soft);
    }
  }

  .status {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 18px;
  }

  .badge {
    width: 60px;
    height: 60px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--tone-soft);
    color: var(--tone);
  }

  .what h2 {
    font-size: 26px;
    font-weight: 750;
    text-wrap: balance;
  }

  .what p {
    margin-top: 4px;
    color: var(--ink-2);
    font-size: 15px;
  }

  .count {
    font-size: 44px;
    font-weight: 800;
    color: var(--tone);
    font-variant-numeric: tabular-nums;
    min-width: 2ch;
    text-align: right;
  }

  /* The mode, chosen in one place: a single control, the current one lit. */
  .switch {
    display: grid;
    grid-template-columns: repeat(var(--n), minmax(0, 1fr));
    gap: 6px;
    padding: 6px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .opt {
    display: grid;
    gap: 4px;
    align-content: start;
    padding: 12px 14px;
    border: 0;
    border-radius: calc(var(--r-md) - 4px);
    background: transparent;
    color: var(--ink);
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: background 0.15s var(--ease);
  }

  .opt:not(:disabled):hover {
    background: var(--surface);
  }

  .opt:disabled {
    cursor: default;
  }

  .opt.on {
    background: var(--tone);
    color: var(--surface);
    box-shadow: var(--shadow);
  }

  .opt.on small {
    color: inherit;
    opacity: 0.85;
  }

  .opt.blocked {
    color: var(--ink-3);
  }

  .opt:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 2px;
  }

  .opt-head {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 16px;
  }

  .opt small {
    font-size: 12.5px;
    line-height: 1.4;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
  }

  .why {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: -6px 0 0;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    background: var(--warm-soft);
    color: var(--warm-ink);
    font-size: 13px;
    font-weight: 600;
  }

  .tools {
    display: flex;
    justify-content: flex-end;
    margin-top: -6px;
  }

  .link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border: 0;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .cols {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 380px), 1fr));
    gap: 20px;
    align-items: start;
  }

  .card h3 {
    font-size: 13px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-3);
    margin-bottom: 10px;
  }

  .card ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
  }

  .sensors li {
    display: grid;
    grid-template-columns: auto 1fr auto;
    grid-template-areas: 'dot name state' '. where where';
    align-items: center;
    gap: 2px 10px;
    padding: 10px 0;
    border-top: 1px solid var(--line);
  }

  .sensors li:first-child,
  .past li:first-child {
    border-top: 0;
  }

  .dot {
    grid-area: dot;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--good);
  }

  .dot.on {
    background: var(--warm);
  }

  .name {
    grid-area: name;
    font-weight: 600;
  }

  .state {
    grid-area: state;
    font-size: 14px;
    color: var(--ink-2);
  }

  .state.on {
    color: var(--warm-ink);
    font-weight: 700;
  }

  .where {
    grid-area: where;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .tag {
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--surface-2);
    font-size: 12px;
    color: var(--ink-2);
  }

  .where small {
    margin-left: auto;
    color: var(--ink-3);
    font-size: 12px;
  }

  .past li {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: start;
    gap: 10px;
    padding: 6px 0;
    border-top: 1px solid var(--line);
    color: var(--ink-2);
  }

  /* Thirty events would run far below the sensors: the latest show, the
     rest scroll within the card. */
  .past ul {
    max-height: 340px;
    overflow-y: auto;
    overscroll-behavior: contain;
    padding-right: 4px;
  }

  .past li span {
    display: grid;
  }

  .past li b {
    color: var(--ink);
    font-weight: 600;
  }

  .past li small {
    color: var(--ink-3);
  }

  .past time {
    font-size: 12.5px;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .past .ev-triggered :global(svg),
  .past .ev-arm_failure :global(svg) {
    color: var(--alert);
  }

  .muted {
    color: var(--ink-3);
  }

  @media (max-width: 640px) {
    .switch {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .panel.tone-alert {
      animation: none;
    }

    .opt {
      transition: none;
    }
  }
</style>
