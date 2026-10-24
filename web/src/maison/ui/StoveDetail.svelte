<script>
  import { device, value, num, act } from '../lib/home.svelte.js';
  import { dayName } from '../lib/program.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  // A pellet stove's settings and programs (Micronova), set from here when
  // the stove lets Moli (never switched on or off here). A program only
  // switches the stove on then off: no setpoint of its own.
  let { id, name } = $props();

  const d = $derived(device(id));
  const point = (key) => d?.points.find((p) => p.key === key);
  const can = (key) => !!point(key)?.access?.write;
  const on = $derived(value(id, 'on') === true);
  const real = $derived(value(id, 'real_power'));
  const flue = $derived(value(id, 'flue_temperature'));
  const alarm = $derived(value(id, 'alarm'));

  // A setting sent shows at once and until the stove says it (an order
  // takes a few seconds through the cloud), or for half a minute at most.
  let sent = $state({});
  const shown = (key) => (key in sent ? sent[key] : value(id, key));
  $effect(() => {
    for (const key of Object.keys(sent)) if (value(id, key) === sent[key]) delete sent[key];
  });
  async function send(key, v, label) {
    sent[key] = v;
    const done = await act(`${id}/${key}`, v, label);
    if (done !== 'ok') delete sent[key];
    else setTimeout(() => sent[key] === v && delete sent[key], 30_000);
  }

  const power = $derived(shown('power_level'));
  const powerMin = $derived(point('power_level')?.kind.min ?? 1);
  const powerMax = $derived(point('power_level')?.kind.max ?? 5);
  const setPower = (n) => send('power_level', n, t('commun.poele.label_puissance', { name, n }));

  const fan = $derived(shown('fan'));
  const fanSteps = $derived(point('fan')?.kind.values ?? []);
  const stepLabel = (s) => (s === 'off' ? t('commun.poele.ventilation_arret') : s === 'auto' ? t('commun.poele.ventilation_auto') : s);
  const setFan = (s) => send('fan', s, t('commun.poele.label_ventilation', { name, what: stepLabel(s) }));

  const ecoStop = $derived(shown('eco_stop'));
  const delay = $derived(shown('eco_stop_delay'));
  const delayMax = $derived(point('eco_stop_delay')?.kind.max ?? 30);
  // Five minutes apart, and the stove's own value if it is between.
  const delays = $derived.by(() => {
    const list = Array.from({ length: Math.floor(delayMax / 5) + 1 }, (_, i) => i * 5);
    if (typeof delay === 'number' && !list.includes(delay)) list.push(delay);
    return list.sort((a, b) => a - b);
  });
  const setEco = (v) => send('eco_stop', v, t(v ? 'commun.poele.label_eco_oui' : 'commun.poele.label_eco_non', { name }));
  const setDelay = (m) => send('eco_stop_delay', m, t('commun.poele.label_delai', { name, min: m }));

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
    const label = (day) => dayName(day - 1, locale());
    return runs
      .flatMap(([a, b]) => (b - a >= 2 ? [`${label(a)} – ${label(b)}`] : b > a ? [label(a), label(b)] : [label(a)]))
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

  // A day's bars: the active programs that tick it.
  const bars = (day) => set.filter((p) => p.on && p.days?.includes(day + 1) && p.stop > p.start);

  // ---- programs, changed here ----------------------------------------------------
  // A copy is changed, nothing leaves before « Enregistrer »; then the
  // stove takes it register by register and says it when read again.
  let draft = $state(null);
  let saved = $state(null);
  const strip = (list) => list.map((p) => ({ on: p.on, start: p.start, stop: p.stop, days: [...p.days].sort((a, b) => a - b) }));
  const same = (a, b) => JSON.stringify(strip(a)) === JSON.stringify(strip(b));
  const changed = $derived(!!draft && !!programs && !same(draft, programs));
  const wrong = (p) => p.on && p.start >= p.stop;
  const covers = (p) => p.on && p.days.includes(today + 1) && p.start <= minute && minute < p.stop;
  const waiting = $derived(!!saved && !!programs && !same(saved, programs));

  function edit() {
    draft = strip(programs);
    saved = null;
  }
  /** `HH:MM` → minutes, to ten; an end at 00:00 is midnight at the end of the day. */
  function minutes(text, end) {
    const [h, m] = text.split(':').map(Number);
    if (!Number.isFinite(h) || !Number.isFinite(m)) return null;
    const total = Math.min(1440, Math.round((h * 60 + m) / 10) * 10);
    return end && total === 0 ? 1440 : total;
  }
  function setTime(i, part, text) {
    const v = minutes(text, part === 'stop');
    if (v != null) draft[i][part] = v;
  }
  function toggleDay(i, day) {
    const days = draft[i].days;
    draft[i].days = days.includes(day) ? days.filter((x) => x !== day) : [...days, day].sort((a, b) => a - b);
  }
  async function save() {
    const list = strip(draft);
    const done = await act(`${id}/programs`, JSON.stringify(list), t('commun.poele.label_programmes', { name }));
    if (done === 'ok') {
      saved = list;
      draft = null;
    }
  }
