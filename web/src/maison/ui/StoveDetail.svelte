<script>
  import { device, value, num } from '../lib/home.svelte.js';
  import { dayName } from '../lib/program.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  // A pellet stove's settings and programs (Micronova), as it reports them.
  // A program only switches the stove on then off: no setpoint of its own.
  let { id } = $props();

  const d = $derived(device(id));
  const on = $derived(value(id, 'on') === true);
  const power = $derived(value(id, 'power_level'));
  const powerMax = $derived(d?.points.find((p) => p.key === 'power_level')?.kind.max ?? 5);
  const real = $derived(value(id, 'real_power'));
  const fan = $derived(value(id, 'fan'));
  const ecoStop = $derived(value(id, 'eco_stop'));
  const delay = $derived(value(id, 'eco_stop_delay'));
  const flue = $derived(value(id, 'flue_temperature'));
  const alarm = $derived(value(id, 'alarm'));

  const programs = $derived.by(() => {
    try {
      const list = JSON.parse(value(id, 'programs') ?? 'null');
      return Array.isArray(list) ? list.map((p, i) => ({ ...p, n: i + 1 })) : null;
    } catch {
      return null;
    }
  });
  // A program never set (no day, no hours) is not listed, only counted.
  const set = $derived(programs?.filter((p) => p.days?.length || p.start !== p.stop) ?? []);
  const unset = $derived((programs?.length ?? 0) - set.length);

  const hm = (min) => `${String(Math.floor(min / 60)).padStart(2, '0')}:${String(min % 60).padStart(2, '0')}`;
  /** « lun. – ven. », « sam., dim. », « tous les jours » (1 = Monday). */
  function daysOf(days) {
    if (!days?.length) return t('commun.poele.aucun_jour');
    if (days.length === 7) return t('commun.poele.tous_les_jours');
    const runs = [];
    for (const day of [...days].sort((a, b) => a - b)) {
      const last = runs.at(-1);
      if (last && day === last[1] + 1) last[1] = day;
      else runs.push([day, day]);
    }
    const name = (day) => dayName(day - 1, locale());
    return runs
      .flatMap(([a, b]) => (b - a >= 2 ? [`${name(a)} – ${name(b)}`] : b > a ? [name(a), name(b)] : [name(a)]))
      .join(', ');
  }

  // Now, ticking each minute: the row of today, the stove's clock against ours.
  let now = $state(new Date());
  $effect(() => {
    const tick = setInterval(() => (now = new Date()), 60_000);
    return () => clearInterval(tick);
  });
  const today = $derived((now.getDay() + 6) % 7);
  const minute = $derived(now.getHours() * 60 + now.getMinutes());

  // The stove's own time, the one its programs follow. Read each minute: a
  // few minutes apart is the reading's age, not the clock; a gap is said to
  // the nearest five minutes.
  const stoveTime = $derived.by(() => {
    const m = /^(\d{4})-(\d\d)-(\d\d)T(\d\d):(\d\d)$/.exec(value(id, 'clock') ?? '');
    return m ? new Date(+m[1], m[2] - 1, +m[3], +m[4], +m[5]) : null;
  });
  const drift = $derived(stoveTime ? Math.round((stoveTime - now) / 300_000) * 5 : 0);
  const gap = (min) => {
    const h = Math.floor(min / 60);
    const m = min % 60;
    if (!h) return t('commun.poele.ecart_min', { m });
    return m ? t('commun.poele.ecart_h', { h, m: String(m).padStart(2, '0') }) : t('commun.poele.ecart_heures', { h });
  };

  const fanLabel = $derived(fan === 'off' ? t('commun.poele.ventilation_arret') : fan === 'auto' ? t('commun.poele.ventilation_auto') : fan);
  // A day's bars: the active programs that tick it.
  const bars = (day) => set.filter((p) => p.on && p.days?.includes(day + 1) && p.stop > p.start);
</script>

