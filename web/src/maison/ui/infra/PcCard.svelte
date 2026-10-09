<script>
  import { onMount } from 'svelte';
  import { value, act } from '../../lib/home.svelte.js';
  import { asHuman, CANCELLED } from '../../lib/auto.svelte.js';
  import { t, locale } from '../../../lib/i18n.svelte.js';
  import Icon from '../Icon.svelte';
  import Gauge from './Gauge.svelte';

  /** A computer of the house: awake or not (and wake it), and with the Moli
   *  agent everything it does (processor, graphics card, memory, disks, the
   *  busiest processes) and, for the phone, Claude and Codex opened on it:
   *  woken from here, its owner finds their agents on the phone (PIN asked). */
  let { pc } = $props();

  const v = (k) => value(pc.id, k);
  const name = $derived(pc.label?.name || pc.native_name);
  const on = $derived(v('power') === true);
  const hasAgent = $derived(pc.points.some((p) => p.key === 'agent'));
  const connected = $derived(v('agent') === true);
  const canWake = $derived(pc.points.some((p) => p.key === 'power' && p.access?.write));

  // The agent's last report (processes, disks), from Moli.
  let view = $state(null);
  async function load() {
    if (!hasAgent) return;
    try {
      const res = await fetch(`/api/machines/${encodeURIComponent(pc.id)}`);
      if (res.ok) view = await res.json();
    } catch {
      /* next round */
    }
  }
  onMount(() => {
    load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });
  const report = $derived(view?.report ?? {});

  // ---- words and numbers ----
  const GB = 1024 ** 3;
  const size = (b) =>
    typeof b === 'number'
      ? b >= GB
        ? t('systeme.format.go', { value: (b / GB).toLocaleString(locale(), { maximumFractionDigits: b < 10 * GB ? 1 : 0 }) })
        : t('systeme.format.mo', { value: Math.round(b / 1024 ** 2) })
      : '—';
  function since(seconds) {
    if (typeof seconds !== 'number') return '—';
    const d = Math.floor(seconds / 86400);
    const h = Math.floor((seconds % 86400) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    return d ? t('systeme.format.duree_jours', { d, h }) : h ? t('systeme.format.duree_heures', { h, m }) : t('systeme.format.duree_minutes', { m });
  }

  // ---- power ----
  let confirm = $state(null);
  let confirmTimer;
  function twice(what, run) {
    if (confirm !== what) {
      confirm = what;
      clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirm = null), 5000);
      return;
    }
    confirm = null;
    run();
  }

  // ---- the phone: Claude and Codex open on the PC ----
  const claudeOpen = $derived(v('claude_app') === true);
  const codexOpen = $derived(v('codex_app') === true);
  const waiting = $derived((view?.pending ?? []).some((o) => o.kind === 'remote'));
  let sending = $state(false);
  let said = $state('');
  let error = $state('');

  async function post(url) {
    const res = await fetch(url, { method: 'POST', headers: { 'x-moli-origin': 'ui' } });
    const data = await res.json().catch(() => ({}));
    if (!res.ok) {
      const err = new Error(data.error ?? t('systeme.infra.pc.erreur', { status: res.status }));
      err.status = res.status;
      throw err;
    }
    return data;
  }

  async function remote() {
    if (sending) return;
    sending = true;
    error = '';
    said = '';
    try {
      const done = await asHuman(t('systeme.infra.pc.act_ouvrir', { name }), () =>
        post(`/api/machines/${encodeURIComponent(pc.id)}/remote`),
      );
      said = done.waking ? t('systeme.infra.pc.reveil', { name }) : t('systeme.infra.pc.demande');
      load();
    } catch (err) {
      if (err.message !== CANCELLED) error = err.message;
    } finally {
      sending = false;
    }
  }</script>

