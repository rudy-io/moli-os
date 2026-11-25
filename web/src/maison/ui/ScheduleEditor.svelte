<script>
  import { device, value, reachable, act } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { scheduleSpec, parseDay, formatDay, sameDay, addStep, moveStep, setStepDeg, removeStep, writeDays, hhmm, minutesOf, suggestMinute, programmesOf, GRID } from '../lib/schedule.js';

  // A valve's weekly schedule, one day at a time. Writing it is an order like any
  // other: it goes through `act`, the hub's normal path and the guard.
  // `day`: the day shown, which a week view beside it may choose.
  let { id, name, day = $bindable() } = $props();

  const d = $derived(device(id));
  const spec = $derived(scheduleSpec(d));
  let edited = $state(null);
  let applyTo = $state([]);
  // Copying the day as the valve holds it, unchanged, to other days.
  let copying = $state(false);
  let saving = $state(false);

  // A Moli heating room keeps a week per house programme: the one edited is chosen
  // here, the one the house plays by default.
  const HOUSE = 'chauffage:maison';
  const programmes = $derived(id.startsWith('chauffage:') ? programmesOf(d, device(HOUSE)) : []);
  const playing = $derived(value(HOUSE, 'programme'));
  let programme = $state(null);
  const picked = $derived(programmes.find((p) => p.name === programme) ?? programmes.find((p) => p.name === playing) ?? programmes[0]);
  const first = $derived(programmes[0]);
  const pickedKey = $derived(picked?.key);
  const keyOf = (key) => (picked ? `weekly_schedule_${picked.key}_${key}` : `weekly_schedule_${key}`);

  const current = $derived(day && spec?.days.includes(day) ? day : spec?.days[0]);
  const own = $derived(current ? value(id, keyOf(current)) : null);
  // A programme's day never set plays the first programme's.
  const inherited = $derived(!!picked && pickedKey !== first.key && (own == null || own === ''));
  const stored = $derived(inherited ? value(id, `weekly_schedule_${first.key}_${current}`) : own);
  const known = $derived(typeof stored === 'string' && stored.length > 0);
  const steps = $derived(spec ? (edited ?? parseDay(spec, stored)) : []);
  const dirty = $derived(!!spec && edited != null && (!known || !sameDay(spec, edited, parseDay(spec, stored))));
  // Editing writes the day shown and the days added; copying, only the days chosen.
  const targets = $derived(copying ? applyTo.filter((k) => k !== current) : applyTo.length ? applyTo : current ? [current] : []);
  const others = $derived(spec ? spec.days.filter((k) => k !== current) : []);
  const spot = $derived(spec ? suggestMinute(spec, steps) : null);

  // Which of a TRV-ZBT's groups the texts belong to.
  const editGroup = $derived(value(id, 'schedule_group_to_edit'));
  const activeGroup = $derived(value(id, 'schedule_active_group'));
  const otherGroup = $derived(editGroup != null && activeGroup != null && String(editGroup) !== String(activeGroup));

  const dayName = (key) => new Date(2024, 0, 1 + ['monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday'].indexOf(key)).toLocaleDateString(locale(), { weekday: 'short' }).replace('.', '');

  function pick(key) {
    day = key;
  }

  // Another day, chosen here or beside: what was being edited belonged to the other one.
  $effect(() => {
    void day;
    void pickedKey;
    edited = null;
    applyTo = [];
    copying = false;
  });

  const edit = (next) => {
    edited = next;
    copying = false;
    applyTo = [];
  };
  const cancel = () => {
    edited = null;
    copying = false;
    applyTo = [];
  };
  // Every other day, or back to none.
  function all() {
    const full = others.every((k) => targets.includes(k));
    applyTo = full ? [] : copying ? [...others] : [current, ...others];
  }
  const toggle = (key) => (applyTo = targets.includes(key) ? targets.filter((k) => k !== key) : [...targets, key]);

  async function save() {
    if (!spec || saving || !reachable(id)) return;
    if (!dirty && !(copying && targets.length)) return;
    const text = formatDay(spec, dirty ? edited : steps);
    saving = true;
    const { stopped } = await writeDays(
      targets.filter((key) => spec.days.includes(key)),
      (key) => act(`${id}/${keyOf(key)}`, text, t('commun.vanne.prog.label', { name })),
    );
    saving = false;
    if (!stopped) cancel();
  }

  // Back to the first programme's day.
  async function inherit() {
    if (!current || inherited || saving || !reachable(id)) return;
    saving = true;
    await act(`${id}/${keyOf(current)}`, '', t('commun.vanne.prog.label', { name }));
    saving = false;
  }

  // A field the table did not take back shows what the table holds again.
  function moveTo(e, i) {
    const min = minutesOf(e.currentTarget.value);
    const next = min == null ? steps : moveStep(spec, steps, i, min);
    if (!sameDay(spec, next, steps)) edit(next);
    else e.currentTarget.value = hhmm(steps[i].min);
  }

  function setDeg(e, i) {
    const next = setStepDeg(spec, steps, i, e.currentTarget.value === '' ? null : e.currentTarget.valueAsNumber);
    if (!sameDay(spec, next, steps)) edit(next);
    else e.currentTarget.value = String(steps[i].deg);
  }
