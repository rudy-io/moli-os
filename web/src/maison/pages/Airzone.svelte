<script>
  import { tick, untrack } from 'svelte';
  import { hub, home, reloadLayout } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { deviceName } from '../../lib/format.js';
  import { airzoneInventory, readingOf, airzoneAlerts } from '../lib/airzone.js';
  import { filterZoneSystems, zoneActivityCounts } from '../lib/airzone-view.js';
  import Icon from '../ui/Icon.svelte';
  import AirzoneZoneDetail from '../ui/AirzoneZoneDetail.svelte';
  import AirzoneZoneCard from '../ui/AirzoneZoneCard.svelte';
  import AirzoneReadings from '../ui/AirzoneReadings.svelte';
  import AirzoneManagement from '../ui/AirzoneManagement.svelte';
  import AirzoneAlerts from '../ui/AirzoneAlerts.svelte';
  import AirzoneAdd from '../ui/AirzoneAdd.svelte';
  import AirzoneGateways from '../ui/AirzoneGateways.svelte';
  import HistoryPanel from '../../components/HistoryPanel.svelte';

  let { route = 'airzone' } = $props();
  let search = $state('');
  let systemFilter = $state('');
  let activityFilter = $state('all');
  let searchInput = $state(null);
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
  const group = $derived(zone ? systems.find((s) => s.zones.some((d) => d.id === zone.id)) : null);
  const controls = $derived(group?.controls.filter((d) => d.instance === zone?.instance && d.members?.includes(zone?.id)) ?? []);
  const candidates = $derived(filterZoneSystems(systems, {search, system: systemFilter, language: locale()}));
  const visible = $derived(filterZoneSystems(candidates, {activity: activityFilter, connected: hub.connected}));
  const activityCounts = $derived(zoneActivityCounts(candidates.flatMap((s) => s.zones), hub.connected));
  const filtered = $derived(Boolean(search || systemFilter || activityFilter !== 'all'));
  async function resetFilters() {
    search = ''; systemFilter = ''; activityFilter = 'all';
    await tick();
    searchInput?.focus();
  }
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
  const optional = (device, key) => {
    const value = device.online === true ? readingOf(device, key)?.value : null;
    return t(value === true ? 'airzone_ui.available' : value === false ? 'airzone_ui.unavailable' : 'airzone_ui.availability_unknown');
  };
  const driverName = (driver) => ['running', 'waiting', 'backoff', 'stopped'].includes(driver.status?.state)
    ? t(`airzone_ui.driver.${driver.status.state}`) : t('airzone_ui.availability_unknown');
</script>

