<script>
  import { onMount } from 'svelte';
  import { hub, home, value, relative } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import Gauge from '../ui/infra/Gauge.svelte';
  import Spark from '../ui/infra/Spark.svelte';
  import PcCard from '../ui/infra/PcCard.svelte';

  /** The house's machines, the digital side of « is everything fine at
   *  home? »: the server (live from Moli), its disks and containers (the host
   *  script, every minute), the computers (awake? wake them), the box, Moli. */

  let infra = $state(null);
  let infraMissing = $state(false);
  async function load() {
    try {
      const res = await fetch('/api/infra');
      infraMissing = res.status === 404;
      if (res.ok) infra = await res.json();
    } catch {
      /* next round */
    }
  }
  onMount(() => {
    load();
    const timer = setInterval(load, 30_000);
    return () => clearInterval(timer);
  });

  const devices = $derived(Object.values(hub.devices));
  const server = $derived(devices.find((d) => d.model === 'Serveur de la maison'));
  const pcs = $derived(devices.filter((d) => d.model === 'Ordinateur'));
  const box = $derived(devices.find((d) => d.points.some((p) => p.key === 'external_ip')));
  const sv = (k) => (server ? value(server.id, k) : null);

  // ---- words and numbers ----
  const GB = 1024 ** 3;
  const gb = (bytes) => (typeof bytes === 'number' ? t('systeme.format.go', { value: (bytes / GB).toLocaleString(locale(), { maximumFractionDigits: bytes < 10 * GB ? 1 : 0 }) }) : '—');
  const mb = (bytes) => (typeof bytes === 'number' ? (bytes < GB ? t('systeme.format.mo', { value: Math.round(bytes / 1024 ** 2) }) : gb(bytes)) : '—');
  function since(seconds) {
    if (typeof seconds !== 'number') return '—';
    const d = Math.floor(seconds / 86400);
    const h = Math.floor((seconds % 86400) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    return d ? t('systeme.format.duree_jours', { d, h }) : h ? t('systeme.format.duree_heures', { h, m }) : t('systeme.format.duree_minutes', { m });
  }

  // ---- containers ----
  const containers = $derived(infra?.containers ?? []);
  const running = $derived(containers.filter((c) => c.state === 'running'));
  const stopped = $derived(containers.filter((c) => c.state !== 'running'));
  let allRunning = $state(false);
  let showStopped = $state(false);
  const shown = $derived(allRunning ? running : running.slice(0, 8));
  const cpuSum = $derived(running.reduce((sum, c) => sum + (c.cpu ?? 0), 0));

  // ---- what needs a look ----
  const issues = $derived.by(() => {
    const out = [];
    if (sv('cpu_temperature') >= 85) out.push(t('systeme.infra.alerte.processeur', { temp: sv('cpu_temperature') }));
    if (sv('ram') >= 90) out.push(t('systeme.infra.alerte.memoire', { pct: sv('ram') }));
    // The swap full too: the machine is short of memory, it slows down.
    if (sv('swap') >= 95 && sv('ram') >= 75) out.push(t('systeme.infra.alerte.echange', { pct: sv('swap') }));
    for (const d of infra?.disks ?? []) if (d.percent >= 90) out.push(t('systeme.infra.alerte.disque', { label: d.label, pct: Math.round(d.percent) }));
    for (const c of running) if (c.health === 'unhealthy') out.push(t('systeme.infra.alerte.conteneur', { name: c.name }));
    for (const u of infra?.failed ?? []) out.push(t('systeme.infra.alerte.service', { name: u }));
    for (const b of infra?.backups ?? []) {
      if (!b.ok) out.push(t('systeme.infra.alerte.sauvegarde_erreur', { label: b.label }));
      else if (home.now - b.at > 36 * 3_600_000) out.push(t('systeme.infra.alerte.sauvegarde_ancienne', { label: b.label, days: Math.round((home.now - b.at) / 86_400_000) }));
    }
    if (server && server.online === false) out.push(t('systeme.infra.alerte.serveur'));
    return out;
  });
  const stale = $derived(infra && home.now - infra.ts > 5 * 60_000);
</script>

<div class="infra">
  <header class="head">
    <div>
      <h1 class="page-title">{t('systeme.infra.titre')}</h1>
      <p class="page-sub">{t('systeme.infra.sous_titre')}</p>
    </div>
    {#if issues.length}
      <span class="chip warm"><Icon name="warning" size={15} />{t('systeme.infra.a_voir', { count: issues.length })}</span>
    {:else}
      <span class="chip good"><Icon name="ok" size={15} />{t('systeme.infra.tout_va_bien')}</span>
    {/if}
  </header>

  {#if issues.length}
    <ul class="issues">
      {#each issues as issue (issue)}<li><Icon name="warning" size={15} />{issue}</li>{/each}
    </ul>
  {/if}

  <div class="grid">
    {#if server}
      <section class="card server">
        <div class="card-head">
          <h2><Icon name="server" size={18} />{server.label?.name || server.native_name}</h2>
          <span class="muted small">{t('systeme.infra.serveur', { since: since(sv('uptime')) })}</span>
        </div>
        <div class="gauges">
          <Gauge label={t('systeme.infra.processeur')} value={sv('cpu')} sub={sv('cores') ? t('systeme.infra.coeurs', { cores: sv('cores'), load: sv('load') ?? '—' }) : ''} warn={[75, 92]} />
          <Gauge label={t('systeme.infra.memoire')} value={sv('ram')} sub={sv('ram_used') != null ? t('systeme.infra.go_sur', { used: sv('ram_used'), total: sv('ram_total') }) : ''} warn={[85, 95]} />
          <Gauge label={t('systeme.infra.temperature')} value={sv('cpu_temperature')} unit="°" max={100} warn={[75, 85]} sub={t('systeme.infra.processeur_min')} />
          {#if infra?.disks?.[0]}
            <Gauge label={t('systeme.infra.disque')} value={infra.disks[0].percent} sub={t('systeme.infra.libres', { size: gb(infra.disks[0].free) })} warn={[85, 93]} />
          {/if}
        </div>
        <div class="facts">
          <span><Icon name="lan" size={15} />↓ {sv('net_down') ?? '—'} · ↑ {sv('net_up') ?? '—'} Mbit/s</span>
          {#if sv('disk_temperature') != null}<span><Icon name="harddisk" size={15} />SSD {sv('disk_temperature')} °C</span>{/if}
          {#if sv('swap')}<span><Icon name="memory" size={15} />{t('systeme.infra.echange', { pct: sv('swap') })}</span>{/if}
        </div>
        <div class="sparks">
          <Spark point="{server.id}/cpu" label={t('systeme.infra.processeur')} unit={t('systeme.format.unite_pourcent')} max={100} />
          <Spark point="{server.id}/cpu_temperature" label={t('systeme.infra.temperature')} unit=" °C" />
        </div>
      </section>
    {/if}

    <section class="card">
      <div class="card-head">
        <h2><Icon name="docker" size={18} />{t('systeme.infra.conteneurs')}</h2>
        <span class="muted small">{t('systeme.infra.en_marche', { count: running.length })}{stopped.length ? ` · ${t('systeme.infra.arretes', { count: stopped.length })}` : ''}{running.length ? ` · ${t('systeme.infra.processeur_pct', { pct: Math.round(cpuSum) })}` : ''}</span>
      </div>
      {#if infraMissing}
        <p class="muted">{t('systeme.infra.releve_absent')}</p>
      {:else if !infra}
        <p class="muted">…</p>
      {:else}
        <ul class="containers">
          {#each shown as c (c.name)}
            <li class:sick={c.health === 'unhealthy'}>
              <i class="dot" class:healthy={c.health === 'healthy'} class:sick={c.health === 'unhealthy'}></i>
              <span class="name" title={c.image}>{c.name}</span>
              <span class="bar" title={t('systeme.infra.processeur_pct', { pct: c.cpu ?? 0 })}><i style:width="{Math.min(100, (c.cpu ?? 0) * 2)}%"></i></span>
              <span class="num cpu">{c.cpu != null ? t('systeme.format.pourcent', { value: c.cpu.toLocaleString(locale()) }) : '—'}</span>
              <span class="num mem">{mb(c.memory)}</span>
            </li>
          {/each}
        </ul>
        <div class="more">
          {#if running.length > 8}
            <button class="link" onclick={() => (allRunning = !allRunning)}>{allRunning ? t('systeme.infra.moins') : t('systeme.infra.les_autres', { count: running.length - 8 })}</button>
          {/if}
          {#if stopped.length}
            <button class="link" onclick={() => (showStopped = !showStopped)}>{showStopped ? t('systeme.infra.cacher_arretes') : t('systeme.infra.voir_arretes', { count: stopped.length })}</button>
          {/if}
        </div>
        {#if showStopped}
          <ul class="stopped">
            {#each stopped as c (c.name)}<li><i class="dot off"></i>{c.name}<small class="muted">{c.status}</small></li>{/each}
          </ul>
        {/if}
        {#if stale}<p class="muted small">{t('systeme.infra.releve_ancien', { when: relative(infra.ts, home.now) })}</p>{/if}
      {/if}
    </section>

    {#if infra}
      <section class="card">
        <div class="card-head"><h2><Icon name="harddisk" size={18} />{t('systeme.infra.disques_sauvegardes')}</h2></div>
        <ul class="disks">
          {#each infra.disks as d (d.path)}
            <li>
              <span class="label">{d.label} <small class="muted">{d.path}</small></span>
              <span class="track" class:warm={d.percent >= 85} class:alert={d.percent >= 93}><i style:width="{d.percent}%"></i></span>
              <span class="num">{gb(d.used)} / {gb(d.total)}</span>
            </li>
          {/each}
        </ul>
        <ul class="backups">
          {#each infra.backups as b (b.label)}
            <li class:bad={!b.ok}>
              <Icon name={b.ok ? 'ok' : 'ko'} size={16} />
              <span><b>{b.label}</b><small class="muted">{relative(b.at, home.now)}{b.last ? ` · ${b.last}` : ''}</small></span>
            </li>
          {/each}
          {#if infra.failed?.length}
            <li class="bad"><Icon name="ko" size={16} /><span><b>{t('systeme.infra.services_echec')}</b><small>{infra.failed.join(', ')}</small></span></li>
          {/if}
        </ul>
      </section>
    {/if}

    {#each pcs as pc (pc.id)}
      <PcCard {pc} />
    {/each}

    {#if box}
      <section class="card">
        <div class="card-head">
          <h2><Icon name="lan" size={18} />{box.label?.name || box.native_name}</h2>
          <span class="chip" class:good={value(box.id, 'connected') === true} class:alert={value(box.id, 'connected') === false}>{value(box.id, 'connected') === false ? t('systeme.infra.internet_coupe') : t('systeme.infra.internet_ok')}</span>
        </div>
        <dl class="kv">
          <dt>{t('systeme.infra.connectee_depuis')}</dt><dd>{since(value(box.id, 'uptime'))}</dd>
          <dt>{t('systeme.infra.ligne')}</dt><dd class="num">↓ {value(box.id, 'line_down') ?? '—'} · ↑ {value(box.id, 'line_up') ?? '—'} Mbit/s</dd>
          <dt>{t('systeme.infra.adresse_publique')}</dt><dd class="num">{value(box.id, 'external_ip') ?? '—'}</dd>
        </dl>
      </section>
    {/if}

    {#if hub.stats}
      <section class="card">
        <div class="card-head"><h2><Icon name="chip" size={18} />Moli</h2><span class="muted small">{t('systeme.infra.depuis', { since: since((hub.stats.uptime_ms ?? 0) / 1000) })}</span></div>
        <dl class="kv">
          <dt>{t('systeme.infra.memoire')}</dt><dd class="num">{mb(hub.stats.rss_bytes)}</dd>
          <dt>{t('systeme.infra.appareils')}</dt><dd class="num">{t('systeme.infra.appareils_points', { devices: hub.stats.devices, points: hub.stats.points })}</dd>
          <dt>{t('systeme.infra.pilotes')}</dt><dd class="num">{t('systeme.infra.pilotes_marche', { running: hub.stats.drivers_running, total: hub.stats.drivers })}</dd>
        </dl>
      </section>
    {/if}
  </div>
</div>

<style>
  .infra {
    display: grid;
    gap: 20px;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: flex-start;
    gap: 10px;
  }

  .issues {
    list-style: none;
    margin: 0;
    padding: 12px 16px;
    border-radius: var(--r-md);
    background: var(--warm-soft);
    color: var(--warm-ink);
    display: grid;
    gap: 6px;
    font-size: 14px;
    font-weight: 600;
  }

  .issues li {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 420px), 1fr));
    gap: 20px;
    align-items: start;
  }

  .card {
    display: grid;
    gap: 14px;
  }

  .card-head {
    flex-wrap: wrap;
    gap: 6px 12px;
  }

  .small {
    font-size: 13px;
  }

  .server {
    grid-column: 1 / -1;
  }

  .gauges {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: 12px;
  }

  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 18px;
    font-size: 13px;
    color: var(--ink-2);
  }

  .facts span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .sparks {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 16px;
  }

  .containers,
  .stopped,
  .disks,
  .backups {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  .containers li {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) 70px 54px 64px;
    align-items: center;
    gap: 10px;
    font-size: 13.5px;
  }

  .containers .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--good);
  }

  .dot.healthy {
    box-shadow: 0 0 0 3px var(--good-soft);
  }

  .dot.sick {
    background: var(--alert);
  }

  .dot.off {
    background: var(--ink-3);
    opacity: 0.5;
  }

  .bar {
    height: 6px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .bar i {
    display: block;
    height: 100%;
    background: var(--cool);
  }

  .cpu,
  .mem {
    text-align: right;
    color: var(--ink-2);
  }

  .more {
    display: flex;
    gap: 16px;
  }

  .link {
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    color: var(--cool);
    cursor: pointer;
  }

  .stopped li {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--ink-2);
  }

  .stopped small {
    margin-left: auto;
  }

  .disks li {
    display: grid;
    grid-template-columns: minmax(0, 1.3fr) minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    font-size: 13.5px;
  }

  .disks .label {
    font-weight: 600;
  }

  .track {
    height: 10px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .track i {
    display: block;
    height: 100%;
    background: var(--good);
  }

  .track.warm i {
    background: var(--warm);
  }

  .track.alert i {
    background: var(--alert);
  }

  .backups li {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    color: var(--good);
  }

  .backups li.bad {
    color: var(--alert);
  }

  .backups span {
    display: grid;
    color: var(--ink);
    font-size: 13.5px;
  }

  .backups small {
    font-size: 12px;
  }

  .kv {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 16px;
    margin: 0;
    font-size: 14px;
  }

  .kv dt {
    color: var(--ink-3);
  }

  .kv dd {
    margin: 0;
    font-weight: 600;
  }


  @media (max-width: 520px) {
    .containers li {
      grid-template-columns: auto minmax(0, 1fr) 54px 60px;
    }

    .containers .bar {
      display: none;
    }
  }
</style>