</script>

{#if spec}
  <div class="schedule" class:unreachable={!reachable(id)}>
    {#if programmes.length > 1}
      <div class="days" role="group" aria-label={t('commun.vanne.prog.programme')}>
        {#each programmes as p (p.key)}
          <button class:active={p.key === pickedKey} onclick={() => (programme = p.name)} aria-pressed={p.key === pickedKey}>{p.name}</button>
        {/each}
      </div>
      {#if picked && picked.name !== playing && playing != null}
        <p class="note">{t('commun.vanne.prog.joue', { programme: playing })}</p>
      {/if}
    {/if}
    <div class="days" role="group" aria-label={t('commun.vanne.prog.jour')}>
      {#each spec.days as key (key)}
        <button class:active={key === current} onclick={() => pick(key)} aria-pressed={key === current}>{dayName(key)}</button>
      {/each}
    </div>
    {#if otherGroup}
      <p class="note">{t('commun.vanne.prog.groupe', { edit: editGroup, active: activeGroup })}</p>
    {/if}
    {#if !known}
      <p class="note">{t('commun.vanne.prog.inconnu')}</p>
    {/if}
    {#if inherited}
      <p class="note">{t('commun.vanne.prog.repris', { programme: first.name })}</p>
    {:else if picked && pickedKey !== first.key && !dirty}
      <button class="add" disabled={saving || !reachable(id)} onclick={inherit}>{t('commun.vanne.prog.reprendre', { programme: first.name })}</button>
    {/if}
    <ul>
      {#each steps as step, i (step.min)}
        <li>
          <input
            class="num time"
            type="time"
            step={GRID * 60}
            value={hhmm(step.min)}
            disabled={i === 0}
            aria-label={t('commun.vanne.prog.heure')}
            onchange={(e) => moveTo(e, i)}
          />
          <input
            class="num deg"
            type="number"
            min={spec.min}
            max={spec.max}
            step={spec.step}
            value={step.deg}
            aria-label={t('commun.clim.consigne')}
            onchange={(e) => setDeg(e, i)}
          />
          {#if i > 0}
            <button class="remove" aria-label={t('commun.vanne.prog.supprimer', { time: hhmm(step.min) })} onclick={() => edit(removeStep(spec, steps, i))}>×</button>
          {/if}
        </li>
      {/each}
    </ul>
    <button class="add" disabled={spot == null} onclick={() => edit(addStep(spec, steps, spot))}>
      {steps.length >= spec.maxSteps ? t('commun.vanne.prog.plein', { max: spec.maxSteps }) : t('commun.vanne.prog.ajouter')}
    </button>
    {#if dirty || copying}
      {@const what = copying ? t('commun.vanne.prog.copier_vers', { day: dayName(current) }) : t('commun.vanne.prog.appliquer')}
      <div class="days" role="group" aria-label={what}>
        <span class="muted">{what}</span>
        {#each spec.days as key (key)}
          <button class:active={targets.includes(key)} disabled={key === current} onclick={() => toggle(key)} aria-pressed={targets.includes(key)}>{dayName(key)}</button>
        {/each}
        {#if others.length > 1}
          <button class="all" onclick={all}>{t('commun.vanne.prog.tous')}</button>
        {/if}
      </div>
      <div class="actions">
        <button onclick={cancel}>{t('commun.annuler')}</button>
        <button class="primary" disabled={saving || !reachable(id) || (copying && !targets.length)} onclick={save}>
          {copying ? t('commun.vanne.prog.copier_action') : t('commun.vanne.prog.enregistrer')}
        </button>
      </div>
    {:else if known && others.length}
      <button class="add" disabled={!reachable(id)} onclick={() => (copying = true)}>{t('commun.vanne.prog.copier')}</button>
    {/if}
  </div>
{/if}

<style>
  .schedule {
    display: grid;
    gap: 8px;
  }

  .schedule.unreachable {
    opacity: 0.55;
  }

  .days {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  button {
    border: none;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
    padding: 6px 12px;
    border-radius: 999px;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  button.active,
  button.primary {
    background: var(--ink);
    color: var(--surface);
  }

  ul {
    display: grid;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  input {
    border: none;
    border-radius: 8px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    padding: 6px 8px;
  }

  .time {
    width: 124px;
  }

  input:disabled {
    color: var(--ink-2);
  }

  .deg {
    width: 72px;
  }

  .remove {
    padding: 4px 10px;
    font-size: 16px;
    line-height: 1;
  }

  .add {
    justify-self: start;
  }

  .all {
    background: none;
    text-decoration: underline;
  }

  .actions {
    display: flex;
    gap: 8px;
  }

  .note,
  .muted {
    margin: 0;
    font-size: 12.5px;
    color: var(--ink-2);
  }
</style>
