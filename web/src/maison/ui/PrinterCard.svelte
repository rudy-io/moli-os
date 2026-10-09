<script>
  import { hub, device, value, reachable, nameOf, act, num, clock, home } from '../lib/home.svelte.js';
  import { phase, PHASES, jobName, duration, cancelWord } from '../lib/printers.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** One 3D printer: the print (image, progress, time left), its heads and
   *  filaments, temperatures, and the hand on it (pause, resume, cancel,
   *  light). Cancelling asks twice. */
  let { id } = $props();

  const d = $derived(device(id));
  const v = (k) => value(id, k);
  const ok = $derived(reachable(id));
  const state = $derived(ok ? phase(v('state')) : 'off');
  const look = $derived(state === 'off' ? { label: t('salon.eteinte'), tone: '' } : PHASES[state]);
  const active = $derived(state === 'printing' || state === 'paused');
  const progress = $derived(Number(v('progress') ?? 0));
  const left = $derived(v('remaining'));
  const finish = $derived(active && typeof left === 'number' ? home.now + left * 60_000 : null);
  const job = $derived(jobName(v('job')));
  const can = (key) => d?.points.some((p) => p.key === key && p.access?.write);

  // Tool heads (Snapmaker U1: four), or the Bambu's filament units.
  const tools = $derived.by(() => {
    const out = [];
    for (let i = 1; d?.points.some((p) => p.key === `tool${i}_temperature`); i++) {
      out.push({
        n: i,
        temp: v(`tool${i}_temperature`),
        target: v(`tool${i}_target`),
        filament: v(`tool${i}_filament`),
        color: v(`tool${i}_color`),
        active: v('active_tool') === i,
      });
    }
    return out;
  });
  const ams = $derived(
    Object.values(hub.devices)
      .filter((x) => x.id.startsWith(`${id}_AMS_`) || x.id === `${id}_ExternalSpool`)
      .flatMap((x) =>
        x.points
          .filter((p) => /^slot_\d$|^spool$/.test(p.key))
          .map((p) => ({ key: `${x.id}/${p.key}`, text: value(x.id, p.key), name: p.label })),
      ),
  );
  const swatch = (text) => /#([0-9a-f]{6})/i.exec(String(text ?? ''))?.[0] ?? null;

  // The thumbnail of the job, refreshed when the job changes.
  const image = $derived(`/api/devices/${encodeURIComponent(id)}/snapshot?job=${encodeURIComponent(v('job') ?? '')}`);
  let noImage = $state(false);
  $effect(() => {
    image;
    noImage = false;
  });

  let confirm = $state(false);
  let confirmTimer;
  function order(word, label) {
    act(`${id}/control`, word, t(label, { nom: nameOf(id) }));
  }
  function cancel() {
    if (!confirm) {
      confirm = true;
      clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirm = false), 5000);
      return;
    }
    confirm = false;
    order(cancelWord(d), 'salon.impression.act_annuler');
  }
  const temp = (degrees) => (typeof degrees === 'number' ? `${num(degrees)}°` : '—');
</script>