<div class="airzone-page">
  <header class="page-heading" class:overview={route === 'airzone'}>
    <div>
      {#if detail}<a class="back" href="#/airzone"><Icon name="chevron-left" size={18} />{t('airzone_ui.back')}</a>{/if}
      {#if detail || installation}<div class="eyebrow"><Icon name="air-conditioner" size={18} />{detail && group ? systemLabel(group) : t('airzone_ui.title')}</div>{/if}
      <h1 bind:this={heading} tabindex="-1">{zone ? deviceName(zone) : installation ? t('airzone_ui.installation') : management ? t('airzone_ui.manage') : alerts ? t('airzone_ui.alerts') : adding ? t('airzone_ui.add') : t('airzone_ui.title')}</h1>
      {#if !zone || zone.label?.room || zone.native_room}<p>{zone ? zone.label?.room || zone.native_room : installation ? t('airzone_ui.installation_help') : management ? t('airzone_ui.manage_help') : alerts ? t('airzone_ui.alerts_help') : adding ? t('airzone_ui.add_help') : t('airzone_ui.subtitle')}</p>{/if}
    </div>
    <span class="read-only" title={t('airzone_ui.read_only_help')}><Icon name="lock" size={16} />{t('airzone_ui.read_only')}</span>
  </header>

  {#if !detail}
    <nav class="view-tabs" aria-label={t('airzone_ui.title')}>
      <a href="#/airzone" aria-current={route === 'airzone' ? 'page' : undefined}><Icon name="home" size={18} />{t('airzone_ui.zones')}</a>
      <a href="#/airzone/alerts" aria-current={alerts ? 'page' : undefined}><Icon name="alert" size={18} />{t('airzone_ui.alerts')}{#if alertReport.items.length}<span class="alert-count">{alertReport.items.length}</span>{/if}</a>
      <a href="#/airzone/devices" aria-current={management || adding ? 'page' : undefined}><Icon name="tune" size={18} />{t('airzone_navigation.devices')}</a>
      <a href="#/airzone/installation" aria-current={installation ? 'page' : undefined}><Icon name="tune" size={18} />{t('airzone_navigation.installation')}</a>
    </nav>
  {/if}

  {#if !hub.connected && inventory.byId.size}
    <p class="notice" role="status"><Icon name="offline" size={18} />{t('airzone_ui.connection_lost')}</p>
  {/if}

  {#if !hub.connected && !inventory.instances.length && !installation && !adding}
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
      <AirzoneGateways connected={hub.connected} drivers={hub.drivers} registryRevision={hub.driverRevision} />
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
    <AirzoneZoneDetail {zone} {controls} onhistory={(key) => { hub.chart = `${zone.id}/${key}`; }} />
  {:else if !inventory.zones.length}
    <a class="add-link" href="#/airzone/add">{t('airzone_navigation.add_gateway')}<Icon name="chevron-right" size={18} /></a>
    <div class="empty"><Icon name="air-conditioner" size={32} /><h2>{t('airzone_ui.empty')}</h2><p>{t('airzone_ui.empty_help')}</p><a href="#/atelier">{t('airzone_ui.back_workshop')}</a></div>
  {:else}
    {#if alertReport.items.length}<a class="alert-summary" href="#/airzone/alerts"><Icon name="alert" size={20} /><span>{t('airzone_ui.alert_count',{count:alertReport.items.length})}</span><Icon name="chevron-right" size={18} /></a>{/if}
    <div class="overview-heading">
      <div class="inventory-counts"><strong>{t('airzone_ui.zone_count', { count: inventory.zones.length })}</strong><span>{t('airzone_ui.system_count', { count: zoneSystems.length })}</span><span>{t('airzone_ui.online_count', { count: onlineCount })}</span></div>
      <a class="add-link" href="#/airzone/add">{t('airzone_navigation.add_gateway')}<Icon name="chevron-right" size={18} /></a>
    </div>
    <div class="overview-bar">
      <div class="filters">
        <label class="search"><Icon name="search" size={18} /><input bind:this={searchInput} type="search" aria-label={t('airzone_ui.search')} placeholder={t('airzone_ui.search_hint')} bind:value={search} /></label>
        <select aria-label={t('airzone_ui.filter_system')} bind:value={systemFilter}><option value="">{t('airzone_ui.all_systems')}</option>{#each zoneSystems as system (system.id)}<option value={system.id}>{systemLabel(system)}</option>{/each}</select>
      </div>
    <div class="activity-filters" role="group" aria-label={t('airzone_navigation.filter_activity')}>
      {#each ['all', 'demand', 'unavailable'] as activity}
        <button class="filter-chip" disabled={!hub.connected && activity === 'demand'} aria-pressed={activityFilter === activity} onclick={() => { activityFilter = activity; }}>
          {t(`airzone_navigation.${activity}`)}<span class="num">{activity === 'demand' && !hub.connected ? '—' : activityCounts[activity]}</span>
        </button>
      {/each}
      {#if filtered}<button class="filter-reset" onclick={resetFilters}>{t('airzone_ui.reset')}</button>{/if}
    </div>
    </div>
    {#each visible as system (system.id)}
      <section class="system-zones">
        <header class="section-heading"><h2>{systemLabel(system)}</h2><span>{t('airzone_ui.zone_count', { count: system.zones.length })}</span></header>
        <div class="zone-grid">{#each system.zones as device (device.id)}<AirzoneZoneCard {device} href={`#/airzone/${encodeURIComponent(device.id)}`} />{/each}</div>
      </section>
    {:else}
      <div class="empty" role="status"><p>{t('airzone_ui.no_results')}</p></div>
    {/each}
  {/if}
</div>

{#if hub.chart}
  <!-- svelte-ignore a11y_no_static_element_interactions (Keeps keyboard focus inside the existing dialog.) -->
  <div class="history-shell" bind:this={historyShell} onkeydown={containHistoryFocus}><HistoryPanel point={hub.chart} onclose={() => (hub.chart = null)} /></div>
{/if}

<style>
  .airzone-page { max-width: 1240px; margin: 0 auto; padding: 8px 0 40px; }
  .page-heading { display:flex; justify-content:space-between; align-items:center; gap:24px; margin-bottom:26px; }
  .page-heading > div { min-width:0; }
  h1 { font-size:clamp(2rem,4vw,2.75rem); font-weight:700; line-height:1.1; letter-spacing:-.04em; overflow-wrap:anywhere; }
  h1:focus-visible { outline:none; }
  .page-heading p { margin-top:12px; color:var(--ink-2); max-width:48rem; }
  .eyebrow { display:flex; align-items:center; gap:8px; color:var(--ink-2); font-size:.875rem; font-weight:600; margin-bottom:10px; }
  .read-only { display:inline-flex; align-items:center; gap:7px; border:1px solid var(--line); border-radius:99px; padding:8px 12px; color:var(--ink-2); font-size:.8125rem; flex:none; }
  .view-tabs { display:grid; grid-template-columns:repeat(4,minmax(0,1fr)); gap:4px; padding:5px; border:1px solid var(--line); border-radius:18px; background:var(--surface-3); margin-bottom:28px; max-width:700px; }
  .view-tabs a { min-width:0; min-height:46px; display:flex; align-items:center; justify-content:center; gap:7px; padding:10px 12px; border-radius:13px; color:var(--ink-2); text-decoration:none; font-size:.875rem; transition:background 140ms var(--ease),box-shadow 140ms var(--ease); }
  .view-tabs a[aria-current] { color:var(--ink); background:var(--surface); box-shadow:var(--shadow); font-weight:650; }
  .view-tabs a:active { background:var(--surface-2); }
  .alert-count { display:inline-grid; place-items:center; min-width:20px; height:20px; padding:0 4px; border-radius:99px; background:var(--warm-soft); color:var(--warm-ink); font-size:.75rem; }
  .alert-summary { display:flex; gap:12px; align-items:center; margin-bottom:22px; padding:15px 18px; border-radius:var(--r-md); background:var(--warm-soft); color:var(--warm-ink); text-decoration:none; min-height:48px; }
  .alert-summary span { flex:1; }
  .overview-heading { display:flex; align-items:center; justify-content:space-between; gap:20px; margin-bottom:20px; }
  .add-link { display:inline-flex; align-items:center; gap:8px; min-height:44px; padding:10px 16px; border:1px solid var(--line); border-radius:99px; background:var(--surface); color:var(--ink); font-size:.875rem; font-weight:600; text-decoration:none; }
  .airzone-page > .add-link { margin-bottom:20px; }
  .overview-bar { display:flex; justify-content:space-between; gap:16px; flex-wrap:wrap; align-items:center; margin-bottom:28px; }
  .inventory-counts { display:flex; gap:14px; flex-wrap:wrap; font-size:.875rem; color:var(--ink-2); }
  .inventory-counts strong { color:var(--ink); }
  .filters { display:flex; flex:1; gap:10px; }
  .search { flex:1; min-width:0; display:flex; align-items:center; gap:8px; padding:0 14px; color:var(--ink-2); background:var(--surface); border:1px solid var(--line); border-radius:14px; }
  input { min-width:0; width:100%; min-height:46px; padding:12px 0; border:0; background:transparent; font:inherit; color:var(--ink); }
  select { max-width:240px; min-height:46px; padding:10px 12px; border:1px solid var(--line); border-radius:14px; background:var(--surface); color:var(--ink); font:inherit; }
  .activity-filters { display:flex; flex-wrap:wrap; align-items:center; gap:8px; }
  .filter-chip { display:inline-flex; align-items:center; gap:10px; min-height:44px; padding:10px 15px; border:1px solid var(--line); border-radius:99px; background:transparent; font-size:.875rem; }
  .filter-chip span { font-size:.8125rem; min-width:18px; text-align:center; }
  .filter-chip[aria-pressed='true'] { background:var(--ink); color:var(--surface); border-color:var(--ink); }
  .filter-reset { min-height:44px; border:0; border-radius:12px; padding:10px 12px; background:transparent; text-decoration:underline; font-size:.875rem; }
  .section-heading { display:flex; align-items:center; justify-content:space-between; gap:14px; margin-bottom:14px; }
  .section-heading h2 { font-size:1.125rem; font-weight:650; letter-spacing:-.02em; }
  .section-heading > span { color:var(--ink-2); font-size:.875rem; }
  .system-zones + .system-zones { margin-top:32px; }
  .zone-grid { display:grid; grid-template-columns:repeat(auto-fill,minmax(min(100%,238px),1fr)); gap:16px; }
  .back, .text-link { display:inline-flex; gap:6px; align-items:center; min-height:44px; color:var(--ink-2); font-size:.875rem; text-decoration:none; }
  .back { margin-bottom:12px; }
  .back:hover, .text-link:hover { text-decoration:underline; }
  .notice { display:flex; align-items:flex-start; gap:10px; padding:16px 20px; border-radius:14px; margin-bottom:20px; background:var(--warm-soft); color:var(--warm-ink); font-size:.875rem; }
  .empty { min-height:220px; display:flex; flex-direction:column; align-items:center; justify-content:center; gap:18px; text-align:center; padding:32px 20px; color:var(--ink-2); }
  .empty h2 { font-size:1.25rem; color:var(--ink); }
  .empty p { max-width:560px; line-height:1.6; }
  .empty a { color:var(--ink-2); }
  .installation-card { min-width:0; padding:26px; background:var(--surface); border-radius:var(--r-lg); box-shadow:var(--shadow); }
  .explanation { color:var(--ink-2); font-size:.875rem; line-height:1.6; margin:12px 0 18px; }
  .history-shell { position: fixed; inset: 0; z-index: 100; --card: var(--surface); --radius: var(--r-lg); --stroke: 1px; --copper: var(--cool); }
  .history-shell :global(.panel) { border-color: var(--line); box-shadow: var(--shadow-lift); }
  .history-shell :global(.ranges button) { min-height: 44px; min-width: 44px; }
  h3 { font-size: 14px; color: var(--ink-2); margin: 16px 0 12px; font-weight: 650; }
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
  @media (max-width:1000px) {
    .overview-bar { align-items:flex-start; flex-direction:column; }
    .filters { width:100%; } .search { flex:1; } input { width:100%; }
  }
  @media (max-width:700px) {
    .page-heading { align-items:flex-start; flex-direction:column; gap:14px; margin-bottom:20px; }
    .page-heading.overview { flex-direction:row; align-items:center; gap:12px; }
    .page-heading.overview p { display:none; }
    .page-heading.overview .read-only { max-width:155px; line-height:1.4; }
    .view-tabs { gap:2px; padding:4px; }
    .view-tabs a { flex-wrap:wrap; padding:10px 4px; gap:4px; font-size:.8125rem; }
    .view-tabs a :global(svg) { display:none; }
    .overview-heading { align-items:flex-start; flex-direction:column; gap:14px; }
    .filters { flex-direction:row; } select { max-width:150px; width:45%; } .search { min-width:0; }
    .activity-filters { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); width:100%; gap:6px; }
    .filter-chip { justify-content:center; padding:10px 6px; gap:5px; font-size:.8125rem; }
    .filter-reset { grid-column:1/-1; justify-self:start; }
    .zone-grid { grid-template-columns:repeat(2,minmax(0,1fr)); gap:12px; }
    .installation-card { padding:20px; }
    .optional-summary { grid-template-columns:minmax(0,1fr); }
  }
  @media (max-width:430px) { .zone-grid { grid-template-columns:minmax(0,1fr); } }
  @media (max-width:340px) { .page-heading.overview { flex-direction:column; align-items:flex-start; } .filters { flex-direction:column; } select { max-width:none; width:100%; } .filter-chip { flex-wrap:wrap; } }
  @media (prefers-reduced-motion:reduce) { .view-tabs a { transition:none; } }
  @media (prefers-contrast:more) { .view-tabs, .add-link, .filter-chip { border-color:var(--ink-2); } }
</style>
