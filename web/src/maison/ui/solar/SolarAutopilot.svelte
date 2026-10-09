<script>
  import Icon from '../Icon.svelte';
  import { ico } from './solar-icons.js';
  import { pct, pts, eurDelta, kwhs, hours, loadName, noteOf } from './format.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** What Moli would steer onto the sun: each movable load, a switch, and
   *  its effect computed on the spot. On top, the panels alone against the
   *  panels with Moli. */
  let { loads = [], on = {}, shares = {}, effects = {}, plain = null, totals = null, ontoggle, onshare } = $props();

  const SHARES = [0.25, 0.5, 0.75, 1];
  const anyOn = $derived(loads.some((l) => on[l.id]));
  const scale = (t) => (t?.days ? 30 / t.days : 1);
  const extra = $derived(plain && totals ? (totals.gain - plain.gain) * scale(totals) : 0);
</script>

<div class="compare" aria-label={t('energie.solaire.auto.comparaison')}>
  <div class="row">
    <span class="who">{t('energie.solaire.auto.seuls')}</span>
    <span class="track"><i style:width="{(plain?.autoconsumption ?? 0) * 100}%"></i></span>
    <b class="num">{pct(plain?.autoconsumption)}</b>
  </div>
  <div class="row moli">
    <span class="who">{t('energie.solaire.auto.avec_moli')}</span>
    <span class="track"><i style:width="{(totals?.autoconsumption ?? 0) * 100}%"></i></span>
    <b class="num">{pct(totals?.autoconsumption)}</b>
  </div>
  <p class="muted small">
    {#if anyOn}
      {t('energie.solaire.auto.part_rapporte', { eur: eurDelta(extra) })}
    {:else}
      {t('energie.solaire.auto.part_active')}
    {/if}
  </p>
</div>

<ul class="loads">
  {#each loads as l (l.id)}
    {@const e = effects[l.id]}
    {@const active = !!on[l.id]}
    <li class:active>
      <span class="badge"><Icon path={ico(l.icon)} size={22} /></span>
      <div class="body">
        <div class="head">
          <b>{loadName(l)}</b>
          {#if l.hypothetical}<span class="chip warm">{t('energie.solaire.hypothetique')}</span>{/if}
        </div>
        <p class="muted small">
          {t('energie.solaire.auto.habituel', { hours: hours(l.usual), kwh: kwhs(l.perDay) })}
        </p>
        {#if e}
          <p class="effect small">
            {#if active}
              {e.window ? t('energie.solaire.auto.lancerait', { from: e.window.from, to: e.window.to }) : t('energie.solaire.auto.pas_de_surplus')}
            {:else}
              {t('energie.solaire.auto.si_pilote')}
            {/if}
            <span class="chip {e.gain > 0.05 ? 'good' : ''}">{t('energie.solaire.par_mois', { eur: eurDelta(e.gain) })}</span>
            <span class="chip {e.points > 0.5 ? 'good' : ''}">{t('energie.solaire.pts_autoconsommation', { pts: pts(e.points) })}</span>
          </p>
        {/if}
        {#if active}
          <div class="share">
            <span class="small">{t('energie.solaire.auto.part')} <small class="muted">{t('energie.solaire.hypothese')}</small></span>
            <div class="seg" role="group" aria-label={t('energie.solaire.auto.part_de', { name: loadName(l) })}>
              {#each SHARES as s (s)}
                <button class:on={(shares[l.id] ?? l.flex) === s} onclick={() => onshare?.(l.id, s)}>{t('energie.pct', { n: Math.round(s * 100) })}</button>
              {/each}
            </div>
          </div>
        {/if}
        <p class="note">{noteOf(l.kinds, l.hypothetical)}</p>
      </div>
      <button
        class="switch"
        role="switch"
        aria-checked={active}
        aria-label={t('energie.solaire.auto.piloter', { name: loadName(l) })}
        onclick={() => ontoggle?.(l.id, !active)}
      >
        <span class="knob"></span>
      </button>
    </li>
  {:else}
    <li class="empty muted">{t('energie.solaire.auto.aucun')}</li>
  {/each}
</ul>

<style>
  .compare {
    display: grid;
    gap: 10px;
    padding: 16px;
    border-radius: var(--r-md);
    background: var(--surface-2);
    margin-bottom: 16px;
  }

  .row {
    display: grid;
    grid-template-columns: minmax(130px, 200px) minmax(0, 1fr) 52px;
    align-items: center;
    gap: 12px;
  }

  .who {
    font-size: 14px;
    font-weight: 600;
    color: var(--ink-2);
  }

  .moli .who {
    color: var(--ink);
  }

  .track {
    height: 12px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .track i {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: color-mix(in srgb, var(--self) 45%, var(--surface-3));
    transition: width 0.6s var(--ease);
  }

  .moli .track i {
    background: var(--self);
  }

  .row b {
    text-align: right;
    font-size: 16px;
    font-weight: 700;
  }

  .small {
    font-size: 13px;
  }

  .loads {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 10px;
  }

  .loads li {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: 14px;
    align-items: start;
    padding: 14px;
    border-radius: var(--r-md);
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--line);
    transition: box-shadow 0.25s var(--ease), background 0.25s var(--ease);
  }

  .loads li.active {
    background: color-mix(in srgb, var(--self) 6%, var(--surface));
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--self) 55%, var(--line));
  }

  .loads li.empty {
    display: block;
    font-size: 14px;
  }

  .badge {
    width: 44px;
    height: 44px;
    border-radius: 14px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .active .badge {
    background: var(--good-soft);
    color: var(--self);
  }

  .body {
    display: grid;
    gap: 6px;
    min-width: 0;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    font-size: 15px;
  }

  .head b {
    font-weight: 700;
  }

  .effect {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 8px;
    color: var(--ink-2);
  }

  .effect .chip {
    padding: 4px 10px;
    font-size: 12.5px;
  }

  .note {
    font-size: 12px;
    color: var(--ink-3);
    line-height: 1.4;
  }

  .share {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
  }

  .seg {
    display: inline-flex;
    padding: 3px;
    background: var(--surface-2);
    border-radius: 999px;
    gap: 2px;
  }

  .seg button {
    border: 0;
    background: none;
    border-radius: 999px;
    padding: 5px 10px;
    font-size: 12.5px;
    font-weight: 650;
    color: var(--ink-3);
  }

  .seg button.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  /* The switch: a big, plain toggle. */
  .switch {
    width: 52px;
    height: 32px;
    flex: none;
    border: 0;
    border-radius: 999px;
    background: var(--surface-3);
    padding: 3px;
    display: flex;
    transition: background 0.25s var(--ease);
  }

  .switch .knob {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 25%);
    transition: transform 0.25s var(--ease);
  }

  .switch[aria-checked='true'] {
    background: var(--self);
  }

  .switch[aria-checked='true'] .knob {
    transform: translateX(20px);
  }

  @media (max-width: 560px) {
    .row {
      grid-template-columns: minmax(0, 1fr) 48px;
    }

    .row .track {
      grid-column: 1 / -1;
      grid-row: 2;
    }

    .loads li {
      grid-template-columns: minmax(0, 1fr) auto;
    }

    .badge {
      display: none;
    }
  }
</style>
