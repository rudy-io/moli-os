<script>
  import { onMount, tick } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import TuyaCloudKeys from '../ui/TuyaCloudKeys.svelte';
  import AssistantSettings from '../ui/AssistantSettings.svelte';
  import AssistantExchanges from '../ui/AssistantExchanges.svelte';
  import DashboardCode from '../ui/DashboardCode.svelte';
  import PeopleCard from '../ui/PeopleCard.svelte';
  import { hub, home } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';

  // « Comment ça marche » : the system as it runs, drawn from /api/system and
  // the live event stream. Nothing here is written by hand: a new driver or
  // brick shows up by itself.
  let sys = $state(null);
  let error = $state('');
  let chosen = $state(null);
  let active = $state({});
  let perMinute = $state(0);
  let map = $state();
  let cards = {};
  let links = $state([]);

  async function load() {
    try {
      const res = await fetch('/api/system');
      if (!res.ok) throw new Error(res.statusText);
      sys = await res.json();
      error = '';
      await tick();
      measure();
    } catch (err) {
      error = err.message;
    }
  }

  onMount(() => {
    load();
    const slow = setInterval(load, 10_000);
    // Which drivers just spoke: their link to the core lights up.
    const fast = setInterval(() => {
      const now = Date.now();
      const seen = {};
      let minute = 0;
      for (const [point, ts] of Object.entries(hub.changed)) {
        if (now - ts < 60_000) minute += 1;
        if (now - ts < 1_600) seen[point.slice(0, point.indexOf(':'))] = true;
      }
      active = seen;
      perMinute = minute;
    }, 400);
    const onResize = () => measure();
    addEventListener('resize', onResize);
    return () => {
      clearInterval(slow);
      clearInterval(fast);
      removeEventListener('resize', onResize);
    };
  });

  /** Connectors between the cards, from their real positions. */
  function measure() {
    if (!map || !sys) return;
    const box = map.getBoundingClientRect();
    const at = (id, side) => {
      const el = cards[id];
      if (!el) return null;
      const r = el.getBoundingClientRect();
      return { x: (side === 'right' ? r.right : r.left) - box.left, y: r.top + r.height / 2 - box.top };
    };
    const out = [];
    for (const d of sys.drivers) {
      const a = at(`driver:${d.instance}`, 'right');
      const b = at('core', 'left');
      if (a && b) out.push({ id: `driver:${d.instance}`, a, b, driver: d.instance });
    }
    for (const br of sys.bricks) {
      const a = at('core', 'right');
      const b = at(`brick:${br.id}`, 'left');
      if (a && b) out.push({ id: `brick:${br.id}`, a, b, off: !br.active });
    }
    for (const s of sys.surfaces) {
      const a = at('bricks-out', 'right');
      const b = at(`surface:${s.id}`, 'left');
      if (a && b) out.push({ id: `surface:${s.id}`, a, b });
    }
    links = out;
  }

  const path = ({ a, b }) => {
    const dx = Math.max(30, (b.x - a.x) / 2);
    return `M ${a.x} ${a.y} C ${a.x + dx} ${a.y}, ${b.x - dx} ${b.y}, ${b.x} ${b.y}`;
  };

  // ---- words ------------------------------------------------------------------
  // Driver kinds: their name and description are in the catalog
  // (systeme.pilotes.<kind>.nom / .desc); a kind not listed shows as it is.
  const KINDS = ['z2m', 'hue', 'tuya', 'reolink', 'sonos', 'helpers', 'philips', 'frigate', 'bambu', 'tapo', 'igd', 'presence', 'telegram', 'phones', 'profile'];
  const kindWord = (kind, part) => (KINDS.includes(kind) ? t(`systeme.pilotes.${kind}.${part}`) : null);
  const driverName = (d) => (d.kind === 'profile' ? d.instance.charAt(0).toUpperCase() + d.instance.slice(1) : (kindWord(d.kind, 'nom') ?? d.kind));

  // Bricks: their icon here, their description in the catalog (systeme.briques.<id>.desc).
  const BRICKS = { guard: 'shield', history: 'history', energy: 'bolt', automations: 'robot', assistant: 'sparkles', cameras: 'cctv' };
  const brickWord = (id) => (Object.hasOwn(BRICKS, id) ? t(`systeme.briques.${id}.desc`) : '');
  const SURFACES = { maison: 'home', atelier: 'atelier', mcp: 'robot', rest: 'branch' };

  // Commandable devices in no room: nobody can tell whether they sit in a
  // bedroom. (Weather, tariffs, the house's own helpers belong to no room.)
  const roomless = $derived(
    Object.values(hub.devices)
      .filter((d) => !d.label?.room && !d.native_room && d.model !== 'Aide')
      .filter((d) => d.points.some((p) => p.access?.write))
      .map((d) => ({ id: d.id, name: d.label?.name || d.native_name, writable: d.points.some((p) => p.access?.write) }))
      .sort((a, b) => Number(b.writable) - Number(a.writable) || a.name.localeCompare(b.name, locale()))
  );

  const brickFacts = (b) => {
    const f = b.facts ?? {};
    switch (b.id) {
      case 'guard':
        return `${t('systeme.briques.faits.pieces', { count: f.protected_rooms?.length ?? 0 })}${f.quiet_hours?.active ? ` · ${t('systeme.briques.faits.calmes')}` : ''}`;
      case 'history':
        return f.db_bytes ? t('systeme.format.mo', { value: (f.db_bytes / 1e6).toFixed(1) }) : t('systeme.briques.faits.arrete');
      case 'energy':
        return f.meters ? t('systeme.briques.faits.compteurs', { count: f.meters, kwh: Math.round(f.today_kwh ?? 0) }) : t('systeme.briques.faits.arrete');
      case 'automations':
        return t('systeme.briques.faits.automatismes', { live: f.live ?? 0, drafts: f.drafts ?? 0, runs: f.runs_24h ?? 0 });
      case 'assistant':
        return b.active ? t('systeme.briques.faits.modele', { model: f.model }) : t('systeme.briques.faits.sans_cle');
      case 'cameras':
        return t('systeme.briques.faits.cameras', { count: f.cameras ?? 0 });
      default:
        return '';
    }
  };

  const last = $derived(sys?.perf?.at(-1));
  // Idle CPU is only measured idle: a dashboard showing a camera live is use, not rest.
  const idle = $derived(sys?.perf?.findLast((p) => !p.clients?.total));
  const mem = (side) => side?.mem_mib ?? side?.cgroup_mib;
  const num = (n, d = 0) => Number(n).toLocaleString(locale(), { maximumFractionDigits: d });
  const fmtMem = (mib) => (mib == null ? '—' : mib >= 1024 ? t('systeme.format.go', { value: num(mib / 1024, 2) }) : t('systeme.format.mo', { value: Math.round(mib) }));
  const ratio = (a, b) => (a && b ? Math.round(a / b) : null);
  const STATES = ['running', 'waiting', 'starting'];
  const statusText = (s) => (STATES.includes(s?.state) ? t(`systeme.page.statut.${s.state}`) : (s?.state ?? '?'));

  function choose(kind, item) {
    chosen = { kind, item };
  }
