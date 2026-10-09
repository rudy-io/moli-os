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

<header>
  <div class="brand">
    <svg viewBox="0 0 32 32" aria-hidden="true">
      <circle cx="16" cy="16" r="15" fill="var(--sun)" />
      <circle cx="16" cy="16" r="8.5" fill="none" stroke="#161512" stroke-width="4.3" />
    </svg>
    <span>moli <b>os</b></span>
    <span class="live" class:on={hub.connected} title={hub.connected ? t('systeme.entete.flux_connecte') : t('systeme.entete.reconnexion')}></span>
  </div>

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

  <HumanSession />

  <ul class="drivers" aria-label={t('systeme.entete.pilotes')}>
    {#each hub.drivers as d (d.instance)}
      <li class={d.status.state} title={d.status.error ?? d.status.state}>
        <span class="dot"></span>{d.instance}
      </li>
    {/each}
  </ul>
</header>

<style>
  header {
    display: flex;
    align-items: center;
    gap: 32px;
    flex-wrap: wrap;
    padding: 14px 28px;
    border-bottom: var(--stroke) solid var(--ink);
    background: var(--card);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 22px;
    font-weight: 800;
    letter-spacing: -0.02em;
  }
  .brand svg {
    width: 28px;
    height: 28px;
  }
  .brand b {
    color: var(--copper);
    font-weight: 800;
  }
  .live {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ink-3);
  }
  .live.on {
    background: var(--ok);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 25%, transparent);
  }
  .stats {
    display: flex;
    gap: 28px;
    margin: 0;
  }
  .stats div {
    display: grid;
  }
  dt {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-3);
  }
  dd {
    margin: 0;
    font-size: 18px;
    font-weight: 700;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .vs {
    font: inherit;
    font-size: 11px;
    font-weight: 700;
    padding: 2px 6px;
    border: 0;
    border-radius: 3px;
    background: var(--sun);
    color: #161512;
    cursor: pointer;
  }
  .drivers {
    display: flex;
    gap: 8px;
    margin: 0 0 0 auto;
    padding: 0;
    list-style: none;
  }
  .drivers li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border: var(--stroke) solid var(--line);
    border-radius: 999px;
    font-size: 13px;
    font-weight: 600;
  }
  .dot {
    width: 7px;
    height: 7px;
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
    padding: 4px 10px;
    border-radius: 999px;
    background: var(--ink);
    color: var(--bg);
    font-size: 13px;
    font-weight: 600;
  }
  @media (max-width: 700px) {
    header {
      padding: 12px 16px;
      gap: 16px;
    }
    .stats {
      gap: 18px;
      flex-wrap: wrap;
    }
    .drivers {
      margin-left: 0;
    }
  }
</style>