<div class="stove">
  <dl class="settings">
    {#if typeof power === 'number'}
      <div>
        <dt>{t('commun.poele.puissance')}</dt>
        <dd>
          <span class="num">{num(power)} / {num(powerMax)}</span>
          {#if on && typeof real === 'number' && real !== power}<small>{t('commun.poele.brule_a', { n: num(real) })}</small>{/if}
        </dd>
      </div>
    {/if}
    {#if fan != null}
      <div>
        <dt>{t('commun.poele.ventilation')}</dt>
        <dd>{fanLabel}</dd>
      </div>
    {/if}
    {#if ecoStop != null}
      <div>
        <dt>{t('commun.poele.eco_stop')}</dt>
        <dd>
          {#if ecoStop}{typeof delay === 'number' ? t('commun.poele.eco_stop_apres', { min: num(delay) }) : t('commun.poele.oui')}{:else}{t('commun.poele.non')}{/if}
        </dd>
      </div>
    {/if}
    {#if typeof flue === 'number'}
      <div>
        <dt>{t('commun.poele.fumees')}</dt>
        <dd class="num">{num(flue)}°</dd>
      </div>
    {/if}
    {#if stoveTime}
      <div>
        <dt>{t('commun.poele.horloge')}</dt>
        <dd>
          <span class="num">{hm(stoveTime.getHours() * 60 + stoveTime.getMinutes())}</span>
          <small>{stoveTime.toLocaleDateString(locale(), { weekday: 'short', day: 'numeric', month: 'short' })}</small>
        </dd>
      </div>
    {/if}
  </dl>
  {#if alarm && alarm !== 'OK'}
    <p class="warn"><Icon name="alert" size={16} />{t('commun.poele.alarme_code', { code: alarm })}</p>
  {/if}
  {#if stoveTime && Math.abs(drift) > 5}
    <p class="warn">
      <Icon name="clock" size={16} />{t(drift > 0 ? 'commun.poele.horloge_avance' : 'commun.poele.horloge_retard', { gap: gap(Math.abs(drift)) })}
    </p>
  {/if}

  {#if programs}
    <section class="programs">
      <h4>{t('commun.poele.programmes')}</h4>
      <p class="muted">{t('commun.poele.programmes_aide')}</p>
      {#if set.length}
        <ul>
          {#each set as p (p.n)}
            <li class:idle={!p.on}>
              <b>P{p.n}</b>
              <span class="num">{hm(p.start)} → {hm(p.stop)}</span>
              <span class="days">{daysOf(p.days)}</span>
              <em>{p.on ? t('commun.poele.actif') : t('commun.poele.inactif')}</em>
            </li>
          {/each}
        </ul>
        <div class="grid" role="table" aria-label={t('commun.poele.programmes')}>
          {#each Array.from({ length: 7 }, (_, i) => i) as day (day)}
            <div class="row" role="row">
              <span class="day" class:today={day === today} role="rowheader">{dayName(day, locale())}</span>
              <div class="bar">
                {#each bars(day) as p (p.n)}
                  <i
                    style="left:{(p.start / 1440) * 100}%;width:{((p.stop - p.start) / 1440) * 100}%"
                    title="P{p.n} · {hm(p.start)} → {hm(p.stop)}"
                    role="cell"></i>
                {/each}
                {#if day === today}<b class="here" style="left:{(minute / 1440) * 100}%"></b>{/if}
              </div>
            </div>
          {/each}
          <div class="row hours" aria-hidden="true">
            <span class="day"></span>
            <div class="ticks">
              {#each [0, 6, 12, 18, 24] as h (h)}<span style="left:{(h / 24) * 100}%">{h}</span>{/each}
            </div>
          </div>
        </div>
      {:else}
        <p class="muted">{t('commun.poele.aucun_programme')}</p>
      {/if}
      {#if unset > 0}
        <p class="muted">{t('commun.poele.programmes_vides', { count: unset })}</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .stove {
    display: grid;
    gap: 10px;
    width: 100%;
  }

  .settings {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 8px 14px;
  }

  dt {
    font-size: 11.5px;
    color: var(--ink-3);
    font-weight: 600;
  }

  dd {
    margin: 0;
    font-size: 15px;
    font-weight: 650;
  }

  dd small {
    margin-left: 4px;
    font-size: 12px;
    font-weight: 500;
    color: var(--ink-3);
  }

  .warn {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--warm-ink);
  }

  .warn :global(svg) {
    flex: none;
    margin-top: 1px;
  }

  .programs {
    display: grid;
    gap: 6px;
  }

  h4 {
    font-size: 14px;
    font-weight: 700;
  }

  .muted {
    font-size: 12px;
    color: var(--ink-3);
  }

  ul {
    display: grid;
    gap: 4px;
    list-style: none;
    padding: 0;
  }

  li {
    display: grid;
    grid-template-columns: 26px auto 1fr auto;
    align-items: baseline;
    gap: 8px;
    font-size: 13px;
  }

  li .days {
    color: var(--ink-2);
    min-width: 0;
  }

  li em {
    font-style: normal;
    font-size: 11.5px;
    font-weight: 650;
    color: var(--warm-ink);
  }

  li.idle {
    color: var(--ink-3);
  }

  li.idle .days,
  li.idle em {
    color: var(--ink-3);
  }

  .grid {
    display: grid;
    gap: 3px;
    margin-top: 4px;
  }

  .row {
    display: grid;
    grid-template-columns: 34px 1fr;
    align-items: center;
    gap: 6px;
  }

  .day {
    font-size: 11px;
    color: var(--ink-3);
    text-transform: capitalize;
  }

  .day.today {
    color: var(--ink);
    font-weight: 700;
  }

  .bar {
    position: relative;
    height: 12px;
    border-radius: 4px;
    overflow: hidden;
    background: var(--surface-2);
  }

  .bar i {
    position: absolute;
    top: 0;
    bottom: 0;
    background: var(--warm);
  }

  .here {
    position: absolute;
    top: -2px;
    bottom: -2px;
    width: 2px;
    background: var(--ink);
    transform: translateX(-50%);
  }

  .ticks {
    position: relative;
    height: 12px;
  }

  .ticks span {
    position: absolute;
    font-size: 10px;
    color: var(--ink-3);
    transform: translateX(-50%);
  }
</style>
