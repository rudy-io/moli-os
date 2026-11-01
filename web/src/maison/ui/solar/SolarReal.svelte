<script>
  import { hub, nameOf } from '../../lib/home.svelte.js';
  import { command } from '../../../lib/hub.svelte.js';
  import { flowNow } from '../../lib/solar-sim.js';
  import { t } from '../../../lib/i18n.svelte.js';
  import Icon from '../Icon.svelte';
  import { ico } from './solar-icons.js';
  import { pct, kw, kwhs, eur } from './format.js';
  import SolarFlow from './SolarFlow.svelte';

  /** The home's real panels (`solar` meters of `[energy]`): what they make
   *  right now, where it goes, what the day and the month gave, and the loads
   *  Moli runs on the surplus. `summary`: `/api/energy`. */
  let { summary } = $props();

  /** A point's live value (event stream), else what the summary said. */
  const live = (point, fallback = null) => {
    if (!point) return fallback;
    const i = point.indexOf('/');
    const v = hub.devices[point.slice(0, i)]?.state?.[point.slice(i + 1)]?.value;
    return typeof v === 'number' ? v : fallback;
  };

  const panels = $derived(summary.meters.filter((m) => m.role === 'solar'));
  const feed = $derived(summary.meters.find((m) => m.role === 'total'));
  const panelW = $derived(panels.map((m) => ({ ...m, w: live(m.power, m.power_w) })));
  const pvW = $derived(panelW.reduce((sum, m) => sum + Math.max(0, m.w ?? 0), 0));
  const gridW = $derived(feed ? live(feed.power, feed.power_w) : (summary.flows?.grid_w ?? null));
  const flow = $derived(flowNow(pvW / 1000, gridW == null ? pvW / 1000 : Math.max(0, (gridW + pvW) / 1000)));

  const periods = $derived([
    { key: 'today', label: t('energie.solaire.reel.aujourdhui'), p: summary.today },
    { key: 'month', label: t('energie.solaire.reel.mois'), p: summary.month },
  ]);

  // The loads Moli drives (`charges:<id>`).
  const loads = $derived(Object.values(hub.devices).filter((d) => d.instance === 'charges'));
  const MODES = ['auto', 'heures_creuses', 'marche', 'arret'];
  const why = (d) => {
    const r = d.state?.reason?.value;
    return r ? t(`energie.solaire.reel.raison.${r}`) : '';
  };
</script>

<section class="card real" aria-label={t('energie.solaire.reel.titre')}>
  <div class="card-head">
    <h2><Icon path={ico('solar')} size={18} />{t('energie.solaire.reel.titre')}</h2>
    <span class="chip live">{t('energie.solaire.reel.mesure')}</span>
  </div>
  <div class="grid">
    <div class="flow">
      <SolarFlow {flow} />
      <p class="muted small center">
        {gridW == null ? t('energie.solaire.reel.sans_pince') : gridW < 0 ? t('energie.solaire.reel.injecte_now', { kw: kw(-gridW / 1000) }) : t('energie.solaire.reel.achete_now', { kw: kw(gridW / 1000) })}
      </p>
    </div>
    <div class="figures">
      {#each periods as { key, label, p } (key)}
        <div class="period">
          <h3>{label}</h3>
          <dl>
            <dt>{t('energie.solaire.reel.production')}</dt>
            <dd class="pv">{kwhs(p.production?.kwh)}</dd>
            <dt>{t('energie.solaire.reel.autoconso')}</dt>
            <dd>{pct(p.self_consumption_rate)} <span class="muted">({kwhs(p.self_consumed)})</span></dd>
            <dt>{t('energie.solaire.reel.autoprod')}</dt>
            <dd>{pct(p.self_sufficiency)}</dd>
            <dt>{t('energie.solaire.reel.injecte')}</dt>
            <dd>{kwhs(p.export?.kwh)}{#if p.export?.cost != null} <span class="muted">({eur(p.export.cost)})</span>{/if}</dd>
            <dt>{t('energie.solaire.reel.achete')}</dt>
            <dd>{kwhs(p.total?.kwh)}{#if p.total?.cost != null} <span class="muted">({eur(p.total.cost)})</span>{/if}</dd>
          </dl>
        </div>
      {/each}
    </div>
  </div>

  {#if panelW.length > 1}
    <ul class="panels">
      {#each panelW as m (m.id)}
        <li><span>{m.name}</span><b>{m.w == null ? '–' : kw(Math.max(0, m.w) / 1000)}</b></li>
      {/each}
    </ul>
  {/if}

  {#each loads as d (d.id)}
    <div class="load">
      <div>
        <b>{nameOf(d.id)}</b>
        <span class="muted small">{why(d)}{#if d.state?.ran_minutes?.value} · {t('energie.solaire.reel.tourne', { n: d.state.ran_minutes.value })}{/if}</span>
      </div>
      <div class="modes" role="radiogroup" aria-label={t('energie.solaire.reel.mode')}>
        {#each MODES as m (m)}
          <button
            type="button"
            role="radio"
            aria-checked={d.state?.mode?.value === m}
            class:on={d.state?.mode?.value === m}
            onclick={() => command(`${d.id}/mode`, m)}>{t(`energie.solaire.reel.modes.${m}`)}</button
          >
        {/each}
      </div>
    </div>
  {/each}
</section>

<style>
  .real {
    display: grid;
    gap: 16px;
  }

  .chip.live {
    background: color-mix(in srgb, var(--good) 16%, var(--surface));
    color: var(--good);
    font-weight: 700;
  }

  .grid {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 20px;
    align-items: center;
  }

  @media (max-width: 720px) {
    .grid {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .figures {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 14px;
  }

  .period h3 {
    margin: 0 0 6px;
    font-size: 14px;
    color: var(--ink-2);
  }

  dl {
    display: grid;
    grid-template-columns: auto auto;
    gap: 4px 12px;
    margin: 0;
    font-size: 14px;
  }

  dt {
    color: var(--ink-2);
  }

  dd {
    margin: 0;
    text-align: right;
    font-weight: 650;
    font-variant-numeric: tabular-nums;
  }

  dd .muted {
    margin-left: 0.3em;
    white-space: nowrap;
  }

  dd.pv {
    color: var(--pv-ink, var(--ink));
  }

  .panels {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 18px;
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: 13.5px;
  }

  .panels li {
    display: flex;
    gap: 8px;
  }

  .load {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding-top: 12px;
    border-top: 1px solid var(--line, color-mix(in srgb, var(--ink) 10%, transparent));
  }

  .load > div:first-child {
    display: grid;
    gap: 2px;
  }

  .modes {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .modes button {
    padding: 6px 12px;
    border: 0;
    border-radius: 999px;
    background: var(--surface-2, var(--surface));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--ink) 14%, transparent);
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
  }

  .modes button.on {
    background: color-mix(in srgb, var(--pv, var(--sun)) 22%, var(--surface));
    color: var(--ink);
  }

  .center {
    text-align: center;
  }
</style>
