<script>
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { formatValue, unitOf } from '../../lib/format.js';
  import { pointSections, readingOf } from '../lib/airzone.js';
  import { climateMode } from '../lib/climate.js';
  import Icon from './Icon.svelte';

  let { device } = $props();
  const sections = $derived(pointSections(device));
  const latest = $derived.by(() => {
    const stamps = (device.points ?? []).map((p) => readingOf(device, p.key)?.ts).filter((ts) => ts != null);
    return stamps.length ? Math.max(...stamps) : null;
  });

  function shown(point, reading) {
    if (!reading) return '—';
    if (typeof reading.value === 'string' && !reading.value.trim()) return t('airzone_ui.not_set');
    const mode = point.kind.type === 'enum' && (point.key === 'mode' || point.key.endsWith('_mode'))
      ? climateMode(reading.value) : null;
    return mode ? t(mode.label) : formatValue(point, reading.value);
  }
</script>

<section class="readings" aria-label={t('airzone_ui.parameters')}>
  {#if device.online !== true}
    <p class="stale" role="status"><Icon name="offline" size={18} /><span><strong>{t(device.online === false ? 'airzone_ui.resource_offline' : 'airzone_ui.availability_unknown')}</strong><br />{t('airzone_ui.latest')}{#if latest != null} · {new Date(latest).toLocaleString(locale())}{/if}</span></p>
  {/if}
  {#each sections as section (section.id)}
    <details open={section.id === 'comfort'}>
      <summary>
        <span class="section-title">{t(`airzone_ui.sections.${section.id}`)}</span>
        <span class="count num">{section.points.length}</span>
        <span class="chevron"><Icon name="chevron-down" size={20} /></span>
      </summary>
      <dl>
        {#each section.points as point (point.key)}
          {@const reading = readingOf(device, point.key)}
          <div class="reading">
            <dt>{point.label || point.key}</dt>
            <dd class="num">{shown(point, reading)}{#if reading && unitOf(point)}<span class="unit">{unitOf(point)}</span>{/if}</dd>
          </div>
        {/each}
      </dl>
    </details>
  {/each}
</section>

<style>
  .readings { display: grid; gap: 12px; min-width: 0; }
  .stale { display: flex; align-items: flex-start; gap: 10px; padding: 14px; border-radius: 12px; background: var(--surface-2); color: var(--ink-2); font-size: 12px; line-height: 1.6; }
  details { min-width: 0; border: 1px solid var(--line); border-radius: var(--r-md); background: var(--surface); overflow: clip; }
  summary { display: flex; align-items: center; gap: 12px; min-width: 0; min-height: 64px; padding: 16px 20px; list-style: none; cursor: pointer; }
  summary::-webkit-details-marker { display: none; }
  summary:focus-visible { outline: 3px solid var(--cool); outline-offset: -4px; border-radius: var(--r-md); }
  .section-title { flex: 1; min-width: 0; font-size: 15px; font-weight: 650; overflow-wrap: anywhere; }
  .count { color: var(--ink-3); font-size: 12px; }
  .chevron { flex: none; color: var(--ink-3); transition: transform 160ms var(--ease); }
  details[open] .chevron { transform: rotate(180deg); }
  dl { margin: 0; padding: 0 20px 12px; }
  .reading { display: grid; grid-template-columns: minmax(0, 1.15fr) minmax(0, 1fr); align-items: baseline; gap: 12px; min-width: 0; padding: 13px 0; border-top: 1px solid var(--line); }
  dt,
  dd { min-width: 0; margin: 0; overflow-wrap: anywhere; font-size: 13px; line-height: 1.5; }
  dt { color: var(--ink-2); }
  dd { color: var(--ink); text-align: right; font-weight: 600; white-space: pre-wrap; }
  .unit { margin-left: 4px; color: var(--ink-3); font-weight: 400; }
  @media (max-width: 420px) {
    summary { padding-right: 16px; padding-left: 16px; }
    dl { padding-right: 16px; padding-left: 16px; }
  }
  @media (prefers-reduced-motion: reduce) {
    .chevron { transition: none; }
  }
</style>
