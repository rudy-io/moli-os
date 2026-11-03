<script>
  import { hub } from '../lib/hub.svelte.js';
  import { formatBytes, formatDuration } from '../lib/format.js';
  import { t } from '../lib/i18n.svelte.js';
  import HumanSession from './HumanSession.svelte';

  // Reference point: the Home Assistant container this hub runs next to,
  // as last measured on the host (`data/bench.json`).
  const ratio = $derived(
    hub.stats?.rss_bytes && hub.haMemory ? Math.round(hub.haMemory / hub.stats.rss_bytes) : null,
  );
</script>

<header class="head">
  <div>
    <h1 class="page-title">
      {t('commun.nav.atelier')}
      <span class="live" class:on={hub.connected} title={hub.connected ? t('systeme.entete.flux_connecte') : t('systeme.entete.reconnexion')}></span>
    </h1>
    <p class="page-sub">{t('commun.atelier.sous_titre')}</p>
  </div>
  <HumanSession />
</header>

<section class="card overview">
  <dl class="stats">
    <div>
      <dt>{t('systeme.entete.memoire')}</dt>
      <dd class="num">
        {formatBytes(hub.stats?.rss_bytes)}
        {#if ratio}<button class="vs" title={t('systeme.entete.comparer')} onclick={() => (hub.bench = true)}>÷{ratio} vs HA</button>{/if}
      </dd>
    </div>
    <div>
      <dt>{t('systeme.entete.appareils')}</dt>
      <dd class="num">{hub.stats?.devices ?? '—'}</dd>
    </div>
    <div>
      <dt>{t('systeme.entete.points')}</dt>
      <dd class="num">{hub.stats?.points ?? '—'}</dd>
    </div>
    <div>
      <dt>{t('systeme.entete.en_ligne_depuis')}</dt>
      <dd class="num">{formatDuration(hub.stats?.uptime_ms)}</dd>
    </div>
  </dl>

  {#if hub.guard?.quiet_hours?.active}
    <span class="quiet" title={t('systeme.entete.heures_calmes_aide', { timezone: hub.guard.quiet_hours.timezone })}>
      {t('systeme.entete.heures_calmes', { to: hub.guard.quiet_hours.to })}
    </span>
  {/if}

  <ul class="drivers" aria-label={t('systeme.entete.pilotes')}>
    {#each hub.drivers as d (d.instance)}
      <li class={d.status.state} title={d.status.error ?? d.status.state}>
        <span class="dot"></span>{d.instance}
      </li>
    {/each}
  </ul>
</section>

<style>
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    flex-wrap: wrap;
    gap: 12px 20px;
    margin-bottom: 18px;
  }
  .page-title {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .live {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--ink-3);
  }
  .live.on {
    background: var(--ok);
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--ok) 22%, transparent);
  }
  .overview {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 18px 32px;
    padding: 18px 22px;
    margin-bottom: 28px;
  }
  .stats {
    display: flex;
    flex-wrap: wrap;
    gap: 16px 32px;
    margin: 0;
  }
  .stats div {
    display: grid;
    gap: 2px;
  }
  dt {
    font-size: 13px;
    color: var(--ink-3);
  }
  dd {
    margin: 0;
    font-size: 22px;
    font-weight: 700;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .vs {
    font: inherit;
    font-size: 12px;
    font-weight: 700;
    padding: 3px 9px;
    border: 0;
    border-radius: 999px;
    background: var(--sun-soft);
    color: var(--copper);
    cursor: pointer;
  }
  .drivers {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .drivers li {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 6px 12px;
    border-radius: 999px;
    background: var(--surface-2, var(--card));
    font-size: 13.5px;
    font-weight: 600;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ink-3);
  }
  .running .dot {
    background: var(--ok);
  }
  .backoff .dot {
    background: var(--alarm);
  }
  .waiting .dot {
    background: var(--sun);
  }
  .quiet {
    padding: 6px 12px;
    border-radius: 999px;
    background: var(--cool-soft, var(--ink));
    color: var(--cool, var(--bg));
    font-size: 13.5px;
    font-weight: 600;
  }
</style>
