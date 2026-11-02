<script>
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { describeProgramme } from '../lib/somfy-programmes.js';
  import SomfyCloudActions from './SomfyCloudActions.svelte';
  let { kind, definition, context, index = 0 } = $props();
  let programme = $derived(describeProgramme(kind, definition, context));
  const stateText = (enabled) => t(`systeme.somfy.programme.${enabled === true ? 'enabled' : enabled === false ? 'disabled' : 'unknown_enabled'}`);
  function dateText(value) {
    if (!value) return t('systeme.somfy.programme.unspecified');
    if (value.kind !== 'date') return t(`systeme.somfy.programme.${value.kind === 'unbounded' ? 'unbounded' : 'unknown_date'}`);
    const date = new Date(0);
    date.setUTCFullYear(value.year, value.month - 1, value.day);
    return new Intl.DateTimeFormat(locale(), { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' }).format(date);
  }
  function daysText(days) {
    if (!days) return '';
    if (days.kind === 'specific') return dateText(days.date);
    if (days.kind === 'daily') return t(`systeme.somfy.programme.${programme.period?.kind === 'days' && programme.period.value === 1 ? 'every_day' : 'day_cycle'}`);
    if (days.kind === 'weekdays' && days.weekdays.length === 7) return t('systeme.somfy.programme.every_day');
    if (days.kind === 'weekdays' && days.weekdays.length) return days.weekdays.map(day => t(`systeme.somfy.programme.day.${day}`)).join(' · ');
    return t('systeme.somfy.programme.warning.invalid_days');
  }
  const timeText = (time) => time.kind === 'time'
    ? `${String(time.hours).padStart(2, '0')}:${String(time.minutes).padStart(2, '0')}`
    : t(`systeme.somfy.programme.time.${time.kind}`);
</script>

<section class="programme">
  <div class="heading">
    <b>{programme.label ?? t('systeme.somfy.programme.number', { number: index + 1 })}</b>
    <span class:disabled={programme.enabled === false}>{stateText(programme.enabled)}</span>
  </div>
  {#if programme.days}<p class="days">{daysText(programme.days)}</p>{/if}
  {#if programme.dates.start || programme.dates.end}
    <p class="muted">{t('systeme.somfy.programme.date_range', { start: dateText(programme.dates.start), end: dateText(programme.dates.end) })}</p>
  {/if}
  {#if programme.period && programme.period.value !== 1}
    <p class="muted">{t(`systeme.somfy.programme.period.${programme.period.kind}`, { value: programme.period.value })}</p>
  {/if}
  {#if programme.priority !== null}<p class="muted">{t('systeme.somfy.programme.priority', { value: programme.priority })}</p>{/if}
  {#if programme.dayEnabled !== null}<p class="muted">{t('systeme.somfy.programme.day_state', { state: stateText(programme.dayEnabled) })}</p>{/if}
  {#each programme.warnings as warning}<p class="notice">{t(`systeme.somfy.programme.warning.${warning}`)}</p>{/each}
  {#if kind === 'scenarios'}
    <SomfyCloudActions actions={programme.actions} total={programme.totalActions} truncated={programme.truncated} />
  {:else}
    <div class="events">
      {#each programme.triggers as trigger}
        <article class:disabled={trigger.enabled === false}>
          <div class="event-time"><strong>{timeText(trigger.time)}</strong><small>{stateText(trigger.enabled)}</small></div>
          <div class="event-actions">
            {#if trigger.groupLabel}<b>{trigger.groupLabel}</b>{/if}
            {#if trigger.groupEnabled !== null}<p class="muted">{t('systeme.somfy.programme.group_state', { state: stateText(trigger.groupEnabled) })}</p>{/if}
            {#if trigger.time.offsetNative !== undefined}<p class="muted">{t('systeme.somfy.programme.offset', { value: trigger.time.offsetNative })}</p>{/if}
            <SomfyCloudActions actions={trigger.actions} total={trigger.totalActions} truncated={trigger.truncated} />
          </div>
        </article>
      {:else}<p class="muted">{t('systeme.somfy.programme.no_times')}</p>{/each}
    </div>
  {/if}
</section>

<style>
  .programme { display: grid; gap: 10px; min-width: 0; }
  .heading { display: flex; flex-wrap: wrap; gap: 8px 16px; justify-content: space-between; align-items: baseline; }
  .heading span, small, .muted { color: var(--ink-2); font-size: 12px; }
  .heading b { font-size: 15px; }
  p { margin: 0; overflow-wrap: anywhere; }
  .days { font-weight: 600; font-size: 14px; }
  .notice { color: var(--warm-ink); font-size: 12px; }
  .events { display: grid; gap: 10px; }
  article { display: grid; grid-template-columns: minmax(130px, 1fr) minmax(0, 3fr); gap: 16px; padding: 14px; border-radius: var(--r-sm); background: var(--surface); }
  .event-time, .event-actions { display: grid; align-content: start; gap: 8px; min-width: 0; overflow-wrap: anywhere; }
  .event-time strong { font-size: 15px; }
  .disabled { color: var(--ink-2); }
  @media (max-width: 600px) { article { grid-template-columns: 1fr; gap: 12px; } }
</style>
