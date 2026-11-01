<script>
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { collections, safeDate } from '../lib/somfy-cloud.js';
  let { observation, details = {}, busy = '', showDefinition } = $props();
  const date = (value) => safeDate(value, locale()) ?? t('systeme.somfy.unknown_date');
</script>

<div class="catalog">
  <p class="qualification" role="status">{t(`systeme.somfy.${observation.freshness.current ? 'fresh' : 'stale'}`)}</p>
  {#if observation.catalog.provider_timezone}<p>{t('systeme.somfy.zone', { zone: observation.catalog.provider_timezone })}</p>{/if}
  <p class="muted">{t('systeme.somfy.zone_note')}</p>
  {#each collections as kind}
    {@const collection = observation.catalog.collections[kind]}
    <details class="collection">
      <summary><b>{t(`systeme.somfy.collection.${kind}`)}</b><span>{t(`systeme.somfy.status.${collection?.status ?? 'not_observed'}`)} · {collection?.data?.definitions?.length ?? 0}</span></summary>
      <p>{t('systeme.somfy.last', { date: date(collection?.last_observation_ms) })}</p>
      <p class="muted">{t('systeme.somfy.attempt', { date: date(collection?.last_attempt_ms) })}</p>
      {#if collection?.data && !observation.freshness.collections?.[kind]}<p class="qualification">{t('systeme.somfy.stale')}</p>{/if}
      {#each collection?.data?.definitions ?? [] as definition, index}
        {@const key = `${kind}:${index}`}
        <div class="definition">
          <b>{definition.label ?? definition.id ?? `#${index + 1}`}</b>
          {#if definition.id}<code>{definition.id}</code>{/if}
          <small>{t('systeme.somfy.opaque')}</small>
          {#if details[key] !== undefined}
            <pre tabindex="0">{details[key]}</pre>
          {:else}
            <button type="button" disabled={Boolean(busy)} onclick={() => showDefinition(kind, index)}>{busy === key ? t('systeme.somfy.loading') : t('systeme.somfy.definition')}</button>
          {/if}
        </div>
      {:else}
        <p>{t(collection?.data ? 'systeme.somfy.empty' : 'systeme.somfy.unobserved')}</p>
      {/each}
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
  code, small { color: var(--ink-2); }
  button { justify-self: start; max-width: 100%; border: 1px solid var(--line); border-radius: var(--r-sm); padding: 8px 12px; font: inherit; color: var(--ink); background: var(--surface); cursor: pointer; }
  pre { margin: 0; min-width: 0; max-width: 100%; max-height: 320px; overflow: auto; padding: 12px; background: var(--surface); border-radius: var(--r-sm); font-size: 12px; }
</style>
