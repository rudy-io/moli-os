<script>
  import Icon from '../Icon.svelte';
  import { ico } from './solar-icons.js';
  import { perKwh } from './format.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';

  /** The simulation's hypotheses, all on show and all adjustable. */
  let {
    kwc = $bindable(),
    battery = $bindable(),
    sellPrice = $bindable(),
    batteryCost = $bindable(),
    tilt = $bindable(),
    azimuth = $bindable(),
    losses = $bindable(),
    prices = {},
    priced = false,
    buyPrice = 0.2,
    cloud = null,
    lat,
    lon,
    onreset,
  } = $props();

  const KWC = [3, 6, 9];
  const BATTERY = [0, 5, 10];
  const AZIMUTH = [
    [90, 'energie.solaire.reglages.est'],
    [135, 'energie.solaire.reglages.sud_est'],
    [180, 'energie.solaire.reglages.sud'],
    [225, 'energie.solaire.reglages.sud_ouest'],
    [270, 'energie.solaire.reglages.ouest'],
  ];
  const TILT = [15, 30, 45];
  const LOSSES = [0.1, 0.14, 0.2];

  const round = (v, step) => Math.round(v / step) * step;
  const deg = (v) => v.toLocaleString(locale(), { maximumFractionDigits: 1 });
</script>

