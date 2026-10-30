<script>
  import { mdiSpeedometerMedium, mdiSpeedometerSlow } from '@mdi/js';
  import { device, value, act, nameOf, num, pending, reachable, coverPosition, coverPositionKey } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { hub } from '../../lib/hub.svelte.js';
  import Icon from './Icon.svelte';
  import ShutterIcon from './ShutterIcon.svelte';
  import Slider from './Slider.svelte';
  import { coverCommands, coverPositionTarget, hasSlowCoverControl } from '../lib/cover-controls.js';
  import { canWrite } from '../lib/capabilities.js';

  /** A shutter: tap to toggle open/close, advanced controls for precise positioning. */
  let { id, name = null, compact = false } = $props();

  const d = $derived(device(id));
  const available = $derived(!!d && d.online !== false);
  const connected = $derived(hub.connected === true);
  const live = $derived(available && connected);
  const ok = $derived(reachable(id));
  const label = $derived(nameOf(id, name));

  // Speed control
  let speedChoice = $state(null);
  const speed = $derived(speedChoice?.id === id ? speedChoice.speed : 'normal');
  const hasSlow = $derived(hasSlowCoverControl(d));

  const pos = $derived(coverPosition(id));
  const posKey = $derived(coverPositionKey(id));
  const busy = $derived(pending(id, posKey) || pending(id, 'control'));
  const offers = $derived(d?.points.find((p) => p.key === 'control')?.kind.values ?? []);

  const moving = $derived(value(id, 'moving'));
  const going = $derived(
    moving === 'UP' ? t('commun.volet.monte')
      : moving === 'DOWN' ? t('commun.volet.descend')
        : moving && String(moving).toLowerCase() !== 'stop' ? t('commun.volet.mouvement') : '',
  );

  const isOpen = $derived(pos != null && pos >= 50);
  const status = $derived.by(() => {
    const parts = [];
    if (pos != null) parts.push(t('commun.volet.ouvert_a', { pct: num(pos) }));
    if (going) parts.push(going);
    return parts.join(' · ');
  });

  function selectSpeed(next) {
    if (next === 'normal' || next === 'slow' && hasSlow) {
      speedChoice = { id, speed: next };
    }
  }

  function tap() {
    const cmd = isOpen ? 'close' : 'open';
    if (!offers.includes(cmd)) return;
    const action = t(isOpen ? 'commun.volet.fermer' : 'commun.volet.ouvrir').toLowerCase();
    act(`${id}/control`, cmd, t('commun.action', { label, action }));
  }

  function sendOrder(command, key) {
    if (!offers.includes(command)) return;
    act(`${id}/control`, command, t('commun.action', { label, action: t(key).toLowerCase() }));
  }

  // [command sent to the device, catalogue key of its button, its icon]
  const BUTTONS = [
    ['open', 'commun.volet.ouvrir', 'chevron-up'],
    ['stop', 'commun.volet.stop', 'stop'],
    ['close', 'commun.volet.fermer', 'chevron-down'],
    ['cycle', 'commun.volet.impulsion', 'power'],
  ];
</script>

