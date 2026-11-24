<script>
  import { value, device } from '../../lib/home.svelte.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';
  import { reader, unitReader, mbits } from '../../lib/box.js';
  import Icon from '../Icon.svelte';
  import Gauge from './Gauge.svelte';

  /** The internet box (home.json `infra.box`, `lib/box.js`): its
   *  temperatures as gauges, its rates, fans, disk and Wi-Fi. Only the
   *  points the house names; read only. */
  let { box, link = null, since = null } = $props();

  const read = reader(value);
  const unitOf = unitReader(device);
  // A rate in its point's unit, or the one home.json gives for a point that declares none.
  const rate = (point) => mbits(read(point), unitOf(point) ?? box.rate_unit ?? null);
  const num = (x, digits = 0) => (typeof x === 'number' ? x.toLocaleString(locale(), { maximumFractionDigits: digits }) : '—');
  const down = $derived(rate(box.down));
  const up = $derived(rate(box.up));
  const free = $derived(read(box.free));
  const used = $derived(typeof free === 'number' ? Math.max(0, Math.min(100, 100 - free)) : null);
  const degraded = $derived(read(box.degraded));
  const wifi = $derived(read(box.wifi));
</script>

<section class="card">
  <div class="card-head">
    <h2><Icon name="lan" size={18} />{box.name ?? t('systeme.infra.box.titre')}</h2>
    {#if box.down || box.up}
      <span class="muted small num">↓ {num(down, 1)} · ↑ {num(up, 1)} Mbit/s</span>
    {/if}
    {#if link}
      {@const connected = value(link.id, 'connected')}
      <span class="chip" class:good={connected === true} class:alert={connected === false}>{connected === false ? t('systeme.infra.internet_coupe') : t('systeme.infra.internet_ok')}</span>
    {/if}
  </div>

  {#if box.temperatures?.length}
    <div class="gauges">
      {#each box.temperatures as s, i (i)}
        <Gauge label={s.label} value={read(s.point)} unit="°" max={100} warn={[s.warn ?? 70, s.alert ?? 85]} />
      {/each}
    </div>
  {/if}

  <dl class="kv">
    {#if link}
      <dt>{t('systeme.infra.connectee_depuis')}</dt><dd>{since ? since(value(link.id, 'uptime')) : '—'}</dd>
      <dt>{t('systeme.infra.ligne')}</dt><dd class="num">↓ {value(link.id, 'line_down') ?? '—'} · ↑ {value(link.id, 'line_up') ?? '—'} Mbit/s</dd>
      <dt>{t('systeme.infra.adresse_publique')}</dt><dd class="num">{value(link.id, 'external_ip') ?? '—'}</dd>
    {/if}
    {#each box.fans ?? [] as f, i (i)}
      {@const rpm = read(f.point)}
      <dt>{f.label}</dt>
      <dd class="num" class:warm={typeof rpm === 'number' && typeof f.warn === 'number' && rpm >= f.warn}>{t('systeme.infra.box.tours', { rpm: num(rpm) })}</dd>
    {/each}
    {#if box.free}
      <dt>{t('systeme.infra.box.disque')}</dt>
      <dd>
        <span class="track" class:warm={used >= 85} class:alert={used >= 90}><i style:width="{used ?? 0}%"></i></span>
        <span class="num">{t('systeme.infra.box.occupe', { pct: num(used) })}</span>
      </dd>
    {/if}
    {#if box.degraded}
      <dt>{t('systeme.infra.box.raid')}</dt>
      <dd class:alert={degraded === true}>{degraded === true ? t('systeme.infra.box.raid_degrade') : degraded === false ? t('systeme.infra.box.raid_ok') : '—'}</dd>
    {/if}
    {#if box.wifi}
      <dt>{t('systeme.infra.box.wifi')}</dt>
      <dd>{wifi === true ? t('systeme.infra.box.wifi_on') : wifi === false ? t('systeme.infra.box.wifi_off') : '—'}</dd>
    {/if}
  </dl>
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

  .kv {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 8px 16px;
    margin: 0;
    font-size: 14px;
    align-items: center;
  }

  .kv dt {
    color: var(--ink-3);
  }

  .kv dd {
    margin: 0;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .warm {
    color: var(--warm-ink);
  }

  .alert {
    color: var(--alert);
  }

  .track {
    flex: 1;
    max-width: 220px;
    height: 8px;
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
</style>
