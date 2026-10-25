<script>
  import { onMount } from 'svelte';
  import { home, hub, roomList, roomNamed, nameOf, savePref } from '../lib/home.svelte.js';
  import { floorsOf } from '../lib/floors.js';
  import { probesOf, lineOf, scaleOf, tempKey } from '../lib/temperatures.js';
  import { seriesColor } from '../lib/palette.js';
  import { t } from '../../lib/i18n.svelte.js';
  import TempChart from '../ui/TempChart.svelte';

  /** Each room's temperature over the last day, week or month, floor by
   *  floor as on the Pièces page: every room on the same scale, so they
   *  compare at a glance; a moment touched on one shows on all; outside
   *  (the weather's) dashed on each when asked. */

  const RANGES = [
    ['day', 24, 'maison.temperatures.jour'],
    ['week', 24 * 7, 'maison.temperatures.semaine'],
    ['month', 24 * 30, 'maison.temperatures.mois'],
  ];
  const remembered = (key, fallback) => {
    try {
      return localStorage.getItem(key) ?? fallback;
    } catch {
      return fallback;
    }
  };
  let range = $state(remembered('temperatures-range', 'day'));
  let outside = $state(remembered('temperatures-dehors', '') === '1');
  const hours = $derived(RANGES.find((r) => r[0] === range)?.[1] ?? 24);
  function pick(key) {
    range = key;
    savePref('temperatures-range', key);
  }
  function toggleOutside() {
    outside = !outside;
    savePref('temperatures-dehors', outside ? '1' : '');
  }

  let plan = $state(null);
  onMount(async () => {
    try {
      const res = await fetch('/api/plan');
      if (res.ok) plan = await res.json();
    } catch {
      /* without the plan: one list, no floors */
    }
  });

  // The rooms something measures, each with what measures it.
  const rooms = $derived(
    roomList()
      .map((r) => ({ ...r, probes: probesOf(r) }))
      .filter((r) => r.probes.length),
  );
  const placed = $derived(floorsOf(rooms, plan, roomNamed));
  const floors = $derived(
    placed.sections.map((s) => ({ ...s, rooms: rooms.filter((r) => placed.of.get(r.name) === s.id) })).filter((s) => s.rooms.length),
  );

  // Outside: the weather's temperature, when the house says where it is.
  const weather = $derived(home.config?.outdoor?.weather);
  const outPoint = $derived.by(() => {
    const d = hub.devices[weather];
    const key = tempKey(d);
    return key ? `${weather}/${key}` : null;
  });

  // The history of every point shown, read again every five minutes.
  let lines = $state({});
  let frame = $state({ from: Date.now() - 24 * 3_600_000, to: Date.now() });
  // As one text: a device's new reading changes no point to read.
  const points = $derived([...new Set([...rooms.flatMap((r) => r.probes.map((p) => p.point)), ...(outPoint ? [outPoint] : [])])].sort().join('\n'));
  let loaded = $state(false);

  async function load(list, span) {
    const to = Date.now();
    const got = await Promise.all(
      list.map((point) =>
        fetch(`/api/history?point=${encodeURIComponent(point)}&hours=${span}&points=240`)
          .then((r) => (r.ok ? r.json() : null))
          .then((series) => [point, lineOf(series)])
          .catch(() => [point, []]),
      ),
    );
    lines = Object.fromEntries(got);
    frame = { from: to - span * 3_600_000, to };
    loaded = true;
  }
  $effect(() => {
    const list = points ? points.split('\n') : [];
    const span = hours;
    if (!list.length) return;
    load(list, span);
    const timer = setInterval(() => load(list, span), 5 * 60_000);
    return () => clearInterval(timer);
  });

  const outdoor = $derived(outside && outPoint ? (lines[outPoint] ?? []) : null);
  // One scale for every room (outside included when shown).
  const scale = $derived(scaleOf([...rooms.flatMap((r) => r.probes.map((p) => lines[p.point] ?? [])), ...(outdoor ? [outdoor] : [])]));
  let hover = $state(null);
</script>

<div class="temperatures">
  <header class="head">
    <div>
      <h1 class="page-title">{t('maison.temperatures.titre')}</h1>
      <p class="page-sub">{t('maison.temperatures.sous_titre')}</p>
    </div>
    <div class="controls">
      <div class="seg" role="group" aria-label={t('maison.temperatures.periode')}>
        {#each RANGES as [key, , label] (key)}
          <button class:on={range === key} onclick={() => pick(key)}>{t(label)}</button>
        {/each}
      </div>
      {#if outPoint}
        <button class="toggle" class:on={outside} aria-pressed={outside} onclick={toggleOutside}>
          <i></i>{t('maison.temperatures.comparer_dehors')}
        </button>
      {/if}
    </div>
  </header>

  {#if !rooms.length}
    <p class="muted">{t('maison.temperatures.vide')}</p>
  {:else if !loaded}
    <p class="muted">{t('maison.temperatures.chargement')}</p>
  {:else}
    {#each floors as floor (floor.id)}
      <section class="floor" aria-label={floor.name ?? t('maison.temperatures.titre')}>
        {#if floors.length > 1 && floor.name}<h2 class="floor-title">{floor.name}</h2>{/if}
        <div class="grid">
          {#each floor.rooms as room (room.name)}
            <TempChart
              name={room.name}
              icon={room.icon}
              lines={room.probes.map((p, i) => ({ id: p.point, name: nameOf(p.id), color: seriesColor(i), line: lines[p.point] ?? [] }))}
              {outdoor}
              {scale}
              from={frame.from}
              to={frame.to}
              {range}
              bind:hover />
          {/each}
        </div>
      </section>
    {/each}
  {/if}
</div>

<style>
  .temperatures {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    justify-content: space-between;
    gap: 12px 16px;
  }

  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
  }

  .seg {
    display: inline-flex;
    padding: 4px;
    background: var(--surface-2);
    border-radius: 999px;
    gap: 2px;
  }

  .seg button {
    border: 0;
    background: none;
    border-radius: 999px;
    padding: 7px 12px;
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-3);
    cursor: pointer;
  }

  .seg button.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  /* Outside on the charts: a dashed key that fills when on. */
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    border: 0;
    border-radius: 999px;
    padding: 8px 14px;
    background: var(--surface-2);
    color: var(--ink-3);
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
  }

  .toggle i {
    width: 16px;
    height: 2px;
    background: repeating-linear-gradient(90deg, currentColor 0 4px, transparent 4px 7px);
  }

  .toggle.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .floor {
    display: grid;
    gap: 12px;
  }

  .floor-title {
    margin: 4px 0 0;
    font-size: 13px;
    font-weight: 750;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-3);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 330px), 1fr));
    gap: 16px;
  }

  @media (max-width: 560px) {
    .temperatures,
    .grid {
      gap: 12px;
    }

    .seg button {
      padding: 6px 10px;
      font-size: 12px;
    }
  }
</style>
