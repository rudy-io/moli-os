<script>
  import { home, value, relative } from '../../lib/home.svelte.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';
  import Icon from '../Icon.svelte';
  import Gauge from './Gauge.svelte';
  import Spark from './Spark.svelte';

  /** A Proxmox node (the `proxmox` driver, `lib/proxmox.js`): the node's
   *  gauges, one line per guest, its storages, the last backup, its disks.
   *  Read only: no button, the driver never changes anything. */
  let { group } = $props();

  const nameOf = (d) => d.label?.name || d.native_name;
  const v = (d, k) => (d ? value(d.id, k) : null);
  const pct = (x) => (typeof x === 'number' ? t('systeme.format.pourcent', { value: Math.round(x) }) : '—');
  const gio = (x) =>
    typeof x === 'number' ? t('systeme.format.go', { value: x.toLocaleString(locale(), { maximumFractionDigits: x < 10 ? 1 : 0 }) }) : '—';
  function hours(h) {
    if (typeof h !== 'number') return '—';
    const d = Math.floor(h / 24);
    return d ? t('systeme.format.duree_jours', { d, h: Math.floor(h % 24) }) : t('systeme.format.duree_heures', { h: Math.floor(h), m: Math.round((h % 1) * 60) });
  }
  const tone = (x, warm = 80, alert = 90) => (typeof x !== 'number' ? '' : x >= alert ? 'alert' : x >= warm ? 'warm' : '');

  const node = $derived(group.node);
  const up = $derived(v(node, 'up') === true);
  const running = $derived(group.guests.filter((g) => v(g, 'running') === true).length);
  const updates = $derived(v(node, 'updates'));
  const backup = $derived(group.backup);
  const backupAt = $derived(v(backup, 'last_run') ? Date.parse(v(backup, 'last_run')) : null);
</script>