<div class="tile" class:open={isOpen} class:busy class:off-grid={!ok || !d} class:compact>
  <button class="hit" onclick={tap} disabled={!d || !ok} aria-pressed={isOpen} aria-label={t('commun.action', { label, action: status })}>
    <ShutterIcon {id} size={compact ? 40 : 46} />
    <span class="text">
      <b>{label}</b>
      <small>{status}</small>
    </span>
    <span class="switch" aria-hidden="true"><i></i></span>
  </button>

  {#if hasSlow || speed === 'slow'}
    <div class="speed" role="group" aria-label={t('commun.volet.vitesse_prochaine')}>
      <span class="speed-label">{t('commun.volet.vitesse')}</span>
      <div class="segments">
        <button type="button" class="speed-btn" aria-pressed={speed === 'normal'} onclick={() => selectSpeed('normal')}
          title={t('commun.volet.vitesse_normale')} aria-label={t('commun.volet.vitesse_normale')}>
          <Icon path={mdiSpeedometerMedium} size={18} />
        </button>
        <button type="button" class="speed-btn" aria-pressed={speed === 'slow'} disabled={!hasSlow} onclick={() => selectSpeed('slow')}
          title={t('commun.volet.vitesse_lente')} aria-label={t('commun.volet.vitesse_lente')}>
          <Icon path={mdiSpeedometerSlow} size={18} />
        </button>
      </div>
    </div>
  {/if}

  {#if posKey}
    <div class="controls">
      <Slider
        min={0}
        max={100}
        step={1}
        value={pos ?? 0}
        label={t('commun.volet.position_label', { label })}
        oncommit={(v) => act(`${id}/${posKey}`, v, t('commun.action', { label, action: t('commun.volet.position') }))} />
      {#each BUTTONS.filter(([cmd]) => offers.includes(cmd)) as [cmd, key, ico] (cmd)}
        <button class="action-btn" onclick={() => sendOrder(cmd, key)} title={t(key)} aria-label={t(key)}>
          <Icon name={ico} size={18} />
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .tile {
    position: relative;
    border-radius: var(--r-md);
    background: var(--surface-2);
    transition: background 0.35s var(--ease), box-shadow 0.35s var(--ease);
    overflow: hidden;
  }

  .tile.open {
    background: linear-gradient(155deg, var(--warm-soft) 0%, var(--surface) 70%);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--warm) 25%, transparent);
  }

  .hit {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    min-height: 112px;
    padding: 16px;
    display: grid;
    grid-template-columns: auto 1fr;
    grid-template-rows: auto 1fr;
    gap: 10px 12px;
    cursor: pointer;
    touch-action: manipulation;
    transition: background-color 160ms ease, transform 160ms ease;
  }

  .compact .hit {
    min-height: 76px;
    grid-template-columns: auto 1fr auto;
    grid-template-rows: auto;
    align-items: center;
  }

  .hit:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: -3px;
    border-radius: var(--r-md);
  }

  .text {
    grid-column: 1 / -1;
    align-self: end;
    display: grid;
    gap: 2px;
  }

  .compact .text {
    grid-column: auto;
    align-self: center;
  }

  .text b {
    font-weight: 650;
    font-size: 16px;
  }

  .text small {
    color: var(--ink-3);
    font-size: 13px;
    font-weight: 550;
  }

  .open .text small {
    color: var(--warm-ink);
  }

  /* Toggle switch top right */
  .switch {
    position: absolute;
    top: 18px;
    right: 16px;
    width: 40px;
    height: 24px;
    border-radius: 999px;
    background: var(--surface-3);
    transition: background 0.3s var(--ease);
  }

  .compact .switch {
    position: relative;
    top: auto;
    right: auto;
  }

  .switch i {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 0.3s var(--ease);
  }

  .open .switch {
    background: var(--warm);
  }

  .open .switch i {
    transform: translateX(16px);
  }

  .busy :global(.shutter-icon) {
    animation: pulse 1s ease-in-out infinite;
  }

  @keyframes pulse {
    50% {
      opacity: 0.55;
    }
  }

  .off-grid {
    opacity: 0.55;
  }

  .speed {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin: 0 16px 12px;
  }

  .speed-label {
    font-size: 12px;
    color: var(--ink-2);
    font-weight: 550;
  }

  .segments {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--r-sm);
    background: var(--surface-3);
  }

  .speed-btn {
    all: unset;
    box-sizing: border-box;
    width: 34px;
    height: 34px;
    display: grid;
    place-items: center;
    cursor: pointer;
    border-radius: var(--r-xs);
    transition: all 0.2s var(--ease);
  }

  .speed-btn[aria-pressed="true"] {
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 12%);
  }

  .speed-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 12px;
    margin: -6px 16px 16px;
  }

  .action-btn {
    all: unset;
    box-sizing: border-box;
    flex: none;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink-2);
    display: grid;
    place-items: center;
    cursor: pointer;
    transition: all 0.2s var(--ease);
  }

  .action-btn:hover {
    background: var(--surface-3);
    transform: scale(1.05);
  }

  .action-btn:active {
    transform: scale(0.98);
  }

  .action-btn:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }

  @media (prefers-reduced-motion: reduce) {
    .hit,
    .switch,
    .switch i,
    .action-btn,
    .speed-btn {
      transition: none;
    }
    .busy :global(.shutter-icon) {
      animation: none;
    }
  }
</style>
