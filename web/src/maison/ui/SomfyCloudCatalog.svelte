<script>
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { collections, safeDate } from '../lib/somfy-cloud.js';
  import { createProgrammeContext } from '../lib/somfy-programmes.js';
  import SomfyCloudProgramme from './SomfyCloudProgramme.svelte';
  let { observation, details = {}, busy = '', showDefinition } = $props();
  let expanded = $state({ calendar_rules: true });
  let context = $derived(createProgrammeContext(observation.catalog, observation.freshness.collections));
  const readable = new Set(['scenarios', 'calendar_days', 'calendar_rules']);
  const displayLimit = 50;
  const date = (value) => safeDate(value, locale()) ?? t('systeme.somfy.unknown_date');
</script>

<div class="catalog">
  <p class="qualification" role="status">{t(`systeme.somfy.${observation.freshness.current ? 'fresh' : 'stale'}`)}</p>
  {#if observation.catalog.provider_timezone}<p>{t('systeme.somfy.zone', { zone: observation.catalog.provider_timezone })}</p>{/if}
  <p class="muted">{t('systeme.somfy.zone_note')}</p>
  <p class="muted">{t('systeme.somfy.programme.read_only')}</p>
  {#each collections as kind}
    {@const collection = observation.catalog.collections[kind]}
    <details class="collection" open={expanded[kind] ?? false} ontoggle={(event) => expanded[kind] = event.currentTarget.open}>
      <summary><b>{t(`systeme.somfy.collection.${kind}`)}</b><span>{t(`systeme.somfy.status.${collection?.status ?? 'not_observed'}`)}{#if collection?.data}{' · '}{collection.data.definitions.length}{#if !observation.freshness.collections?.[kind]}{' · '}{t('systeme.somfy.historical_count')}{/if}{/if}</span></summary>
      <p>{t('systeme.somfy.last', { date: date(collection?.last_observation_ms) })}</p>
      <p class="muted">{t('systeme.somfy.attempt', { date: date(collection?.last_attempt_ms) })}</p>
      {#if collection?.data && !observation.freshness.collections?.[kind]}<p class="qualification">{t('systeme.somfy.stale')}</p>{/if}
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
      {#if (collection?.data?.definitions.length ?? 0) > displayLimit}<p class="qualification">{t('systeme.somfy.programme.limited', { count: displayLimit })}</p>{/if}
    </details>
  {/each}
</div>

<style>
  .catalog { display: grid; gap: 10px; min-width: 0; }
  p { margin: 0; font-size: 13px; overflow-wrap: anywhere; }
  .qualification { color: var(--warm-ink); }
  .collection { border: 1px solid var(--line); border-radius: var(--r-sm); padding: 12px; min-width: 0; }
  summary { cursor: pointer; overflow-wrap: anywhere; }
  summary span { display: block; font-size: 12px; color: var(--ink-2); margin-top: 4px; }
  details > p { margin-top: 10px; }
  .definition { display: grid; gap: 7px; min-width: 0; border-top: 1px solid var(--line); padding-top: 12px; margin-top: 12px; overflow-wrap: anywhere; }
  code { color: var(--ink-2); }
  .original summary { color: var(--ink-2); font-size: 12px; }
  .original code, .original button, .original pre { display: block; margin-top: 10px; }
  button { justify-self: start; max-width: 100%; border: 1px solid var(--line); border-radius: var(--r-sm); padding: 8px 12px; font: inherit; color: var(--ink); background: var(--surface); cursor: pointer; }
  pre { margin: 0; min-width: 0; max-width: 100%; max-height: 320px; overflow: auto; padding: 12px; background: var(--surface); border-radius: var(--r-sm); font-size: 12px; }
</style>
