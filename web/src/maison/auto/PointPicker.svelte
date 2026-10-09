<script>
  import { hub, roomOf, nameOf, value } from '../lib/home.svelte.js';
  import { pointName, splitPoint, valueText } from '../lib/auto.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** Choosing a device and one of its values, by name and room.
   *  `mode`: write (it can be set), number, any. */
  let { point = '', mode = 'any', onpick } = $props();

  // Open at first when nothing is chosen yet (only then).
  // svelte-ignore state_referenced_locally
  let open = $state(!point);
  let search = $state('');
  let expanded = $state(null);

  const POWER = ['on', 'on_off', 'state', 'switch', 'switch_1', 'power'];
  const fits = (p) =>
    mode === 'write' ? p.access?.write : mode === 'number' ? p.kind.type === 'numeric' : true;
  const norm = (s) => String(s ?? '').normalize('NFD').replace(/\p{M}/gu, '').toLowerCase();

  const devices = $derived.by(() => {
    const q = norm(search);
    return Object.values(hub.devices)
      .map((d) => ({ d, points: d.points.filter(fits), room: roomOf(d) ?? t('automatismes.picker.sans_piece') }))
      .filter((x) => x.points.length)
      .filter((x) => !q || norm(`${nameOf(x.d.id)} ${x.room} ${x.points.map((p) => p.label).join(' ')}`).includes(q))
      .sort((a, b) => a.room.localeCompare(b.room, locale()) || nameOf(a.d.id).localeCompare(nameOf(b.d.id), locale()));
  });

  function choose(d, points) {
    // One value, or an obvious on/off: pick it straight away.
    const power = points.find((p) => POWER.includes(p.key) || p.semantic === 'on_off');
    if (points.length === 1 || (mode === 'write' && power && points.length <= 3)) {
      pick(`${d.id}/${(points.length === 1 ? points[0] : power).key}`);
    } else {
      expanded = expanded === d.id ? null : d.id;
    }
  }

  function pick(p) {
    onpick?.(p);
    open = false;
    search = '';
    expanded = null;
  }

  const current = $derived(point ? splitPoint(point) : null);
</script>

<div class="picker">
  <button class="chosen" class:empty={!point} onclick={() => (open = !open)}>
    <Icon name={point ? 'tune' : 'plus'} size={18} />
    <span>{point ? pointName(point) : t('automatismes.picker.choisir')}</span>
    {#if current}<small class="muted">{valueText(point, value(current[0], current[1]))}</small>{/if}
  </button>
  {#if open}
    <div class="list">
      <input placeholder={t('automatismes.picker.chercher')} bind:value={search} aria-label={t('automatismes.picker.chercher_aria')} />
      <ul>
        {#each devices as { d, points, room }, i (d.id)}
          {#if i === 0 || devices[i - 1].room !== room}<li class="room">{room}</li>{/if}
          <li>
            <button class="device" class:off={d.online === false} onclick={() => choose(d, points)}>
              <b>{nameOf(d.id)}</b>
              {#if d.online === false}<small>{t('automatismes.picker.injoignable')}</small>{/if}
              {#if points.length > 1}<Icon name={expanded === d.id ? 'chevron-up' : 'chevron-down'} size={16} />{/if}
            </button>
            {#if expanded === d.id}
              <ul class="points">
                {#each points as p (p.key)}
                  <li>
                    <button onclick={() => pick(`${d.id}/${p.key}`)}>
                      <span>{p.label}</span>
                      <small class="muted">{valueText(`${d.id}/${p.key}`, value(d.id, p.key))}</small>
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </li>
        {:else}
          <li class="muted none">{t('automatismes.picker.aucun')}</li>
        {/each}
      </ul>
    </div>
  {/if}
</div>

<style>
  .picker {
    display: grid;
    gap: 8px;
  }

  .chosen {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    border: 0;
    border-radius: 14px;
    background: var(--surface-2);
    color: var(--ink);
    text-align: left;
    font-weight: 600;
  }

  .chosen span {
    flex: 1;
  }

  .chosen.empty {
    color: var(--cool);
    background: var(--cool-soft);
  }

  .list {
    border-radius: 16px;
    background: var(--surface-2);
    padding: 8px;
    display: grid;
    gap: 6px;
  }

  input {
    border: 0;
    border-radius: 12px;
    padding: 10px 12px;
    background: var(--surface);
    font: inherit;
    color: var(--ink);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .list > ul {
    max-height: 280px;
    overflow-y: auto;
  }

  .room {
    font-size: 11px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--ink-3);
    font-weight: 700;
    padding: 10px 8px 4px;
  }

  .device,
  .points button {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 9px 10px;
    border: 0;
    border-radius: 10px;
    background: none;
    color: var(--ink);
    text-align: left;
  }

  .device:hover,
  .points button:hover {
    background: var(--surface);
  }

  .device b {
    flex: 1;
    font-weight: 600;
  }

  .device.off {
    opacity: 0.6;
  }

  .device small {
    font-size: 12px;
    color: var(--ink-3);
  }

  .points {
    margin: 0 0 6px 12px;
  }

  .points span {
    flex: 1;
  }

  .none {
    padding: 10px;
  }
</style>