<section class="card printer" data-state={state}>
  <div class="head">
    <span class="logo"><Icon name="printer3d" size={22} /></span>
    <div class="title">
      <h2>{nameOf(id)}</h2>
      {#if d?.model || d?.manufacturer}<span class="muted">{d.model ?? d.manufacturer}</span>{/if}
    </div>
    <span class="chip {look.tone}">{look.label}</span>
    {#if can('light') && ok}
      <button class="round" class:lit={v('light') === true} onclick={() => act(`${id}/light`, v('light') !== true, t('salon.impression.act_lumiere', { nom: nameOf(id) }))} aria-pressed={v('light') === true} title={t('salon.impression.lumiere')}>
        <Icon name="light" size={18} />
      </button>
    {/if}
  </div>

  <div class="job">
    <div class="thumb">
      {#if !noImage && v('job')}
        <img src={image} alt="" onerror={() => (noImage = true)} />
      {:else}
        <Icon name="nozzle" size={40} />
      {/if}
    </div>
    <div class="facts">
      <b class="name">{job || (state === 'off' ? t('salon.impression.imprimante_eteinte') : t('salon.impression.aucune_impression'))}</b>
      {#if active || state === 'done'}
        <div class="bar" role="progressbar" aria-valuenow={progress} aria-valuemin="0" aria-valuemax="100">
          <i style:width="{Math.min(100, progress)}%"></i>
        </div>
        <div class="line">
          <strong class="num">{num(progress, progress < 10 ? 1 : 0)} %</strong>
          {#if typeof v('layer') === 'number' && v('total_layers')}<span class="muted">{t('salon.impression.couche', { n: num(v('layer')), total: num(v('total_layers')) })}</span>{/if}
        </div>
        {#if active && finish}
          <span class="when">{t('salon.impression.reste', { duree: duration(left) })} · {t('salon.impression.fin_vers')} <b>{clock(finish)}</b></span>
        {:else if state === 'done' && typeof v('print_duration') === 'number'}
          <span class="when">{t('salon.impression.faite_en', { duree: duration(v('print_duration')) })}{typeof v('filament_used') === 'number' ? ` · ${t('salon.impression.filament_m', { n: num(v('filament_used'), 1) })}` : ''}</span>
        {/if}
      {/if}
      {#if v('message') && (state === 'error' || state === 'paused')}<p class="message">{v('message')}</p>{/if}
    </div>
  </div>

  <div class="temps">
    <span><small>{t('salon.impression.buse')}</small><b class="num">{temp(v('nozzle_temperature'))}</b>{#if v('nozzle_target')}<small class="num"> → {temp(v('nozzle_target'))}</small>{/if}</span>
    {#if d?.points.some((p) => p.key === 'bed_temperature')}
      <span><small>{t('salon.impression.plateau')}</small><b class="num">{temp(v('bed_temperature'))}</b>{#if v('bed_target')}<small class="num"> → {temp(v('bed_target'))}</small>{/if}</span>
    {/if}
    {#if d?.points.some((p) => p.key === 'chamber_temperature')}
      <span><small>{t('salon.impression.enceinte')}</small><b class="num">{temp(v('chamber_temperature'))}</b></span>
    {/if}
  </div>

  {#if tools.length}
    <ul class="tools" aria-label={t('salon.impression.tetes')}>
      {#each tools as tool (tool.n)}
        <li class:on={tool.active}>
          <i class="spool" style:background={tool.color || null}></i>
          <span class="tool-name">{t('salon.impression.tete', { n: tool.n })}{tool.active ? ` · ${t('salon.impression.active')}` : ''}</span>
          <small>{tool.filament ?? '—'}</small>
          <small class="num">{temp(tool.temp)}{#if tool.target} → {temp(tool.target)}{/if}</small>
        </li>
      {/each}
    </ul>
  {:else if ams.length}
    <ul class="tools" aria-label={t('salon.impression.filaments')}>
      {#each ams as a (a.key)}
        <li>
          <i class="spool" style:background={swatch(a.text)}></i>
          <span class="tool-name">{a.name}</span>
          <small>{a.text ?? '—'}</small>
        </li>
      {/each}
    </ul>
  {/if}

  {#if can('control') && (active || confirm)}
    <div class="controls">
      {#if state === 'paused'}
        <button class="act" onclick={() => order('resume', 'salon.impression.act_reprendre')}><Icon name="play" size={18} />{t('salon.impression.reprendre')}</button>
      {:else}
        <button class="act" onclick={() => order('pause', 'salon.impression.act_pause')}><Icon name="pause" size={18} />{t('salon.pause')}</button>
      {/if}
      <button class="act danger" class:confirm onclick={cancel}>
        <Icon name="stop" size={18} />{confirm ? t('salon.impression.confirmer') : t('commun.annuler')}
      </button>
    </div>
  {/if}

  {#if typeof v('jobs_total') === 'number'}
    <p class="totals muted">
      {t('salon.impression.totaux', { count: v('jobs_total'), prints: num(v('jobs_total')), heures: num(v('print_hours_total')) })}{typeof v('filament_total') === 'number' ? ` · ${t('salon.impression.km_filament', { km: num(v('filament_total') / 1000, 2) })}` : ''}
    </p>
  {/if}
</section>

<style>
  .printer {
    display: grid;
    gap: 16px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .logo {
    width: 44px;
    height: 44px;
    border-radius: 14px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  [data-state='printing'] .logo {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .title {
    display: grid;
    flex: 1;
    min-width: 0;
  }

  .title h2 {
    font-size: 18px;
    font-weight: 700;
  }

  .title .muted {
    font-size: 13px;
  }

  .round {
    width: 38px;
    height: 38px;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-3);
    cursor: pointer;
  }

  .round.lit {
    background: var(--warm);
    color: #fff;
  }

  .job {
    display: grid;
    grid-template-columns: 120px minmax(0, 1fr);
    gap: 16px;
    align-items: center;
  }

  .thumb {
    width: 120px;
    height: 120px;
    border-radius: var(--r-md);
    background: var(--surface-2);
    display: grid;
    place-items: center;
    color: var(--ink-3);
    overflow: hidden;
  }

  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }

  .facts {
    display: grid;
    gap: 8px;
    min-width: 0;
  }

  .name {
    font-size: 16px;
    font-weight: 650;
    overflow-wrap: anywhere;
  }

  .bar {
    height: 10px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }

  .bar i {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--warm);
    transition: width 0.8s var(--ease);
  }

  [data-state='done'] .bar i {
    background: var(--good);
  }

  [data-state='paused'] .bar i {
    background: var(--cool);
  }

  .line {
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
  }

  .line strong {
    font-size: 26px;
    font-weight: 300;
    letter-spacing: -0.02em;
  }

  .when {
    font-size: 14px;
    color: var(--ink-2);
  }

  .message {
    margin: 0;
    font-size: 13px;
    color: var(--alert);
  }

  .temps {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 20px;
  }

  .temps span {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
  }

  .temps small {
    color: var(--ink-3);
    font-size: 12px;
    font-weight: 600;
  }

  .temps b {
    font-size: 16px;
    font-weight: 650;
  }

  .tools {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 150px), 1fr));
    gap: 8px;
  }

  .tools li {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px 10px;
    align-items: center;
    padding: 10px 12px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }

  .tools li.on {
    box-shadow: inset 0 0 0 2px var(--warm);
  }

  .spool {
    grid-row: span 3;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 15%), inset 0 0 0 7px rgb(255 255 255 / 25%);
  }

  .tool-name {
    font-size: 13px;
    font-weight: 700;
  }

  .tools small {
    font-size: 12px;
    color: var(--ink-3);
    overflow-wrap: anywhere;
  }

  .controls {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
  }

  .act {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    border: 0;
    border-radius: 999px;
    padding: 10px 18px;
    font: inherit;
    font-weight: 650;
    background: var(--surface-2);
    color: var(--ink);
    cursor: pointer;
  }

  .act.danger {
    color: var(--alert);
  }

  .act.danger.confirm {
    background: var(--alert);
    color: #fff;
  }

  .totals {
    margin: 0;
    font-size: 13px;
  }

  @media (max-width: 560px) {
    .job {
      grid-template-columns: 88px minmax(0, 1fr);
      gap: 12px;
    }

    .thumb {
      width: 88px;
      height: 88px;
    }
  }
</style>