</script>

<div class="systeme">
  <header>
    <h1 class="page-title">{t('systeme.page.titre')}</h1>
    <p class="page-sub">{t('systeme.page.sous_titre')}</p>
  </header>

  {#if error}<p class="muted">{error}</p>{/if}

  <PeopleCard />

  <DashboardCode />

  <AssistantSettings />

  <AssistantExchanges />

  <TuyaCloudKeys />

  <!-- Only where a host script measures Moli against a Home Assistant. -->
  {#if sys && last?.ha}
    <section class="versus">
      <div class="vs-head"><b>{t('systeme.page.face_a_ha')}</b><span class="muted">{t('systeme.page.mesure')}{#if sys.coverage?.total} · {t('systeme.page.couverture', { covered: sys.coverage.covered, total: sys.coverage.total })}{/if}</span></div>
      <div class="vs">
        <div>
          <small>{t('systeme.page.memoire')}</small>
          <p><b class="num">{fmtMem(mem(last?.moli) ?? sys.stats.rss_bytes / 2 ** 20)}</b> <span class="muted">{t('systeme.page.contre', { value: fmtMem(mem(last?.ha)) })}</span></p>
          {#if ratio(mem(last?.ha), mem(last?.moli))}<span class="big num">÷{ratio(mem(last.ha), mem(last.moli))}</span>{/if}
        </div>
        <div>
          <small>{t('systeme.page.cpu_repos')}</small>
          <p><b class="num">{t('systeme.format.pourcent', { value: idle?.moli?.cpu_pct != null ? (idle.moli.cpu_pct < 0.05 ? `< ${num(0.05, 2)}` : num(idle.moli.cpu_pct, 2)) : '—' })}</b> <span class="muted">{t('systeme.page.contre', { value: t('systeme.format.pourcent', { value: idle?.ha?.cpu_pct != null ? num(idle.ha.cpu_pct, 2) : '—' }) })}</span></p>
          {#if idle?.moli?.cpu_pct >= 0.05 && idle?.ha?.cpu_pct}<span class="big num">÷{(idle.ha.cpu_pct / idle.moli.cpu_pct).toFixed(0)}</span>{/if}
        </div>
        <div>
          <small>{t('systeme.page.taille')}</small>
          <p><b class="num">{t('systeme.format.mo', { value: last?.moli?.image_mb != null ? num(last.moli.image_mb, 1) : '—' })}</b> <span class="muted">{t('systeme.page.contre', { value: last?.ha?.image_mb ? t('systeme.format.go', { value: num(last.ha.image_mb / 1000, 1) }) : '—' })}</span></p>
          {#if ratio(last?.ha?.image_mb, last?.moli?.image_mb)}<span class="big num">÷{ratio(last.ha.image_mb, last.moli.image_mb)}</span>{/if}
        </div>
        <div>
          <small>{t('systeme.page.reponse')}</small>
          <p><b class="num">{last?.moli?.api?.['/api/devices']?.p95 != null ? num(last.moli.api['/api/devices'].p95, 1) : '—'} ms</b> <span class="muted">{t('systeme.page.pour_tous')}</span></p>
          {#if last?.alerts?.length}<span class="alert">⚠ {last.alerts[0]}</span>{:else if last}<span class="ok">{t('systeme.page.budget_tenu')}</span>{/if}
        </div>
      </div>
      {#if sys.perf.length > 1}
        <div class="trend" aria-label={t('systeme.page.tendance_aria')}>
          {#each sys.perf as p, i (i)}
            <i style="height:{Math.max(8, (p.moli.rss_mib / Math.max(...sys.perf.map((x) => x.moli.rss_mib))) * 40)}px" title={t('systeme.page.tendance_titre', { version: p.version, value: p.moli.rss_mib })}></i>
          {/each}
          <small class="muted">{t('systeme.page.tendance')}</small>
        </div>
      {/if}
    </section>

    {#if roomless.length}
      <section class="roomless">
        <p><Icon name="info" size={16} /><b>{t('systeme.page.sans_piece', { count: roomless.length })}</b>
          <span class="muted">{t('systeme.page.sans_piece_aide')}</span></p>
        <p class="names">
          {#each roomless as d, i (d.id)}{i ? ' · ' : ''}<span class:writable={d.writable}>{d.name}</span>{/each}
        </p>
      </section>
    {/if}

    <div class="map" bind:this={map}>
      <svg class="links" aria-hidden="true">
        {#each links as l (l.id)}
          <path d={path(l)} class:hot={l.driver && active[l.driver]} class:off={l.off} />
        {/each}
      </svg>

      <section class="col">
        <h2>{t('systeme.page.les_appareils')} <span class="muted">{sys.stats.devices}</span></h2>
        {#each sys.drivers as d (d.instance)}
          <button class="node driver" class:hot={active[d.instance]} bind:this={cards[`driver:${d.instance}`]} onclick={() => choose('driver', d)}>
            <i class="dot {d.status?.state}"></i>
            <span><b>{driverName(d)}</b><small>{t('systeme.page.appareils_n', { count: d.devices })}{d.offline ? ` · ${t('systeme.page.injoignables', { count: d.offline })}` : ''}</small></span>
          </button>
        {/each}
      </section>

      <section class="col center">
        <h2>{t('systeme.page.coeur')}</h2>
        <button class="node core" bind:this={cards.core} onclick={() => choose('core', null)}>
          <span class="core-logo" aria-hidden="true"></span>
          <b>Moli OS</b>
          <small>{t('systeme.page.coeur_chiffres', { devices: sys.stats.devices, points: sys.stats.points })}</small>
          <small class="pulse"><i class="live"></i>{t('systeme.page.par_minute', { count: perMinute })}</small>
          <ul>
            <li>{t('systeme.page.coeur_etat')}</li>
            <li>{t('systeme.page.coeur_garde')}</li>
            <li>{t('systeme.page.coeur_journal')}</li>
            <li>{t('systeme.page.coeur_coffre')}</li>
          </ul>
        </button>
      </section>

      <section class="col">
        <h2>{t('systeme.page.les_briques')}</h2>
        <div class="bricks" bind:this={cards['bricks-out']}>
          {#each sys.bricks as b (b.id)}
            <button class="node brick" class:off={!b.active} bind:this={cards[`brick:${b.id}`]} onclick={() => choose('brick', b)}>
              <span class="ic"><Icon name={BRICKS[b.id] ?? 'info'} size={18} /></span>
              <span><b>{b.name}</b><small>{brickFacts(b)}</small></span>
            </button>
          {/each}
          <div class="node soon">
            <span class="ic"><Icon name="plus" size={18} /></span>
            <span><b>{t('systeme.page.bientot')}</b><small>{t('systeme.page.bientot_aide')}</small></span>
          </div>
        </div>
      </section>

      <section class="col">
        <h2>{t('systeme.page.pour_qui')}</h2>
        {#each sys.surfaces as s (s.id)}
          <button class="node surface" bind:this={cards[`surface:${s.id}`]} onclick={() => choose('surface', s)}>
            <span class="ic"><Icon name={SURFACES[s.id] ?? 'info'} size={18} /></span>
            <span><b>{s.name}</b><small>{s.what}</small></span>
          </button>
        {/each}
      </section>
    </div>

    {#if chosen}
      <aside class="explain card">
        <button class="x" onclick={() => (chosen = null)} aria-label={t('commun.fermer')}><Icon name="close" size={18} /></button>
        {#if chosen.kind === 'driver'}
          {@const d = chosen.item}
          <h3>{driverName(d)} <small class="muted">({d.instance})</small></h3>
          <p>{kindWord(d.kind, 'desc') ?? t('systeme.pilotes.defaut')}</p>
          <p class="muted">{d.status?.reason ? t('systeme.page.publie_raison', { points: d.points, devices: d.devices, status: statusText(d.status), reason: d.status.reason }) : t('systeme.page.publie', { points: d.points, devices: d.devices, status: statusText(d.status) })}</p>
          <p class="how">{t('systeme.page.entre_sort')}</p>
        {:else if chosen.kind === 'core'}
          <h3>{t('systeme.page.coeur')}</h3>
          <p>{t('systeme.page.coeur_explique')}</p>
          <p class="how">{t('systeme.page.coeur_programme', { mb: Math.round((sys.stats.rss_bytes ?? 0) / 2 ** 20), min: Math.round(sys.stats.uptime_ms / 60000) })}</p>
        {:else if chosen.kind === 'brick'}
          {@const b = chosen.item}
          <h3>{b.name} <small class="muted">{b.active ? t('systeme.page.active') : t('systeme.page.inactive')}</small></h3>
          <p>{brickWord(b.id)}</p>
          <p class="how">{brickFacts(b)}</p>
        {:else}
          {@const s = chosen.item}
          <h3>{s.name}</h3>
          <p>{t('systeme.page.surface', { what: s.what })}</p>
        {/if}
      </aside>
    {/if}
  {:else if !error}
    <p class="muted">{t('systeme.chargement')}</p>
  {/if}
</div>

<style>
  .systeme {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  .systeme > :global(*) {
    min-width: 0;
  }

  .roomless {
    display: grid;
    gap: 6px;
    padding: 14px 20px;
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
    font-size: 13.5px;
  }

  .roomless p {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin: 0;
  }

  .roomless .names {
    color: var(--ink-2);
  }

  .roomless .writable {
    color: var(--ink);
    font-weight: 600;
  }

  .versus {
    display: grid;
    gap: 14px;
    padding: 20px;
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .vs-head {
    display: flex;
    gap: 12px;
    align-items: baseline;
    flex-wrap: wrap;
  }

  .vs {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
    gap: 16px;
  }

  .vs > div {
    display: grid;
    gap: 2px;
    align-content: start;
  }

  .vs small {
    font-size: 12px;
    font-weight: 700;
    color: var(--ink-3);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .vs p b {
    font-size: 20px;
  }

  .big {
    font-size: 34px;
    font-weight: 300;
    color: var(--good);
    letter-spacing: -0.03em;
    line-height: 1;
  }

  .ok {
    color: var(--good);
    font-weight: 700;
  }

  .alert {
    color: var(--alert);
    font-weight: 700;
    font-size: 13px;
  }

  .trend {
    display: flex;
    align-items: flex-end;
    gap: 4px;
    height: 48px;
    /* Delivery after delivery, the bars grow in number: never wider than the card. */
    overflow: hidden;
    min-width: 0;
  }

  .trend i {
    width: 10px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--good) 60%, transparent);
  }

  .trend small {
    margin-left: 10px;
    align-self: center;
    font-size: 12px;
  }

  .map {
    position: relative;
    display: grid;
    grid-template-columns: minmax(170px, 1fr) minmax(200px, 1fr) minmax(220px, 1.2fr) minmax(170px, 1fr);
    gap: 48px;
    align-items: center;
    padding: 10px 0;
  }

  .links {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
    pointer-events: none;
  }

  .links path {
    fill: none;
    stroke: color-mix(in srgb, var(--ink-3) 35%, transparent);
    stroke-width: 1.6;
    transition: stroke 0.3s;
  }

  .links path.hot {
    stroke: var(--warm);
    stroke-width: 2.6;
    stroke-dasharray: 6 5;
    animation: flow 0.6s linear infinite;
  }

  .links path.off {
    stroke-dasharray: 3 5;
  }

  @keyframes flow {
    to {
      stroke-dashoffset: -11;
    }
  }

  .col {
    display: grid;
    gap: 8px;
    align-content: center;
    position: relative;
  }

  .col h2 {
    font-size: 12px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-3);
    font-weight: 750;
    margin-bottom: 4px;
  }

  .node {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 10px 12px;
    border: 0;
    border-radius: 14px;
    background: var(--surface);
    box-shadow: var(--shadow);
    text-align: left;
    color: var(--ink);
    transition: transform 0.2s var(--ease), box-shadow 0.3s;
  }

  .node:hover {
    transform: translateY(-1px);
  }

  .node span {
    display: grid;
    min-width: 0;
  }

  .node b {
    font-weight: 700;
    font-size: 14px;
  }

  .node small {
    font-size: 12px;
    color: var(--ink-3);
  }

  .driver.hot {
    box-shadow: var(--shadow), 0 0 0 2px var(--warm);
  }

  .dot {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--ink-3);
  }

  .dot.running {
    background: var(--good);
  }

  .dot.waiting,
  .dot.starting {
    background: var(--warm);
  }

  .dot.failed,
  .dot.backoff {
    background: var(--alert);
  }

  .core {
    display: grid;
    justify-items: center;
    gap: 4px;
    padding: 22px 16px;
    text-align: center;
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-lift), inset 0 0 0 2px color-mix(in srgb, var(--sun) 50%, transparent);
  }

  .core-logo {
    width: 46px;
    height: 46px;
    border-radius: 50%;
    background: var(--sun);
    box-shadow: inset 0 0 0 9px var(--sun), inset 0 0 0 14px #161512;
    margin-bottom: 6px;
  }

  .core b {
    font-size: 18px;
  }

  .core ul {
    list-style: none;
    margin: 10px 0 0;
    padding: 0;
    display: grid;
    gap: 4px;
    font-size: 12.5px;
    color: var(--ink-2);
  }

  .pulse {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .live {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--good);
    animation: blink 1.4s infinite;
  }

  @keyframes blink {
    50% {
      opacity: 0.3;
    }
  }

  .bricks {
    display: grid;
    gap: 8px;
  }

  .ic {
    flex: none;
    width: 34px;
    height: 34px;
    border-radius: 11px;
    display: grid !important;
    place-items: center;
    background: var(--good-soft);
    color: var(--good);
  }

  .brick.off .ic {
    background: var(--surface-2);
    color: var(--ink-3);
  }

  .brick.off {
    opacity: 0.6;
  }

  .soon {
    background: none;
    box-shadow: inset 0 0 0 2px var(--line);
    color: var(--ink-3);
  }

  .soon .ic {
    background: var(--surface-2);
    color: var(--ink-3);
  }

  .surface .ic {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .explain {
    position: relative;
    display: grid;
    gap: 8px;
    max-width: 720px;
  }

  .explain h3 {
    font-size: 19px;
  }

  .explain .how {
    color: var(--ink-2);
    font-size: 14px;
  }

  .x {
    position: absolute;
    top: 14px;
    right: 14px;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    border: 0;
    background: var(--surface-2);
    display: grid;
    place-items: center;
  }

  @media (max-width: 1100px) {
    .map {
      grid-template-columns: 1fr 1fr;
      gap: 24px;
    }

    .links {
      display: none;
    }
  }

  @media (max-width: 640px) {
    .map {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
