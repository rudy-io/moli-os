<script>
  import { hub, setLabel } from '../lib/hub.svelte.js';
  import { deviceName, isPrimary } from '../lib/format.js';
  import { t } from '../lib/i18n.svelte.js';
  import Point from './Point.svelte';

  let { device } = $props();

  const primary = $derived(device.points.filter(isPrimary));
  const secondary = $derived(device.points.filter((p) => !isPrimary(p) && p.semantic !== 'battery'));
  const battery = $derived(device.points.find((p) => p.semantic === 'battery'));
  const batteryLevel = $derived(battery ? device.state[battery.key]?.value : null);
  const rooms = $derived(
    [...new Set(Object.values(hub.devices).map((d) => d.label?.room).filter(Boolean))].sort(),
  );

  let editing = $state(false);
  let name = $state('');
  let room = $state('');

  function edit() {
    name = device.label?.name ?? '';
    room = device.label?.room ?? '';
    editing = true;
  }

  async function save(e) {
    e.preventDefault();
    editing = false;
    await setLabel(device.id, { name, room });
  }
</script>

<article class:offline={device.online === false}>
  <header>
    {#if editing}
      <form onsubmit={save}>
        <!-- svelte-ignore a11y_autofocus -->
        <input bind:value={name} placeholder={deviceName({ ...device, label: {} })} aria-label={t('systeme.appareil.nom')} autofocus />
        <input bind:value={room} list="rooms-{device.id}" placeholder={t('systeme.appareil.piece')} aria-label={t('systeme.appareil.piece')} />
        <datalist id="rooms-{device.id}">
          {#each rooms as r (r)}<option value={r}></option>{/each}
        </datalist>
        <div class="actions">
          <button type="submit">{t('systeme.appareil.enregistrer')}</button>
          <button type="button" class="ghost" onclick={() => (editing = false)}>{t('commun.annuler')}</button>
        </div>
      </form>
    {:else}
      <button class="name" onclick={edit} title={t('systeme.appareil.renommer')}>{deviceName(device)}</button>
      <div class="meta">
        {#if batteryLevel != null}
          <span class="battery num" class:low={batteryLevel <= 20} title={t('systeme.appareil.pile')}>{batteryLevel}%</span>
        {/if}
        {#if device.online === false}<span class="badge off">{t('systeme.appareil.hors_ligne')}</span>{/if}
      </div>
    {/if}
  </header>

  <div class="points">
    {#each primary as point (point.key)}
      <Point {device} {point} big={!point.access.write} />
    {/each}
    {#if primary.length === 0}
      <p class="none">{t('systeme.appareil.aucune_mesure')}</p>
    {/if}
  </div>

  {#if secondary.length}
    <details>
      <summary>{t('systeme.appareil.details')} <span class="num">{secondary.length}</span></summary>
      <div class="points small">
        {#each secondary as point (point.key)}
          <Point {device} {point} />
        {/each}
      </div>
    </details>
  {/if}

  <footer>
    <span>{[device.manufacturer, device.model].filter(Boolean).join(' · ')}</span>
    <span class="mono" title={t('systeme.appareil.identite')}>{device.id}</span>
  </footer>
</article>

<style>
  article {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px 16px 12px;
    background: var(--card);
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
  }
  .offline {
    border-color: var(--line);
    opacity: 0.7;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 8px;
  }
  .name {
    all: unset;
    cursor: text;
    font-size: 16px;
    font-weight: 700;
    line-height: 1.25;
    border-bottom: 1px dashed transparent;
  }
  .name:hover,
  .name:focus-visible {
    border-bottom-color: var(--ink-3);
  }
  .meta {
    display: flex;
    gap: 6px;
    flex: none;
  }
  .battery,
  .badge {
    font-size: 11px;
    font-weight: 700;
    padding: 2px 6px;
    border-radius: 3px;
    border: 1.5px solid var(--line);
    color: var(--ink-2);
  }
  .battery.low,
  .badge.off {
    border-color: var(--alarm);
    color: var(--alarm);
  }
  .points {
    display: grid;
    gap: 2px;
  }
  .none {
    margin: 0;
    color: var(--ink-3);
    font-size: 13px;
  }
  details {
    border-top: 1px solid var(--line);
    padding-top: 6px;
  }
  summary {
    cursor: pointer;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-3);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  summary .num {
    font-weight: 500;
  }
  .small {
    margin-top: 6px;
  }
  footer {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    margin-top: auto;
    padding-top: 8px;
    border-top: 1px solid var(--line);
    color: var(--ink-3);
    font-size: 11.5px;
  }
  footer span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  form {
    display: grid;
    gap: 6px;
    width: 100%;
  }
  form input {
    border: var(--stroke) solid var(--line);
    border-radius: var(--radius);
    background: var(--bg);
    padding: 5px 8px;
  }
  .actions {
    display: flex;
    gap: 6px;
  }
  .actions button {
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    background: var(--sun);
    color: #161512;
    font-weight: 700;
    padding: 4px 10px;
    cursor: pointer;
  }
  .actions .ghost {
    background: transparent;
    color: var(--ink);
    border-color: var(--line);
  }
</style>
