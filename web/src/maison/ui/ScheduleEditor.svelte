<script>
  import { device, value, reachable, act } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { scheduleSpec, parseDay, formatDay, sameDay, addStep, moveStep, setStepDeg, removeStep, writeDays, hhmm, minutesOf, suggestMinute, GRID } from '../lib/schedule.js';

  // A valve's weekly schedule, one day at a time. Writing it is an order like any
  // other: it goes through `act`, the hub's normal path and the guard.
  let { id, name } = $props();

  const d = $derived(device(id));
  const spec = $derived(scheduleSpec(d));
  let day = $state(null);
  let edited = $state(null);
  let applyTo = $state([]);
  let saving = $state(false);

  const current = $derived(day && spec?.days.includes(day) ? day : spec?.days[0]);
  const stored = $derived(current ? value(id, `weekly_schedule_${current}`) : null);
  const known = $derived(typeof stored === 'string' && stored.length > 0);
  const steps = $derived(spec ? (edited ?? parseDay(spec, stored)) : []);
  const dirty = $derived(!!spec && edited != null && (!known || !sameDay(spec, edited, parseDay(spec, stored))));
  const targets = $derived(applyTo.length ? applyTo : current ? [current] : []);
  const spot = $derived(spec ? suggestMinute(spec, steps) : null);

  // Which of a TRV-ZBT's groups the texts belong to.
  const editGroup = $derived(value(id, 'schedule_group_to_edit'));
  const activeGroup = $derived(value(id, 'schedule_active_group'));
  const otherGroup = $derived(editGroup != null && activeGroup != null && String(editGroup) !== String(activeGroup));

  const dayName = (key) => new Date(2024, 0, 1 + ['monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday'].indexOf(key)).toLocaleDateString(locale(), { weekday: 'short' }).replace('.', '');

  function pick(key) {
    day = key;
    edited = null;
    applyTo = [];
  }

  const edit = (next) => (edited = next);
  const toggle = (key) => (applyTo = targets.includes(key) ? targets.filter((k) => k !== key) : [...targets, key]);

  async function save() {
    if (!spec || !dirty || saving || !reachable(id)) return;
    const text = formatDay(spec, edited);
    saving = true;
    const { stopped } = await writeDays(
      targets.filter((key) => spec.days.includes(key)),
      (key) => act(`${id}/weekly_schedule_${key}`, text, t('commun.vanne.prog.label', { name })),
    );
    saving = false;
    if (!stopped) {
      edited = null;
      applyTo = [];
    }
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
    {#if dirty}
      <div class="days" role="group" aria-label={t('commun.vanne.prog.appliquer')}>
        <span class="muted">{t('commun.vanne.prog.appliquer')}</span>
        {#each spec.days as key (key)}
          <button class:active={targets.includes(key)} disabled={key === current} onclick={() => toggle(key)} aria-pressed={targets.includes(key)}>{dayName(key)}</button>
        {/each}
      </div>
      <div class="actions">
        <button onclick={() => (edited = null)}>{t('commun.annuler')}</button>
        <button class="primary" disabled={saving || !reachable(id)} onclick={save}>{t('commun.vanne.prog.enregistrer')}</button>
      </div>
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