<section class="card proxmox">
  <div class="card-head">
    <h2><Icon name="server" size={18} />{nameOf(node)}</h2>
    <span class="muted small">
      {#if up}
        {t('systeme.infra.proxmox.allume', { since: hours(v(node, 'uptime')) })}{v(node, 'version') ? ` · Proxmox VE ${v(node, 'version')}` : ''}
      {:else}
        {t('systeme.infra.proxmox.injoignable')}
      {/if}
    </span>
  </div>

  <div class="gauges">
    <Gauge label={t('systeme.infra.processeur')} value={v(node, 'cpu')} warn={[75, 92]} />
    <Gauge
      label={t('systeme.infra.memoire')}
      value={v(node, 'memory')}
      sub={v(node, 'memory_used') != null ? t('systeme.infra.go_sur', { used: Math.round(v(node, 'memory_used') * 10) / 10, total: Math.round(v(node, 'memory_total') * 10) / 10 }) : ''}
      warn={[85, 95]}
    />
    <Gauge label={t('systeme.infra.proxmox.disque_systeme')} value={v(node, 'root')} warn={[85, 93]} />
  </div>
  {#if node}
    <!-- The server's last day, under its gauges. -->
    <div class="sparks">
      <Spark point="{node.id}/cpu" label={t('systeme.infra.processeur')} unit={t('systeme.format.unite_pourcent')} max={100} hours={24} />
      <Spark point="{node.id}/memory" label={t('systeme.infra.memoire')} unit={t('systeme.format.unite_pourcent')} max={100} hours={24} />
    </div>
  {/if}
  {#if typeof updates === 'number' && updates > 0}
    <p class="note"><Icon name="warning" size={15} />{t('systeme.infra.proxmox.mises_a_jour', { count: updates })}</p>
  {/if}

  <div class="block">
    <h3>{t('systeme.infra.proxmox.invites')} <span class="muted">{t('systeme.infra.proxmox.en_marche_sur', { running, total: group.guests.length })}</span></h3>
    <ul class="guests">
      <li class="legend" aria-hidden="true"><span></span><span></span><span>{t('systeme.infra.memoire')}</span><span>{t('systeme.infra.disque')}</span></li>
      {#each group.guests as g (g.id)}
        {@const on = v(g, 'running') === true}
        <li class:bad={v(g, 'problem') === true} title={v(g, 'detail') || ''}>
          <i class="dot" class:off={!on} class:sick={v(g, 'problem') === true}></i>
          <span class="name">{nameOf(g)}</span>
          {#if on}
            <span class="track {tone(v(g, 'memory'))}" title={pct(v(g, 'memory'))}><i style:width="{v(g, 'memory') ?? 0}%"></i></span>
            <span class="num">{gio(v(g, 'memory_used'))}</span>
          {:else}
            <span class="muted stopped">{t('systeme.infra.proxmox.arrete')}</span>
          {/if}
          <span class="num disk {tone(v(g, 'disk'))}">{v(g, 'disk') != null ? pct(v(g, 'disk')) : ''}</span>
        </li>
      {/each}
    </ul>
  </div>

  {#if group.storages.length}
    <div class="block">
      <h3>{t('systeme.infra.proxmox.stockages')}</h3>
      <ul class="storages">
        {#each group.storages as s (s.id)}
          <li class:bad={v(s, 'problem') === true}>
            <span class="name">{s.id.slice(s.id.indexOf('_') + 1)}</span>
            <span class="track {tone(v(s, 'used'), 85, 90)}"><i style:width="{v(s, 'used') ?? 0}%"></i></span>
            <span class="num">{t('systeme.infra.proxmox.sur', { used: pct(v(s, 'used')), total: gio(v(s, 'total')) })}</span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  <ul class="health">
    {#if backup}
      <li class:bad={v(backup, 'problem') === true}>
        <Icon name={v(backup, 'problem') === true ? 'ko' : 'ok'} size={16} />
        <span>
          <b>{t('systeme.infra.proxmox.sauvegarde')}</b>
          <small class="muted">
            {#if v(backup, 'running') === true}
              {t('systeme.infra.proxmox.sauvegarde_en_cours')}
            {:else if backupAt}
              {v(backup, 'last_result') === 'OK' ? t('systeme.infra.proxmox.reussie') : v(backup, 'last_result')} · {relative(backupAt, home.now)}
            {:else}
              {t('systeme.infra.proxmox.aucune_sauvegarde')}
            {/if}
          </small>
        </span>
      </li>
    {/if}
    {#each group.disks as d (d.id)}
      <li class:bad={v(d, 'problem') === true}>
        <Icon name={v(d, 'problem') === true ? 'ko' : 'ok'} size={16} />
        <span>
          <b>{d.model || nameOf(d)}</b>
          <small class="muted">{t('systeme.infra.proxmox.disque_sante', { health: v(d, 'health') ?? '—', life: pct(v(d, 'life')) })}</small>
        </span>
      </li>
    {/each}
  </ul>
</section>

<style>
  .card {
    display: grid;
    gap: 14px;
    grid-column: 1 / -1;
  }

  .card-head {
    flex-wrap: wrap;
    gap: 6px 12px;
  }

  .small {
    font-size: 13px;
  }

  .gauges {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: 12px;
  }

  .sparks {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 12px;
    margin-top: 12px;
  }

  .note {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: 13px;
    color: var(--warm-ink);
  }

  .block {
    display: grid;
    gap: 8px;
  }

  h3 {
    margin: 0;
    font-size: 14px;
    font-weight: 700;
  }

  h3 .muted {
    font-weight: 500;
    margin-left: 6px;
  }

  .guests,
  .storages,
  .health {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  .guests li {
    display: grid;
    grid-template-columns: auto minmax(90px, 1fr) minmax(80px, 2fr) 64px 48px;
    align-items: center;
    gap: 10px;
    font-size: 13.5px;
  }

  .guests .legend {
    font-size: 11px;
    color: var(--ink-3);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .guests .legend span:nth-child(3) {
    grid-column: 3 / 5;
  }

  .guests .legend span:last-child,
  .disk {
    text-align: right;
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }

  .stopped {
    grid-column: 3 / 5;
    font-size: 13px;
  }

  .num {
    color: var(--ink-2);
    text-align: right;
  }

  .num.warm {
    color: var(--warm-ink);
    font-weight: 700;
  }

  .num.alert {
    color: var(--alert);
    font-weight: 700;
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--good);
  }

  .dot.off {
    background: var(--ink-3);
    opacity: 0.5;
  }

  .dot.sick {
    background: var(--alert);
    opacity: 1;
  }

  li.bad .name {
    color: var(--alert);
  }

  .track {
    height: 8px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .track i {
    display: block;
    height: 100%;
    background: var(--cool);
  }

  .track.warm i {
    background: var(--warm);
  }

  .track.alert i {
    background: var(--alert);
  }

  .storages li {
    display: grid;
    grid-template-columns: minmax(0, 140px) minmax(0, 1fr) 130px;
    align-items: center;
    gap: 12px;
    font-size: 13.5px;
  }

  .health {
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  }

  .health li {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    color: var(--good);
  }

  .health li.bad {
    color: var(--alert);
  }

  .health span {
    display: grid;
    color: var(--ink);
    font-size: 13.5px;
  }

  .health small {
    font-size: 12px;
  }

  @media (max-width: 520px) {
    .guests li {
      grid-template-columns: auto minmax(0, 1fr) 64px 44px;
    }

    .guests .track,
    .guests .legend span:nth-child(3) {
      display: none;
    }

    .stopped {
      grid-column: 3;
    }
  }
</style>
