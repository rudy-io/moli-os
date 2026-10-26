<script>
  // A heater's week program in full, opened from its card's strip: what runs
  // now, what each colour means, the week in large bars that say their order
  // (its glyph, as on the card's buttons) or their setpoint, and a day's runs
  // written out. The cog edits it: a brush painted along a day, a precise
  // range, a day copied to others, the week sent to other heaters of the
  // same wiring too; the range and the brush are chosen in either order;
  // nothing leaves before « Save ».
  import { onMount } from 'svelte';
  import { value, num, act, hub, nameOf } from '../lib/home.svelte.js';
  import { parseWeek, spans, hhmm, warmth, nowIn, dayName, SLOTS, slotsOf, runsOf, sentRuns, unread } from '../lib/program.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  let { id, name, wiring, modeLabel, modes, onclose } = $props();

  let dialog;
  // Opened on the dialog itself: on a phone its first button (« Modifier »)
  // would otherwise wear the keyboard's focus ring.
  onMount(() => {
    dialog.showModal();
    dialog.focus();
  });

  const read = $derived(parseWeek(value(id, 'week_program')));
  // Editing: the week being drawn, seven days of 96 quarters (null: a day
  // not read). Saved: what was sent, shown until the heater says it back.
  let draft = $state(null);
  let sent = $state(null);
  const editing = $derived(draft != null);
  const week = $derived(draft ? draft.map((d) => d && runsOf(d)) : (sent ?? read));
  // Compared quarter by quarter: the same week, however its runs are written.
  const same = (a, b) => JSON.stringify(a?.map(slotsOf)) === JSON.stringify(b?.map(slotsOf));
  $effect(() => {
    if (!sent) return;
    if (same(read, sent)) sent = null;
    const late = setTimeout(() => (sent = null), 30_000);
    return () => clearTimeout(late);
  });

  const bySetpoint = $derived(wiring === 'switch');
  // On switch wiring, a program read before the module was rewired has no
  // setpoints yet: said, and not edited until Moli has read it again.
  const stale = $derived(unread(read, wiring));
  const deg = (c) => num(c, c % 1 ? 1 : 0);
  // What a run says, and the class that colours it.
  const what = (s) => (s.celsius != null ? `${deg(s.celsius)} °C` : modeLabel(s.order));
  const tint = (s) => (s.celsius != null ? warmth(s.celsius) : (s.order ?? 'unknown'));

  // The orders a program holds on this wiring, warmest first, and what each
  // means on a pilot wire.
  const ORDERS = { four: ['comfort', 'eco', 'hors'], six: ['comfort', 'comfort_1', 'comfort_2', 'eco', 'hors'] };
  const HINTS = {
    comfort_1: 'commun.programme.aide_confort_1',
    comfort_2: 'commun.programme.aide_confort_2',
    eco: 'commun.programme.aide_eco',
    hors: 'commun.programme.aide_hors',
  };
  // On switch wiring: the setpoints the week holds, warmest first.
  const setpoints = $derived(
    bySetpoint
      ? [...new Set((week ?? []).flatMap((runs) => spans(runs, wiring).map((s) => s.celsius)).filter((c) => c != null))].sort((a, b) => b - a)
      : [],
  );

  let now = $state(new Date());
  $effect(() => {
    const tick = setInterval(() => (now = new Date()), 60_000);
    return () => clearInterval(tick);
  });
  const today = $derived(nowIn(now).day);
  const slot = $derived(nowIn(now).slot);
  const current = $derived(spans(week?.[today], wiring).find((s) => s.at <= slot && slot < s.end));
  // The day written out (and edited): today until another is tapped.
  let picked = $state(null);
  const shown = $derived(picked ?? today);

  // A run wide enough says what it is inside its bar (a setpoint needs more room).
  const roomy = (s) => s.end - s.at >= (s.celsius != null ? 8 : 5);

  // ---- editing ----
  let asking = $state(false);
  // What this heater holds as far as the editor knows: the program read,
  // then what was saved. Only what differs from it can be saved.
  let base = $state(null);
  function edit() {
    asking = false;
    draft = read.map((runs) => slotsOf(runs));
    base = read.map((runs) => slotsOf(runs));
  }
  function stop() {
    draft = null;
    range = null;
    status = null;
    targets = [];
    alsoTo = [];
  }

  // The brush: an order (four or six) or a setpoint (switch), 5 to 30 °C.
  // Changed with a range chosen, it paints that range again.
  let order = $state('comfort');
  let celsius = $state(19);
  const brushLabel = $derived(bySetpoint ? `${deg(celsius)} °C` : modeLabel(order));
  const paintWith = (s) => (bySetpoint ? { ...s, celsius } : { ...s, order });
  const pickOrder = (key) => ((order = key), paint());
  const pickCelsius = (c) => ((celsius = c), paint());
  const nudge = (delta) => pickCelsius(Math.min(30, Math.max(5, celsius + delta)));

  // The range chosen on a day, `hi` the quarter after it: drawn along the
  // day or typed in « A precise range », which always says it.
  let range = $state(null);
  function choose(day, lo, hi) {
    range = { day, lo, hi };
    from = hhmm(lo);
    to = hhmm(hi % SLOTS);
  }
  function paint() {
    if (!range || !draft?.[range.day]) return;
    const { day, lo, hi } = range;
    draft[day] = draft[day].map((s, i) => (i >= lo && i < hi ? paintWith(s) : s));
  }

  // A stroke along a day: from where the finger went down to where it is,
  // painted over the day as it was (going back unpaints).
  let stroke = $state(null);
  const slotAt = (e, el) => {
    const box = el.getBoundingClientRect();
    return Math.min(SLOTS - 1, Math.max(0, Math.floor(((e.clientX - box.left) / box.width) * SLOTS)));
  };
  function down(e, day) {
    if (!draft?.[day]) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    const at = slotAt(e, e.currentTarget);
    stroke = { day, from: at, to: at, base: draft[day] };
    picked = day;
    repaint();
  }
  function move(e) {
    if (!stroke) return;
    stroke.to = slotAt(e, e.currentTarget);
    repaint();
  }
  function repaint() {
    const { day, from, to, base } = stroke;
    const lo = Math.min(from, to);
    const hi = Math.max(from, to);
    choose(day, lo, hi + 1);
    draft[day] = base.map((s, i) => (i >= lo && i <= hi ? paintWith(s) : s));
  }
  const rangeText = $derived(
    range
      ? t('commun.programme.trait', { day: dayName(range.day, locale()), from: hhmm(range.lo), to: hhmm(range.hi), what: brushLabel })
      : null,
  );

  // A precise range on the day shown (an end at or before the start:
  // midnight): typed, it becomes the range chosen; applied, it is painted.
  let from = $state('06:00');
  let to = $state('08:00');
  const quarter = (text) => {
    const [h, m] = (text || '0:0').split(':').map(Number);
    return h * 4 + Math.floor(m / 15);
  };
  function typed() {
    const a = quarter(from);
    range = { day: shown, lo: a, hi: quarter(to) > a ? quarter(to) : SLOTS };
  }
  function applyRange() {
    typed();
    paint();
  }
  // Another day picked: its range is to choose.
  function pickDay(day) {
    picked = day;
    if (range?.day !== day) range = null;
  }

  // The day shown, copied to others.
  let targets = $state([]);
  const SETS = [
    ['commun.programme.semaine', [0, 1, 2, 3, 4]],
    ['commun.programme.week_end', [5, 6]],
    ['commun.programme.tous', [0, 1, 2, 3, 4, 5, 6]],
  ];
  const toggleDay = (d) => (targets = targets.includes(d) ? targets.filter((x) => x !== d) : [...targets, d]);
  function copyDays() {
    for (const d of targets) if (d !== shown && draft[d] && draft[shown]) draft[d] = draft[shown].map((s) => ({ ...s }));
    targets = [];
  }

  // Other heaters on the same wiring that keep a program, to send it to too.
  const others = $derived(
    Object.values(hub.devices).filter(
      (d) => d.id !== id && value(d.id, 'device_mode_type') === wiring && d.points.some((p) => p.key === 'week_program' && p.access?.write),
    ),
  );
  let alsoTo = $state([]);
  const toggleOther = (other) => (alsoTo = alsoTo.includes(other) ? alsoTo.filter((x) => x !== other) : [...alsoTo, other]);

  // The days that changed: only those are sent to this heater.
  const changed = $derived(draft ? draft.map((d, i) => !!d && JSON.stringify(d) !== JSON.stringify(base?.[i])) : []);
  const dirty = $derived(changed.some(Boolean) || alsoTo.length > 0);
  let busy = $state(false);
  // After « Save »: `waiting` for the heater to say back `expect`, `taken`,
  // or `late` (not said within 30 s); `others`: how many other heaters it
  // went to.
  let status = $state(null);
  $effect(() => {
    if (status?.state !== 'waiting') return;
    if (same(read, status.expect)) status = { ...status, state: 'taken' };
    const late = setTimeout(() => status?.state === 'waiting' && (status = { ...status, state: 'late' }), 30_000);
    return () => clearTimeout(late);
  });
  // A new change: the last outcome is no longer news.
  $effect(() => {
    if (dirty) status = null;
  });
  const statusText = $derived.by(() => {
    if (!status) return null;
    const others = status.others ? ' ' + t('commun.programme.et_autres', { count: status.others }) : '';
    if (status.state === 'waiting') return t('commun.programme.attente') + others;
    if (status.state === 'taken') return t('commun.programme.pris') + others;
    if (status.state === 'late') return t('commun.programme.pas_confirme') + others;
    return t('commun.programme.envoye_autres', { count: status.others });
  });
  async function save() {
    busy = true;
    const days = (keep) => JSON.stringify({ days: draft.map((d, i) => (d && keep(i) ? sentRuns(d, wiring) : null)) });
    const results = [];
    if (changed.some(Boolean)) {
      results.push(await act(`${id}/week_program`, days((i) => changed[i]), t('commun.programme.label_ecrire', { name })));
    }
    for (const other of alsoTo) {
      if (results.includes('held')) break;
      results.push(await act(`${other}/week_program`, days(() => true), t('commun.programme.label_ecrire', { name: nameOf(other) })));
    }
    busy = false;
    if (results.includes('error')) return;
    const own = changed.some(Boolean);
    if (own) sent = draft.map((d, i) => (d ? runsOf(d) : read?.[i]));
    base = draft.map((d) => d && d.map((s) => ({ ...s })));
    const others = alsoTo.length;
    alsoTo = [];
    // Held for a code: the guard says the rest.
    if (results.includes('held')) return;
    status = { state: own ? 'waiting' : 'others', others, expect: sent };
  }

  // Closing while editing gives the edits up first.
  function close() {
    if (editing) stop();
    else dialog.close();
  }
  // « Done », nothing left to save: back to the card.
  function finish() {
    stop();
    dialog.close();
  }
