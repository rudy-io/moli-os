<script>
  import { device, value, act, nameOf, num, positionPoint, coverOrder } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** A shutter: open, stop, close (whatever the device offers), and, when
   *  its opening can be ordered, a slider. */
  let { id, name = null } = $props();

  const d = $derived(device(id));
  const label = $derived(nameOf(id, name));
  // How far it is open: the `position` sense, else Tuya's `percent_control`.
  const posPoint = $derived(positionPoint(d) ?? d?.points.find((p) => p.key === 'percent_control') ?? null);
  const pos = $derived(posPoint ? value(id, posPoint.key) : null);
  // The order point: Tuya `control` (open), Zigbee2MQTT `state` (OPEN).
  const order = $derived(d?.points.find((p) => p.key === 'control') ?? coverOrder(d));
  const offers = $derived(order?.kind.values ?? []);
  // The slider only for the `position` sense: Tuya's scale is left alone.
  const settable = $derived(posPoint?.semantic === 'position' && posPoint.access?.write);
  const moving = $derived(value(id, 'moving'));
  // `UP` / `DOWN` (Lonsonho) say the way; `forward` / `reverse` (SONOFF) do not, so only « moving ».
  const going = $derived(
    moving === 'UP'
      ? t('commun.volet.monte')
      : moving === 'DOWN'
        ? t('commun.volet.descend')
        : moving && String(moving).toLowerCase() !== 'stop'
          ? t('commun.volet.mouvement')
          : '',
  );
  let dragging = $state(null);
  const shown = $derived(dragging ?? pos);
  const status = $derived(
    [shown != null ? t('commun.volet.ouvert_a', { pct: num(shown) }) : '', going].filter(Boolean).join(' · '),
  );
  // [command, catalogue key of its button]
  const BUTTONS = [
    ['open', 'commun.volet.ouvrir'],
    ['stop', 'commun.volet.stop'],
    ['close', 'commun.volet.fermer'],
  ];
  // The command as the device spells it: `open` on Tuya, `OPEN` on Zigbee2MQTT.
  const spelled = (cmd) => offers.find((v) => v.toLowerCase() === cmd);

  function commit(e) {
    const pct = Math.round(Number(e.currentTarget.value));
    dragging = null;
    act(`${id}/${posPoint.key}`, pct, t('commun.action', { label, action: t('commun.volet.ouvert_a', { pct: num(pct) }).toLowerCase() }));
  }
</script>

<div class="cover">
  <Icon name="shutter" size={22} />
  <span class="name">{label}<small class="muted">{status}</small></span>
  {#each BUTTONS.filter(([cmd]) => spelled(cmd)) as [cmd, key] (cmd)}
    <button onclick={() => act(`${id}/${order.key}`, spelled(cmd), t('commun.action', { label, action: t(key).toLowerCase() }))}>{t(key)}</button>
  {/each}
  {#if settable}
    <input
      class="level"
      type="range"
      min="0"
      max="100"
      step="1"
      value={shown ?? 0}
      oninput={(e) => (dragging = Number(e.currentTarget.value))}
      onchange={commit}
      aria-label={t('commun.volet.ouverture', { label })} />
  {/if}
</div>

<style>
  .cover {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    background: var(--surface-2);
    border-radius: var(--r-md);
    color: var(--ink-2);
    flex-wrap: wrap;
  }

  .name {
    flex: 1;
    display: grid;
    font-weight: 650;
    color: var(--ink);
    min-width: 120px;
  }

  small {
    font-weight: 500;
  }

  .level {
    flex: 1 1 100%;
    min-width: 0;
    margin: 0;
    accent-color: var(--warm);
    height: 28px;
  }

  button {
    border: 0;
    background: var(--surface);
    border-radius: 999px;
    padding: 8px 12px;
    font-size: 13px;
    font-weight: 650;
    box-shadow: var(--shadow);
  }
</style>
