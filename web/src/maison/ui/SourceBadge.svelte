<script>
  // Where an outside value comes from (#210, #246): « mesuré » (a station),
  // « prévu » (the forecast), or « prévu · station muette » when a station
  // has the meaning but `dehors:meteo` fell back to the forecast. Shown
  // without hovering (tablets have none). It decides nothing: it reads what
  // `dehors:meteo` says (`<key>_source`, `stations`).
  import { home, value, device, since, nameOf, relative } from '../lib/home.svelte.js';
  import { OUTSIDE, idsOf, pick, originOf } from '../lib/outside.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** `key`: the meaning on `dehors:meteo`. `interactive`: a tap tells the
   *  source and the age (off inside another button). */
  let { key, interactive = true } = $props();

  let open = $state(false);
  const stations = $derived(idsOf(value(OUTSIDE, 'stations')));
  const source = $derived(value(OUTSIDE, `${key}_source`));
  const kind = $derived(device(OUTSIDE) && value(OUTSIDE, key) != null ? originOf(source, stations) : null);
  /** The fallback plays: a station has this meaning, the forecast gave it. */
  const quiet = $derived.by(() => {
    if (kind !== 'forecast') return null;
    const semantic = device(OUTSIDE)?.points.find((p) => p.key === key)?.semantic;
    for (const id of stations) {
      const p = pick(device(id)?.points, semantic, key);
      if (!p) continue;
      const ts = since(id, p.key);
      return ts ? t('maison.dehors.station_muette', { age: relative(ts, home.now) }) : t('maison.dehors.station_sans_mesure');
    }
    return null;
  });
  const words = $derived(kind === 'measured' ? t('maison.dehors.mesure') : quiet ? t('maison.dehors.prevu_repli') : t('maison.dehors.prevu'));
</script>

{#if kind}
  {#if interactive}
    <button type="button" class="src {kind}" aria-expanded={open} onclick={() => (open = !open)}>{words}</button>
    {#if open}
      <em class="muted why">{source ? nameOf(source) : ''} · {relative(since(OUTSIDE, key), home.now)}{#if quiet} · {quiet}{/if}</em>
    {/if}
  {:else}
    <span class="src {kind}">{words}</span>
  {/if}
{/if}

<style>
  .src {
    display: inline-flex;
    align-items: center;
    margin-left: 6px;
    padding: 1px 7px;
    border: 0;
    border-radius: 999px;
    font: inherit;
    font-size: 11px;
    font-weight: 650;
    line-height: 1.5;
    vertical-align: middle;
  }

  button.src {
    cursor: pointer;
  }

  .src.measured {
    color: var(--good);
    background: color-mix(in srgb, var(--good) 14%, transparent);
  }

  .src.forecast {
    color: var(--ink-3);
    background: var(--surface-3);
  }

  .why {
    display: block;
    flex-basis: 100%;
    font-size: 12px;
  }
</style>