<details class="settings">
  <summary>
    <span class="title"><Icon path={ico('tune')} size={18} />{t('energie.solaire.reglages.titre')}</span>
    <span class="muted small">{t('energie.solaire.reglages.resume', { kwc, battery: battery ? t('energie.solaire.reglages.batterie_n', { n: battery }) : t('energie.solaire.reglages.sans_batterie'), price: perKwh(sellPrice) })}</span>
    <span class="chev" aria-hidden="true"><Icon name="chevron-down" size={20} /></span>
  </summary>

  <div class="grid">
    <div class="field">
      <span class="label">{t('energie.solaire.reglages.puissance')}</span>
      <div class="seg" role="group" aria-label={t('energie.solaire.reglages.puissance')}>
        {#each KWC as k (k)}<button class:on={kwc === k} onclick={() => (kwc = k)}>{k} {t('energie.solaire.kwc')}</button>{/each}
      </div>
      <span class="hint">{t('energie.solaire.reglages.puissance_aide', { m2: Math.round(kwc * 5) })}</span>
    </div>

    <div class="field">
      <span class="label">{t('energie.solaire.reglages.batterie')}</span>
      <div class="seg" role="group" aria-label={t('energie.solaire.reglages.batterie')}>
        {#each BATTERY as v (v)}<button class:on={battery === v} onclick={() => (battery = v)}>{v ? `${v} kWh` : t('energie.solaire.reglages.aucune')}</button>{/each}
      </div>
      <span class="hint">{t('energie.solaire.reglages.batterie_aide')}</span>
    </div>

    <div class="field">
      <span class="label">{t('energie.solaire.reglages.orientation')}</span>
      <div class="seg wrap" role="group" aria-label={t('energie.solaire.reglages.orientation_panneaux')}>
        {#each AZIMUTH as [v, l] (v)}<button class:on={azimuth === v} onclick={() => (azimuth = v)}>{t(l)}</button>{/each}
      </div>
    </div>

    <div class="field">
      <span class="label">{t('energie.solaire.reglages.inclinaison')}</span>
      <div class="seg" role="group" aria-label={t('energie.solaire.reglages.inclinaison_panneaux')}>
        {#each TILT as v (v)}<button class:on={tilt === v} onclick={() => (tilt = v)}>{v}°</button>{/each}
      </div>
    </div>

    <div class="field">
      <span class="label">{t('energie.solaire.reglages.pertes')}</span>
      <div class="seg" role="group" aria-label={t('energie.solaire.reglages.pertes')}>
        {#each LOSSES as v (v)}<button class:on={losses === v} onclick={() => (losses = v)}>{t('energie.pct', { n: Math.round(v * 100) })}</button>{/each}
      </div>
      <span class="hint">{t('energie.solaire.reglages.pertes_aide')}</span>
    </div>

    <div class="field">
      <span class="label">{t('energie.solaire.reglages.rachat')} <small class="muted">{t('energie.solaire.hypothese')}</small></span>
      <div class="stepper">
        <button onclick={() => (sellPrice = Math.max(0, round(sellPrice - 0.01, 0.01)))} aria-label={t('energie.solaire.reglages.rachat_baisser')}>−</button>
        <b class="num">{perKwh(sellPrice)}</b>
        <button onclick={() => (sellPrice = Math.min(0.3, round(sellPrice + 0.01, 0.01)))} aria-label={t('energie.solaire.reglages.rachat_monter')}>+</button>
      </div>
      <span class="hint">{t('energie.solaire.reglages.rachat_aide')}</span>
    </div>

    <div class="field">
      <span class="label">{t('energie.solaire.reglages.prix_batterie')} <small class="muted">{t('energie.solaire.hypothese')}</small></span>
      <div class="stepper">
        <button onclick={() => (batteryCost = Math.max(100, batteryCost - 50))} aria-label={t('energie.solaire.reglages.prix_batterie_baisser')}>−</button>
        <b class="num">{batteryCost} €/kWh</b>
        <button onclick={() => (batteryCost = Math.min(2000, batteryCost + 50))} aria-label={t('energie.solaire.reglages.prix_batterie_monter')}>+</button>
      </div>
      <span class="hint">{t('energie.solaire.reglages.prix_batterie_aide')}</span>
    </div>

    <div class="field">
      <span class="label">{t('energie.solaire.reglages.prix_achat')}</span>
      {#if priced}
        <b class="fact">{prices.hp && prices.hc ? t('energie.solaire.reglages.prix_maison_detail', { hp: perKwh(prices.hp), hc: perKwh(prices.hc) }) : t('energie.solaire.reglages.prix_maison')}</b>
        <span class="hint">{t('energie.solaire.reglages.prix_maison_aide')}</span>
      {:else}
        <b class="fact">{perKwh(buyPrice)} <small class="muted">{t('energie.solaire.hypothese')}</small></b>
        <span class="hint">{t('energie.solaire.reglages.prix_inconnu')}</span>
      {/if}
    </div>
  </div>

  <ul class="facts">
    <li><b>{t('energie.solaire.reglages.lieu')}</b> {t('energie.solaire.reglages.lieu_texte', { lat: deg(lat), lon: deg(lon) })}</li>
    <li>
      <b>{t('energie.solaire.reglages.ciel')}</b> {t('energie.solaire.reglages.ciel_texte')}{#if cloud != null} {t('energie.solaire.reglages.ciel_aujourdhui', { n: Math.round(cloud) })}{/if}.
    </li>
    <li><b>{t('energie.solaire.reglages.pas_de_temps')}</b> {t('energie.solaire.reglages.pas_de_temps_texte')}</li>
    <li><b>{t('energie.solaire.reglages.abonnement')}</b> {t('energie.solaire.reglages.abonnement_texte')}</li>
  </ul>

  <button class="reset" onclick={() => onreset?.()}>{t('energie.solaire.reglages.reset')}</button>
</details>

<style>
  .settings {
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
    padding: 4px 20px;
  }

  summary {
    list-style: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 14px;
    padding: 16px 0;
    cursor: pointer;
  }

  summary::-webkit-details-marker {
    display: none;
  }

  .title {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 15px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .chev {
    margin-left: auto;
    color: var(--ink-3);
    transition: transform 0.25s var(--ease);
  }

  details[open] .chev {
    transform: rotate(180deg);
  }

  .small {
    font-size: 13px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
    gap: 18px 28px;
    padding: 6px 0 18px;
  }

  .field {
    display: grid;
    gap: 8px;
    align-content: start;
    justify-items: start;
  }

  .label {
    font-size: 13px;
    font-weight: 700;
    color: var(--ink-2);
  }

  .hint {
    font-size: 12px;
    color: var(--ink-3);
    line-height: 1.4;
  }

  .fact {
    font-size: 14px;
    font-weight: 600;
  }

  .seg {
    display: inline-flex;
    padding: 4px;
    background: var(--surface-2);
    border-radius: 999px;
    gap: 2px;
  }

  .seg.wrap {
    flex-wrap: wrap;
    border-radius: 18px;
  }

  .seg button {
    border: 0;
    background: none;
    border-radius: 999px;
    padding: 7px 12px;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-3);
  }

  .seg button.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .stepper {
    display: inline-flex;
    align-items: center;
    gap: 12px;
  }

  .stepper b {
    min-width: 96px;
    text-align: center;
    font-weight: 650;
  }

  .stepper button {
    width: 34px;
    height: 34px;
    border: 0;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink-2);
    font-size: 18px;
    font-weight: 650;
  }

  .facts {
    list-style: none;
    margin: 0;
    padding: 14px 0 0;
    border-top: 1px solid var(--line);
    display: grid;
    gap: 8px;
    font-size: 13px;
    color: var(--ink-2);
    line-height: 1.45;
  }

  .facts b {
    color: var(--ink);
    font-weight: 650;
    margin-right: 4px;
  }

  .reset {
    margin: 16px 0 18px;
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    background: var(--surface-2);
    color: var(--ink-2);
    font-size: 13px;
    font-weight: 650;
  }
</style>
