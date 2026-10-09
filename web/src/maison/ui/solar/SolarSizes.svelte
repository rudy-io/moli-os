<script>
  import { pct, eur, kwhs } from './format.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** Three sizes of installation side by side, with the same pilotage and
   *  battery: more panels cover more of the house, but a bigger share of
   *  their output goes to the grid. `sizes`: `[{ kwc, ...summarize }]`. */
  let { sizes = [], current, onpick } = $props();

  const per = (t) => (t.days ? 30 / t.days : 1);
</script>

<div class="sizes" role="group" aria-label={t('energie.solaire.tailles.comparer')}>
  {#each sizes as s (s.kwc)}
    <button class="size" class:on={s.kwc === current} onclick={() => onpick?.(s.kwc)} aria-pressed={s.kwc === current}>
      <span class="kwc">{s.kwc} <small>{t('energie.solaire.kwc')}</small></span>
      <span class="line"><span class="muted">{t('energie.solaire.tailles.produit')}</span><b class="num">{kwhs(s.pv * per(s))}</b></span>
      <span class="line"><span class="muted">{t('energie.solaire.kpi.autoconsommation')}</span><b class="num">{pct(s.autoconsumption)}</b></span>
      <span class="line"><span class="muted">{t('energie.solaire.kpi.autoproduction')}</span><b class="num">{pct(s.autoproduction)}</b></span>
      <span class="line gain"><span>{t('energie.solaire.tailles.gain')}</span><b class="num">{eur(s.gain * per(s))}</b></span>
      <span class="unit muted">{t('energie.solaire.tailles.par_mois')}</span>
    </button>
  {/each}
</div>

<style>
  .sizes {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 10px;
  }

  .size {
    display: grid;
    gap: 6px;
    text-align: left;
    padding: 14px;
    border: 0;
    border-radius: var(--r-md);
    background: var(--surface-2);
    color: inherit;
    transition: box-shadow 0.2s var(--ease), background 0.2s var(--ease);
  }

  .size.on {
    background: var(--surface);
    box-shadow: inset 0 0 0 2px var(--pv), var(--shadow);
  }

  .kwc {
    font-size: 26px;
    font-weight: 300;
    letter-spacing: -0.02em;
    margin-bottom: 4px;
  }

  .kwc small {
    font-size: 14px;
    font-weight: 600;
    color: var(--ink-3);
  }

  .line {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-size: 13px;
  }

  .line b {
    font-weight: 650;
  }

  .gain {
    padding-top: 6px;
    border-top: 1px solid var(--line);
    font-weight: 650;
  }

  .unit {
    font-size: 11.5px;
    text-align: right;
  }

  @media (max-width: 560px) {
    .sizes {
      grid-template-columns: minmax(0, 1fr);
    }

    .size {
      grid-template-columns: auto 1fr;
      column-gap: 16px;
    }

    .kwc {
      grid-row: span 5;
      align-self: center;
    }
  }
</style>