</script>

<dialog
  bind:this={dialog}
  {onclose}
  oncancel={(e) => editing && (e.preventDefault(), stop())}
  onclick={(e) => e.target === dialog && !editing && dialog.close()}
  aria-label={t('commun.programme.titre')}
  tabindex="-1">
  <header>
    <div>
      <h2>{name}</h2>
      <p class="sub">{t(editing ? 'commun.programme.modifier' : 'commun.programme.titre')}</p>
    </div>
    <div class="actions">
      {#if read && !editing && !asking && !stale}
        <button class="round" onclick={() => (asking = true)} title={t('commun.programme.modifier')} aria-label={t('commun.programme.modifier')}><Icon name="cog" size={22} /></button>
      {/if}
      <button class="round" onclick={close} aria-label={editing ? t('commun.annuler') : t('commun.fermer')}><Icon name="close" size={22} /></button>
    </div>
  </header>

  {#if stale && !editing}
    <p class="stale"><Icon name="timer" size={18} />{t('commun.programme.relecture')}</p>
  {/if}

  {#if asking}
    <section class="confirm" aria-label={t('commun.programme.confirmer_titre')}>
      <span class="cog"><Icon name="cog" size={22} /></span>
      <div>
        <h3>{t('commun.programme.confirmer_titre')}</h3>
        <p>{t('commun.programme.confirmer_texte')}</p>
        <div class="buttons">
          <button class="soft" onclick={() => (asking = false)}>{t('commun.annuler')}</button>
          <button class="primary" onclick={edit}>{t('commun.programme.oui_modifier')}</button>
        </div>
      </div>
    </section>
  {/if}

  {#if editing}
    <section class="tools">
      <p class="muted">{t(bySetpoint ? 'commun.programme.pinceau_temp' : 'commun.programme.pinceau')}</p>
      {#if bySetpoint}
        <div class="stepper">
          <button onclick={() => nudge(-0.5)} aria-label={t('commun.clim.moins_chaud')}>−</button>
          <b class="num swatch-text {warmth(celsius)}">{deg(celsius)} °C</b>
          <button onclick={() => nudge(0.5)} aria-label={t('commun.clim.plus_chaud')}>+</button>
          {#each setpoints as c (c)}
            <button class="chip-temp" class:active={c === celsius} onclick={() => pickCelsius(c)}>{deg(c)}°</button>
          {/each}
        </div>
      {:else}
        <div class="brushes" role="radiogroup" aria-label={t('commun.programme.pinceau')}>
          {#each ORDERS[wiring] ?? [] as key (key)}
            <button class="brush" class:active={order === key} role="radio" aria-checked={order === key} onclick={() => pickOrder(key)}>
              <i class="swatch {key}">
                {#if modes[key]?.text}<span class="num">{modes[key].text}</span>{:else}<Icon name={modes[key]?.icon ?? 'tune'} size={13} />{/if}
              </i>
              {modeLabel(key)}
            </button>
          {/each}
        </div>
      {/if}
      <p class="stroke num" aria-live="polite">{rangeText ?? ' '}</p>
    </section>
  {:else if current && !stale}
    <p class="now">
      <i class="swatch {tint(current)}"></i>
      {t('commun.programme.maintenant', { what: what(current), until: hhmm(current.end) })}
      {#if sent}<small>· {t('commun.programme.en_attente')}</small>{/if}
    </p>
  {/if}

  {#if !editing}
    <section class="legend" aria-label={t('commun.programme.legende')}>
      {#if bySetpoint}
        <p class="muted">{t('commun.programme.couleur_temp')}</p>
        <ul>
          {#each setpoints as c (c)}
            <li><i class="swatch {warmth(c)}"></i><b class="num">{deg(c)} °C</b></li>
          {/each}
        </ul>
      {:else}
        <ul>
          {#each ORDERS[wiring] ?? [] as key (key)}
            <li>
              <i class="swatch {key}">
                {#if modes[key]?.text}<span class="num">{modes[key].text}</span>{:else}<Icon name={modes[key]?.icon ?? 'tune'} size={13} />{/if}
              </i>
              <b>{modeLabel(key)}</b>
              {#if HINTS[key]}<small>{t(HINTS[key])}</small>{/if}
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}

  {#snippet bar(runs, day)}
    {#each spans(runs, wiring) as s (s.at)}
      <i class="run {tint(s)}" style="width:{((s.end - s.at) / SLOTS) * 100}%" title="{hhmm(s.at)}–{hhmm(s.end)} · {what(s)}">
        {#if roomy(s)}
          {#if s.celsius != null}<span class="num">{deg(s.celsius)}°</span>
          {:else if modes[s.order]?.text}<span class="num">{modes[s.order].text}</span>
          {:else if modes[s.order]}<Icon name={modes[s.order].icon} size={14} />{/if}
        {/if}
      </i>
    {/each}
    {#if day === today}<b class="here" style="left:{(slot / SLOTS) * 100}%"></b>{/if}
    {#if editing && range?.day === day}<b class="chosen" style="left:{(range.lo / SLOTS) * 100}%;width:{((range.hi - range.lo) / SLOTS) * 100}%"></b>{/if}
  {/snippet}
  {#snippet dayLabel(day)}
    <span class="day" class:today={day === today}><span class="long">{dayName(day, locale(), 'long')}</span><span class="short">{dayName(day, locale())}</span></span>
  {/snippet}

  {#if week}
    <div class="week" class:editing>
      {#each week as runs, day (day)}
        {#if editing}
          <div class="row" class:picked={day === shown} class:changed={changed[day]}>
            <button class="pick" onclick={() => pickDay(day)} aria-pressed={day === shown}>{@render dayLabel(day)}</button>
            <span
              class="bar paint"
              role="button"
              tabindex="-1"
              aria-label={t('commun.programme.peindre_jour', { day: dayName(day, locale(), 'long') })}
              onpointerdown={(e) => down(e, day)}
              onpointermove={move}
              onpointerup={() => (stroke = null)}
              onpointercancel={() => (stroke = null)}>
              {@render bar(runs, day)}
            </span>
          </div>
        {:else}
          <button class="row" class:picked={day === shown} onclick={() => (picked = day)} aria-pressed={day === shown}>
            {@render dayLabel(day)}
            <span class="bar">{@render bar(runs, day)}</span>
          </button>
        {/if}
      {/each}
      <div class="row hours" aria-hidden="true">
        <span class="day"></span>
        <span class="ticks">
          {#each [0, 3, 6, 9, 12, 15, 18, 21, 24] as h (h)}<span class:minor={h % 6} style="left:{(h / 24) * 100}%">{h}h</span>{/each}
        </span>
      </div>
    </div>

    <section class="runs">
      <h3>{dayName(shown, locale(), 'long')}</h3>
      {#if week[shown]}
        <ol>
          {#each spans(week[shown], wiring) as s (s.at)}
            <li class:live={!editing && shown === today && s.at <= slot && slot < s.end}>
              <span class="num">{hhmm(s.at)} – {hhmm(s.end)}</span>
              <i class="swatch {tint(s)}"></i>
              <b>{what(s)}</b>
            </li>
          {/each}
        </ol>
      {:else}
        <p class="muted">{t('commun.programme.jour_vide')}</p>
      {/if}
      {#if !editing}<p class="muted hint">{t('commun.programme.toucher_jour')}</p>{/if}
    </section>

    {#if editing && draft[shown]}
      <section class="block">
        <h4>{t('commun.programme.plage')}</h4>
        <div class="range">
          <label>{t('commun.programme.de')} <input type="time" step="900" bind:value={from} onchange={typed} /></label>
          <label>{t('commun.programme.a')} <input type="time" step="900" bind:value={to} onchange={typed} /></label>
          <button class="soft" onclick={applyRange}>{t('commun.programme.appliquer', { what: brushLabel })}</button>
        </div>
      </section>

      <section class="block">
        <h4>{t('commun.programme.copier_vers', { day: dayName(shown, locale(), 'long') })}</h4>
        <div class="days">
          {#each [0, 1, 2, 3, 4, 5, 6] as d (d)}
            <button class="day-chip" class:active={targets.includes(d)} disabled={d === shown || !draft[d]} aria-pressed={targets.includes(d)} onclick={() => toggleDay(d)}>
              {dayName(d, locale())}
            </button>
          {/each}
        </div>
        <div class="days">
          {#each SETS as [label, set] (label)}
            <button class="link" onclick={() => (targets = set.filter((d) => d !== shown && draft[d]))}>{t(label)}</button>
          {/each}
          <button class="soft" disabled={!targets.length} onclick={copyDays}>{t('commun.programme.copier')}</button>
        </div>
      </section>

      <section class="block">
        <h4>{t('commun.programme.autres')}</h4>
        {#if others.length}
          <ul class="others">
            {#each others as other (other.id)}
              <li>
                <label><input type="checkbox" checked={alsoTo.includes(other.id)} onchange={() => toggleOther(other.id)} /> {nameOf(other.id)}</label>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="muted">{t('commun.programme.aucun_autre')}</p>
        {/if}
      </section>

      <footer>
        <p class="status {status?.state ?? ''}" aria-live="polite">
          {#if statusText}
            <Icon name={status.state === 'waiting' ? 'timer' : status.state === 'late' ? 'warning' : 'ok'} size={18} />{statusText}
          {/if}
        </p>
        <button class="soft" onclick={dirty ? stop : finish}>{t(dirty ? 'commun.annuler' : 'commun.programme.terminer')}</button>
        <button class="primary" disabled={busy || !dirty} onclick={save}>
          {busy ? t('commun.programme.envoi') : t('commun.programme.enregistrer')}
        </button>
      </footer>
    {/if}
  {/if}
</dialog>

<style>
  dialog {
    border: 0;
    padding: 18px;
    width: min(760px, 96vw);
    max-height: 94dvh;
    overflow-y: auto;
    border-radius: var(--r-xl);
    background: var(--surface);
    color: var(--ink);
    box-shadow: 0 30px 80px rgb(0 0 0 / 40%);
  }

  dialog:focus {
    outline: none;
  }

  dialog::backdrop {
    background: rgb(4 6 12 / 60%);
    backdrop-filter: blur(4px);
  }

  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }

  h2 {
    font-size: 22px;
    font-weight: 750;
  }

  .sub {
    font-size: 13px;
    color: var(--ink-3);
    font-weight: 600;
  }

  .round {
    width: 40px;
    height: 40px;
    flex: none;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink);
    cursor: pointer;
  }

  .now {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 14px;
    margin-bottom: 14px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    font-weight: 650;
  }

  .stale {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 14px;
    margin-bottom: 14px;
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--warm) 14%, var(--surface));
    font-size: 13.5px;
    font-weight: 600;
  }

  .legend {
    display: grid;
    gap: 6px;
    margin-bottom: 16px;
  }

  .legend ul {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 16px;
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .legend li {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13.5px;
  }

  .legend small {
    color: var(--ink-3);
  }

  .muted {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .swatch {
    display: inline-grid;
    place-items: center;
    width: 26px;
    height: 20px;
    flex: none;
    border-radius: 6px;
    color: #fff;
    font-style: normal;
    font-size: 11.5px;
    font-weight: 750;
  }

  .now .swatch {
    width: 14px;
    height: 14px;
    border-radius: 50%;
  }

  .week {
    display: grid;
    gap: 4px;
  }

  .row {
    display: grid;
    grid-template-columns: 52px 1fr;
    align-items: center;
    gap: 8px;
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .day {
    font-size: 12.5px;
    color: var(--ink-3);
    text-transform: capitalize;
  }

  .day.today {
    color: var(--ink);
    font-weight: 750;
  }

  .row.picked .bar {
    outline: 2px solid var(--ink-2);
    outline-offset: 1px;
  }

  .bar {
    position: relative;
    display: flex;
    height: 30px;
    border-radius: 8px;
    overflow: hidden;
    background: var(--surface-2);
  }

  .run {
    display: grid;
    place-items: center;
    height: 100%;
    color: #fff;
    font-style: normal;
    font-size: 12px;
    font-weight: 750;
    box-shadow: inset -1px 0 0 rgb(0 0 0 / 12%);
    overflow: hidden;
  }

  /* Orders (four or six), as the card's strip. */
  .comfort {
    background: var(--warm);
  }

  .comfort_1 {
    background: color-mix(in srgb, var(--warm) 70%, var(--surface-2));
  }

  .comfort_2 {
    background: color-mix(in srgb, var(--warm) 45%, var(--surface-2));
    color: var(--warm-ink);
  }

  .eco {
    background: var(--good);
  }

  .hors {
    background: var(--cool);
  }

  .off,
  .unknown {
    background: var(--ink-3);
  }

  /* Setpoints (switch), as the card's strip. */
  .frost {
    background: #4f8cff;
  }

  .cold {
    background: #3fc1d9;
  }

  .cool {
    background: #7fd3b0;
    color: #0d3b2c;
  }

  .good {
    background: #4cc26a;
  }

  .warm {
    background: #ffb340;
    color: #5a3300;
  }

  .hot {
    background: #ff5a3c;
  }

  .here {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--ink);
    box-shadow: 0 0 0 1px var(--surface);
    transform: translateX(-50%);
  }

  /* The range chosen, while editing. */
  .chosen {
    position: absolute;
    top: 0;
    bottom: 0;
    border: 2px dashed var(--ink);
    border-radius: 6px;
    background: rgb(255 255 255 / 18%);
    pointer-events: none;
  }

  .ticks {
    position: relative;
    height: 16px;
  }

  .ticks span {
    position: absolute;
    font-size: 11px;
    color: var(--ink-3);
    transform: translateX(-50%);
  }

  /* On a phone the hours every six only. */
  @media (max-width: 520px) {
    .ticks span.minor {
      display: none;
    }

    .row {
      grid-template-columns: 38px 1fr;
    }

    .day .long {
      display: none;
    }
  }

  @media (min-width: 521px) {
    .day .short {
      display: none;
    }
  }

  .runs {
    margin-top: 18px;
    display: grid;
    gap: 8px;
  }

  h3 {
    font-size: 16px;
    font-weight: 750;
    text-transform: capitalize;
  }

  ol {
    display: grid;
    gap: 4px;
    list-style: none;
    padding: 0;
    margin: 0;
  }

  ol li {
    display: grid;
    grid-template-columns: 112px 26px 1fr;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    border-radius: 10px;
    font-size: 14px;
  }

  ol li.live {
    background: var(--surface-2);
    font-weight: 650;
  }

  .hint {
    margin-top: 4px;
  }
  .actions {
    display: flex;
    gap: 8px;
  }

  .confirm {
    display: flex;
    gap: 14px;
    padding: 14px 16px;
    margin-bottom: 14px;
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--warm) 14%, var(--surface));
  }

  .confirm .cog {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    flex: none;
    border-radius: 50%;
    background: var(--warm);
    color: #fff;
  }

  .confirm h3 {
    text-transform: none;
    margin-bottom: 4px;
  }

  .confirm p {
    font-size: 14px;
    color: var(--ink-2);
  }

  .buttons,
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 12px;
  }

  footer {
    flex-wrap: wrap;
    align-items: center;
    position: sticky;
    bottom: -18px;
    margin: 18px -18px -18px;
    padding: 12px 18px 18px;
    background: var(--surface);
    box-shadow: 0 -8px 16px -12px rgb(0 0 0 / 35%);
  }

  .soft,
  .primary,
  .link,
  .brush,
  .day-chip,
  .chip-temp,
  .stepper button {
    border: none;
    font: inherit;
    cursor: pointer;
  }

  .soft,
  .primary {
    padding: 9px 18px;
    border-radius: 999px;
    font-weight: 650;
    background: var(--surface-2);
    color: var(--ink);
  }

  .primary {
    background: var(--ink);
    color: var(--surface);
  }

  .soft:disabled,
  .primary:disabled,
  .day-chip:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .link {
    padding: 4px 6px;
    background: none;
    color: var(--cool);
    font-weight: 650;
  }

  .tools {
    display: grid;
    gap: 10px;
    margin-bottom: 8px;
  }

  .brushes,
  .stepper,
  .days,
  .range {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  .brush {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 12px 5px 5px;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink);
    font-size: 13.5px;
    font-weight: 600;
  }

  .brush.active {
    box-shadow: 0 0 0 2px var(--ink);
    background: var(--surface);
  }

  .stepper button {
    width: 38px;
    height: 38px;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink);
    font-size: 20px;
  }

  .stepper .chip-temp {
    width: auto;
    height: 30px;
    padding: 0 10px;
    border-radius: 999px;
    font-size: 13px;
    font-weight: 650;
  }

  .chip-temp.active {
    box-shadow: 0 0 0 2px var(--ink);
  }

  .swatch-text {
    padding: 6px 12px;
    border-radius: 10px;
    color: #fff;
    min-width: 76px;
    text-align: center;
  }

  .stroke {
    min-height: 1.3em;
    font-weight: 650;
    color: var(--ink-2);
  }

  .week.editing .bar.paint {
    cursor: crosshair;
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
  }

  .pick {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .row.changed .day::after {
    content: ' •';
    color: var(--warm);
  }

  .block {
    display: grid;
    gap: 8px;
    margin-top: 16px;
  }

  h4 {
    font-size: 14px;
    font-weight: 700;
  }

  .range label {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 14px;
  }

  .range input {
    padding: 6px 8px;
    border: 1px solid var(--surface-3);
    border-radius: 10px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
  }

  .day-chip {
    padding: 6px 12px;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink);
    font-size: 13px;
    font-weight: 600;
    text-transform: capitalize;
  }

  .day-chip.active {
    background: var(--ink);
    color: var(--surface);
  }

  .others {
    display: grid;
    gap: 6px;
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .others label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
  }

  .status {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1 1 100%;
    min-height: 0;
    font-size: 13.5px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .status:empty {
    display: none;
  }

  .status.taken,
  .status.others {
    color: var(--good);
  }

  .status.late {
    color: var(--warm-ink);
  }

  .now small {
    font-weight: 500;
    color: var(--ink-3);
  }
</style>
