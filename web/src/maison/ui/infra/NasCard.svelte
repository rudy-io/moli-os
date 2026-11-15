<script>
  import { value, num } from '../../lib/home.svelte.js';
  import { shortDate } from '../../lib/nas.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';
  import Icon from '../Icon.svelte';
  import Gauge from './Gauge.svelte';

  /** A NAS of the house (Synology): is it on, how loaded and how warm, how
   *  full its volumes, how its disks and its UPS, and how the last backups
   *  went. Read live from its driver; the card has no order of its own. */
  let { group } = $props();

  const nas = $derived(group.nas);
  const v = (id, k) => value(id, k);
  const up = $derived(v(nas.id, 'up') === true);
  const name = $derived(nas.label?.name || nas.native_name);

  const go = (n) => (typeof n === 'number' ? t('systeme.format.go', { value: n.toLocaleString(locale(), { maximumFractionDigits: n < 10 ? 1 : 0 }) }) : '—');
  function since(hours) {
    if (typeof hours !== 'number') return '—';
    const d = Math.floor(hours / 24);
    const h = Math.floor(hours % 24);
    const m = Math.floor((hours * 60) % 60);
    return d ? t('systeme.format.duree_jours', { d, h }) : h ? t('systeme.format.duree_heures', { h, m }) : t('systeme.format.duree_minutes', { m });
  }
  const wrong = (d) => v(d.id, 'problem') === true;
</script>

<section class="card">
  <div class="card-head">
    <h2><Icon name="harddisk" size={18} />{name}</h2>
    {#if up}
      <span class="muted small">{t('systeme.infra.nas_allume', { since: since(v(nas.id, 'uptime')) })}</span>
    {:else}
      <span class="chip">{t('systeme.infra.nas_eteint')}</span>
    {/if}
  </div>

  {#if up}
    <div class="gauges">
      <Gauge label={t('systeme.infra.processeur')} value={v(nas.id, 'cpu')} warn={[75, 92]} />
      <Gauge label={t('systeme.infra.memoire')} value={v(nas.id, 'memory')} warn={[85, 95]} />
      <Gauge label={t('systeme.infra.temperature')} value={v(nas.id, 'temperature')} unit="°" max={100} warn={[60, 70]} />
    </div>

    {#if group.volumes.length}
      <ul class="volumes">
        {#each group.volumes as vol (vol.id)}
          {@const used = v(vol.id, 'used')}
          <li class:bad={wrong(vol)}>
            <span class="label">{vol.label?.name || vol.native_name}</span>
            <span class="track" class:warm={used >= 85} class:alert={used >= 93 || wrong(vol)}><i style:width="{used ?? 0}%"></i></span>
            <span class="num">{typeof used === 'number' ? `${num(used, 0)} %` : '—'} · {t('systeme.infra.libres', { size: go(v(vol.id, 'free')) })}</span>
          </li>
        {/each}
      </ul>
    {/if}

    {#if group.disks.length}
      <ul class="drives">
        {#each group.disks as disk (disk.id)}
          <li class:bad={wrong(disk)} title={v(disk.id, 'detail') || undefined}>
            <i class="dot" class:sick={wrong(disk)}></i>
            <span>{disk.label?.name || disk.native_name}</span>
            <small class="muted">{typeof v(disk.id, 'temperature') === 'number' ? `${num(v(disk.id, 'temperature'), 0)} °C` : '—'}</small>
          </li>
        {/each}
      </ul>
    {/if}

    {#if group.ups}
      <p class="ups" class:bad={wrong(group.ups)}>
        <Icon name={wrong(group.ups) ? 'ko' : 'ok'} size={16} />
        {t('systeme.infra.nas_onduleur', { pct: typeof v(group.ups.id, 'battery') === 'number' ? num(v(group.ups.id, 'battery'), 0) : '—' })}
      </p>
    {/if}
  {/if}

  {#if group.backups.length}
    <ul class="backups">
      {#each group.backups as task (task.id)}
        <li class:bad={wrong(task)}>
          <Icon name={wrong(task) ? 'ko' : 'ok'} size={16} />
          <span>
            <b>{task.label?.name || task.native_name}</b>
            <small class="muted">{shortDate(v(task.id, 'last_run')) ?? '—'}{wrong(task) && v(task.id, 'detail') ? ` · ${v(task.id, 'detail')}` : ''}</small>
          </span>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
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

  .gauges {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: 12px;
  }

  .volumes,
  .backups {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  .volumes li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    font-size: 13.5px;
  }

  .volumes .label {
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

  .drives {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: 6px 12px;
    font-size: 13.5px;
  }

  .drives li {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .drives li.bad {
    color: var(--alert);
    font-weight: 600;
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--good);
  }

  .dot.sick {
    background: var(--alert);
  }

  .ups {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    font-size: 13.5px;
    color: var(--good);
  }

  .ups.bad {
    color: var(--alert);
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
</style>