<section class="card pc" class:wide={connected}>
  <div class="card-head">
    <h2><Icon name="computer" size={18} />{name}</h2>
    <span class="chip" class:good={on}>{on ? (connected ? t('systeme.infra.pc.allume_depuis', { since: since(v('uptime')) }) : t('systeme.infra.pc.allume')) : t('systeme.infra.pc.eteint')}</span>
  </div>

  {#if !on}
    {#if canWake}
      <button class="primary" onclick={() => act(`${pc.id}/power`, true, t('systeme.infra.pc.act_allumer', { name }))}><Icon name="power" size={18} />{t('systeme.infra.pc.allumer')}</button>
      <p class="muted small">{t('systeme.infra.pc.reseau')}</p>
    {/if}
  {:else if hasAgent && !connected}
    <p class="muted small">{t('systeme.infra.pc.agent_muet')}</p>
  {:else if !hasAgent}
    <p class="muted small">{t('systeme.infra.pc.sans_agent')}</p>
  {/if}

  {#if connected}
    <div class="gauges">
      <Gauge label={t('systeme.infra.processeur')} value={v('cpu')} warn={[80, 95]} sub={v('cpu_temperature') != null ? `${v('cpu_temperature')} °C` : ''} />
      <Gauge label={t('systeme.infra.pc.carte_graphique')} value={v('gpu_load')} warn={[85, 97]} sub={v('gpu_power') != null ? `${v('gpu_power')} W` : ''} />
      <Gauge label={t('systeme.infra.memoire')} value={v('ram')} warn={[85, 95]} sub={v('ram_used') != null ? t('systeme.infra.go_sur', { used: v('ram_used'), total: v('ram_total') }) : ''} />
      <Gauge label={t('systeme.infra.pc.graphique')} value={v('gpu_temperature')} unit="°" max={100} warn={[78, 87]} sub={v('gpu_memory') != null ? t('systeme.infra.go_sur', { used: v('gpu_memory'), total: v('gpu_memory_total') }) : t('systeme.infra.pc.temperature_min')} />
    </div>
    <div class="facts">
      <span><Icon name="lan" size={15} />↓ {v('net_down') ?? '—'} · ↑ {v('net_up') ?? '—'} Mbit/s</span>
      {#if report.user}<span><Icon name="account" size={15} />{report.user}</span>{/if}
      {#if v('cpu_temperature') == null}<span class="muted">{t('systeme.infra.pc.capteurs')}</span>{/if}
    </div>

    <div class="cols">
      <div>
        <h3>{t('systeme.infra.pc.ce_qui_tourne')}</h3>
        <ul class="procs">
          {#each report.processes ?? [] as p (p.name)}
            <li><span class="pname">{p.name}</span><span class="num">{t('systeme.format.pourcent', { value: p.cpu?.toLocaleString(locale(), { maximumFractionDigits: 1 }) ?? '—' })}</span><span class="num muted">{size(p.memory)}</span></li>
          {:else}
            <li class="muted">…</li>
          {/each}
        </ul>
      </div>
      <div>
        <h3>{t('systeme.infra.pc.disques')}</h3>
        <ul class="disks">
          {#each report.disks ?? [] as d (d.name)}
            {@const pct = d.total ? Math.round((d.used / d.total) * 100) : 0}
            <li>
              <span>{d.name} <small class="muted">{d.label ?? ''}</small></span>
              <span class="track" class:warm={pct >= 85} class:alert={pct >= 93}><i style:width="{pct}%"></i></span>
              <span class="num small">{t('systeme.infra.libres', { size: size(d.total - d.used) })}</span>
            </li>
          {/each}
        </ul>
      </div>
    </div>

    <div class="power">
      <button class="ghost" onclick={() => twice('sleep', () => act(`${pc.id}/sleep`, true, t('systeme.infra.pc.act_veille', { name })))}>{confirm === 'sleep' ? t('systeme.infra.pc.sur_veille') : t('systeme.infra.pc.veille')}</button>
      <button class="ghost" onclick={() => twice('restart', () => act(`${pc.id}/restart`, true, t('systeme.infra.pc.act_redemarrer', { name })))}>{confirm === 'restart' ? t('systeme.infra.pc.sur_redemarrer') : t('systeme.infra.pc.redemarrer')}</button>
      <button class="ghost danger" onclick={() => twice('off', () => act(`${pc.id}/power`, false, t('systeme.infra.pc.act_eteindre', { name })))}>{confirm === 'off' ? t('systeme.infra.pc.sur_eteindre') : t('systeme.infra.pc.eteindre')}</button>
    </div>
  {/if}

  {#if hasAgent}
    <div class="phone">
      <h3><Icon name="sparkles" size={16} />{t('systeme.infra.pc.telephone')}</h3>
      {#if connected}
        <div class="apps">
          <span class="app" class:on={claudeOpen}><i></i>{t(claudeOpen ? 'systeme.infra.pc.app_ouverte' : 'systeme.infra.pc.app_fermee', { app: 'Claude' })}</span>
          <span class="app" class:on={codexOpen}><i></i>{t(codexOpen ? 'systeme.infra.pc.app_ouverte' : 'systeme.infra.pc.app_fermee', { app: 'Codex' })}</span>
        </div>
      {:else if on}
        <p class="note warn">{t('systeme.infra.pc.session_fermee')}</p>
      {/if}
      {#if !(connected && claudeOpen && codexOpen)}
        <button class="primary" onclick={remote} disabled={sending || waiting}>
          <Icon name="power" size={18} />{sending ? t('systeme.infra.pc.envoi') : waiting ? t('systeme.infra.pc.attente') : on ? t('systeme.infra.pc.ouvrir') : t('systeme.infra.pc.allumer_ouvrir')}
        </button>
      {/if}
      <p class="muted small">
        {connected && claudeOpen && codexOpen ? t('systeme.infra.pc.pret') : t('systeme.infra.pc.ensuite')}
      </p>
      {#if said}<p class="note good">{said}</p>{/if}
      {#if error}<p class="note bad">{error}</p>{/if}
    </div>
  {/if}</section>

<style>
  .pc {
    display: grid;
    gap: 14px;
  }

  .pc.wide {
    grid-column: 1 / -1;
  }

  .card-head {
    flex-wrap: wrap;
    gap: 6px 12px;
  }

  .small {
    font-size: 13px;
  }

  h3 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 0 8px;
    font-size: 14px;
    font-weight: 700;
    color: var(--ink-2);
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

  .cols {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 18px;
  }

  .procs,
  .disks {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 5px;
  }

  .procs li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 56px 70px;
    gap: 10px;
    font-size: 13.5px;
  }

  .pname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }

  .procs .num {
    text-align: right;
  }

  .disks li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1.2fr) auto;
    align-items: center;
    gap: 10px;
    font-size: 13.5px;
  }

  .track {
    height: 9px;
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

  .power {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .ghost {
    border: 1px solid var(--line);
    background: var(--surface);
    border-radius: 999px;
    padding: 8px 14px;
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-2);
    cursor: pointer;
  }

  .ghost.danger {
    color: var(--alert);
  }

  .phone {
    display: grid;
    gap: 10px;
    padding-top: 6px;
    border-top: 1px solid var(--line);
  }

  .apps {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .app {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    border-radius: 999px;
    background: var(--surface-2);
    font-size: 14px;
    font-weight: 650;
    color: var(--ink-3);
  }

  .app i {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--ink-3);
    opacity: 0.5;
  }

  .app.on {
    background: var(--good-soft);
    color: var(--ink);
  }

  .app.on i {
    background: var(--good);
    opacity: 1;
    box-shadow: 0 0 0 3px rgb(44 154 136 / 20%);
  }

  .primary {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 46px;
    padding: 0 18px;
    border: none;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .note {
    margin: 0;
    font-size: 13px;
  }

  .note.good {
    color: var(--good);
  }

  .note.bad {
    color: var(--alert);
  }

  .note.warn {
    padding: 10px 12px;
    border-radius: var(--r-sm);
    background: var(--warm-soft);
    color: var(--warm-ink);
  }
</style>