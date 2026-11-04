<script>
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { collections, safeDate } from '../lib/somfy-cloud.js';
  import { createProgrammeContext } from '../lib/somfy-programmes.js';
  import SomfyCloudProgramme from './SomfyCloudProgramme.svelte';
  import Icon from './Icon.svelte';
  let { observation, details = {}, busy = '', showDefinition, initialSection = 'scenarios' } = $props();
  let expanded = $state({ scenarios: true, calendar_rules: true });
  let selected = $state(initialSection);
  const primary = $derived(selected === 'programmes' ? 'calendar_rules' : 'scenarios');
  let context = $derived(createProgrammeContext(observation.catalog, observation.freshness.collections));
  const readable = new Set(['scenarios', 'calendar_days', 'calendar_rules']);
  const displayLimit = 50;
  const date = (value) => safeDate(value, locale()) ?? t('systeme.somfy.unknown_date');
</script>

<div class="catalog">
  <p class:qualification={!observation.freshness.current} role="status">{t(`systeme.somfy.${observation.freshness.current ? 'fresh' : 'stale'}`)}</p>
  <div class="sections" role="group" aria-label={t('systeme.somfy.catalog_title')}>
    <button class:selected={selected === 'scenarios'} aria-pressed={selected === 'scenarios'} onclick={() => selected = 'scenarios'}><Icon name="play-circle" size={20} />{t('systeme.somfy.scenarios_tab')}</button>
    <button class:selected={selected === 'programmes'} aria-pressed={selected === 'programmes'} onclick={() => selected = 'programmes'}><Icon name="clock" size={20} />{t('systeme.somfy.programmes_tab')}</button>
  </div>
  {#if selected === 'programmes'}<p class="muted">{t('systeme.somfy.programme.read_only')}</p>{/if}
  {@render collectionView(primary)}
  <details class="advanced">
    <summary>{t('systeme.somfy.advanced')}<span>{t('systeme.somfy.advanced_hint')}</span></summary>
  {#if observation.catalog.provider_timezone}<p>{t('systeme.somfy.zone', { zone: observation.catalog.provider_timezone })}</p>{/if}
  <p class="muted">{t('systeme.somfy.zone_note')}</p>
  <p class="muted">{t('systeme.somfy.programme.read_only')}</p>
    {#each collections.filter(kind => !['scenarios', 'calendar_rules'].includes(kind)) as kind}{@render collectionView(kind)}{/each}
  </details>

</div>

{#snippet collectionView(kind)}
    {@const collection = observation.catalog.collections[kind]}
    <details class="collection" open={expanded[kind] ?? false} ontoggle={(event) => expanded[kind] = event.currentTarget.open}>
      <summary><b>{t(`systeme.somfy.collection.${kind}`)}</b><span>{t(`systeme.somfy.status.${collection?.status ?? 'not_observed'}`)}{#if collection?.data}{' · '}{collection.data.definitions.length}{#if !observation.freshness.collections?.[kind]}{' · '}{t('systeme.somfy.historical_count')}{/if}{/if}</span></summary>

      {#if collection?.data && !observation.freshness.collections?.[kind]}<p class="qualification">{t('systeme.somfy.stale')}</p>{/if}
      <div class:scenario-grid={kind === 'scenarios'}>
      {#each (collection?.data?.definitions ?? []).slice(0, displayLimit) as definition, index}
        {@const key = `${kind}:${index}`}
        <div class="definition">
          {#if expanded[kind] && readable.has(kind)}
            <SomfyCloudProgramme {kind} {definition} {context} {index} />
          {:else}
            <b>{definition.label ?? definition.id ?? `#${index + 1}`}</b>
          {/if}
          <details class="original">
            <summary>{t('systeme.somfy.programme.origin')}</summary>
            <p>{t('systeme.somfy.last', { date: date(collection?.last_observation_ms) })}</p>
            <p>{t('systeme.somfy.attempt', { date: date(collection?.last_attempt_ms) })}</p>
            {#if definition.id}<code>{definition.id}</code>{/if}
            {#if details[key] !== undefined}
              <pre tabindex="0">{details[key]}</pre>
            {:else}
              <button type="button" disabled={Boolean(busy)} onclick={() => showDefinition(kind, index)}>{busy === key ? t('systeme.somfy.loading') : t('systeme.somfy.definition')}</button>
            {/if}
          </details>
        </div>
      {:else}
        <p>{t(collection?.data ? 'systeme.somfy.empty' : 'systeme.somfy.unobserved')}</p>
      {/each}
      </div>
      {#if (collection?.data?.definitions.length ?? 0) > displayLimit}<p class="qualification">{t('systeme.somfy.programme.limited', { count: displayLimit })}</p>{/if}
    </details>

{/snippet}

<style>
  .sections { display: flex; flex-wrap: wrap; gap: 8px; }
  .sections button { display: inline-flex; align-items: center; gap: 8px; flex: 1; justify-content: center; min-height: 48px; }
  .sections .selected { background: var(--ink); color: var(--surface); border-color: var(--ink); }
  .advanced { border-top: 1px solid var(--line); padding-top: 16px; }
  .advanced > .collection { margin-top: 12px; }
  button:focus-visible, summary:focus-visible { outline: 3px solid var(--blue); outline-offset: 3px; }
  .catalog { display: grid; gap: 10px; min-width: 0; }
  p { margin: 0; font-size: 13px; overflow-wrap: anywhere; }
  .qualification { color: var(--warm-ink); }
  .collection { border: 1px solid var(--line); border-radius: var(--r-sm); padding: 12px; min-width: 0; }
  summary { min-height: 44px; cursor: pointer; overflow-wrap: anywhere; }
  summary span { display: block; font-size: 12px; color: var(--ink-2); margin-top: 4px; }
  details > p { margin-top: 10px; }
  .scenario-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 300px), 1fr)); gap: 14px; align-items: start; }
  .scenario-grid .definition { border: 1px solid var(--line); border-radius: var(--r-md); background: var(--surface); padding: 18px; }
  .definition { display: grid; gap: 7px; min-width: 0; border-top: 1px solid var(--line); padding-top: 12px; margin-top: 12px; overflow-wrap: anywhere; }
  code { color: var(--ink-2); }
  .original summary { color: var(--ink-2); font-size: 12px; }
  .original code, .original button, .original pre { display: block; margin-top: 10px; }
  button { justify-self: start; max-width: 100%; border: 1px solid var(--line); border-radius: var(--r-sm); padding: 8px 12px; font: inherit; color: var(--ink); background: var(--surface); cursor: pointer; min-height: 44px; }
  pre { margin: 0; min-width: 0; max-width: 100%; max-height: 320px; overflow: auto; padding: 12px; background: var(--surface); border-radius: var(--r-sm); font-size: 12px; }
</style>
