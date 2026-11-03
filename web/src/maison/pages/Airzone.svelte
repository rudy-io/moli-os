<script>
  import { tick, untrack } from 'svelte';
  import { hub, home, reloadLayout } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { deviceName, formatValue } from '../../lib/format.js';
  import { airzoneInventory, zoneSummary, readingOf, batterySummary, airzoneAlerts } from '../lib/airzone.js';
  import { climateMode } from '../lib/climate.js';
  import Icon from '../ui/Icon.svelte';
  import AirzoneZoneCard from '../ui/AirzoneZoneCard.svelte';
  import AirzoneReadings from '../ui/AirzoneReadings.svelte';
  import AirzoneManagement from '../ui/AirzoneManagement.svelte';
  import AirzoneAlerts from '../ui/AirzoneAlerts.svelte';
  import AirzoneAdd from '../ui/AirzoneAdd.svelte';
  import HistoryPanel from '../../components/HistoryPanel.svelte';

  let { route = 'airzone' } = $props();
  let search = $state('');
  let systemFilter = $state('');
  let heading = $state(null);
  let historyShell = $state(null);
  let focusedRoute;
  const inventory = $derived(airzoneInventory(hub.devices, hub.drivers, home.config?.hidden ?? []));
  const allInventory = $derived(airzoneInventory(hub.devices, hub.drivers));
  const nativeDevices = $derived([...allInventory.byId.values()]);
  const alertReport = $derived(airzoneAlerts(hub.devices, hub.drivers, {connected:hub.connected}));
  const systems = $derived(inventory.instances.flatMap((instance) => instance.systems.map((system) => ({ ...system, instance }))));
  const installation = $derived(route === 'airzone/installation');
  const management = $derived(route === 'airzone/devices' || route.startsWith('airzone/devices/'));
  const alerts = $derived(route === 'airzone/alerts');
  const adding = $derived(route === 'airzone/add');
  const detail = $derived(route.startsWith('airzone/') && !installation && !management && !alerts && !adding);
  const managedId = $derived.by(() => {
    if (!route.startsWith('airzone/devices/')) return null;
    try {return decodeURIComponent(route.slice('airzone/devices/'.length));} catch {return null;}
  });
  const zoneId = $derived.by(() => {
    if (!detail) return null;
    try { return decodeURIComponent(route.slice('airzone/'.length)); }
    catch { return null; }
  });
  const zone = $derived(inventory.zones.find((d) => d.id === zoneId));
  const summary = $derived(zone ? zoneSummary(zone) : null);
  const battery = $derived(zone ? batterySummary(zone) : null);
  const coverage = $derived(zone ? readingOf(zone, 'coverage') : null);
  const group = $derived(zone ? systems.find((s) => s.zones.some((d) => d.id === zone.id)) : null);
  const controls = $derived(group?.controls.filter((d) => d.instance === zone?.instance && d.members?.includes(zone?.id)) ?? []);
  const norm = (text) => String(text ?? '').normalize('NFD').replace(/\p{M}/gu, '').trim().toLocaleLowerCase(locale());
  const query = $derived(norm(search));
  const visible = $derived(systems.filter((s) => !systemFilter || s.id === systemFilter).map((s) => ({
    ...s,
    zones: s.zones.filter((d) => !query || norm([deviceName(d), d.label?.room, d.native_room].filter(Boolean).join(' ')).includes(query)),
  })).filter((s) => s.zones.length));
  const onlineCount = $derived(inventory.zones.filter((d) => d.online === true).length);
  const zoneSystems = $derived(systems.filter((s) => s.zones.length));
  const multipleInstances = $derived(inventory.instances.length > 1);

  $effect(() => {
    if (systemFilter && !systems.some((s) => s.id === systemFilter)) systemFilter = '';
  });

  $effect(() => {
    const current = route;
    if (current === focusedRoute) return;
    const previous = focusedRoute;
    focusedRoute = current;
    if (previous != null) untrack(() => { hub.chart = null; });
    untrack(async () => {
      await tick();
      if (route !== current) return;
      // Route changes announce the new heading; returning restores the zone
      // link. Live readings never move a person's keyboard focus.
      const returnLink = current === 'airzone' && previous?.startsWith('airzone/')
        ? [...(heading?.closest('.airzone-page')?.querySelectorAll('a.zone-card') ?? [])]
          .find((link) => link.getAttribute('href') === `#/${previous}`) : null;
      (returnLink ?? heading)?.focus();
    });
  });

  $effect(() => {
    if (!hub.chart || !historyShell) return;
    const previous = document.activeElement;
    historyShell.querySelector('button.close')?.focus();
    return () => { if (previous?.isConnected) previous.focus(); };
  });

  function containHistoryFocus(event) {
    if (event.key !== 'Tab') return;
    const buttons = [...historyShell.querySelectorAll('button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), [tabindex="0"]')];
    const first = buttons[0], last = buttons.at(-1);
    if (event.shiftKey && event.target === first) { event.preventDefault(); last?.focus(); }
    else if (!event.shiftKey && event.target === last) { event.preventDefault(); first?.focus(); }
  }

  const systemName = (s) => s.systemId == null ? t('airzone_ui.system_unknown') : t('airzone_ui.system', { number: s.systemId });
  const systemLabel = (s) => multipleInstances ? `${s.instance.id} · ${systemName(s)}` : systemName(s);
  const time = (ts) => new Date(ts).toLocaleString(locale(), { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
  const modeName = (mode) => climateMode(mode) ? t(climateMode(mode).label) : mode ?? t('airzone_ui.unknown');
  const optional = (device, key) => {
    const value = device.online === true ? readingOf(device, key)?.value : null;
    return t(value === true ? 'airzone_ui.available' : value === false ? 'airzone_ui.unavailable' : 'airzone_ui.availability_unknown');
  };
  const driverName = (driver) => ['running', 'waiting', 'backoff', 'stopped'].includes(driver.status?.state)
    ? t(`airzone_ui.driver.${driver.status.state}`) : t('airzone_ui.availability_unknown');
</script>

{#snippet metric(label, reading, prominent = false)}
  <button class="metric" class:prominent disabled={!reading} aria-label={reading ? t('airzone_ui.history_value', { label, value: `${formatValue(reading.point, reading.value)}${reading.unit ? ` ${reading.unit}` : ''}` }) : undefined}
    onclick={() => { if (zone && reading) hub.chart = `${zone.id}/${reading.key}`; }}>
    <span class="metric-label">{label}</span>
    <span class="measurement num">{reading ? formatValue(reading.point, reading.value) : '—'}{#if reading?.unit}<small>{reading.unit}</small>{/if}</span>
    {#if reading?.ts}<span class="sample">{t('airzone_ui.sample_at', { time: time(reading.ts) })}<Icon name="history" size={16} /></span>{/if}
  </button>
{/snippet}

<div class="airzone-page">
  <header class="page-heading">
    <div>
      {#if detail}<a class="back" href="#/airzone"><Icon name="chevron-left" size={18} />{t('airzone_ui.back')}</a>{/if}
      {#if detail || installation}<div class="eyebrow"><Icon name="air-conditioner" size={18} />{detail && group ? systemLabel(group) : t('airzone_ui.title')}</div>{/if}
      <h1 bind:this={heading} tabindex="-1">{zone ? deviceName(zone) : installation ? t('airzone_ui.installation') : management ? t('airzone_ui.manage') : alerts ? t('airzone_ui.alerts') : adding ? t('airzone_ui.add') : t('airzone_ui.title')}</h1>
      <p>{zone ? zone.label?.room || zone.native_room || systemName(group ?? {}) : installation ? t('airzone_ui.installation_help') : management ? t('airzone_ui.manage_help') : alerts ? t('airzone_ui.alerts_help') : adding ? t('airzone_ui.add_help') : t('airzone_ui.subtitle')}</p>
    </div>
    <span class="read-only" title={t('airzone_ui.read_only_help')}><Icon name="lock" size={16} />{t('airzone_ui.read_only')}</span>
  </header>

  {#if !detail}
    <nav class="view-tabs" aria-label={t('airzone_ui.title')}>
      <a href="#/airzone" aria-current={route === 'airzone' ? 'page' : undefined}><Icon name="home" size={18} />{t('airzone_ui.zones')}</a>
      <a href="#/airzone/alerts" aria-current={alerts ? 'page' : undefined}><Icon name="alert" size={18} />{t('airzone_ui.alerts')}{#if alertReport.items.length}<span class="alert-count">{alertReport.items.length}</span>{/if}</a>
      <a href="#/airzone/devices" aria-current={management || adding ? 'page' : undefined}><Icon name="tune" size={18} />{t('airzone_ui.manage')}</a>
      <a href="#/airzone/installation" aria-current={installation ? 'page' : undefined}><Icon name="tune" size={18} />{t('airzone_ui.installation')}</a>
    </nav>
  {/if}

  {#if !hub.connected && inventory.byId.size}
    <p class="notice" role="status"><Icon name="offline" size={18} />{t('airzone_ui.connection_lost')}</p>
  {/if}

  {#if !hub.connected && !inventory.instances.length}
    <div class="empty" role="status"><Icon name="air-conditioner" size={32} /><p>{t('airzone_ui.loading')}</p></div>
  {:else if management}
    <a class="add-link" href="#/airzone/add">{t('airzone_ui.add')}<Icon name="chevron-right" size={18} /></a>
    <AirzoneManagement devices={nativeDevices} hidden={home.config?.hidden ?? []} connected={hub.connected} selectedId={managedId} onchanged={reloadLayout} />
  {:else if alerts}
    <AirzoneAlerts report={alertReport} devices={nativeDevices} hidden={home.config?.hidden ?? []} />
  {:else if adding}
    <AirzoneAdd drivers={hub.drivers} connected={hub.connected} />
  {:else if installation}
    <div class="installation-stack">
      {#each inventory.instances as instance (instance.id)}
        <section class="installation-card">
          <header class="section-heading"><h2>{multipleInstances ? instance.id : t('airzone_ui.gateway')}</h2><span class="connection-state">{driverName(instance.driver)}</span></header>
          {#each instance.gateways as gateway (gateway.id)}
            <h3>{deviceName(gateway)}</h3>
            <div class="optional-summary">
              <div><span>{t('airzone_ui.dhw')}</span><strong>{optional(gateway, 'dhw_available')}</strong></div>
              <div><span>{t('airzone_ui.iaq')}</span><strong>{optional(gateway, 'iaq_available')}</strong></div>
            </div>
            <p class="explanation">{t('airzone_ui.optional_help')}</p>
            <AirzoneReadings device={gateway} />
          {/each}
          {#each instance.systems as system (system.id)}
            <details class="resource-group">
              <summary aria-label={`${systemName(system)} · ${t('airzone_ui.zone_count', { count: system.zones.length })}`}><span>{systemName(system)}</span><small>{t('airzone_ui.zone_count', { count: system.zones.length })}</small></summary>
              <div class="resource-content">
                {#each [...system.metadata, ...system.controls, ...system.iaq] as resource (resource.id)}
                  <section><h3>{deviceName(resource)}</h3><AirzoneReadings device={resource} /></section>
                {/each}
              </div>
            </details>
          {/each}
          {#each [...instance.dhw, ...instance.other] as resource (resource.id)}
            <details class="resource-group"><summary>{deviceName(resource)}</summary><div class="resource-content"><AirzoneReadings device={resource} /></div></details>
          {/each}
        </section>
      {/each}
      <section class="scope-note"><Icon name="clock" size={22} /><div><h2>{t('airzone_ui.schedules')}</h2><p>{t('airzone_ui.schedules_help')}</p></div></section>
      <a class="text-link" href="#/atelier">{t('airzone_ui.back_workshop')}<Icon name="chevron-right" size={18} /></a>
    </div>
  {:else if detail && !zone}
    <div class="empty"><Icon name="air-conditioner" size={32} /><p>{t('airzone_ui.not_found')}</p><a href="#/airzone">{t('airzone_ui.back')}</a></div>
  {:else if zone}
    {#if summary.availability !== 'online'}<p class="notice" role="status"><Icon name="offline" size={18} />{summary.availability === 'offline' ? t('airzone_ui.offline_help') : t('airzone_ui.availability_unknown')}</p>{/if}
    {#if summary.online && summary.problem === true}<p class="notice problem" role="status"><Icon name="alert" size={18} />{t('airzone_ui.problem')}</p>{/if}
    <div class="detail-grid">
      <section class="comfort-card" class:stale={summary.availability !== 'online'}>
        <header class="section-heading"><h2>{t('airzone_ui.measurements')}</h2><span class="state-chip">{t(`airzone_ui.status.${summary.status}`)}</span></header>
        <div class="climate-metrics">
          {@render metric(t('airzone_ui.current'), summary.temperature, true)}
          {@render metric(t('airzone_ui.target'), summary.target)}
        </div>
        {#if !summary.target && (summary.heatingTarget || summary.coolingTarget)}
          <div class="split-targets">{@render metric(t('airzone_ui.heating_target'), summary.heatingTarget)}{@render metric(t('airzone_ui.cooling_target'), summary.coolingTarget)}</div>
        {/if}
        <div class="supporting">
          <div><Icon name="air-conditioner" size={20} /><span>{t('airzone_ui.mode')}</span><strong>{modeName(summary.mode)}</strong></div>
          {#if summary.humidity}<div><Icon name="humidity" size={20} /><span>{t('airzone_ui.humidity')}</span><strong>{formatValue(summary.humidity.point, summary.humidity.value)}{summary.humidity.unit ?? ''}</strong></div>{/if}
        </div>
        <p class="read-hint"><Icon name="lock" size={16} />{t('airzone_ui.read_only_help')}</p>
      </section>
      <section class="activity-card">
        <span class="activity-glyph"><Icon name="air-conditioner" size={28} /></span>
        <h2>{t('airzone_ui.activity')}</h2>
        <strong class="activity-status">{summary.availability === 'online' ? t(summary.activity === 'demand' ? 'airzone_ui.status.demand' : summary.activity === 'no_reported_demand' ? 'airzone_ui.status.idle' : 'airzone_ui.unknown') : t('airzone_ui.latest')}</strong>
        <p>{t('airzone_ui.activity_help')}</p>
        <dl class="demands">
          {#each summary.demands as demand (demand.key)}
            {@const point = zone.points.find((p) => p.key === demand.key)}
            <div><dt>{point?.label ?? demand.key}</dt><dd>{point ? formatValue(point, demand.value) : '—'}</dd></div>
          {/each}
        </dl>
      </section>
    </div>
    <section class="parameters-card"><h2>{t('airzone_ui.parameters')}</h2><AirzoneReadings device={zone} /></section>
    {#if controls.length}
      <section class="parameters-card"><h2>{t('airzone_ui.shared')}</h2><p class="explanation">{t('airzone_ui.shared_help')}</p>
        {#each controls as control (control.id)}<h3>{deviceName(control)}</h3><AirzoneReadings device={control} />{/each}
      </section>
    {/if}
    <section class="parameters-card battery-card">
      <div><h2>{t('airzone_health.battery')}</h2>
        <strong>{battery.status === 'level' ? `${battery.percent} %` : battery.status === 'wired' ? t('airzone_health.wired') : t('airzone_health.battery_missing')}</strong>
        {#if battery.radio === true}<p>{t('airzone_health.radio')}</p>{/if}
        {#if battery.availability !== 'online'}<p>{t('airzone_ui.latest')}</p>{/if}
        {#if coverage?.unit === '%' && Number.isInteger(coverage.value) && coverage.value >= 0 && coverage.value <= 100}<p>{t('airzone_health.signal')} : {coverage.value} %</p>{/if}
      </div><p class="explanation">{t('airzone_health.battery_help')}</p>
    </section>
    <a class="text-link" href={`#/airzone/devices/${encodeURIComponent(zone.id)}`}>{t('airzone_ui.manage')}<Icon name="chevron-right" size={18} /></a>
    <a class="text-link" href="#/airzone/installation">{t('airzone_ui.installation')}<Icon name="chevron-right" size={18} /></a>
  {:else if !inventory.zones.length}
    <div class="empty"><Icon name="air-conditioner" size={32} /><h2>{t('airzone_ui.empty')}</h2><p>{t('airzone_ui.empty_help')}</p><a href="#/atelier">{t('airzone_ui.back_workshop')}</a></div>
  {:else}
    {#if alertReport.items.length}<a class="alert-summary" href="#/airzone/alerts"><Icon name="alert" size={20} /><span>{t('airzone_ui.alert_count',{count:alertReport.items.length})}</span><Icon name="chevron-right" size={18} /></a>{/if}
    <div class="overview-bar">
      <div class="inventory-counts"><strong>{t('airzone_ui.zone_count', { count: inventory.zones.length })}</strong><span>{t('airzone_ui.system_count', { count: zoneSystems.length })}</span><span>{t('airzone_ui.online_count', { count: onlineCount })}</span></div>
      <div class="filters">
        <label class="search"><Icon name="search" size={18} /><input type="search" aria-label={t('airzone_ui.search')} placeholder={t('airzone_ui.search_hint')} bind:value={search} /></label>
        <select aria-label={t('airzone_ui.filter_system')} bind:value={systemFilter}><option value="">{t('airzone_ui.all_systems')}</option>{#each zoneSystems as system (system.id)}<option value={system.id}>{systemLabel(system)}</option>{/each}</select>
      </div>
    </div>
    {#each visible as system (system.id)}
      <section class="system-zones">
        <header class="section-heading"><h2>{systemLabel(system)}</h2><span>{t('airzone_ui.zone_count', { count: system.zones.length })}</span></header>
        <div class="zone-grid">{#each system.zones as device (device.id)}<AirzoneZoneCard {device} href={`#/airzone/${encodeURIComponent(device.id)}`} />{/each}</div>
      </section>
    {:else}
      <div class="empty" role="status"><p>{t('airzone_ui.no_results')}</p><button onclick={() => { search = ''; systemFilter = ''; }}>{t('airzone_ui.reset')}</button></div>
    {/each}
  {/if}
</div>

{#if hub.chart}
  <!-- svelte-ignore a11y_no_static_element_interactions (Keeps keyboard focus inside the existing dialog.) -->
  <div class="history-shell" bind:this={historyShell} onkeydown={containHistoryFocus}><HistoryPanel point={hub.chart} onclose={() => (hub.chart = null)} /></div>
{/if}

<style>
  .add-link {display:inline-flex; align-items:center; gap:8px; min-height:44px; padding:10px 18px; border-radius:var(--r-sm); background:var(--cool-soft); color:var(--cool); font-weight:650; margin-bottom:20px; text-decoration:none;}
  .alert-count {display:inline-grid; place-items:center; min-width:22px; height:22px; border-radius:50%; padding:0 5px; background:var(--alert-soft); color:var(--alert); font-size:12px;}
  .alert-summary {display:flex; gap:12px; align-items:center; margin-bottom:20px; padding:15px 18px; border-radius:var(--r-md); background:var(--warm-soft); color:var(--warm-ink); text-decoration:none; min-height:48px;}
  .alert-summary span {flex:1;}
  .battery-card {display:grid; grid-template-columns:1fr 1fr; gap:20px;}
  .battery-card strong {font-size:22px;}
  .battery-card p {font-size:13px; color:var(--ink-2); margin:8px 0 0;}
  @media(max-width:700px) {.battery-card {grid-template-columns:1fr;}}
  .airzone-page { max-width: 1240px; margin: 0 auto; padding: 8px 0 40px; }
  .history-shell { position: fixed; inset: 0; z-index: 100; --card: var(--surface); --radius: var(--r-lg); --stroke: 1px; --copper: var(--cool); }
  .history-shell :global(.panel) { border-color: var(--line); box-shadow: var(--shadow-lift); }
  .history-shell :global(.ranges button) { min-height: 44px; min-width: 44px; }
  .page-heading { display: flex; justify-content: space-between; align-items: center; gap: 24px; margin-bottom: 28px; }
  .eyebrow { display: flex; align-items: center; gap: 8px; color: var(--cool); font-size: 13px; font-weight: 650; margin-bottom: 10px; }
  h1 { font-size: clamp(30px, 4vw, 44px); font-weight: 700; line-height: 1.1; letter-spacing: -.04em; overflow-wrap: anywhere; }
  h1:focus-visible { outline: none; }
  .page-heading p { margin-top: 12px; color: var(--ink-2); max-width: 48rem; }
  .read-only { display: inline-flex; align-items: center; gap: 7px; border: 1px solid var(--line); border-radius: 12px; padding: 10px 14px; color: var(--ink-2); font-size: 13px; flex: none; }
  .view-tabs { display: flex; flex-wrap: wrap; gap: 6px; border-bottom: 1px solid var(--line); margin-bottom: 26px; }
  .view-tabs a { min-height: 48px; display: inline-flex; align-items: center; gap: 8px; padding: 12px 18px; color: var(--ink-2); text-decoration: none; border-bottom: 2px solid transparent; }
  .view-tabs a[aria-current] { color: var(--cool); border-bottom-color: var(--cool); font-weight: 650; }
  .overview-bar { display: flex; justify-content: space-between; gap: 20px; align-items: center; margin-bottom: 28px; }
  .inventory-counts { display: flex; gap: 14px; flex-wrap: wrap; font-size: 13px; color: var(--ink-2); }
  .inventory-counts strong { color: var(--ink); }
  .filters { display: flex; gap: 10px; }
  .search { display: flex; align-items: center; gap: 8px; padding: 0 14px; color: var(--ink-3); background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  input { min-width: 0; width: 210px; padding: 12px 0; border: 0; background: transparent; font: inherit; color: var(--ink); font-size: 14px; }
  select { max-width: 240px; min-height: 44px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); color: var(--ink); font: inherit; font-size: 14px; }
  .section-heading { display: flex; align-items: center; justify-content: space-between; gap: 14px; margin-bottom: 16px; }
  .section-heading h2, .parameters-card > h2 { font-size: 18px; font-weight: 650; letter-spacing: -.02em; }
  .section-heading > span { color: var(--ink-3); font-size: 13px; }
  .system-zones + .system-zones { margin-top: 32px; }
  .zone-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 238px), 1fr)); gap: 16px; }
  .back, .text-link { display: inline-flex; gap: 6px; align-items: center; min-height: 44px; color: var(--cool); font-size: 14px; text-decoration: none; }
  .back { margin-bottom: 12px; }
  .back:hover, .text-link:hover { text-decoration: underline; }
  .detail-grid { display: grid; grid-template-columns: minmax(0, 1.65fr) minmax(260px, 1fr); gap: 20px; margin-bottom: 20px; }
  .comfort-card, .activity-card, .parameters-card, .installation-card { min-width: 0; padding: 26px; background: var(--surface); border-radius: var(--r-lg); box-shadow: var(--shadow); }
  .climate-metrics, .split-targets { display: grid; grid-template-columns: 1.2fr 1fr; gap: 14px; }
  .metric { min-width: 0; display: flex; flex-direction: column; align-items: flex-start; gap: 12px; border: 0; padding: 24px 18px; border-radius: var(--r-md); background: var(--surface-2); text-align: left; }
  .metric.prominent { background: var(--cool-soft); }
  .metric:not(:disabled):hover { box-shadow: inset 0 0 0 1px var(--cool); }
  .metric-label { font-size: 13px; color: var(--ink-2); }
  .measurement { font-size: clamp(34px, 4vw, 54px); font-weight: 600; line-height: 1; letter-spacing: -.045em; overflow-wrap: anywhere; }
  .prominent .measurement { font-size: clamp(44px, 5vw, 68px); }
  .measurement small { font-size: .36em; margin-left: 5px; color: var(--ink-2); letter-spacing: -.01em; }
  .sample { display: flex; gap: 8px; align-items: center; font-size: 11px; color: var(--ink-3); margin-top: auto; }
  .split-targets { margin-top: 14px; }
  .supporting { display: flex; flex-wrap: wrap; gap: 22px; padding: 22px 0; }
  .supporting > div { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; font-size: 13px; color: var(--ink-2); }
  .supporting strong { color: var(--ink); font-weight: 600; }
  .read-hint { display: flex; gap: 8px; align-items: flex-start; font-size: 12px; color: var(--ink-3); line-height: 1.6; }
  .activity-glyph { display: inline-flex; color: var(--warm-ink); background: var(--warm-soft); padding: 12px; border-radius: 16px; margin-bottom: 18px; }
  .activity-card h2 { font-size: 15px; color: var(--ink-2); margin-bottom: 10px; }
  .activity-status { font-size: 21px; font-weight: 650; letter-spacing: -.02em; }
  .activity-card p, .explanation { color: var(--ink-2); font-size: 13px; line-height: 1.6; margin: 12px 0 18px; }
  .demands { display: grid; gap: 10px; margin: 20px 0 0; }
  .demands div { display: flex; justify-content: space-between; gap: 12px; font-size: 13px; }
  dt { color: var(--ink-2); } dd { margin: 0; font-weight: 600; }
  .parameters-card { margin-bottom: 20px; }
  .parameters-card > h2 { margin-bottom: 18px; }
  h3 { font-size: 14px; color: var(--ink-2); margin: 16px 0 12px; font-weight: 650; }
  .notice { display: flex; align-items: flex-start; gap: 10px; padding: 16px 20px; border-radius: 14px; margin-bottom: 20px; background: var(--warm-soft); color: var(--warm-ink); font-size: 14px; }
  .notice.problem { background: var(--alert-soft); color: var(--alert); }
  .stale .measurement { color: var(--ink-2); }
  .empty { min-height: 220px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 18px; text-align: center; padding: 32px 20px; color: var(--ink-2); }
  .empty h2 { font-size: 20px; color: var(--ink); }
  .empty p { max-width: 560px; line-height: 1.6; }
  .empty a { color: var(--cool); }
  .empty button { min-height: 44px; border: 1px solid var(--line); background: var(--surface); padding: 10px 18px; border-radius: 12px; }
  .installation-stack { display: grid; gap: 20px; max-width: 900px; }
  .optional-summary { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin: 18px 0; }
  .optional-summary > div { display: flex; flex-direction: column; gap: 9px; background: var(--surface-2); border-radius: 14px; padding: 16px; font-size: 13px; color: var(--ink-2); }
  .optional-summary strong { color: var(--ink); font-weight: 600; }
  .resource-group { border-top: 1px solid var(--line); margin-top: 18px; }
  .resource-group > summary { padding: 18px 0; min-height: 48px; cursor: pointer; font-weight: 600; }
  .resource-group small { margin-left: 12px; font-weight: 400; color: var(--ink-3); }
  .resource-content { padding-bottom: 12px; }
  .resource-content > section + section { margin-top: 24px; }
  .scope-note { display: flex; align-items: flex-start; gap: 14px; padding: 22px; border: 1px solid var(--line); border-radius: 20px; color: var(--ink-2); }
  .scope-note h2 { font-size: 15px; color: var(--ink); margin-bottom: 8px; }
  .scope-note p { font-size: 13px; line-height: 1.6; }
  @media (max-width: 1000px) { .overview-bar { align-items: flex-start; flex-direction: column; } .filters { width: 100%; } .search { flex: 1; } input { width: 100%; } }
  @media (max-width: 700px) {
    .page-heading { align-items: flex-start; flex-direction: column; gap: 18px; margin-bottom: 20px; }
    .read-only { padding: 8px 12px; }
    .view-tabs {display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:0;}
    .view-tabs a { min-width:0; padding:12px 8px; font-size:12px; gap:6px; }
    .filters { flex-direction: row; } select { max-width: 148px; } .search { padding: 0 10px; min-width: 0; }
    .zone-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
    .detail-grid { grid-template-columns: minmax(0, 1fr); }
    .comfort-card, .activity-card, .parameters-card, .installation-card { padding: 20px; }
    .climate-metrics { grid-template-columns: 1fr 1fr; gap: 10px; }
    .metric { padding: 20px 12px; }
    .measurement { font-size: 34px; } .prominent .measurement { font-size: 46px; }
    .sample { font-size: 10px; gap: 3px; flex-wrap: wrap; }
    .optional-summary { grid-template-columns: minmax(0, 1fr); }
  }
</style>