</script>

<div class="stove">
  <dl class="settings">
    {#if typeof power === 'number'}
      <div>
        <dt>{t('commun.poele.puissance')}</dt>
        <dd>
          {#if can('power_level')}
            <span class="step" role="group" aria-label={t('commun.poele.puissance')}>
              <button onclick={() => setPower(power - 1)} disabled={power <= powerMin} aria-label={t('commun.poele.moins')}>−</button>
              <b class="num">{num(power)} / {num(powerMax)}</b>
              <button onclick={() => setPower(power + 1)} disabled={power >= powerMax} aria-label={t('commun.poele.plus')}>+</button>
            </span>
          {:else}
            <span class="num">{num(power)} / {num(powerMax)}</span>
          {/if}
          {#if on && typeof real === 'number' && real !== power}<small>{t('commun.poele.brule_a', { n: num(real) })}</small>{/if}
        </dd>
      </div>
    {/if}
    {#if fan != null}
      <div>
        <dt>{t('commun.poele.ventilation')}</dt>
        <dd>
          {#if can('fan')}
            <select value={fan} onchange={(e) => setFan(e.currentTarget.value)} aria-label={t('commun.poele.ventilation')}>
              {#each fanSteps as s (s)}<option value={s}>{stepLabel(s)}</option>{/each}
            </select>
          {:else}
            {stepLabel(fan)}
          {/if}
        </dd>
      </div>
    {/if}
    {#if ecoStop != null}
      <div class:wide={can('eco_stop')}>
        <dt>{t('commun.poele.eco_stop')}</dt>
        <dd>
          {#if can('eco_stop')}
            <span class="eco">
              <button class="toggle" class:active={ecoStop} onclick={() => setEco(!ecoStop)} aria-pressed={ecoStop}>
                {ecoStop ? t('commun.poele.oui') : t('commun.poele.non')}
              </button>
              {#if ecoStop && typeof delay === 'number' && can('eco_stop_delay')}
                <select value={delay} onchange={(e) => setDelay(Number(e.currentTarget.value))} aria-label={t('commun.poele.eco_stop_delai')}>
                  {#each delays as m (m)}<option value={m}>{t('commun.poele.apres_min', { min: m })}</option>{/each}
                </select>
              {/if}
            </span>
          {:else if ecoStop}
            {typeof delay === 'number' ? t('commun.poele.eco_stop_apres', { min: num(delay) }) : t('commun.poele.oui')}
          {:else}
            {t('commun.poele.non')}
          {/if}
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
  {#if ecoStop && can('eco_stop')}
    <p class="muted">{t('commun.poele.eco_stop_aide')}</p>
  {/if}
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
      <div class="head">
        <h4>{t('commun.poele.programmes')}</h4>
        {#if can('programs') && !draft}
          <button class="link" onclick={edit}><Icon name="tune" size={16} />{t('commun.poele.modifier')}</button>
        {/if}
      </div>
      <p class="muted">{t('commun.poele.programmes_aide')}</p>
      {#if draft}
        <ul class="edit">
          {#each draft as p, i (i)}
            <li class:idle={!p.on}>
              <div class="line">
                <b>P{i + 1}</b>
                <input type="time" step="600" value={hm(p.start)} onchange={(e) => setTime(i, 'start', e.currentTarget.value)} aria-label={t('commun.poele.debut', { n: i + 1 })} />
                <span>→</span>
                <input type="time" step="600" value={hm(p.stop % 1440)} onchange={(e) => setTime(i, 'stop', e.currentTarget.value)} aria-label={t('commun.poele.fin', { n: i + 1 })} />
              </div>
              <div class="days" role="group" aria-label={t('commun.poele.jours', { n: i + 1 })}>
                <button class="toggle" class:active={p.on} onclick={() => (draft[i].on = !p.on)} aria-pressed={p.on}>
                  {p.on ? t('commun.poele.actif') : t('commun.poele.inactif')}
                </button>
                {#each Array.from({ length: 7 }, (_, k) => k + 1) as day (day)}
                  <button class="day-chip" class:active={p.days.includes(day)} onclick={() => toggleDay(i, day)} aria-pressed={p.days.includes(day)} title={dayName(day - 1, locale(), 'long')}>
                    {dayName(day - 1, locale(), 'narrow')}
                  </button>
                {/each}
              </div>
              {#if wrong(p)}
                <p class="warn">{t('commun.poele.fin_avant_debut')}</p>
              {:else if covers(p) !== covers(programs[i])}
                <p class="warn">{t(covers(p) ? 'commun.poele.peut_allumer' : 'commun.poele.peut_eteindre')}</p>
              {/if}
            </li>
          {/each}
        </ul>
        <div class="actions">
          <button class="ghost" onclick={() => (draft = null)}>{t('commun.annuler')}</button>
          <button class="primary" onclick={save} disabled={!changed || draft.some(wrong)}>{t('commun.poele.enregistrer')}</button>
        </div>
      {:else}
        {#if saved}
          <p class="status">{waiting ? t('commun.poele.envoye') : t('commun.poele.enregistre')}</p>
        {/if}
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
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 10px 14px;
    margin: 0;
  }

  .wide {
    grid-column: 1 / -1;
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

  .step,
  .eco {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .step button {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border: none;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
  }

  .step button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  select,
  input[type='time'] {
    padding: 4px 8px;
    border: none;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 14px;
    font-weight: 600;
  }

  select {
    cursor: pointer;
  }

  .toggle {
    padding: 4px 12px;
    border: none;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
  }

  .toggle.active {
    background: var(--warm);
    color: #fff;
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

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  h4 {
    font-size: 14px;
    font-weight: 700;
  }

  .link {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 12px 5px 9px;
    border: none;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 12.5px;
    font-weight: 650;
    cursor: pointer;
  }

  .muted {
    font-size: 12px;
    color: var(--ink-3);
  }

  .status {
    font-size: 12.5px;
    font-weight: 650;
    color: var(--ink-2);
  }

  ul {
    display: grid;
    gap: 4px;
    list-style: none;
    padding: 0;
    margin: 0;
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

  /* Changing: a program per block, its line then its days. */
  ul.edit {
    gap: 12px;
  }

  ul.edit li {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 6px;
    padding-bottom: 10px;
    border-bottom: 1px solid var(--surface-2);
  }

  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .line b {
    width: 24px;
  }

  ul.edit .days {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 3px;
  }

  ul.edit .days .toggle {
    min-width: 64px;
    margin-right: 4px;
    padding: 4px 8px;
  }

  ul.edit .days .day-chip {
    width: 28px;
    height: 28px;
    border: none;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 12.5px;
    font-weight: 700;
    text-transform: uppercase;
    cursor: pointer;
  }

  ul.edit .days .day-chip.active {
    background: var(--warm);
    color: #fff;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .actions button {
    padding: 8px 16px;
    border: none;
    border-radius: 999px;
    font: inherit;
    font-size: 14px;
    font-weight: 650;
    cursor: pointer;
  }

  .ghost {
    background: var(--surface-2);
    color: var(--ink);
  }

  .primary {
    background: var(--warm);
    color: #fff;
  }

  .primary:disabled {
    opacity: 0.4;
    cursor: default;
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
