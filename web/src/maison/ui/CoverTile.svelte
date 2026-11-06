<script>
  import {
    mdiChevronDoubleUp,
    mdiChevronUp,
    mdiChevronUpCircleOutline,
    mdiSquare,
    mdiSquareOutline,
    mdiChevronDoubleDown,
    mdiChevronDown,
    mdiChevronDownCircleOutline,
  } from '@mdi/js';
  import { device, value, act, nameOf, num, pending, reachable, coverPosition, coverPositionKey } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { hub } from '../../lib/hub.svelte.js';
  import Icon from './Icon.svelte';
  import ShutterIcon from './ShutterIcon.svelte';
  import Slider from './Slider.svelte';
  import { coverCommands, hasSlowCoverControl } from '../lib/cover-controls.js';
  import { canWrite } from '../lib/capabilities.js';

  /** A shutter: tap to toggle open/close, advanced controls for precise positioning. */
  let { id, name = null, compact = false } = $props();

  const d = $derived(device(id));
  const available = $derived(!!d && d.online !== false);
  const connected = $derived(hub.connected === true);
  const live = $derived(available && connected);
  const ok = $derived(reachable(id));
  const label = $derived(nameOf(id, name));

  const hasSlow = $derived(hasSlowCoverControl(d));

  const pos = $derived(coverPosition(id));
  const posKey = $derived(coverPositionKey(id));
  const positionWritable = $derived(connected && posKey && canWrite(d, posKey));
  const busy = $derived(pending(id, posKey) || pending(id, 'control'));

  const moving = $derived(value(id, 'moving'));
  const going = $derived(
    moving === 'UP' ? t('commun.volet.monte')
      : moving === 'DOWN' ? t('commun.volet.descend')
        : moving && String(moving).toLowerCase() !== 'stop' ? t('commun.volet.mouvement') : '',
  );

  const isClosed = $derived(pos === 0);
  const isFullyOpen = $derived(pos === 100);
  const isMoving = $derived(!!going);
  const isOpen = $derived(pos != null && pos >= 50);

  const status = $derived.by(() => {
    const parts = [];
    if (pos != null) parts.push(t('commun.volet.ouvert_a', { pct: num(pos) }));
    if (going) parts.push(going);
    return parts.join(' · ');
  });

  function tap() {
    const cmd = isOpen ? 'close' : 'open';
    const commands = coverCommands(d, 'normal');
    const action = commands.find(c => c.command === cmd);
    if (action?.value && canWrite(d, action.key)) {
      act(`${id}/${action.key}`, action.value, t('commun.action', { label, action: t(isOpen ? 'commun.volet.fermer' : 'commun.volet.ouvrir').toLowerCase() }));
    }
  }

  function sendOrderWithSpeed(command, speed, labelKey) {
    const commands = coverCommands(d, speed);
    const action = commands.find(c => c.command === command);
    if (action?.value && connected && canWrite(d, action.key)) {
      act(`${id}/${action.key}`, action.value, t('commun.action', { label, action: t(labelKey).toLowerCase() }));
    }
  }

  function sendOrder(command, labelKey) {
    sendOrderWithSpeed(command, 'normal', labelKey);
  }
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

  <div class="controls">
    {#if isClosed}
      <!-- Volet fermé : ouvrir normal, ouvrir lent, impulsion haut -->
      <button class="action-btn" onclick={() => sendOrderWithSpeed('open', 'normal', 'commun.volet.ouvrir')}
        title={t('commun.volet.ouvrir')} aria-label={t('commun.volet.ouvrir')}>
        <Icon path={mdiChevronDoubleUp} size={20} />
      </button>
      {#if hasSlow}
        <button class="action-btn" onclick={() => sendOrderWithSpeed('open', 'slow', 'commun.volet.ouvrir')}
          title={t('commun.volet.vitesse_lente')} aria-label={`${t('commun.volet.ouvrir')} ${t('commun.volet.vitesse_lente')}`}>
          <Icon path={mdiChevronUp} size={20} />
        </button>
      {/if}
      <button class="action-btn" onclick={() => sendOrder('cycle', 'commun.volet.impulsion')}
        title={t('commun.volet.impulsion')} aria-label={t('commun.volet.impulsion')}>
        <Icon path={mdiChevronUpCircleOutline} size={20} />
      </button>
    {:else if isMoving}
      <!-- En mouvement : stop, impulsion -->
      <button class="action-btn" onclick={() => sendOrder('stop', 'commun.volet.stop')}
        title={t('commun.volet.stop')} aria-label={t('commun.volet.stop')}>
        <Icon path={mdiSquare} size={20} />
      </button>
      <button class="action-btn" onclick={() => sendOrder('cycle', 'commun.volet.impulsion')}
        title={t('commun.volet.impulsion')} aria-label={t('commun.volet.impulsion')}>
        <Icon path={mdiSquareOutline} size={20} />
      </button>
    {:else if isFullyOpen}
      <!-- Volet ouvert : fermer normal, fermer lent, impulsion bas -->
      <button class="action-btn" onclick={() => sendOrderWithSpeed('close', 'normal', 'commun.volet.fermer')}
        title={t('commun.volet.fermer')} aria-label={t('commun.volet.fermer')}>
        <Icon path={mdiChevronDoubleDown} size={20} />
      </button>
      {#if hasSlow}
        <button class="action-btn" onclick={() => sendOrderWithSpeed('close', 'slow', 'commun.volet.fermer')}
          title={t('commun.volet.vitesse_lente')} aria-label={`${t('commun.volet.fermer')} ${t('commun.volet.vitesse_lente')}`}>
          <Icon path={mdiChevronDown} size={20} />
        </button>
      {/if}
      <button class="action-btn" onclick={() => sendOrder('cycle', 'commun.volet.impulsion')}
        title={t('commun.volet.impulsion')} aria-label={t('commun.volet.impulsion')}>
        <Icon path={mdiChevronDownCircleOutline} size={20} />
      </button>
    {:else}
      <!-- Position intermédiaire : tous les boutons -->
      <button class="action-btn" onclick={() => sendOrderWithSpeed('open', 'normal', 'commun.volet.ouvrir')}
        title={t('commun.volet.ouvrir')} aria-label={t('commun.volet.ouvrir')}>
        <Icon path={mdiChevronDoubleUp} size={20} />
      </button>
      {#if hasSlow}
        <button class="action-btn" onclick={() => sendOrderWithSpeed('open', 'slow', 'commun.volet.ouvrir')}
          title={t('commun.volet.vitesse_lente')} aria-label={`${t('commun.volet.ouvrir')} ${t('commun.volet.vitesse_lente')}`}>
          <Icon path={mdiChevronUp} size={20} />
        </button>
      {/if}
      <button class="action-btn" onclick={() => sendOrder('stop', 'commun.volet.stop')}
        title={t('commun.volet.stop')} aria-label={t('commun.volet.stop')}>
        <Icon path={mdiSquare} size={20} />
      </button>
      {#if hasSlow}
        <button class="action-btn" onclick={() => sendOrderWithSpeed('close', 'slow', 'commun.volet.fermer')}
          title={t('commun.volet.vitesse_lente')} aria-label={`${t('commun.volet.fermer')} ${t('commun.volet.vitesse_lente')}`}>
          <Icon path={mdiChevronDown} size={20} />
        </button>
      {/if}
      <button class="action-btn" onclick={() => sendOrderWithSpeed('close', 'normal', 'commun.volet.fermer')}
        title={t('commun.volet.fermer')} aria-label={t('commun.volet.fermer')}>
        <Icon path={mdiChevronDoubleDown} size={20} />
      </button>
    {/if}
  </div>

  {#if positionWritable}
    <div class="slider-row">
      <Slider
        min={0}
        max={100}
        step={1}
        value={pos ?? 0}
        label={t('commun.volet.position_label', { label })}
        oncommit={(v) => act(`${id}/${posKey}`, v, t('commun.action', { label, action: t('commun.volet.position') }))} />
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

  .controls {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
    margin: 0 16px 12px;
  }

  .slider-row {
    margin: 0 16px 16px;
  }

  .action-btn {
    all: unset;
    box-sizing: border-box;
    flex: none;
    width: 44px;
    height: 44px;
    border-radius: 50%;
    background: var(--surface);
    border: 1px solid var(--line);
    color: var(--ink);
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
    .action-btn {
      transition: none;
    }
    .busy :global(.shutter-icon) {
      animation: none;
    }
  }
</style>
